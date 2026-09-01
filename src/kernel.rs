use std::{
    collections::VecDeque,
    env,
    future::pending,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    process::{Child, Command as ProcessCommand},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout},
};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::capability::CapabilityHost;

const KERNEL_SOURCE: &str = include_str!("../python/kernel.py");
const PROTOCOL_VERSION: u64 = 1;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_RETAINED_STREAM_BYTES: usize = 256 * 1024;
const MAX_RETAINED_DISPLAYS_BYTES: usize = 256 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);
const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct ExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub background_stdout: String,
    pub background_stderr: String,
    pub emitted: Vec<Value>,
    pub truncated: bool,
    emitted_bytes: usize,
}

pub struct Kernel {
    commands: mpsc::Sender<DriverCommand>,
    driver: JoinHandle<Result<()>>,
    next_id: AtomicU64,
}

enum DriverCommand {
    Execute {
        id: String,
        code: String,
        response: oneshot::Sender<Result<ExecutionOutput>>,
    },
    Shutdown {
        id: String,
        response: oneshot::Sender<Result<()>>,
    },
}

enum ActiveResponse {
    Execute(oneshot::Sender<Result<ExecutionOutput>>),
    Shutdown(oneshot::Sender<Result<()>>),
}

struct ActiveExecution {
    id: String,
    deadline: Instant,
    output: ExecutionOutput,
    failure: Option<anyhow::Error>,
    response: ActiveResponse,
}

struct HostReply {
    id: String,
    data: Value,
}

impl Kernel {
    pub async fn spawn() -> Result<Self> {
        let (child, stdin, stdout) = spawn_child()?;
        let mut frames = codec().new_read(stdout);
        let ready = timeout(STARTUP_TIMEOUT, next_value(&mut frames))
            .await
            .context("CPython kernel handshake timed out")??;
        if ready.get("event").and_then(Value::as_str) != Some("ready")
            || ready.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION)
        {
            bail!("incompatible CPython kernel handshake: {ready}");
        }

        let (commands, command_rx) = mpsc::channel(32);
        let driver = tokio::spawn(run_driver(child, stdin, frames, command_rx));
        Ok(Self {
            commands,
            driver,
            next_id: AtomicU64::new(1),
        })
    }

    pub async fn execute(&self, code: &str) -> Result<ExecutionOutput> {
        let id = self.id("cell");
        let (response, result) = oneshot::channel();
        self.commands
            .send(DriverCommand::Execute {
                id,
                code: code.to_owned(),
                response,
            })
            .await
            .context("CPython kernel driver stopped")?;
        result
            .await
            .context("CPython kernel driver dropped execution")?
    }

    pub async fn shutdown(self) -> Result<()> {
        let id = self.id("shutdown");
        let (response, result) = oneshot::channel();
        self.commands
            .send(DriverCommand::Shutdown { id, response })
            .await
            .context("CPython kernel driver stopped")?;
        result
            .await
            .context("CPython kernel driver dropped shutdown")??;
        self.driver.await.context("join CPython kernel driver")?
    }

    fn id(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

fn spawn_child() -> Result<(
    Child,
    tokio::process::ChildStdin,
    tokio::process::ChildStdout,
)> {
    let python = env::var_os("SERAPH_PYTHON").unwrap_or_else(|| "python3".into());
    let mut child = ProcessCommand::new(python)
        .args(["-u", "-c", KERNEL_SOURCE])
        .env("SERAPH_KERNEL_OWNER_PID", std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("start CPython kernel")?;
    let stdin = child.stdin.take().context("kernel stdin unavailable")?;
    let stdout = child.stdout.take().context("kernel stdout unavailable")?;
    Ok((child, stdin, stdout))
}

async fn run_driver(
    mut child: Child,
    stdin: tokio::process::ChildStdin,
    mut frames: FramedRead<tokio::process::ChildStdout, LengthDelimitedCodec>,
    mut commands: mpsc::Receiver<DriverCommand>,
) -> Result<()> {
    let capabilities = Arc::new(CapabilityHost::default());
    let mut sink = codec().new_write(stdin);
    let mut queued = VecDeque::new();
    let mut active: Option<ActiveExecution> = None;
    let mut host_tasks = JoinSet::<HostReply>::new();

    loop {
        if active.is_none()
            && let Some(command) = queued.pop_front()
        {
            start_command(command, &mut active, &mut sink).await?;
        }

        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    child.start_kill().context("terminate orphaned CPython kernel")?;
                    host_tasks.abort_all();
                    return Ok(());
                };
                if active.is_some() {
                    queued.push_back(command);
                } else {
                    start_command(command, &mut active, &mut sink).await?;
                }
            }
            frame = frames.next() => {
                let event = decode_frame(frame)?;
                let shutdown_complete = handle_event(event, &mut active, &capabilities, &mut host_tasks)?;
                if shutdown_complete {
                    finish_shutdown(&mut child, &mut host_tasks).await?;
                    return Ok(());
                }
            }
            reply = host_tasks.join_next(), if !host_tasks.is_empty() => {
                let reply = reply.context("host task set became empty")?
                    .context("host capability task panicked")?;
                send_value(&mut sink, json!({
                    "type": "host_reply",
                    "id": reply.id,
                    "data": reply.data,
                })).await?;
            }
            () = wait_for_deadline(active.as_ref().map(|item| item.deadline)), if active.is_some() => {
                child.start_kill().context("terminate timed-out CPython kernel")?;
                host_tasks.abort_all();
                fail_active(&mut active, anyhow!("Python execution timed out; kernel terminated and outcome_unknown"));
                fail_queued(&mut queued, "kernel terminated after execution timeout");
                return Ok(());
            }
        }
    }
}

async fn start_command(
    command: DriverCommand,
    active: &mut Option<ActiveExecution>,
    sink: &mut FramedWrite<tokio::process::ChildStdin, LengthDelimitedCodec>,
) -> Result<()> {
    let (id, request, response, deadline) = match command {
        DriverCommand::Execute { id, code, response } => {
            let request = json!({"type": "execute", "id": id, "code": code});
            (
                id,
                request,
                ActiveResponse::Execute(response),
                EXECUTION_TIMEOUT,
            )
        }
        DriverCommand::Shutdown { id, response } => {
            let request = json!({"type": "shutdown", "id": id});
            (
                id,
                request,
                ActiveResponse::Shutdown(response),
                STARTUP_TIMEOUT,
            )
        }
    };
    send_value(sink, request).await?;
    *active = Some(ActiveExecution {
        id,
        deadline: Instant::now() + deadline,
        output: ExecutionOutput::default(),
        failure: None,
        response,
    });
    Ok(())
}

fn handle_event(
    event: Value,
    active: &mut Option<ActiveExecution>,
    capabilities: &Arc<CapabilityHost>,
    host_tasks: &mut JoinSet<HostReply>,
) -> Result<bool> {
    let kind = event
        .get("event")
        .and_then(Value::as_str)
        .context("kernel event missing string event field")?;
    if kind == "host_request" {
        spawn_host_request(event, capabilities, host_tasks)?;
        return Ok(false);
    }

    let execution = active
        .as_mut()
        .context("kernel emitted an event without an active request")?;
    let event_id = event.get("id").and_then(Value::as_str);
    if event_id != Some(execution.id.as_str()) && event_id.is_some() {
        bail!("kernel event {kind:?} belongs to unexpected request {event_id:?}");
    }

    match kind {
        "stdout" => {
            let target = if event_id.is_none() {
                &mut execution.output.background_stdout
            } else {
                &mut execution.output.stdout
            };
            append_stream(target, text(&event)?, &mut execution.output.truncated);
        }
        "stderr" => {
            let target = if event_id.is_none() {
                &mut execution.output.background_stderr
            } else {
                &mut execution.output.stderr
            };
            append_stream(target, text(&event)?, &mut execution.output.truncated);
        }
        "display" => append_display(
            &mut execution.output,
            event.get("data").cloned().unwrap_or(Value::Null),
        ),
        "error" => {
            if event_id.is_none() {
                bail!("kernel protocol error: {}", traceback(&event));
            }
            execution.failure = Some(anyhow!(traceback(&event)));
        }
        "done" => {
            if event_id != Some(execution.id.as_str()) {
                bail!("done event has an invalid request id");
            }
            let success = event.get("status").and_then(Value::as_str) == Some("ok");
            let execution = active.take().context("active execution disappeared")?;
            let shutdown = matches!(&execution.response, ActiveResponse::Shutdown(_));
            finish_active(execution, success);
            return Ok(shutdown);
        }
        other => bail!("unknown kernel event {other:?}"),
    }
    Ok(false)
}

fn finish_active(execution: ActiveExecution, success: bool) {
    let failure = execution
        .failure
        .or_else(|| (!success).then(|| anyhow!("Python request failed without an error event")));
    match execution.response {
        ActiveResponse::Execute(response) => {
            let _ = response.send(match failure {
                Some(error) => Err(error),
                None => Ok(execution.output),
            });
        }
        ActiveResponse::Shutdown(response) => {
            let _ = response.send(match failure {
                Some(error) => Err(error),
                None => Ok(()),
            });
        }
    }
}

fn spawn_host_request(
    event: Value,
    capabilities: &Arc<CapabilityHost>,
    host_tasks: &mut JoinSet<HostReply>,
) -> Result<()> {
    let id = event
        .get("id")
        .and_then(Value::as_str)
        .context("host request missing id")?
        .to_owned();
    let data = event.get("data").context("host request missing data")?;
    let method = data
        .get("method")
        .and_then(Value::as_str)
        .context("host request missing method")?
        .to_owned();
    let params = data.get("params").cloned().unwrap_or(Value::Null);
    let capabilities = Arc::clone(capabilities);
    host_tasks.spawn(async move {
        let data = match timeout(CAPABILITY_TIMEOUT, capabilities.dispatch(&method, &params)).await
        {
            Ok(Ok(value)) => json!({"ok": true, "value": value}),
            Ok(Err(error)) => json!({"ok": false, "error": format!("{error:#}")}),
            Err(_) => json!({"ok": false, "error": "capability call timed out"}),
        };
        HostReply { id, data }
    });
    Ok(())
}

async fn send_value(
    sink: &mut FramedWrite<tokio::process::ChildStdin, LengthDelimitedCodec>,
    value: Value,
) -> Result<()> {
    let payload = serde_json::to_vec(&value).context("encode kernel request")?;
    if payload.len() > MAX_FRAME_BYTES {
        bail!("kernel request exceeds {MAX_FRAME_BYTES} bytes");
    }
    sink.send(Bytes::from(payload))
        .await
        .context("write kernel request")
}

fn decode_frame(
    frame: Option<std::result::Result<bytes::BytesMut, std::io::Error>>,
) -> Result<Value> {
    let frame = frame
        .context("CPython kernel closed its protocol stream")?
        .context("read CPython kernel frame")?;
    serde_json::from_slice(&frame).context("decode CPython kernel event")
}

async fn next_value(
    frames: &mut FramedRead<tokio::process::ChildStdout, LengthDelimitedCodec>,
) -> Result<Value> {
    decode_frame(frames.next().await)
}

fn codec() -> tokio_util::codec::length_delimited::Builder {
    let mut builder = LengthDelimitedCodec::builder();
    builder.max_frame_length(MAX_FRAME_BYTES);
    builder
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}

fn append_stream(target: &mut String, text: &str, truncated: &mut bool) {
    let remaining = MAX_RETAINED_STREAM_BYTES.saturating_sub(target.len());
    if text.len() > remaining {
        *truncated = true;
    }
    let mut end = remaining.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}

fn append_display(output: &mut ExecutionOutput, value: Value) {
    let bytes =
        serde_json::to_vec(&value).map_or(MAX_RETAINED_DISPLAYS_BYTES + 1, |data| data.len());
    if output.emitted_bytes.saturating_add(bytes) > MAX_RETAINED_DISPLAYS_BYTES {
        output.truncated = true;
        return;
    }
    output.emitted_bytes += bytes;
    output.emitted.push(value);
}

fn traceback(event: &Value) -> String {
    event
        .get("traceback")
        .and_then(Value::as_array)
        .map(|lines| lines.iter().filter_map(Value::as_str).collect::<String>())
        .filter(|trace| !trace.is_empty())
        .unwrap_or_else(|| "Python execution failed".to_owned())
}

fn fail_active(active: &mut Option<ActiveExecution>, error: anyhow::Error) {
    if let Some(execution) = active.take() {
        match execution.response {
            ActiveResponse::Execute(response) => {
                let _ = response.send(Err(error));
            }
            ActiveResponse::Shutdown(response) => {
                let _ = response.send(Err(error));
            }
        }
    }
}

fn fail_queued(queued: &mut VecDeque<DriverCommand>, reason: &str) {
    for command in queued.drain(..) {
        match command {
            DriverCommand::Execute { response, .. } => {
                let _ = response.send(Err(anyhow!(reason.to_owned())));
            }
            DriverCommand::Shutdown { response, .. } => {
                let _ = response.send(Err(anyhow!(reason.to_owned())));
            }
        }
    }
}

async fn finish_shutdown(child: &mut Child, host_tasks: &mut JoinSet<HostReply>) -> Result<()> {
    host_tasks.abort_all();
    while host_tasks.join_next().await.is_some() {}
    let status = child.wait().await.context("wait for CPython kernel")?;
    if !status.success() {
        bail!("CPython kernel exited with {status}");
    }
    Ok(())
}

fn text(event: &Value) -> Result<&str> {
    event
        .get("text")
        .and_then(Value::as_str)
        .context("kernel stream event missing text")
}
