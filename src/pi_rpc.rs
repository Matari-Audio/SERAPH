use std::{
    collections::VecDeque,
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const SERAPH_EXTENSION: &str = include_str!("../auth/seraph-extension.mjs");
static EXTENSION_WRITE_ID: AtomicU64 = AtomicU64::new(0);

pub enum PiEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart {
        id: String,
        name: String,
        args: Value,
    },
    ToolUpdate {
        id: String,
        result: Value,
    },
    ToolEnd {
        id: String,
        result: Value,
        is_error: bool,
    },
    Error(String),
    Settled,
    Other,
}

pub struct PiRpc {
    _child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    backlog: VecDeque<Value>,
    next_id: u64,
}

impl PiRpc {
    pub async fn spawn(cwd: &Path, model: &str, effort: Option<&str>) -> Result<Self> {
        let executable = env::var_os("SERAPH_PI").unwrap_or_else(|| {
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/pi");
            if source.is_file() {
                source.into_os_string()
            } else {
                "pi".into()
            }
        });
        let agent_dir = env::var_os("SERAPH_PI_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".seraph")))
            .context("HOME is unavailable; set SERAPH_PI_HOME")?;
        tokio::fs::create_dir_all(&agent_dir)
            .await
            .context("create SERAPH Pi home")?;
        let runtime_dir = agent_dir.join("runtime");
        tokio::fs::create_dir_all(&runtime_dir)
            .await
            .context("create SERAPH runtime directory")?;
        let extension = runtime_dir.join("seraph-extension.mjs");
        if !matches!(tokio::fs::read_to_string(&extension).await, Ok(source) if source == SERAPH_EXTENSION)
        {
            let temp = runtime_dir.join(format!(
                ".seraph-extension-{}-{}.tmp",
                std::process::id(),
                EXTENSION_WRITE_ID.fetch_add(1, Ordering::Relaxed)
            ));
            tokio::fs::write(&temp, SERAPH_EXTENSION)
                .await
                .context("write SERAPH extension")?;
            tokio::fs::rename(&temp, &extension)
                .await
                .context("install SERAPH extension")?;
        }
        let seraph_exe = env::current_exe().context("locate SERAPH executable")?;
        let mut command = Command::new(executable);
        command
            .env("PI_CODING_AGENT_DIR", agent_dir)
            .env("SERAPH_EXE", seraph_exe)
            .current_dir(cwd)
            .args([
                "--mode",
                "rpc",
                "--provider",
                "openai-codex",
                "--model",
                model,
                "--approve",
                "--no-skills",
                "--no-prompt-templates",
                "--no-extensions",
                "--extension",
            ])
            .arg(extension)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(effort) = effort {
            command.args(["--thinking", effort]);
        }
        let mut child = command.spawn().context("start Pi RPC backend")?;
        let input = child.stdin.take().context("Pi RPC stdin unavailable")?;
        let output = child.stdout.take().context("Pi RPC stdout unavailable")?;
        let mut rpc = Self {
            _child: child,
            input,
            output: BufReader::new(output).lines(),
            backlog: VecDeque::new(),
            next_id: 1,
        };
        rpc.request("get_state", json!({})).await?;
        Ok(rpc)
    }

    pub async fn prompt(&mut self, message: &str) -> Result<()> {
        self.request("prompt", json!({ "message": message }))
            .await?;
        Ok(())
    }

    pub async fn abort(&mut self) -> Result<()> {
        self.request("abort", json!({})).await?;
        Ok(())
    }

    pub async fn set_model(&mut self, model: &str, effort: Option<&str>) -> Result<()> {
        self.request(
            "set_model",
            json!({ "provider": "openai-codex", "modelId": model }),
        )
        .await?;
        if let Some(effort) = effort {
            self.request("set_thinking_level", json!({ "level": effort }))
                .await?;
        }
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<PiEvent> {
        let message = match self.backlog.pop_front() {
            Some(message) => message,
            None => self.read().await?,
        };
        Ok(match message.get("type").and_then(Value::as_str) {
            Some("message_update") => match message
                .pointer("/assistantMessageEvent/type")
                .and_then(Value::as_str)
            {
                Some("text_delta") => PiEvent::TextDelta(
                    message
                        .pointer("/assistantMessageEvent/delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
                Some("thinking_delta") => PiEvent::ThinkingDelta(
                    message
                        .pointer("/assistantMessageEvent/delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
                _ => PiEvent::Other,
            },
            Some("tool_execution_start") => PiEvent::ToolStart {
                id: required_string(&message, "toolCallId")?.to_owned(),
                name: required_string(&message, "toolName")?.to_owned(),
                args: message.get("args").cloned().unwrap_or(Value::Null),
            },
            Some("tool_execution_update") => PiEvent::ToolUpdate {
                id: required_string(&message, "toolCallId")?.to_owned(),
                result: message.get("partialResult").cloned().unwrap_or(Value::Null),
            },
            Some("tool_execution_end") => PiEvent::ToolEnd {
                id: required_string(&message, "toolCallId")?.to_owned(),
                result: message.get("result").cloned().unwrap_or(Value::Null),
                is_error: message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            Some("message_end")
                if message
                    .pointer("/message/stopReason")
                    .and_then(Value::as_str)
                    == Some("error") =>
            {
                PiEvent::Error(
                    message
                        .pointer("/message/errorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("Pi backend failed")
                        .to_owned(),
                )
            }
            Some("agent_settled") => PiEvent::Settled,
            _ => PiEvent::Other,
        })
    }

    async fn request(&mut self, kind: &str, fields: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({ "id": id.to_string(), "type": kind });
        if let (Some(target), Some(source)) = (request.as_object_mut(), fields.as_object()) {
            target.extend(source.clone());
        }
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        self.input
            .write_all(&line)
            .await
            .context("write Pi RPC request")?;
        self.input.flush().await.context("flush Pi RPC request")?;
        loop {
            let message = self.read().await?;
            if message.get("type").and_then(Value::as_str) == Some("response")
                && message.get("id").and_then(Value::as_str) == Some(&id.to_string())
            {
                if message.get("success").and_then(Value::as_bool) != Some(true) {
                    bail!(
                        "Pi {kind} failed: {}",
                        message
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    );
                }
                return Ok(message.get("data").cloned().unwrap_or(Value::Null));
            }
            self.backlog.push_back(message);
        }
    }

    async fn read(&mut self) -> Result<Value> {
        let line = self
            .output
            .next_line()
            .await?
            .context("Pi RPC backend exited")?;
        serde_json::from_str(&line).context("decode Pi RPC JSONL message")
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("Pi event omitted {field}"))
}
