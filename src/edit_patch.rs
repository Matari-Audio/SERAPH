// Derived from OpenAI Codex's apply-patch parser and SourceFile model at
// 2c3bf4ea793aa5c590932553d242a287380e9cec, then modified for SERAPH's
// strict, update-only, exact-match preflight seam.
// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    str,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, TransactionBehavior};

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const EOF: &str = "*** End of File";
const BOM: &[u8] = b"\xef\xbb\xbf";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEdit {
    pub target: PathBuf,
    pub expected: Vec<u8>,
    pub next: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPatch {
    pub project: PathBuf,
    pub inverse: Vec<PreparedEdit>,
}

impl AppliedPatch {
    /// Restores the exact bytes replaced by this patch if its outputs are unchanged.
    pub fn rollback(&self) -> Result<()> {
        apply_prepared_edits(&self.project, &self.inverse).map(|_| ())
    }
}

struct StagedFile {
    path: Option<PathBuf>,
}

impl StagedFile {
    fn create(target: &Path, contents: &[u8], permissions: fs::Permissions) -> Result<Self> {
        let parent = target
            .parent()
            .with_context(|| format!("{} has no parent directory", target.display()))?;
        let file_name = target
            .file_name()
            .with_context(|| format!("{} has no file name", target.display()))?;

        for _ in 0..1024 {
            let mut name = OsString::from(".");
            name.push(file_name);
            name.push(format!(
                ".seraph-{}-{}",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let path = parent.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let staged = Self { path: Some(path) };
                    file.write_all(contents)
                        .with_context(|| format!("stage edit for {}", target.display()))?;
                    file.set_permissions(permissions).with_context(|| {
                        format!("preserve permissions for {}", target.display())
                    })?;
                    file.sync_all()
                        .with_context(|| format!("sync staged edit for {}", target.display()))?;
                    return Ok(staged);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create staged edit for {}", target.display()));
                }
            }
        }
        bail!("could not allocate a staged file for {}", target.display())
    }

    fn replace(&mut self, target: &Path) -> Result<()> {
        let path = self.path.as_ref().context("staged edit is unavailable")?;
        fs::rename(path, target)
            .with_context(|| format!("atomically replace {}", target.display()))?;
        self.path = None;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

struct StagedEdit<'a> {
    edit: &'a PreparedEdit,
    forward: StagedFile,
    rollback: StagedFile,
}

/// Applies prepared edits through same-directory atomic replacements.
///
/// Every target is byte-checked and both directions are staged before mutation.
/// A failed multi-file commit rolls back files already replaced. The returned
/// inverse delta remains usable only while the applied bytes are unchanged.
/// Native agents sharing a project serialize through its SQLite edit lock.
pub fn apply_prepared_edits(project: &Path, edits: &[PreparedEdit]) -> Result<AppliedPatch> {
    ensure!(!edits.is_empty(), "cannot apply an empty edit set");
    let project = fs::canonicalize(project).context("resolve edit project root")?;
    let mut connection = open_workspace_database(&project)?;
    let _workspace_lock = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("acquire workspace edit lock")?;
    let mut targets = BTreeSet::new();
    let mut staged = Vec::with_capacity(edits.len());

    for edit in edits {
        ensure!(
            targets.insert(edit.target.clone()),
            "edit targets {} more than once",
            edit.target.display()
        );
        ensure!(edit.expected != edit.next, "edit leaves target unchanged");
        let permissions = verify_expected(&project, edit)?;
        staged.push(StagedEdit {
            edit,
            forward: StagedFile::create(&edit.target, &edit.next, permissions.clone())?,
            rollback: StagedFile::create(&edit.target, &edit.expected, permissions)?,
        });
    }

    for index in 0..staged.len() {
        let staged_edit = &mut staged[index];
        let result = verify_expected(&project, staged_edit.edit)
            .and_then(|_| staged_edit.forward.replace(&staged_edit.edit.target));
        if let Err(error) = result {
            let rollback = rollback_committed(&project, &mut staged[..index]);
            return match rollback {
                Ok(()) => Err(error.context("edit commit failed; prior replacements rolled back")),
                Err(rollback_error) => Err(error.context(format!(
                    "edit commit failed and rollback was incomplete: {rollback_error:#}"
                ))),
            };
        }
    }

    Ok(AppliedPatch {
        project,
        inverse: edits
            .iter()
            .map(|edit| PreparedEdit {
                target: edit.target.clone(),
                expected: edit.next.clone(),
                next: edit.expected.clone(),
            })
            .collect(),
    })
}

fn rollback_committed(project: &Path, staged: &mut [StagedEdit<'_>]) -> Result<()> {
    let mut failures = Vec::new();
    for staged in staged.iter_mut().rev() {
        let inverse = PreparedEdit {
            target: staged.edit.target.clone(),
            expected: staged.edit.next.clone(),
            next: staged.edit.expected.clone(),
        };
        if let Err(error) = verify_expected(project, &inverse)
            .and_then(|_| staged.rollback.replace(&staged.edit.target))
        {
            let recovery = staged.rollback.path.take().map_or_else(
                || "recovery artifact unavailable".to_owned(),
                |path| format!("exact recovery bytes retained at {}", path.display()),
            );
            failures.push(format!(
                "{}: {error:#}; {recovery}",
                staged.edit.target.display()
            ));
        }
    }
    ensure!(
        failures.is_empty(),
        "could not restore {}",
        failures.join("; ")
    );
    Ok(())
}

fn verify_expected(project: &Path, edit: &PreparedEdit) -> Result<fs::Permissions> {
    ensure!(
        edit.target.starts_with(project),
        "edit target is outside project root: {}",
        edit.target.display()
    );
    let metadata = inspect_regular_path(&edit.target)?;
    let actual = fs::read(&edit.target)
        .with_context(|| format!("read edit target {}", edit.target.display()))?;
    ensure!(
        actual == edit.expected,
        "edit target changed concurrently: {}",
        edit.target.display()
    );
    inspect_regular_path(&edit.target)
        .with_context(|| format!("reinspect edit target {}", edit.target.display()))?;
    Ok(metadata.permissions())
}

fn open_workspace_database(project: &Path) -> Result<Connection> {
    let state = project.join(".seraph");
    fs::create_dir_all(&state).context("create SERAPH state directory")?;
    let metadata = fs::symlink_metadata(&state).context("inspect SERAPH state directory")?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "SERAPH state path is not a regular directory"
    );
    let database = state.join("edit-lock.sqlite3");
    match fs::symlink_metadata(&database) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "SERAPH edit lock database is not a regular file"
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect SERAPH edit lock database"),
    }
    let connection = Connection::open(&database).context("open SERAPH workspace database")?;
    let metadata =
        fs::symlink_metadata(&database).context("reinspect SERAPH edit lock database")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "SERAPH edit lock database is not a regular file"
    );
    connection
        .busy_timeout(Duration::from_secs(5))
        .context("configure workspace edit lock timeout")?;
    Ok(connection)
}

fn inspect_regular_path(target: &Path) -> Result<fs::Metadata> {
    ensure!(
        target.is_absolute(),
        "edit target must be absolute: {}",
        target.display()
    );
    let mut resolved = PathBuf::new();
    let mut metadata = None;
    for component in target.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::Normal(part) => {
                resolved.push(part);
                let current = fs::symlink_metadata(&resolved)
                    .with_context(|| format!("inspect edit target {}", target.display()))?;
                ensure!(
                    !current.file_type().is_symlink(),
                    "edit target crosses a symlink: {}",
                    target.display()
                );
                metadata = Some(current);
            }
            Component::CurDir | Component::ParentDir => {
                bail!("edit target is not normalized: {}", target.display())
            }
        }
    }
    let metadata = metadata.context("edit target has no file component")?;
    ensure!(
        metadata.is_file(),
        "edit target is not a regular file: {}",
        target.display()
    );
    Ok(metadata)
}

#[derive(Debug)]
struct UpdateFile {
    path: PathBuf,
    chunks: Vec<Chunk>,
}

#[derive(Debug, Default)]
struct Chunk {
    context: Option<String>,
    changes: Vec<Change>,
    eof: bool,
}

#[derive(Debug)]
enum Change {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone)]
struct SourceLine {
    text: String,
    ending: Ending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Lf,
    CrLf,
    Cr,
    None,
}

/// Parses and derives every update before any caller writes a byte.
///
/// V0 deliberately rejects add/delete/move hunks, fuzzy matches, duplicate
/// targets, symlinks, invalid UTF-8, and ambiguous exact matches.
pub fn prepare_exact_patch(root: &Path, patch: &str) -> Result<Vec<PreparedEdit>> {
    let root = fs::canonicalize(root).context("resolve patch root")?;
    let updates = parse(patch)?;
    let mut targets = BTreeSet::new();
    let mut prepared = Vec::with_capacity(updates.len());

    for update in updates {
        let target = resolve_target(&root, &update.path)?;
        ensure!(
            targets.insert(target.clone()),
            "patch targets {} more than once",
            update.path.display()
        );
        let expected = fs::read(&target)
            .with_context(|| format!("read patch target {}", update.path.display()))?;
        let (bom, body) = expected
            .strip_prefix(BOM)
            .map_or((&[][..], expected.as_slice()), |body| (BOM, body));
        let source = str::from_utf8(body)
            .with_context(|| format!("{} is not UTF-8", update.path.display()))?;
        let mut lines = split_source(source);
        apply_chunks(&mut lines, &update.chunks, &update.path)?;
        let mut next = Vec::with_capacity(expected.len());
        next.extend_from_slice(bom);
        render_source(&lines, &mut next);
        ensure!(
            next != expected,
            "patch leaves {} unchanged",
            update.path.display()
        );
        prepared.push(PreparedEdit {
            target,
            expected,
            next,
        });
    }
    Ok(prepared)
}

fn parse(patch: &str) -> Result<Vec<UpdateFile>> {
    let lines: Vec<_> = patch
        .split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    ensure!(
        lines.first() == Some(&BEGIN),
        "patch must start with {BEGIN:?}"
    );
    ensure!(lines.last() == Some(&END), "patch must end with {END:?}");

    let mut updates = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line_number = index + 1;
        let path = lines[index]
            .strip_prefix(UPDATE)
            .with_context(|| format!("unsupported hunk at line {line_number}"))?;
        ensure!(!path.is_empty(), "empty update path at line {line_number}");
        index += 1;

        let mut chunks = Vec::new();
        let mut chunk = Chunk::default();
        while index + 1 < lines.len() && !lines[index].starts_with(UPDATE) {
            let line = lines[index];
            let line_number = index + 1;
            ensure!(
                !chunk.eof,
                "content follows EOF marker at line {line_number}"
            );
            if line.starts_with(MOVE) {
                bail!("move hunks are not supported at line {line_number}");
            }
            if line == EOF {
                ensure!(
                    !chunk.changes.is_empty(),
                    "empty EOF chunk at line {line_number}"
                );
                chunk.eof = true;
                index += 1;
                continue;
            }
            if line == "@@" || line.starts_with("@@ ") {
                if chunk.context.is_some() || !chunk.changes.is_empty() {
                    validate_chunk(&chunk, line_number)?;
                    chunks.push(chunk);
                    chunk = Chunk::default();
                }
                chunk.context = line.strip_prefix("@@ ").map(str::to_owned);
                index += 1;
                continue;
            }
            let (kind, text) = line.split_at_checked(1).with_context(|| {
                format!("patch line {line_number} must start with ' ', '+', or '-'")
            })?;
            chunk.changes.push(match kind {
                " " => Change::Context(text.to_owned()),
                "+" => Change::Add(text.to_owned()),
                "-" => Change::Remove(text.to_owned()),
                _ => bail!("invalid patch line {line_number}"),
            });
            index += 1;
        }
        validate_chunk(&chunk, index + 1)?;
        chunks.push(chunk);
        updates.push(UpdateFile {
            path: PathBuf::from(path),
            chunks,
        });
    }
    ensure!(!updates.is_empty(), "patch contains no update hunks");
    Ok(updates)
}

fn validate_chunk(chunk: &Chunk, line_number: usize) -> Result<()> {
    ensure!(
        !chunk.changes.is_empty(),
        "empty update chunk before line {line_number}"
    );
    ensure!(
        chunk
            .changes
            .iter()
            .any(|change| !matches!(change, Change::Context(_))),
        "update chunk before line {line_number} contains no change"
    );
    Ok(())
}

fn resolve_target(root: &Path, relative: &Path) -> Result<PathBuf> {
    let parts: Vec<_> = relative.components().collect();
    ensure!(
        !parts.is_empty()
            && parts
                .iter()
                .all(|part| matches!(part, Component::Normal(_))),
        "patch target must be a relative path without parent traversal: {}",
        relative.display()
    );
    let mut target = root.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        target.push(part.as_os_str());
        let metadata = fs::symlink_metadata(&target)
            .with_context(|| format!("inspect patch target {}", relative.display()))?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "patch target crosses a symlink"
        );
        ensure!(
            if index + 1 == parts.len() {
                metadata.is_file()
            } else {
                metadata.is_dir()
            },
            "patch target is not a regular file"
        );
    }
    Ok(target)
}

fn apply_chunks(lines: &mut Vec<SourceLine>, chunks: &[Chunk], path: &Path) -> Result<()> {
    let trailing_newline = lines.last().is_some_and(|line| line.ending != Ending::None);
    let default_ending = lines
        .iter()
        .find_map(|line| (line.ending != Ending::None).then_some(line.ending))
        .unwrap_or(Ending::Lf);
    let mut cursor = 0;

    for chunk in chunks {
        if let Some(context) = &chunk.context {
            let matches: Vec<_> = (cursor..lines.len())
                .filter(|index| lines[*index].text == *context)
                .collect();
            ensure!(
                matches.len() == 1,
                "context in {} is missing or ambiguous",
                path.display()
            );
            cursor = matches[0] + 1;
        }

        let old: Vec<_> = chunk
            .changes
            .iter()
            .filter_map(|change| match change {
                Change::Context(text) | Change::Remove(text) => Some(text.as_str()),
                Change::Add(_) => None,
            })
            .collect();
        ensure!(
            !old.is_empty() || chunk.context.is_some() || chunk.eof,
            "insertion in {} has no exact anchor",
            path.display()
        );
        let start = unique_match(lines, &old, cursor, chunk.eof).with_context(|| {
            format!("exact chunk in {} is missing or ambiguous", path.display())
        })?;
        let end = start + old.len();
        ensure!(
            !chunk.eof || end == lines.len(),
            "EOF chunk does not reach end of file"
        );

        let mut matched = lines[start..end].iter();
        let replacement = chunk
            .changes
            .iter()
            .filter_map(|change| match change {
                Change::Context(_) => matched.next().cloned(),
                Change::Remove(_) => {
                    matched.next();
                    None
                }
                Change::Add(text) => Some(SourceLine {
                    text: text.clone(),
                    ending: default_ending,
                }),
            })
            .collect::<Vec<_>>();
        let replacement_len = replacement.len();
        lines.splice(start..end, replacement);
        cursor = start + replacement_len;
    }

    if let Some(last) = lines.last_mut() {
        last.ending = if trailing_newline {
            if last.ending == Ending::None {
                default_ending
            } else {
                last.ending
            }
        } else {
            Ending::None
        };
    }
    Ok(())
}

fn unique_match(lines: &[SourceLine], old: &[&str], cursor: usize, eof: bool) -> Option<usize> {
    if old.is_empty() {
        return (eof || cursor <= lines.len()).then_some(if eof { lines.len() } else { cursor });
    }
    if old.len() > lines.len() {
        return None;
    }
    let candidates = cursor..=lines.len() - old.len();
    let mut matches = candidates.filter(|start| {
        lines[*start..*start + old.len()]
            .iter()
            .map(|line| line.text.as_str())
            .eq(old.iter().copied())
            && (!eof || *start + old.len() == lines.len())
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn split_source(source: &str) -> Vec<SourceLine> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let ending = match bytes[index] {
            b'\n' => Some((Ending::Lf, 1)),
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => Some((Ending::CrLf, 2)),
            b'\r' => Some((Ending::Cr, 1)),
            _ => None,
        };
        if let Some((ending, width)) = ending {
            lines.push(SourceLine {
                text: source[start..index].to_owned(),
                ending,
            });
            index += width;
            start = index;
        } else {
            index += 1;
        }
    }
    if start < source.len() {
        lines.push(SourceLine {
            text: source[start..].to_owned(),
            ending: Ending::None,
        });
    }
    lines
}

fn render_source(lines: &[SourceLine], output: &mut Vec<u8>) {
    for line in lines {
        output.extend_from_slice(line.text.as_bytes());
        output.extend_from_slice(match line.ending {
            Ending::Lf => b"\n",
            Ending::CrLf => b"\r\n",
            Ending::Cr => b"\r",
            Ending::None => b"",
        });
    }
}
