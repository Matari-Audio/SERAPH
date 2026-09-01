use std::{
    collections::BTreeMap,
    env,
    ops::Bound::{Excluded, Unbounded},
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use processkit::ProcessGroup;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, Notify, mpsc, oneshot},
    task::{AbortHandle, JoinHandle},
};

use crate::tasks::TaskBoard;
use crate::tui::{AgentSummary, UiEvent};

const MAX_RESULT_BYTES: usize = 2_048;
const MAX_WAIT_IDS: usize = 8;
const MAX_RUNNING_AGENTS: usize = 8;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChildCommand {
    Start { prompt: String },
    FollowUp { key: String, prompt: String },
    Interrupt,
    Shutdown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChildEvent {
    Ready,
    Queued {
        key: String,
        submission_id: String,
        starts_immediately: bool,
    },
    Running {
        key: Option<String>,
        result: Option<String>,
    },
    Idle {
        result: String,
    },
    Interrupted {
        accepted: bool,
    },
    Failed {
        error: String,
    },
    Stopped,
}

pub struct AgentManager {
    executable: PathBuf,
    project: PathBuf,
    address: String,
    inner: Arc<Inner>,
    mailbox: StdMutex<TaskBoard>,
    aborts: StdMutex<Vec<AbortHandle>>,
}

struct Inner {
    records: Mutex<BTreeMap<u64, AgentRecord>>,
    changed: Notify,
    ui: Option<mpsc::Sender<UiEvent>>,
}

struct AgentRecord {
    id: u64,
    status: AgentStatus,
    prompt: String,
    result: Option<String>,
    control: Arc<Mutex<tokio::process::ChildStdin>>,
    follow_ups: BTreeMap<String, FollowUp>,
    interrupt_pending: bool,
    interrupt_result: Option<bool>,
    task: Option<JoinHandle<()>>,
    stop: Option<oneshot::Sender<()>>,
}

enum AgentStatus {
    Running,
    Idle,
    Failed,
}

struct FollowUp {
    prompt: String,
    result: Option<Value>,
}

impl AgentManager {
    pub fn new(project: PathBuf, ui: Option<mpsc::Sender<UiEvent>>) -> Result<Self> {
        let mailbox = TaskBoard::open(&project)?;
        let address = match env::var("SERAPH_AGENT_ID") {
            Ok(id) => {
                let id = id
                    .parse::<u64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .context("invalid SERAPH agent id")?;
                format!("agent:{id}")
            }
            Err(env::VarError::NotPresent) => "main".into(),
            Err(error) => return Err(error).context("read SERAPH agent id"),
        };
        Ok(Self {
            executable: env::current_exe().context("locate SERAPH executable")?,
            project,
            address,
            inner: Arc::new(Inner {
                records: Mutex::new(BTreeMap::new()),
                changed: Notify::new(),
                ui,
            }),
            mailbox: StdMutex::new(mailbox),
            aborts: StdMutex::new(Vec::new()),
        })
    }

    pub async fn spawn(&self, prompt: &str) -> Result<Value> {
        // ponytail: one native generation prevents orphan trees; add process-group supervision before recursion.
        if env::var_os("SERAPH_AGENT_CHILD").is_some() {
            bail!("nested native agents are not enabled in v0");
        }
        if prompt.trim().is_empty() || prompt.len() > 16 * 1024 {
            bail!("agent prompt must contain 1 to 16384 bytes");
        }
        let mut records = self.inner.records.lock().await;
        if records.values().filter(|record| record.is_alive()).count() >= MAX_RUNNING_AGENTS {
            bail!("at most {MAX_RUNNING_AGENTS} agents may run concurrently");
        }
        let id = self
            .mailbox
            .lock()
            .map_err(|_| anyhow::anyhow!("agent mailbox poisoned"))?
            .allocate_agent_id()?;
        let mut command = Command::new(&self.executable);
        command
            .arg("__agent")
            .env("SERAPH_AGENT_CHILD", "1")
            .env("SERAPH_AGENT_ID", id.to_string())
            .current_dir(&self.project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let group = ProcessGroup::new().context("create SERAPH agent process group")?;
        let mut child = group.spawn(command).context("spawn SERAPH agent")?;
        let control = Arc::new(Mutex::new(
            child
                .stdin
                .take()
                .context("SERAPH agent stdin unavailable")?,
        ));
        let stdout = child
            .stdout
            .take()
            .context("SERAPH agent stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("SERAPH agent stderr unavailable")?;
        let (stop, stop_rx) = oneshot::channel();

        records.insert(
            id,
            AgentRecord {
                id,
                status: AgentStatus::Running,
                prompt: prompt.lines().next().unwrap_or(prompt).trim().to_owned(),
                result: None,
                control: Arc::clone(&control),
                follow_ups: BTreeMap::new(),
                interrupt_pending: false,
                interrupt_result: None,
                task: None,
                stop: Some(stop),
            },
        );

        let inner = Arc::clone(&self.inner);
        let supervisor_control = Arc::clone(&control);
        let task = tokio::spawn(async move {
            let mut events = tokio::spawn(read_child_events(stdout, Arc::clone(&inner), id));
            let stderr = tokio::spawn(read_bounded(stderr));
            let mut completed_events = None;
            let outcome = tokio::select! {
                status = child.wait() => status.context("wait for SERAPH agent"),
                result = &mut events => {
                    let graceful = matches!(result, Ok(Ok(true)));
                    completed_events = Some(result);
                    if graceful {
                        child.wait().await.context("wait for stopped SERAPH agent")
                    } else {
                        terminate_child(&group, &mut child).await
                    }
                },
                _ = stop_rx => {
                    let shutdown = write_child_command(&supervisor_control, &ChildCommand::Shutdown).await;
                    match shutdown {
                        Ok(()) => match tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait()).await {
                            Ok(status) => status.context("wait for SERAPH agent shutdown"),
                            Err(_) => terminate_child(&group, &mut child).await,
                        },
                        Err(_) => terminate_child(&group, &mut child).await,
                    }
                },
            };
            let stderr = stderr.await.unwrap_or_default();
            let event_result = match completed_events {
                Some(result) => result,
                None => events.await,
            };
            let mut records = inner.records.lock().await;
            if let Some(record) = records.get_mut(&id) {
                let graceful = matches!(event_result, Ok(Ok(true)));
                if !graceful {
                    record.status = AgentStatus::Failed;
                    record.result = Some(match outcome {
                        Ok(exit) if !exit.success() && !stderr.is_empty() => bounded_text(&stderr),
                        Ok(exit) => format!("agent exited with {exit}"),
                        Err(error) => bounded_text(error.to_string().as_bytes()),
                    });
                }
            }
            let summaries = records.values().map(AgentRecord::summary).collect();
            drop(records);
            if let Some(ui) = &inner.ui {
                let _ = ui.send(UiEvent::AgentsChanged(summaries)).await;
            }
            inner.changed.notify_waiters();
        });
        self.aborts
            .lock()
            .map_err(|_| anyhow::anyhow!("agent abort registry poisoned"))?
            .push(task.abort_handle());
        if let Some(record) = records.get_mut(&id) {
            record.task = Some(task);
        }
        drop(records);
        if let Err(error) = write_child_command(
            &control,
            &ChildCommand::Start {
                prompt: prompt.into(),
            },
        )
        .await
        {
            if let Some(stop) = self
                .inner
                .records
                .lock()
                .await
                .get_mut(&id)
                .and_then(|record| record.stop.take())
            {
                let _ = stop.send(());
            }
            return Err(error);
        }
        self.publish().await;

        Ok(json!({ "id": id, "address": format!("agent:{id}"), "status": "running" }))
    }

    pub async fn list(&self, after_id: u64, limit: usize) -> Value {
        let records = self.inner.records.lock().await;
        let mut agents: Vec<Value> = records
            .range((Excluded(after_id), Unbounded))
            .take(limit + 1)
            .map(|(_, record)| record.summary_json())
            .collect();
        let truncated = agents.len() > limit;
        if truncated {
            agents.pop();
        }
        let next_after_id = truncated
            .then(|| agents.last().and_then(|agent| agent["id"].as_u64()))
            .flatten();
        json!({ "agents": agents, "next_after_id": next_after_id })
    }

    pub async fn wait(&self, ids: &[u64]) -> Result<Value> {
        if ids.is_empty() || ids.len() > MAX_WAIT_IDS {
            bail!("wait requires 1 to {MAX_WAIT_IDS} agent ids");
        }

        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            let records = self.inner.records.lock().await;
            for id in ids {
                if !records.contains_key(id) {
                    bail!("agent {id} not found");
                }
            }
            let finished: Vec<Value> = ids
                .iter()
                .filter_map(|id| records.get(id).filter(|record| record.is_finished()))
                .map(AgentRecord::json)
                .collect();
            drop(records);

            if finished.len() == ids.len() {
                return Ok(Value::Array(finished));
            }
            changed.await;
        }
    }

    pub async fn follow_up(&self, id: u64, prompt: &str, key: &str) -> Result<Value> {
        if prompt.trim().is_empty() || prompt.len() > 16 * 1024 {
            bail!("follow-up prompt must contain 1 to 16384 bytes");
        }
        if key.trim().is_empty() || key.len() > 128 {
            bail!("follow-up key must contain 1 to 128 bytes");
        }
        let (control, write) = {
            let mut records = self.inner.records.lock().await;
            let record = records
                .get_mut(&id)
                .with_context(|| format!("agent {id} is not owned by this live session"))?;
            if matches!(record.status, AgentStatus::Failed) {
                bail!("agent {id} is not running");
            }
            match record.follow_ups.get(key) {
                Some(existing) if existing.prompt != prompt => {
                    bail!("follow-up key already identifies a different prompt")
                }
                Some(existing) if existing.result.is_some() => {
                    return existing.result.clone().context("follow-up result missing");
                }
                Some(_) => (Arc::clone(&record.control), false),
                None => {
                    record.follow_ups.insert(
                        key.to_owned(),
                        FollowUp {
                            prompt: prompt.to_owned(),
                            result: None,
                        },
                    );
                    (Arc::clone(&record.control), true)
                }
            }
        };
        if write
            && let Err(error) = write_child_command(
                &control,
                &ChildCommand::FollowUp {
                    key: key.into(),
                    prompt: prompt.into(),
                },
            )
            .await
        {
            if let Some(record) = self.inner.records.lock().await.get_mut(&id) {
                record.follow_ups.remove(key);
            }
            return Err(error);
        }
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let records = self.inner.records.lock().await;
            let record = records
                .get(&id)
                .with_context(|| format!("agent {id} disappeared"))?;
            if let Some(result) = record
                .follow_ups
                .get(key)
                .and_then(|follow_up| follow_up.result.clone())
            {
                return Ok(result);
            }
            if matches!(record.status, AgentStatus::Failed) {
                bail!("agent {id} failed before acknowledging follow-up")
            }
            drop(records);
            changed.await;
        }
    }

    pub async fn interrupt(&self, id: u64) -> Result<bool> {
        let (control, write) = {
            let mut records = self.inner.records.lock().await;
            let record = records
                .get_mut(&id)
                .with_context(|| format!("agent {id} is not owned by this live session"))?;
            if !matches!(record.status, AgentStatus::Running) {
                return Ok(false);
            }
            record.interrupt_result = None;
            let write = !record.interrupt_pending;
            record.interrupt_pending = true;
            (Arc::clone(&record.control), write)
        };
        if write && let Err(error) = write_child_command(&control, &ChildCommand::Interrupt).await {
            if let Some(record) = self.inner.records.lock().await.get_mut(&id) {
                record.interrupt_pending = false;
            }
            return Err(error);
        }
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let records = self.inner.records.lock().await;
            let record = records
                .get(&id)
                .with_context(|| format!("agent {id} disappeared"))?;
            if let Some(accepted) = record.interrupt_result {
                return Ok(accepted);
            }
            if matches!(record.status, AgentStatus::Failed) {
                bail!("agent {id} failed before acknowledging interrupt")
            }
            drop(records);
            changed.await;
        }
    }

    pub fn send(&self, recipient: &str, message: &str, key: &str) -> Result<Value> {
        validate_address(recipient)?;
        let mut mailbox = self
            .mailbox
            .lock()
            .map_err(|_| anyhow::anyhow!("agent mailbox poisoned"))?;
        if let Some(id) = recipient
            .strip_prefix("agent:")
            .and_then(|id| id.parse().ok())
            && !mailbox.agent_exists(id)?
        {
            bail!("recipient {recipient} does not exist");
        }
        let (id, inserted) = mailbox.send_message(&self.address, recipient, message, key)?;
        Ok(
            json!({ "id": id, "sender": self.address, "recipient": recipient, "inserted": inserted }),
        )
    }

    pub fn receive(&self, limit: usize, max_projection_bytes: usize) -> Result<Value> {
        self.mailbox
            .lock()
            .map_err(|_| anyhow::anyhow!("agent mailbox poisoned"))?
            .receive_messages(&self.address, limit, max_projection_bytes)
    }

    pub async fn shutdown(&self) {
        let (stops, tasks): (Vec<_>, Vec<_>) = {
            let mut records = self.inner.records.lock().await;
            records
                .values_mut()
                .map(|record| (record.stop.take(), record.task.take()))
                .unzip()
        };
        for stop in stops.into_iter().flatten() {
            let _ = stop.send(());
        }
        for task in tasks.into_iter().flatten() {
            let _ = task.await;
        }
    }

    async fn publish(&self) {
        if let Some(ui) = &self.inner.ui {
            let summaries = self
                .inner
                .records
                .lock()
                .await
                .values()
                .map(AgentRecord::summary)
                .collect();
            let _ = ui.send(UiEvent::AgentsChanged(summaries)).await;
        }
    }
}

fn validate_address(address: &str) -> Result<()> {
    if address == "main"
        || address
            .strip_prefix("agent:")
            .and_then(|id| id.parse::<u64>().ok())
            .is_some_and(|id| id > 0 && address == format!("agent:{id}"))
    {
        return Ok(());
    }
    bail!("recipient must be main or agent:<positive id>")
}

impl AgentRecord {
    fn is_finished(&self) -> bool {
        matches!(self.status, AgentStatus::Idle | AgentStatus::Failed)
    }

    fn is_alive(&self) -> bool {
        !matches!(self.status, AgentStatus::Failed)
    }

    fn json(&self) -> Value {
        match &self.result {
            Some(result) => json!({
                "id": self.id,
                "status": self.status.as_str(),
                "result": result,
            }),
            None => json!({ "id": self.id, "status": self.status.as_str() }),
        }
    }

    fn summary_json(&self) -> Value {
        json!({ "id": self.id, "status": self.status.as_str(), "prompt": self.prompt })
    }

    fn summary(&self) -> AgentSummary {
        AgentSummary {
            id: self.id,
            status: self.status.as_str(),
            prompt: self.prompt.clone(),
            result: self.result.clone(),
        }
    }
}

impl Drop for AgentManager {
    fn drop(&mut self) {
        if let Ok(aborts) = self.aborts.lock() {
            for task in aborts.iter() {
                task.abort();
            }
        }
    }
}

impl AgentStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Failed => "failed",
        }
    }
}

async fn write_child_command(
    control: &Mutex<tokio::process::ChildStdin>,
    command: &ChildCommand,
) -> Result<()> {
    let mut line = serde_json::to_vec(command)?;
    line.push(b'\n');
    let mut control = control.lock().await;
    control
        .write_all(&line)
        .await
        .context("write SERAPH agent command")?;
    control.flush().await.context("flush SERAPH agent command")
}

async fn read_child_events(
    stdout: impl AsyncRead + Unpin,
    inner: Arc<Inner>,
    id: u64,
) -> Result<bool> {
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await.context("read SERAPH agent event")? {
        let event: ChildEvent =
            serde_json::from_str(&line).context("decode SERAPH agent event JSON")?;
        let mut records = inner.records.lock().await;
        let Some(record) = records.get_mut(&id) else {
            return Ok(false);
        };
        match event {
            ChildEvent::Ready => {}
            ChildEvent::Queued {
                key,
                submission_id,
                starts_immediately,
            } => {
                let follow_up = record.follow_ups.get_mut(&key).with_context(|| {
                    format!("agent {id} acknowledged unknown follow-up {key:?}")
                })?;
                follow_up.result = Some(json!({
                    "id": id,
                    "key": key,
                    "queued_submission_id": submission_id,
                    "starts_immediately": starts_immediately,
                }));
                if starts_immediately {
                    record.status = AgentStatus::Running;
                    record.prompt = follow_up
                        .prompt
                        .lines()
                        .next()
                        .unwrap_or(&follow_up.prompt)
                        .trim()
                        .to_owned();
                }
            }
            ChildEvent::Running { key, result } => {
                record.status = AgentStatus::Running;
                if result.is_some() {
                    record.result = result;
                }
                if let Some(follow_up) = key.and_then(|key| record.follow_ups.get(&key)) {
                    record.prompt = follow_up
                        .prompt
                        .lines()
                        .next()
                        .unwrap_or(&follow_up.prompt)
                        .trim()
                        .to_owned();
                }
            }
            ChildEvent::Idle { result } => {
                record.status = AgentStatus::Idle;
                record.result = Some(result);
            }
            ChildEvent::Interrupted { accepted } => {
                record.interrupt_pending = false;
                record.interrupt_result = Some(accepted);
            }
            ChildEvent::Failed { error } => {
                record.status = AgentStatus::Failed;
                record.result = Some(error);
            }
            ChildEvent::Stopped => return Ok(true),
        }
        let summaries = records.values().map(AgentRecord::summary).collect();
        drop(records);
        inner.changed.notify_waiters();
        if let Some(ui) = &inner.ui {
            let _ = ui.send(UiEvent::AgentsChanged(summaries)).await;
        }
    }
    Ok(false)
}

fn append_bounded(output: &mut Vec<u8>, input: &[u8]) {
    let remaining = MAX_RESULT_BYTES.saturating_sub(output.len());
    output.extend_from_slice(&input[..input.len().min(remaining)]);
}

fn bounded_text(bytes: &[u8]) -> String {
    let mut text =
        String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_RESULT_BYTES)]).into_owned();
    if text.len() > MAX_RESULT_BYTES {
        let mut end = MAX_RESULT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

async fn read_bounded(mut input: impl AsyncRead + Unpin) -> Vec<u8> {
    let mut output = Vec::with_capacity(MAX_RESULT_BYTES);
    let mut buffer = [0; 4096];
    while let Ok(read) = input.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        append_bounded(&mut output, &buffer[..read]);
    }
    output
}

async fn terminate_child(group: &ProcessGroup, child: &mut Child) -> Result<ExitStatus> {
    group.kill_all().context("kill SERAPH agent process tree")?;
    child.wait().await.context("reap SERAPH agent")
}
