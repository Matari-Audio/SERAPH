use std::{collections::VecDeque, env, path::PathBuf, process::Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
};

#[derive(Clone)]
pub struct ChatgptTokens {
    pub access_token: String,
    pub account_id: String,
    pub plan_type: Option<String>,
}

pub struct LoginOption {
    pub id: String,
    pub label: String,
}

pub struct LoginProvider {
    pub provider: String,
    pub auth_type: String,
    pub label: String,
    pub signed_in: bool,
}

pub enum PiLoginEvent {
    AuthUrl {
        url: String,
        instructions: Option<String>,
    },
    DeviceCode {
        code: String,
        url: String,
    },
    Prompt {
        kind: String,
        message: String,
        placeholder: Option<String>,
        options: Vec<LoginOption>,
    },
    Progress(String),
    Complete {
        provider: String,
        tokens: Option<ChatgptTokens>,
    },
}

pub struct PiAuth {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<Result<Value>>,
    backlog: VecDeque<Value>,
    next_id: u64,
}

impl PiAuth {
    pub async fn spawn() -> Result<Self> {
        let script = env::var_os("SERAPH_PI_AUTH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("auth/pi-auth.mjs");
                if source.is_file() {
                    source
                } else {
                    env::current_exe()
                        .ok()
                        .and_then(|path| path.parent().map(|path| path.join("auth/pi-auth.mjs")))
                        .unwrap_or(source)
                }
            });
        let mut child = Command::new("node")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("start Pi auth runtime (run npm install first)")?;
        let input = child.stdin.take().context("Pi auth stdin unavailable")?;
        let output = child.stdout.take().context("Pi auth stdout unavailable")?;
        let (messages, output_rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let mut lines = BufReader::new(output).lines();
            loop {
                let message = match lines.next_line().await {
                    Ok(Some(line)) => {
                        serde_json::from_str(&line).context("decode Pi auth JSONL message")
                    }
                    Ok(None) => Err(anyhow::anyhow!("Pi auth runtime exited")),
                    Err(error) => Err(error).context("read from Pi auth"),
                };
                let done = message.is_err();
                if messages.send(message).await.is_err() || done {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output: output_rx,
            backlog: VecDeque::new(),
            next_id: 1,
        })
    }

    pub async fn tokens(&mut self) -> Result<Option<ChatgptTokens>> {
        let value = self.request("tokens", json!({})).await?;
        parse_tokens(&value)
    }

    pub async fn refresh(&mut self) -> Result<Option<ChatgptTokens>> {
        let value = self.request("tokens", json!({ "force": true })).await?;
        parse_tokens(&value)
    }

    pub async fn login_providers(&mut self) -> Result<Vec<LoginProvider>> {
        self.request("providers", json!({}))
            .await?
            .as_array()
            .context("Pi auth providers response was not an array")?
            .iter()
            .map(|provider| {
                Ok(LoginProvider {
                    provider: required_string(provider, "provider")?.to_owned(),
                    auth_type: required_string(provider, "authType")?.to_owned(),
                    label: required_string(provider, "label")?.to_owned(),
                    signed_in: provider
                        .get("signedIn")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect()
    }

    pub async fn models(&mut self, provider: &str) -> Result<Value> {
        self.request("models", json!({ "provider": provider }))
            .await
    }

    pub async fn start_login(&mut self, provider: &str, auth_type: &str) -> Result<u64> {
        self.send(
            "login",
            json!({ "provider": provider, "authType": auth_type }),
        )
        .await
    }

    pub async fn next_login_event(&mut self, id: u64) -> Result<PiLoginEvent> {
        loop {
            let message = match self.backlog.pop_front() {
                Some(message) => message,
                None => self.read().await?,
            };
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                let value = response_result(message)?;
                return Ok(PiLoginEvent::Complete {
                    provider: required_string(&value, "provider")
                        .unwrap_or("openai-codex")
                        .to_owned(),
                    tokens: value
                        .get("accessToken")
                        .is_some()
                        .then(|| parse_tokens(&value))
                        .transpose()?
                        .flatten(),
                });
            }
            match message.get("event").and_then(Value::as_str) {
                Some("auth") => match message.get("type").and_then(Value::as_str) {
                    Some("auth_url") => {
                        return Ok(PiLoginEvent::AuthUrl {
                            url: required_string(&message, "url")?.to_owned(),
                            instructions: message
                                .get("instructions")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        });
                    }
                    Some("device_code") => {
                        return Ok(PiLoginEvent::DeviceCode {
                            code: required_string(&message, "userCode")?.to_owned(),
                            url: required_string(&message, "verificationUri")?.to_owned(),
                        });
                    }
                    Some("info" | "progress") => {
                        return Ok(PiLoginEvent::Progress(
                            required_string(&message, "message")?.to_owned(),
                        ));
                    }
                    _ => {}
                },
                Some("prompt") => {
                    let prompt = message.get("prompt").context("Pi prompt omitted prompt")?;
                    let options = prompt
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|option| {
                            Ok(LoginOption {
                                id: required_string(option, "id")?.to_owned(),
                                label: required_string(option, "label")?.to_owned(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    return Ok(PiLoginEvent::Prompt {
                        kind: required_string(prompt, "type")?.to_owned(),
                        message: required_string(prompt, "message")?.to_owned(),
                        placeholder: prompt
                            .get("placeholder")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        options,
                    });
                }
                _ => {}
            }
        }
    }

    pub async fn answer_prompt(&mut self, value: &str) -> Result<()> {
        self.request("prompt", json!({ "value": value })).await?;
        Ok(())
    }

    pub async fn cancel_login(&mut self) -> Result<()> {
        self.request("cancel", json!({})).await?;
        Ok(())
    }

    pub async fn shutdown(self) -> Result<()> {
        let Self {
            mut child, input, ..
        } = self;
        drop(input);
        child.wait().await.context("wait for Pi auth runtime")?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params).await?;
        self.wait_response(id).await
    }

    async fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "id": id, "method": method });
        if let Some(object) = params.as_object() {
            message
                .as_object_mut()
                .expect("object")
                .extend(object.clone());
        }
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.input
            .write_all(&line)
            .await
            .context("write to Pi auth")?;
        self.input.flush().await.context("flush Pi auth request")?;
        Ok(id)
    }

    async fn wait_response(&mut self, id: u64) -> Result<Value> {
        loop {
            if let Some(index) = self
                .backlog
                .iter()
                .position(|message| message.get("id").and_then(Value::as_u64) == Some(id))
            {
                let message = self.backlog.remove(index).expect("queued response");
                return response_result(message);
            }
            let message = self.read().await?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return response_result(message);
            }
            self.backlog.push_back(message);
        }
    }

    async fn read(&mut self) -> Result<Value> {
        self.output.recv().await.context("Pi auth reader stopped")?
    }
}

fn response_result(message: Value) -> Result<Value> {
    if let Some(error) = message.get("error").and_then(Value::as_str) {
        bail!("Pi auth failed: {error}");
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

fn parse_tokens(value: &Value) -> Result<Option<ChatgptTokens>> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(ChatgptTokens {
        access_token: required_string(value, "accessToken")?.to_owned(),
        account_id: required_string(value, "chatgptAccountId")?.to_owned(),
        plan_type: value
            .get("chatgptPlanType")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("Pi auth omitted string field {field:?}"))
}
