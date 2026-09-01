use std::{collections::VecDeque, env, path::Path, process::Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

use crate::pi_auth::{ChatgptTokens, LoginOption, PiAuth, PiLoginEvent};

pub enum CodexEvent {
    AgentMessageDelta(String),
    ToolCall(ToolCall),
    TurnError(String),
    TurnCompleted(Value),
}

pub enum LoginEvent {
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
    Complete,
}

pub struct ToolCall {
    id: Value,
    pub thread_id: String,
    pub namespace: Option<String>,
    pub tool: String,
    pub arguments: Value,
}

pub struct ToolResult {
    pub text: String,
    pub success: bool,
}

pub struct Codex {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    backlog: VecDeque<Value>,
    next_id: u64,
    auth: PiAuth,
}

impl Codex {
    pub async fn spawn() -> Result<Self> {
        let auth = PiAuth::spawn().await?;
        let executable = env::var_os("SERAPH_CODEX").unwrap_or_else(|| "codex".into());
        let codex_home = env::var_os("SERAPH_CODEX_HOME")
            .map(Into::into)
            .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".seraph/codex")))
            .context("HOME is unavailable; set SERAPH_CODEX_HOME")?;
        tokio::fs::create_dir_all(&codex_home)
            .await
            .context("create isolated SERAPH Codex home")?;
        let mut child = Command::new(executable)
            .env("CODEX_HOME", codex_home)
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("start Codex app-server")?;
        let input = child.stdin.take().context("Codex stdin unavailable")?;
        let output = child.stdout.take().context("Codex stdout unavailable")?;
        let mut codex = Self {
            child,
            input,
            output: BufReader::new(output).lines(),
            backlog: VecDeque::new(),
            next_id: 1,
            auth,
        };

        codex
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "seraph",
                        "title": "SERAPH",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": { "experimentalApi": true },
                }),
            )
            .await?;
        codex
            .write(json!({ "method": "initialized", "params": {} }))
            .await?;
        if let Some(tokens) = codex.auth.tokens().await? {
            codex.install_tokens(&tokens).await?;
        }
        Ok(codex)
    }

    pub async fn account(&mut self, refresh_token: bool) -> Result<Value> {
        self.request("account/read", json!({ "refreshToken": refresh_token }))
            .await
    }

    pub async fn start_chatgpt_login(&mut self, method: &str) -> Result<String> {
        Ok(self.auth.start_login(method).await?.to_string())
    }

    pub async fn cancel_login(&mut self, _login_id: &str) -> Result<()> {
        self.auth.cancel_login().await
    }

    pub async fn next_login_event(&mut self, login_id: &str) -> Result<LoginEvent> {
        let id = login_id.parse().context("invalid Pi login id")?;
        Ok(match self.auth.next_login_event(id).await? {
            PiLoginEvent::AuthUrl { url, instructions } => {
                LoginEvent::AuthUrl { url, instructions }
            }
            PiLoginEvent::DeviceCode { code, url } => LoginEvent::DeviceCode { code, url },
            PiLoginEvent::Prompt {
                kind,
                message,
                placeholder,
                options,
            } => LoginEvent::Prompt {
                kind,
                message,
                placeholder,
                options,
            },
            PiLoginEvent::Progress(message) => LoginEvent::Progress(message),
            PiLoginEvent::Complete(tokens) => {
                self.install_tokens(&tokens).await?;
                LoginEvent::Complete
            }
        })
    }

    pub async fn answer_login_prompt(&mut self, value: &str) -> Result<()> {
        self.auth.answer_prompt(value).await
    }

    pub async fn models(&mut self) -> Result<Value> {
        self.request("model/list", json!({ "includeHidden": false }))
            .await
    }

    pub async fn start_thread(&mut self, cwd: &Path, model: Option<&str>) -> Result<String> {
        let cwd = cwd.to_str().context("workspace path is not UTF-8")?;
        let mut params = json!({
            "cwd": cwd,
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "developerInstructions": "Use seraph.python for local computation and explicit data reduction. Only values passed to emit() return to the model. Do not mutate files from Python.",
            "dynamicTools": [{
                "type": "namespace",
                "name": "seraph",
                "description": "SERAPH's persistent local runtime",
                "tools": [{
                    "type": "function",
                    "name": "python",
                    "description": "Execute code in SERAPH's persistent Python kernel",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "code": {
                                "type": "string",
                                "description": "Python code to execute"
                            }
                        },
                        "required": ["code"],
                        "additionalProperties": false
                    }
                }, {
                    "type": "function",
                    "name": "coordination",
                    "description": "Create, inspect, claim, and finish durable shared tasks",
                    "deferLoading": true,
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["create", "list", "claim", "complete", "fail"]
                            },
                            "id": { "type": "integer", "minimum": 1 },
                            "subject": { "type": "string", "maxLength": 512 },
                            "blocked_by": {
                                "type": "array",
                                "items": { "type": "integer", "minimum": 1 },
                                "uniqueItems": true,
                                "maxItems": 64
                            },
                            "ready_only": { "type": "boolean" },
                            "after_id": { "type": "integer", "minimum": 0 },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                }, {
                    "type": "function",
                    "name": "agents",
                    "description": "Spawn and join SERAPH-native agents with shared project state",
                    "deferLoading": true,
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["spawn", "list", "wait", "interrupt"]
                            },
                            "id": { "type": "integer", "minimum": 1 },
                            "prompt": { "type": "string", "maxLength": 16384 },
                            "ids": {
                                "type": "array",
                                "items": { "type": "integer", "minimum": 1 },
                                "uniqueItems": true,
                                "maxItems": 8
                            },
                            "after_id": { "type": "integer", "minimum": 0 },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                }]
            }]
        });
        if let Some(model) = model {
            params["model"] = Value::String(model.to_owned());
        }

        let response = self.request("thread/start", params).await?;
        response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("thread/start response omitted thread.id")
    }

    pub async fn start_turn(
        &mut self,
        thread_id: &str,
        prompt: &str,
        effort: Option<&str>,
    ) -> Result<String> {
        let mut params = json!({
            "threadId": thread_id,
            "input": [{ "type": "text", "text": prompt }],
        });
        if let Some(effort) = effort {
            params["effort"] = Value::String(effort.to_owned());
        }
        let response = self.request("turn/start", params).await?;
        Ok(response
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("turn/start response omitted turn.id")?
            .to_owned())
    }

    pub async fn next_turn_event(&mut self, thread_id: &str, turn_id: &str) -> Result<CodexEvent> {
        loop {
            let message = self.next_message().await?;
            let method = message.get("method").and_then(Value::as_str);
            if is_server_request(&message) && method != Some("item/tool/call") {
                self.handle_server_request(&message).await?;
                continue;
            }
            match method {
                Some("item/agentMessage/delta") => {
                    if matches_turn(&message, thread_id, turn_id)
                        && let Some(delta) =
                            message.pointer("/params/delta").and_then(Value::as_str)
                    {
                        return Ok(CodexEvent::AgentMessageDelta(delta.to_owned()));
                    }
                }
                Some("item/tool/call") => return self.tool_call(message).map(CodexEvent::ToolCall),
                Some("turn/completed")
                    if message.pointer("/params/turn/id").and_then(Value::as_str)
                        == Some(turn_id) =>
                {
                    let turn = message
                        .pointer("/params/turn")
                        .cloned()
                        .context("turn/completed omitted turn")?;
                    return Ok(CodexEvent::TurnCompleted(turn));
                }
                Some("error") if matches_turn(&message, thread_id, turn_id) => {
                    if message
                        .pointer("/params/willRetry")
                        .and_then(Value::as_bool)
                        == Some(true)
                    {
                        continue;
                    }
                    return Ok(CodexEvent::TurnError(message["params"].to_string()));
                }
                _ => {}
            }
        }
    }

    pub async fn interrupt_turn(&mut self, thread_id: &str, turn_id: &str) -> Result<bool> {
        match self
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.to_string().contains("no active turn to interrupt") => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn respond_tool(&mut self, call: ToolCall, result: ToolResult) -> Result<()> {
        self.write(json!({
            "id": call.id,
            "result": {
                "contentItems": [{ "type": "inputText", "text": result.text }],
                "success": result.success,
            }
        }))
        .await
    }

    pub async fn shutdown(self) -> Result<()> {
        let Self {
            mut child,
            input,
            auth,
            ..
        } = self;
        drop(input);
        child.wait().await.context("wait for Codex app-server")?;
        auth.shutdown().await?;
        Ok(())
    }

    fn tool_call(&self, message: Value) -> Result<ToolCall> {
        let id = message.get("id").cloned().context("tool call omitted id")?;
        let params = message.get("params").context("tool call omitted params")?;
        Ok(ToolCall {
            id,
            thread_id: required_string(params, "threadId")?.to_owned(),
            namespace: params
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tool: required_string(params, "tool")?.to_owned(),
            arguments: params
                .get("arguments")
                .cloned()
                .context("tool call omitted arguments")?,
        })
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({ "method": method, "id": id, "params": params }))
            .await?;

        let message = loop {
            if let Some(message) = self.take_queued(|message| is_response(message, id)) {
                break message;
            }
            let message = self.read().await?;
            if is_server_request(&message) {
                self.handle_server_request(&message).await?;
                continue;
            }
            if is_response(&message, id) {
                break message;
            }
            self.backlog.push_back(message);
        };
        if let Some(error) = message.get("error") {
            bail!("Codex {method} failed: {error}");
        }
        message
            .get("result")
            .cloned()
            .with_context(|| format!("Codex {method} response omitted result"))
    }

    async fn write(&mut self, message: Value) -> Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.input
            .write_all(&line)
            .await
            .context("write to Codex")?;
        self.input.flush().await.context("flush Codex request")
    }

    async fn read(&mut self) -> Result<Value> {
        let line = self
            .output
            .next_line()
            .await
            .context("read from Codex")?
            .context("Codex app-server exited")?;
        serde_json::from_str(&line).context("decode Codex JSONL message")
    }

    async fn next_message(&mut self) -> Result<Value> {
        match self.backlog.pop_front() {
            Some(message) => Ok(message),
            None => self.read().await,
        }
    }

    fn take_queued(&mut self, predicate: impl Fn(&Value) -> bool) -> Option<Value> {
        let index = self.backlog.iter().position(predicate)?;
        self.backlog.remove(index)
    }

    async fn reject_request(&mut self, request: &Value) -> Result<()> {
        self.write(json!({
            "id": request["id"],
            "error": {
                "code": -32601,
                "message": format!(
                    "SERAPH does not support server request {}",
                    request.get("method").and_then(Value::as_str).unwrap_or("<unknown>")
                )
            }
        }))
        .await
    }

    async fn install_tokens(&mut self, tokens: &ChatgptTokens) -> Result<()> {
        self.request(
            "account/login/start",
            json!({
                "type": "chatgptAuthTokens",
                "accessToken": tokens.access_token,
                "chatgptAccountId": tokens.account_id,
                "chatgptPlanType": tokens.plan_type,
            }),
        )
        .await?;
        Ok(())
    }

    async fn handle_server_request(&mut self, request: &Value) -> Result<()> {
        if request.get("method").and_then(Value::as_str)
            == Some("account/chatgptAuthTokens/refresh")
        {
            let tokens = self
                .auth
                .refresh()
                .await?
                .context("Pi has no OpenAI Codex credential to refresh")?;
            return self
                .write(json!({
                    "id": request["id"],
                    "result": {
                        "accessToken": tokens.access_token,
                        "chatgptAccountId": tokens.account_id,
                        "chatgptPlanType": tokens.plan_type,
                    }
                }))
                .await;
        }
        self.reject_request(request).await
    }
}

fn is_server_request(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").and_then(Value::as_str).is_some()
}

fn is_response(message: &Value, id: u64) -> bool {
    message.get("method").is_none() && message.get("id").and_then(Value::as_u64) == Some(id)
}

fn matches_turn(message: &Value, thread_id: &str, turn_id: &str) -> bool {
    message.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
        && message.pointer("/params/turnId").and_then(Value::as_str) == Some(turn_id)
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("response omitted string field {field:?}"))
}
