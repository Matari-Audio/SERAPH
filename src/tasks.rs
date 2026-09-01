use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: i64,
    pub subject: String,
    pub status: String,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub total: usize,
    pub tasks: Vec<TaskSummary>,
}

pub struct TaskBoard {
    connection: Connection,
}

impl TaskBoard {
    pub fn open(project: &Path) -> Result<Self> {
        let state = project.join(".seraph");
        fs::create_dir_all(&state).context("create SERAPH state directory")?;
        let connection =
            Connection::open(state.join("tasks.sqlite3")).context("open SERAPH task database")?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS tasks (
                     id INTEGER PRIMARY KEY,
                     subject TEXT NOT NULL CHECK (trim(subject) <> ''),
                     status TEXT NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending', 'in_progress', 'completed', 'failed')),
                     owner TEXT CHECK (owner IS NULL OR trim(owner) <> '')
                 );
                 CREATE TABLE IF NOT EXISTS task_dependencies (
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                     blocker_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                     PRIMARY KEY (task_id, blocker_id),
                     CHECK (task_id <> blocker_id)
                 );
                 CREATE TABLE IF NOT EXISTS task_claim_attempts (
                     id INTEGER PRIMARY KEY,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                     actor TEXT NOT NULL CHECK (trim(actor) <> ''),
                     claimed INTEGER NOT NULL CHECK (claimed IN (0, 1)),
                     created_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );
                 CREATE TABLE IF NOT EXISTS agent_id_sequence (
                     id INTEGER PRIMARY KEY AUTOINCREMENT
                 );
                 CREATE TABLE IF NOT EXISTS agent_messages (
                     id INTEGER PRIMARY KEY,
                     sender TEXT NOT NULL CHECK (trim(sender) <> ''),
                     recipient TEXT NOT NULL CHECK (trim(recipient) <> ''),
                     body TEXT NOT NULL CHECK (trim(body) <> ''),
                     dedupe_key TEXT NOT NULL CHECK (trim(dedupe_key) <> ''),
                     created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                     delivered_at INTEGER,
                     UNIQUE (sender, dedupe_key)
                 );",
            )
            .context("initialize SERAPH task database")?;
        Ok(Self { connection })
    }

    pub fn create(&mut self, subject: &str, blocked_by: &[i64]) -> Result<i64> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO tasks (subject) VALUES (?1)", [subject])?;
        let id = tx.last_insert_rowid();
        {
            let mut insert =
                tx.prepare("INSERT INTO task_dependencies (task_id, blocker_id) VALUES (?1, ?2)")?;
            for blocker in blocked_by {
                insert.execute(params![id, blocker])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn list_json(&self, ready_only: bool, after_id: i64, limit: usize) -> Result<String> {
        let mut statement = self.connection.prepare(
            "SELECT t.id, t.subject, t.status, t.owner
             FROM tasks t
             WHERE t.id > ?2 AND (?1 = 0 OR (
                 t.status = 'pending'
                 AND NOT EXISTS (
                     SELECT 1
                     FROM task_dependencies d
                     JOIN tasks blocker ON blocker.id = d.blocker_id
                     WHERE d.task_id = t.id AND blocker.status <> 'completed'
                 )))
             ORDER BY t.id
             LIMIT ?3",
        )?;
        let mut tasks = statement
            .query_map(params![ready_only, after_id, (limit + 1) as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .map(|row| {
                let (id, subject, status, owner) = row?;
                // ponytail: task boards are small; replace with one joined query if list latency matters.
                let blocked_by = self
                    .connection
                    .prepare(
                        "SELECT blocker_id FROM task_dependencies
                         WHERE task_id = ?1 ORDER BY blocker_id",
                    )?
                    .query_map([id], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(json!({
                    "id": id,
                    "subject": subject,
                    "status": status,
                    "owner": owner,
                    "blocked_by": blocked_by,
                }))
            })
            .collect::<rusqlite::Result<Vec<Value>>>()?;
        let truncated = tasks.len() > limit;
        if truncated {
            tasks.pop();
        }
        let next_after_id = truncated
            .then(|| tasks.last().and_then(|task| task["id"].as_i64()))
            .flatten();
        Ok(serde_json::to_string(&json!({
            "tasks": tasks,
            "next_after_id": next_after_id,
        }))?)
    }

    pub fn snapshot(&self, limit: usize) -> Result<TaskSnapshot> {
        let total = self
            .connection
            .query_row("SELECT count(*) FROM tasks", [], |row| row.get::<_, i64>(0))?
            .try_into()
            .context("task count exceeds platform limits")?;
        let mut statement = self.connection.prepare(
            "SELECT id, subject, status, owner
             FROM tasks
             ORDER BY CASE status
                 WHEN 'in_progress' THEN 0
                 WHEN 'pending' THEN 1
                 WHEN 'failed' THEN 2
                 ELSE 3
             END, id DESC
             LIMIT ?1",
        )?;
        let tasks = statement
            .query_map([limit as i64], |row| {
                Ok(TaskSummary {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    status: row.get(2)?,
                    owner: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(TaskSnapshot { total, tasks })
    }

    pub fn claim(&mut self, id: i64, owner: &str) -> Result<(bool, Option<i64>)> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claimed = tx.execute(
            "UPDATE tasks SET status = 'in_progress', owner = ?2
             WHERE id = ?1 AND status = 'pending'
               AND (owner IS NULL OR owner = ?2)
               AND NOT EXISTS (
                   SELECT 1
                   FROM task_dependencies d
                   JOIN tasks blocker ON blocker.id = d.blocker_id
                   WHERE d.task_id = tasks.id AND blocker.status <> 'completed'
               )",
            params![id, owner],
        )? == 1;
        let recorded = tx.execute(
            "INSERT INTO task_claim_attempts (task_id, actor, claimed)
             SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tasks WHERE id = ?1)",
            params![id, owner, claimed],
        )? == 1;
        let attempt_id = recorded.then(|| tx.last_insert_rowid());
        tx.commit()?;
        Ok((claimed, attempt_id))
    }

    pub fn complete(&mut self, id: i64, owner: &str) -> Result<Option<(Vec<i64>, bool)>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE tasks SET status = 'completed'
             WHERE id = ?1 AND status = 'in_progress' AND owner = ?2",
            params![id, owner],
        )? != 1
        {
            tx.commit()?;
            return Ok(None);
        }
        let mut unblocked = {
            let mut statement = tx.prepare(
                "SELECT d.task_id
                 FROM task_dependencies d
                 JOIN tasks task ON task.id = d.task_id
                 WHERE d.blocker_id = ?1 AND task.status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1
                       FROM task_dependencies remaining
                       JOIN tasks blocker ON blocker.id = remaining.blocker_id
                       WHERE remaining.task_id = d.task_id
                         AND blocker.status <> 'completed'
                   )
                 ORDER BY d.task_id
                 LIMIT 201",
            )?;
            statement
                .query_map([id], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let truncated = unblocked.len() > 200;
        if truncated {
            unblocked.pop();
        }
        tx.commit()?;
        Ok(Some((unblocked, truncated)))
    }

    pub fn fail(&mut self, id: i64, owner: &str) -> Result<bool> {
        self.finish(id, owner, "failed")
    }

    pub fn allocate_agent_id(&mut self) -> Result<u64> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO agent_id_sequence DEFAULT VALUES", [])?;
        let id = u64::try_from(tx.last_insert_rowid()).context("agent id is out of range")?;
        tx.commit()?;
        Ok(id)
    }

    pub fn agent_exists(&self, id: u64) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_id_sequence WHERE id = ?1)",
            [i64::try_from(id).context("agent id is out of range")?],
            |row| row.get(0),
        )?)
    }

    pub fn send_message(
        &mut self,
        sender: &str,
        recipient: &str,
        body: &str,
        dedupe_key: &str,
    ) -> Result<(i64, bool)> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO agent_messages (sender, recipient, body, dedupe_key)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (sender, dedupe_key) DO NOTHING",
            params![sender, recipient, body, dedupe_key],
        )? == 1;
        let (id, stored_recipient, stored_body): (i64, String, String) = tx.query_row(
            "SELECT id, recipient, body FROM agent_messages
             WHERE sender = ?1 AND dedupe_key = ?2",
            params![sender, dedupe_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if stored_recipient != recipient || stored_body != body {
            bail!("message dedupe key already identifies different content");
        }
        tx.commit()?;
        Ok((id, inserted))
    }

    pub fn receive_messages(
        &mut self,
        recipient: &str,
        limit: usize,
        max_projection_bytes: usize,
    ) -> Result<Value> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut messages = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT id, sender, body, dedupe_key, created_at
                 FROM agent_messages
                 WHERE recipient = ?1 AND delivered_at IS NULL
                 ORDER BY id
                 LIMIT ?2",
            )?;
            let mut rows = statement.query(params![recipient, limit as i64])?;
            while let Some(row) = rows.next()? {
                messages.push(json!({
                    "id": row.get::<_, i64>(0)?,
                    "sender": row.get::<_, String>(1)?,
                    "message": row.get::<_, String>(2)?,
                    "key": row.get::<_, String>(3)?,
                    "created_at": row.get::<_, i64>(4)?,
                }));
                if serde_json::to_vec(&json!({
                    "recipient": recipient,
                    "messages": &messages,
                }))?
                .len()
                    > max_projection_bytes
                {
                    messages.pop();
                    break;
                }
            }
        }
        {
            let mut deliver = tx.prepare(
                "UPDATE agent_messages SET delivered_at = unixepoch()
                 WHERE id = ?1 AND delivered_at IS NULL",
            )?;
            for message in &messages {
                deliver.execute([message["id"].as_i64().context("message id is invalid")?])?;
            }
        }
        tx.commit()?;
        Ok(json!({ "recipient": recipient, "messages": messages }))
    }

    fn finish(&mut self, id: i64, owner: &str, status: &str) -> Result<bool> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let finished = tx.execute(
            "UPDATE tasks SET status = ?3
             WHERE id = ?1 AND status = 'in_progress' AND owner = ?2",
            params![id, owner, status],
        )? == 1;
        tx.commit()?;
        Ok(finished)
    }
}
