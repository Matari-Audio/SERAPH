use std::{collections::HashMap, env, future::Future, pin::Pin, sync::Arc};

use agent_client_protocol::{self as acp, Client as _};
use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xai_acp_lib::{AcpClientTx, AcpGatewaySender};

use crate::{
    pi_auth::{PiAuth, PiLoginEvent},
    pi_rpc::{PiEvent, PiRpc},
    skills,
};

type SpawnFuture =
    Pin<Box<dyn Future<Output = Result<xai_grok_pager::acp::spawn::SpawnedAgent>> + Send>>;

pub fn spawn(cancel: CancellationToken) -> SpawnFuture {
    Box::pin(async move {
        let mut agent = SeraphAgent::new().await?;
        xai_grok_pager::acp::spawn::spawn_custom_agent(
            move |client_tx| {
                agent.client = AcpGatewaySender::new(client_tx);
                Ok(agent)
            },
            &cancel,
        )
        .await
    })
}

struct SeraphAgent {
    client: AcpGatewaySender<acp::AgentSide>,
    auth: Mutex<PiAuth>,
    sessions: Mutex<HashMap<acp::SessionId, SeraphSession>>,
    cancellations: Mutex<HashMap<acp::SessionId, CancellationToken>>,
    default_model: String,
    models: Vec<acp::ModelInfo>,
    authenticated: Mutex<bool>,
}

#[derive(Clone)]
struct SeraphSession {
    backend: Arc<Mutex<PiRpc>>,
    model: String,
    effort: Option<String>,
    skills: Arc<HashMap<String, xai_grok_tools::implementations::skills::types::SkillInfo>>,
}

impl SeraphAgent {
    async fn new() -> Result<Self> {
        let mut auth = PiAuth::spawn().await?;
        let authenticated = auth.tokens().await?.is_some();
        let catalog = auth.models("openai-codex").await?;
        let default_model = catalog
            .as_array()
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| {
                        model.get("id").and_then(serde_json::Value::as_str) == Some("gpt-5.6-sol")
                    })
                    .or_else(|| models.first())
            })
            .and_then(|model| model.get("id"))
            .and_then(serde_json::Value::as_str)
            .context("Pi returned no OpenAI Codex models")?
            .to_owned();
        let models = acp_model_catalog(&catalog)?;
        Ok(Self {
            client: AcpGatewaySender::new(dummy_client()),
            auth: Mutex::new(auth),
            sessions: Mutex::new(HashMap::new()),
            cancellations: Mutex::new(HashMap::new()),
            default_model,
            models,
            authenticated: Mutex::new(authenticated),
        })
    }

    async fn notify(
        &self,
        session_id: acp::SessionId,
        update: acp::SessionUpdate,
    ) -> acp::Result<()> {
        self.client
            .session_notification(acp::SessionNotification::new(session_id, update))
            .await
    }

    async fn notify_ext(
        &self,
        session_id: &acp::SessionId,
        update: serde_json::Value,
    ) -> acp::Result<()> {
        let params = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": session_id.0,
            "update": update,
        }))
        .map_err(acp_error)?;
        self.client
            .ext_notification(acp::ExtNotification::new(
                "x.ai/session_notification",
                params.into(),
            ))
            .await
    }
}

fn dummy_client() -> AcpClientTx {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    tx
}

fn acp_error(error: impl std::fmt::Display) -> acp::Error {
    acp::Error::new(acp::ErrorCode::InternalError.into(), error.to_string())
}

fn prompt_text(blocks: &[acp::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            acp::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn acp_model_catalog(catalog: &serde_json::Value) -> Result<Vec<acp::ModelInfo>> {
    catalog
        .as_array()
        .context("Pi models response was not an array")?
        .iter()
        .map(|model| {
            let id = model
                .get("id")
                .and_then(serde_json::Value::as_str)
                .context("Codex model omitted id")?;
            let name = model
                .get("displayName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(id);
            let mut efforts = vec!["minimal", "low", "medium", "high", "xhigh"];
            if model.pointer("/thinkingLevelMap/max").is_some() {
                efforts.push("max");
            }
            let efforts: Vec<serde_json::Value> = efforts.into_iter().map(Into::into).collect();
            let mut info = acp::ModelInfo::new(id.to_owned(), name.to_owned());
            if model.get("reasoning").and_then(serde_json::Value::as_bool) == Some(true) {
                let mut meta = acp::Meta::new();
                meta.insert("supportsReasoningEffort".into(), true.into());
                meta.insert("reasoningEfforts".into(), efforts.into());
                meta.insert("reasoningEffort".into(), "medium".into());
                info.meta = Some(meta);
            }
            Ok(info)
        })
        .collect()
}

fn model_default_effort(models: &[acp::ModelInfo], id: &str) -> Option<String> {
    models
        .iter()
        .find(|model| model.model_id.0.as_ref() == id)?
        .meta
        .as_ref()?
        .get("reasoningEffort")?
        .as_str()
        .map(str::to_owned)
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for SeraphAgent {
    async fn initialize(&self, _: acp::InitializeRequest) -> acp::Result<acp::InitializeResponse> {
        let auth_methods = if *self.authenticated.lock().await {
            vec![]
        } else {
            vec![acp::AuthMethod::Agent(
                acp::AuthMethodAgent::new("openai-codex", "ChatGPT")
                    .description("Use your Codex subscription through Pi authentication"),
            )]
        };
        Ok(acp::InitializeResponse::new(acp::ProtocolVersion::V1)
            .agent_info(
                acp::Implementation::new("seraph", env!("CARGO_PKG_VERSION")).title("SERAPH"),
            )
            .auth_methods(auth_methods))
    }

    async fn authenticate(
        &self,
        _: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        let mut auth = self.auth.lock().await;
        let login_id = auth
            .start_login("openai-codex", "oauth")
            .await
            .map_err(acp_error)?;
        loop {
            match auth.next_login_event(login_id).await.map_err(acp_error)? {
                PiLoginEvent::AuthUrl { url, .. } => {
                    webbrowser::open(&url).map_err(acp_error)?;
                }
                PiLoginEvent::DeviceCode { url, .. } => {
                    webbrowser::open(&url).map_err(acp_error)?;
                }
                PiLoginEvent::Progress(_) => {}
                PiLoginEvent::Complete { provider, .. } if provider == "openai-codex" => break,
                PiLoginEvent::Complete { .. } => {
                    return Err(acp_error("Pi login selected the wrong provider"));
                }
                PiLoginEvent::Prompt {
                    kind,
                    message,
                    options,
                    ..
                } => {
                    let browser = options.iter().find(|option| option.id == "browser");
                    if kind == "select"
                        && message == "Select OpenAI Codex login method:"
                        && browser.is_some()
                    {
                        auth.answer_prompt(&browser.unwrap().id)
                            .await
                            .map_err(acp_error)?;
                    } else if kind == "manual_code"
                        && message.starts_with("Complete login in your browser")
                    {
                        // Pi races this fallback prompt against its localhost OAuth callback.
                        // The browser callback owns completion; no terminal input is needed.
                        continue;
                    } else {
                        return Err(acp_error(format!(
                            "Pi authentication needs unsupported input: {message}"
                        )));
                    }
                }
            }
        }
        *self.authenticated.lock().await = true;
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        if !*self.authenticated.lock().await {
            return Err(acp::Error::auth_required());
        }
        let effort = model_default_effort(&self.models, &self.default_model);
        let skills = Arc::new(skills::discover(&args.cwd));
        let backend = PiRpc::spawn(&args.cwd, &self.default_model, effort.as_deref())
            .await
            .map_err(acp_error)?;
        let mut sessions = self.sessions.lock().await;
        let session_id = acp::SessionId::new(format!("pi-{}", sessions.len() + 1));
        sessions.insert(
            session_id.clone(),
            SeraphSession {
                backend: Arc::new(Mutex::new(backend)),
                model: self.default_model.clone(),
                effort,
                skills,
            },
        );
        Ok(
            acp::NewSessionResponse::new(session_id).models(acp::SessionModelState::new(
                self.default_model.clone(),
                self.models.clone(),
            )),
        )
    }

    async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> acp::Result<acp::SetSessionModelResponse> {
        let model = args.model_id.0.to_string();
        if !self
            .models
            .iter()
            .any(|candidate| candidate.model_id == args.model_id)
        {
            return Err(acp::Error::invalid_params());
        }
        let effort = args
            .meta
            .as_ref()
            .and_then(|meta| meta.get("reasoningEffort"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .or_else(|| model_default_effort(&self.models, &model));
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(&args.session_id)
            .ok_or_else(acp::Error::invalid_params)?;
        session
            .backend
            .lock()
            .await
            .set_model(&model, effort.as_deref())
            .await
            .map_err(acp_error)?;
        session.model = model;
        session.effort = effort;
        Ok(acp::SetSessionModelResponse::new())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let session = self
            .sessions
            .lock()
            .await
            .get(&args.session_id)
            .cloned()
            .ok_or_else(acp::Error::invalid_params)?;
        let cancel = CancellationToken::new();
        self.cancellations
            .lock()
            .await
            .insert(args.session_id.clone(), cancel.clone());
        let text = skills::expand(&session.skills, prompt_text(&args.prompt))
            .await
            .map_err(acp_error)?;
        session
            .backend
            .lock()
            .await
            .prompt(&text)
            .await
            .map_err(acp_error)?;
        let mut child_streams: HashMap<String, (acp::SessionId, String)> = HashMap::new();
        let result = loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => None,
                event = async { session.backend.lock().await.next_event().await } => Some(event.map_err(acp_error)?),
            };
            let Some(event) = next else {
                session
                    .backend
                    .lock()
                    .await
                    .abort()
                    .await
                    .map_err(acp_error)?;
                break acp::StopReason::Cancelled;
            };
            match event {
                PiEvent::TextDelta(delta) => {
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(delta.into())),
                    )
                    .await?;
                }
                PiEvent::ThinkingDelta(delta) => {
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk::new(delta.into())),
                    )
                    .await?;
                }
                PiEvent::ToolStart {
                    id,
                    name,
                    args: input,
                } => {
                    if name == "seraph_agent" {
                        let child_id =
                            acp::SessionId::new(format!("{}-agent-{id}", args.session_id.0));
                        let description = input
                            .get("prompt")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|prompt| prompt.lines().next())
                            .unwrap_or("SERAPH agent")
                            .trim();
                        self.notify_ext(
                            &args.session_id,
                            serde_json::json!({
                                "sessionUpdate": "subagent_spawned",
                                "subagent_id": child_id.0,
                                "parent_session_id": args.session_id.0,
                                "child_session_id": child_id.0,
                                "subagent_type": "general-purpose",
                                "description": description,
                                "model": &session.model,
                            }),
                        )
                        .await?;
                        child_streams.insert(id.clone(), (child_id, String::new()));
                    }
                    let tool_call_id = acp::ToolCallId::from(id);
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::ToolCall(
                            acp::ToolCall::new(tool_call_id, name)
                                .status(acp::ToolCallStatus::InProgress)
                                .raw_input(Some(input)),
                        ),
                    )
                    .await?;
                }
                PiEvent::ToolUpdate { id, result } => {
                    if let Some((child_id, previous)) = child_streams.get_mut(&id)
                        && let Some(text) = result
                            .pointer("/content/0/text")
                            .and_then(serde_json::Value::as_str)
                    {
                        let delta = text.strip_prefix(previous.as_str()).unwrap_or(text);
                        if !delta.is_empty() {
                            self.notify(
                                child_id.clone(),
                                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                    delta.into(),
                                )),
                            )
                            .await?;
                        }
                        *previous = text.to_owned();
                    }
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                            acp::ToolCallId::from(id),
                            acp::ToolCallUpdateFields::new().raw_output(Some(result)),
                        )),
                    )
                    .await?;
                }
                PiEvent::ToolEnd {
                    id,
                    result,
                    is_error,
                } => {
                    if let Some((child_id, previous)) = child_streams.remove(&id) {
                        if let Some(text) = result
                            .pointer("/content/0/text")
                            .and_then(serde_json::Value::as_str)
                            && let Some(delta) = text.strip_prefix(&previous)
                            && !delta.is_empty()
                        {
                            self.notify(
                                child_id.clone(),
                                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                    delta.into(),
                                )),
                            )
                            .await?;
                        }
                        self.notify_ext(
                            &args.session_id,
                            serde_json::json!({
                                "sessionUpdate": "subagent_finished",
                                "subagent_id": child_id.0,
                                "child_session_id": child_id.0,
                                "status": if is_error { "failed" } else { "completed" },
                                "error": if is_error { result.pointer("/content/0/text").cloned() } else { None },
                                "tool_calls": 0,
                                "turns": 1,
                                "duration_ms": 0,
                                "output": result.pointer("/content/0/text").cloned(),
                            }),
                        )
                        .await?;
                    }
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                            acp::ToolCallId::from(id),
                            acp::ToolCallUpdateFields::new()
                                .status(Some(if is_error {
                                    acp::ToolCallStatus::Failed
                                } else {
                                    acp::ToolCallStatus::Completed
                                }))
                                .raw_output(Some(result)),
                        )),
                    )
                    .await?;
                }
                PiEvent::Error(error) => return Err(acp_error(error)),
                PiEvent::Settled => break acp::StopReason::EndTurn,
                PiEvent::Other => {}
            }
        };
        self.cancellations.lock().await.remove(&args.session_id);
        Ok(acp::PromptResponse::new(result))
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        if let Some(cancel) = self.cancellations.lock().await.get(&args.session_id) {
            cancel.cancel();
        }
        Ok(())
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        if args.method.as_ref() != "x.ai/commands/list" {
            return Ok(acp::ExtResponse::new(
                serde_json::value::RawValue::NULL.to_owned().into(),
            ));
        }
        let params: serde_json::Value =
            serde_json::from_str(args.params.get()).map_err(acp_error)?;
        let session_id = params
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(acp::Error::invalid_params)?;
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(&acp::SessionId::new(session_id))
            .ok_or_else(acp::Error::invalid_params)?;
        let response = serde_json::json!({ "commands": skills::commands(&session.skills) });
        Ok(acp::ExtResponse::new(
            serde_json::value::to_raw_value(&response)
                .map_err(acp_error)?
                .into(),
        ))
    }
}
