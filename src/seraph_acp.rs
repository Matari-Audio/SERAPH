use std::{collections::HashMap, env, future::Future, pin::Pin};

use agent_client_protocol::{self as acp, Client as _};
use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xai_acp_lib::{AcpClientTx, AcpGatewaySender};

use crate::{
    ToolHost,
    agents::AgentManager,
    codex::{Codex, CodexEvent, LoginEvent, ToolResult},
    execute_tool, select_model, signed_in,
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
    codex: Mutex<Codex>,
    tools: Mutex<ToolHost>,
    sessions: Mutex<HashMap<acp::SessionId, String>>,
    cancellations: Mutex<HashMap<acp::SessionId, CancellationToken>>,
    model: String,
    authenticated: Mutex<bool>,
}

impl SeraphAgent {
    async fn new() -> Result<Self> {
        let mut codex = Codex::spawn().await?;
        let authenticated = signed_in(&codex.account(false).await?);
        let (model, _, _) = select_model(&codex.models().await?)?;
        let cwd = env::current_dir().context("read current directory")?;
        Ok(Self {
            client: AcpGatewaySender::new(dummy_client()),
            codex: Mutex::new(codex),
            tools: Mutex::new(ToolHost {
                kernel: None,
                task_board: None,
                edits: Default::default(),
                next_edit_handle: 1,
                project: cwd.clone(),
                agents: AgentManager::new(cwd, None)?,
            }),
            sessions: Mutex::new(HashMap::new()),
            cancellations: Mutex::new(HashMap::new()),
            model,
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
        let mut codex = self.codex.lock().await;
        let login_id = codex
            .start_login("openai-codex", "oauth")
            .await
            .map_err(acp_error)?;
        loop {
            match codex.next_login_event(&login_id).await.map_err(acp_error)? {
                LoginEvent::AuthUrl { url, .. } => {
                    webbrowser::open(&url).map_err(acp_error)?;
                }
                LoginEvent::DeviceCode { url, .. } => {
                    webbrowser::open(&url).map_err(acp_error)?;
                }
                LoginEvent::Progress(_) => {}
                LoginEvent::Complete {
                    backend_ready: true,
                } => break,
                LoginEvent::Complete {
                    backend_ready: false,
                } => {
                    return Err(acp_error("Pi login did not activate the Codex backend"));
                }
                LoginEvent::Prompt {
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
                        codex
                            .answer_login_prompt(&browser.unwrap().id)
                            .await
                            .map_err(acp_error)?;
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
        let thread_id = self
            .codex
            .lock()
            .await
            .start_thread(&args.cwd, Some(&self.model))
            .await
            .map_err(acp_error)?;
        let session_id = acp::SessionId::new(thread_id.clone());
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), thread_id);
        Ok(
            acp::NewSessionResponse::new(session_id).models(acp::SessionModelState::new(
                self.model.clone(),
                vec![acp::ModelInfo::new(self.model.clone(), self.model.clone())],
            )),
        )
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let thread_id = self
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
        let text = prompt_text(&args.prompt);
        let turn_id = self
            .codex
            .lock()
            .await
            .start_turn(&thread_id, &text, None)
            .await
            .map_err(acp_error)?;
        let result = loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => None,
                event = async {
                    self.codex.lock().await.next_turn_event(&thread_id, &turn_id).await
                } => Some(event.map_err(acp_error)?),
            };
            let Some(event) = next else {
                self.codex
                    .lock()
                    .await
                    .interrupt_turn(&thread_id, &turn_id)
                    .await
                    .map_err(acp_error)?;
                break acp::StopReason::Cancelled;
            };
            match event {
                CodexEvent::AgentMessageDelta(delta) => {
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(delta.into())),
                    )
                    .await?;
                }
                CodexEvent::ToolCall(call) => {
                    let tool_call_id = acp::ToolCallId::from(call.id_string());
                    let title = call
                        .namespace
                        .as_ref()
                        .map_or_else(|| call.tool.clone(), |ns| format!("{ns}.{}", call.tool));
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::ToolCall(
                            acp::ToolCall::new(tool_call_id.clone(), title)
                                .status(acp::ToolCallStatus::InProgress)
                                .raw_input(Some(call.arguments.clone())),
                        ),
                    )
                    .await?;
                    let mut tools = self.tools.lock().await;
                    let execution = execute_tool(
                        &mut tools,
                        &call.thread_id,
                        &call.namespace,
                        &call.tool,
                        &call.arguments,
                    )
                    .await;
                    let (text, success) = match execution {
                        Ok(text) => (text, true),
                        Err(error) => (format!("{error:#}"), false),
                    };
                    self.notify(
                        args.session_id.clone(),
                        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                            tool_call_id,
                            acp::ToolCallUpdateFields::new()
                                .status(Some(if success {
                                    acp::ToolCallStatus::Completed
                                } else {
                                    acp::ToolCallStatus::Failed
                                }))
                                .raw_output(Some(serde_json::Value::String(text.clone()))),
                        )),
                    )
                    .await?;
                    self.codex
                        .lock()
                        .await
                        .respond_tool(call, ToolResult { text, success })
                        .await
                        .map_err(acp_error)?;
                }
                CodexEvent::TurnError(error) => return Err(acp_error(error)),
                CodexEvent::TurnCompleted(turn) => {
                    break if turn.get("status").and_then(serde_json::Value::as_str)
                        == Some("interrupted")
                    {
                        acp::StopReason::Cancelled
                    } else {
                        acp::StopReason::EndTurn
                    };
                }
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
}
