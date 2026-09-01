use std::{fs, path::Path};

use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};

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

    pub fn claim(&mut self, id: i64, owner: &str) -> Result<bool> {
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
        tx.commit()?;
        Ok(claimed)
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
