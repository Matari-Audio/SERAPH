use std::{collections::HashSet, path::Path, time::UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{fs::File, io::AsyncReadExt, sync::RwLock};

const CATALOG_GENERATION: u64 = 1;
const DEFAULT_READ_BYTES: u64 = 64 * 1024;
const MAX_READ_BYTES: u64 = 128 * 1024;

#[derive(Clone, Copy)]
struct Descriptor {
    id: &'static str,
    version: &'static str,
    description: &'static str,
    effect: &'static str,
    operations: &'static [&'static str],
}

const FILESYSTEM: Descriptor = Descriptor {
    id: "filesystem",
    version: "1",
    description: "Read bounded file contents and metadata without mutating the workspace",
    effect: "observation",
    operations: &["read_text", "metadata"],
};

const CATALOG: &[Descriptor] = &[FILESYSTEM];

#[derive(Default)]
pub struct CapabilityHost {
    loaded: RwLock<HashSet<&'static str>>,
}

impl CapabilityHost {
    pub async fn dispatch(&self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "capability.search" => self.search(params),
            "capability.load" => self.load(params).await,
            "capability.call" => self.call(params).await,
            _ => bail!("unknown host method {method:?}"),
        }
    }

    fn search(&self, params: &Value) -> Result<Value> {
        let query = string(params, "query")?.to_lowercase();
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .min(50) as usize;

        // ponytail: linear scan is enough for v0; move this behind SQLite FTS
        // once a measured catalog size makes the scan material.
        let matches: Vec<_> = CATALOG
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.id.contains(&query)
                    || item.description.to_lowercase().contains(&query)
            })
            .take(limit)
            .map(|item| {
                json!({
                    "id": item.id,
                    "version": item.version,
                    "description": item.description,
                    "effect": item.effect,
                    "operations": item.operations,
                })
            })
            .collect();
        Ok(Value::Array(matches))
    }

    async fn load(&self, params: &Value) -> Result<Value> {
        let id = string(params, "id")?;
        let descriptor = descriptor(id)?;
        self.loaded.write().await.insert(descriptor.id);
        Ok(json!({
            "id": descriptor.id,
            "version": descriptor.version,
            "generation": CATALOG_GENERATION,
            "effect": descriptor.effect,
            "operations": descriptor.operations,
        }))
    }

    async fn call(&self, params: &Value) -> Result<Value> {
        let handle = params.get("handle").context("missing capability handle")?;
        let id = string(handle, "id")?;
        let descriptor = descriptor(id)?;
        let version = string(handle, "version")?;
        let generation = handle
            .get("generation")
            .and_then(Value::as_u64)
            .context("missing capability generation")?;

        if version != descriptor.version || generation != CATALOG_GENERATION {
            bail!("stale capability handle for {id:?}; load it again");
        }
        if !self.loaded.read().await.contains(descriptor.id) {
            bail!("capability {id:?} is not loaded");
        }

        let operation = string(params, "operation")?;
        let args = params.get("args").context("missing capability arguments")?;
        match (descriptor.id, operation) {
            ("filesystem", "read_text") => read_text(args).await,
            ("filesystem", "metadata") => metadata(args).await,
            _ => bail!("unknown operation {id}.{operation}"),
        }
    }
}

fn descriptor(id: &str) -> Result<Descriptor> {
    CATALOG
        .iter()
        .copied()
        .find(|item| item.id == id)
        .with_context(|| format!("unknown capability {id:?}"))
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string field {field:?}"))
}

async fn read_text(args: &Value) -> Result<Value> {
    let path = Path::new(string(args, "path")?);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_BYTES)
        .min(MAX_READ_BYTES);

    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024) as usize);
    File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let truncated = bytes.len() as u64 > max_bytes;
    if truncated {
        bytes.truncate(max_bytes as usize);
        while std::str::from_utf8(&bytes).is_err_and(|error| error.error_len().is_none()) {
            bytes.pop();
        }
    }
    let text = String::from_utf8(bytes).context("file is not UTF-8 text")?;

    Ok(json!({
        "text": text,
        "bytes_read": text.len(),
        "truncated": truncated,
    }))
}

async fn metadata(args: &Value) -> Result<Value> {
    let path = Path::new(string(args, "path")?);
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("read metadata for {}", path.display()))?;
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok());

    Ok(json!({
        "size_bytes": metadata.len(),
        "is_file": metadata.is_file(),
        "is_dir": metadata.is_dir(),
        "modified_unix_ms": modified_unix_ms,
    }))
}
