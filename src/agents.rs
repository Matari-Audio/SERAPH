use std::{
    collections::BTreeMap,
    env,
    ops::Bound::{Excluded, Unbounded},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use processkit::ProcessGroup;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex, Notify, oneshot},
    task::{AbortHandle, JoinHandle},
};

const MAX_RESULT_BYTES: usize = 2_048;
const MAX_WAIT_IDS: usize = 8;
const MAX_RUNNING_AGENTS: usize = 8;
const AGENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub struct AgentManager {
    executable: PathBuf,
    project: PathBuf,
    inner: Arc<Inner>,
    aborts: StdMutex<Vec<AbortHandle>>,
}

struct Inner {
    next_id: AtomicU64,
    records: Mutex<BTreeMap<u64, AgentRecord>>,
    changed: Notify,
}

struct AgentRecord {
    id: u64,
    status: AgentStatus,
    result: Option<String>,
    task: Option<JoinHandle<()>>,
    stop: Option<oneshot::Sender<()>>,
}

enum AgentStatus {
    Running,
    Completed,
    Failed,
}

impl AgentManager {
    pub fn new(project: PathBuf) -> Result<Self> {
        Ok(Self {
            executable: env::current_exe().context("locate SERAPH executable")?,
            project,
            inner: Arc::new(Inner {
                next_id: AtomicU64::new(1),
                records: Mutex::new(BTreeMap::new()),
                changed: Notify::new(),
            }),
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
        let id = self
            .inner
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| anyhow::anyhow!("agent id space exhausted"))?;
        let mut records = self.inner.records.lock().await;
        if records
            .values()
            .filter(|record| !record.is_finished())
            .count()
            >= MAX_RUNNING_AGENTS
        {
            bail!("at most {MAX_RUNNING_AGENTS} agents may run concurrently");
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("__agent")
            .env("SERAPH_AGENT_CHILD", "1")
            .current_dir(&self.project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let group = ProcessGroup::new().context("create SERAPH agent process group")?;
        let mut child = group.spawn(command).context("spawn SERAPH agent")?;
        let mut control = child
            .stdin
            .take()
            .context("SERAPH agent stdin unavailable")?;
        control
            .write_all(prompt.as_bytes())
            .await
            .context("write SERAPH agent prompt")?;
        control
            .shutdown()
            .await
            .context("close SERAPH agent prompt")?;
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
                result: None,
                task: None,
                stop: Some(stop),
            },
        );

        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            let stdout = tokio::spawn(read_bounded(stdout));
            let stderr = tokio::spawn(read_bounded(stderr));
            let outcome = tokio::select! {
                status = child.wait() => status.context("wait for SERAPH agent").map(Some),
                _ = stop_rx => terminate_child(&group, &mut child).await.map(|_| None),
                _ = tokio::time::sleep(AGENT_TIMEOUT) => {
                    terminate_child(&group, &mut child).await.map(|_| None)
                },
            };
            let stdout = stdout.await.unwrap_or_default();
            let stderr = stderr.await.unwrap_or_default();
            let (status, result) = match outcome {
                Ok(Some(exit)) => {
                    let status = if exit.success() {
                        AgentStatus::Completed
                    } else {
                        AgentStatus::Failed
                    };
                    (status, bounded_result(&stdout, &stderr))
                }
                Ok(None) => (AgentStatus::Failed, "agent cancelled".into()),
                Err(error) => (
                    AgentStatus::Failed,
                    bounded_text(error.to_string().as_bytes()),
                ),
            };
            if let Some(record) = inner.records.lock().await.get_mut(&id) {
                record.status = status;
                record.result = Some(result);
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

        Ok(json!({ "id": id, "status": "running" }))
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
}

impl AgentRecord {
    fn is_finished(&self) -> bool {
        !matches!(self.status, AgentStatus::Running)
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
        json!({ "id": self.id, "status": self.status.as_str() })
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
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

fn bounded_result(stdout: &[u8], stderr: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(MAX_RESULT_BYTES);
    append_bounded(&mut bytes, stdout);
    if !bytes.is_empty() && !stderr.is_empty() {
        append_bounded(&mut bytes, b"\n");
    }
    append_bounded(&mut bytes, stderr);
    bounded_text(&bytes)
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

async fn terminate_child(group: &ProcessGroup, child: &mut Child) -> Result<()> {
    group.kill_all().context("kill SERAPH agent process tree")?;
    child.wait().await.context("reap SERAPH agent")?;
    Ok(())
}
