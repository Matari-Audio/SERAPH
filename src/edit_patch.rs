// Derived from OpenAI Codex's apply-patch parser and SourceFile model at
// 2c3bf4ea793aa5c590932553d242a287380e9cec, then modified for SERAPH's
// strict, update-only, exact-match preflight seam.
// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    str,
};

use anyhow::{Context, Result, bail, ensure};

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const EOF: &str = "*** End of File";
const BOM: &[u8] = b"\xef\xbb\xbf";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEdit {
    pub target: PathBuf,
    pub expected: Vec<u8>,
    pub next: Vec<u8>,
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
