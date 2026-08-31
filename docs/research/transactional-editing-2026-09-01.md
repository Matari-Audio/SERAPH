# Transactional editing mechanisms for SERAPH

_Researched 2026-09-01 against pinned upstream source. Scope: a standalone Rust/Ratatui host, not a Prime Agent fork._

## Decision

SERAPH v0 should use a small Rust patch engine extracted from Codex's Apache-2.0 patch-body parser, update derivation, line-ending model, and committed-delta types. It should **not** depend on the complete `codex-apply-patch` crate. Wrap the extracted engine in a SERAPH-owned commit layer with canonical-path locks, byte compare-and-swap (CAS), explicit partial-failure results, and structured before/after artifacts. Codex supplies the best parser and delta vocabulary; OpenCode supplies the clearest CAS seam; Grok Build supplies the best fail-closed stale-reference UX and LSP event seam; Prime supplies a useful lightweight baseline but no Rust worth transplanting.

Do not call this multi-file atomicity. None of the four inspected implementations offers an all-or-nothing, crash-safe multi-file transaction. Codex and OpenCode apply files sequentially and retain an applied prefix on failure; Grok's “atomic” claim is limited to computing a same-file batch before one direct write; Prime computes one file's replacements before one direct write. [Codex records the committed prefix and marks it inexact after uncertain writes](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L245-L335), [OpenCode documents sequential partial application](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/tool/apply-patch.ts#L69-L84), [Grok validates one file's batch before returning one new string](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/edit/apply.rs#L143-L230), and [Prime writes one completed file directly](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit.ts#L402-L424).

### What “transactional” must mean in SERAPH

Keep four guarantees separate:

1. **Preflight:** parse, resolve paths, read expected bytes, derive every candidate change, and reject invalid/overlapping edits before writing.
2. **Race-safe commit for SERAPH participants:** while holding the canonical target lock, compare current bytes with the bytes used during preflight; reject stale content instead of overwriting it. An uncooperative external process can still write between a userspace comparison and write unless SERAPH adds an OS-level protocol.
3. **Failure truth:** return the exact committed prefix, original/final bytes or artifact handles, and an `exact` flag. Never report a predicted diff as committed.
4. **Durability/rollback:** temp-file replacement, multi-file rollback, and crash recovery are separate later features. A process-local mutex or in-memory inverse is not durable atomicity.

## Source baseline

| System | Pinned source | Release history checked | License relevant to reuse |
|---|---|---|---|
| Codex | [`2c3bf4e` (2026-08-31)](https://github.com/openai/codex/commit/2c3bf4ea793aa5c590932553d242a287380e9cec) | [`rust-v0.151.0`](https://github.com/openai/codex/releases/tag/rust-v0.151.0) | [Apache-2.0](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/LICENSE) |
| OpenCode | [`0428492` (2026-08-31)](https://github.com/anomalyco/opencode/commit/04284921ac8f657555b5a182f5ff055f471543e4) | [`v1.18.25`](https://github.com/anomalyco/opencode/releases/tag/v1.18.25) | [MIT](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/LICENSE) |
| Grok Build | [`bb7f39d` (2026-08-31)](https://github.com/xai-org/grok-build/commit/bb7f39d5858cbf5e00de639367f59debbdcb0138) | repository had no tagged GitHub release at research time | [Apache-2.0](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/LICENSE) |
| Prime Agent | [`9f5edc1` (2026-08-31)](https://github.com/PrimeIntellect-ai/prime-agent/commit/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09) | [`v0.8.1`](https://github.com/PrimeIntellect-ai/prime-agent/releases/tag/v0.8.1) | [MIT](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/LICENSE) |

The findings below are code-derived. Release pages were checked for drift; no non-English first-party source added a unique implementation fact.

## Comparison

| Concern | Codex | OpenCode | Grok Build | Prime Agent |
|---|---|---|---|---|
| Canonical edit form | Multi-file Add/Delete/Update/Move patch | Stable fuzzy string edit plus newer patch/CAS core | Hashline anchored same-file batches; older Codex/OpenCode ports also exist | Same-file multi-string replacement |
| Parse/derive before write | Yes | Yes | Yes | Yes |
| Same-path coordination | No lock in inspected patch subsystem | Process-local keyed lock; newer core adds byte CAS | Lock manager exists, but is not wired to hashline edit | Process-local canonical-path promise queue |
| Stale pre-commit detection | No CAS | New core: byte CAS on updates, exclusive create | No CAS | No CAS |
| Multi-file all-or-nothing | No | No | Not a multi-file tool | Not a multi-file tool |
| Ambiguity | First acceptable ordered match | Usually rejects duplicate final span; one heuristic can choose “best” candidate | Explicit stale / unique shift / ambiguous / not found | Rejects duplicate normalized match and overlap |
| Line endings | Optional per-line LF/CRLF/CR preservation | String edit chooses LF/CRLF; patch normalizes LF | Rebuilds with LF | Chooses LF/CRLF for the whole file |
| BOM | No explicit policy in patch core | Explicit UTF-8 BOM preservation | No explicit policy | Explicit UTF-8 BOM preservation |
| Symlinks | Explicit follow/no-follow policy with runtime selection | Existing paths canonicalized; no explicit edit no-follow policy | Existing paths canonicalized; no explicit edit no-follow policy | `realpath` key and normal filesystem following |
| Formatter/LSP | Diff tracking; no formatter/LSP in patch core | Stable edit formats, recomputes final diff, then reports LSP diagnostics | Cross-cutting mutation-to-LSP reminder; no hashline formatter | Neither |
| Review | Exact-delta net Git diff plus rich TUI | Unified diff metadata | Best Ratatui rendering, conservative hunk stitching | Good preview/result reconciliation |
| Rollback | None in patch engine | Separate Git snapshot/revert system | Separate prompt rewind/file-state system | None |

## Codex: transplant the patch core, not the crate

### Mechanism

Codex defines a model-friendly grammar for Add, Delete, Update, Move, ordered chunks, context anchors, and end-of-file constraints. Its parser is deliberately lenient around markers and heredocs, while its streaming parser accepts deltas and returns the hunks accumulated so far for live previews. [The grammar and hunk model are explicit in `parser.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/parser.rs#L1-L142); [the lenient heredoc boundary exception is narrow](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/parser.rs#L230-L273); [`StreamingPatchParser::push_delta` exposes partial hunks](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/streaming_parser.rs#L21-L151).

Update matching tries exact, trailing-whitespace-insensitive, fully trimmed, then typographic-punctuation-normalized comparisons. It returns the first match at or after the current cursor; ordered chunks and context reduce ambiguity, but uniqueness is not proven. [The search order and first-return behavior are visible in `seek_sequence.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/seek_sequence.rs#L1-L105). SERAPH should keep this tolerant ladder only after exact matching, but add an ambiguity check before accepting a tolerant candidate.

Codex separates syntactic parsing from filesystem verification. Verification resolves every path, rejects two operations aimed at the same resolved source path, reads delete contents, and pre-derives update contents/diffs into an `ApplyPatchAction`. [See `try_verify_apply_patch_args`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/invocation.rs#L200-L295). This is useful preflight, but there is no per-path lock or expected-byte CAS in the inspected patch executor, so SERAPH must not treat the verified action as a race-proof commit authorization.

Codex's strongest recent mechanism is its committed-delta model. Success returns ordered Add/Delete/Update/Move data with old and new content. Failure carries mutations definitely committed before the error. A write error marks the delta inexact because truncation may have occurred before the error surfaced. [The delta shapes and failure wrapper are defined here](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L245-L335); [the sequential writer and inexact-on-write-error rule are here](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L421-L533). Codex core forwards the runtime's committed delta even when execution fails, rather than substituting the preflight prediction. [See the handler's result split](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/apply_patch.rs#L558-L614).

The current writer is permissive in ways SERAPH should not inherit silently: Add records and overwrites pre-existing content, while Move writes or overwrites the destination before removing the source. A failed source removal therefore leaves a committed destination write in the partial delta. [See Add overwrite capture](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L504-L534) and [Move ordering/partial-delta conversion](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L596-L691). SERAPH Add should default to create-new; Move should require an explicit overwrite policy.

The optional preservation mode keeps each unchanged line's LF, CRLF, or CR ending, uses the first observed ending for inserted lines, and preserves Codex's historical trailing-newline behavior. Standalone defaults still normalize to LF. [The mode/default are explicit](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L55-L95); [`SourceFile` implements per-line retention](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/text_file.rs#L1-L120). There is no explicit UTF-8 BOM split/join policy in this patch core, so SERAPH must add one rather than infer support from ordinary string retention.

Symlink handling is explicit: standalone callers follow links by default, and the Codex runtime selects follow/no-follow according to the sandbox attempt. [The public option is here](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/lib.rs#L72-L86); [runtime selection is here](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/runtimes/apply_patch.rs#L168-L207). Even without SERAPH sandboxing, the edit API should expose a stable policy because following a symlink changes which file owns the delta and lock.

For review, `TurnDiffTracker` consumes only exact committed deltas, invalidates itself otherwise, and builds a net Git-style diff without rereading potentially changed workspace files. [Tracking/invalidation](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/turn_diff_tracker.rs#L47-L112) and [bounded `similar`-based rendering](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/turn_diff_tracker.rs#L308-L365) are good models for SERAPH artifacts.

### Reuse seam and weight

Do not add `codex-apply-patch` as a dependency. Its package currently depends on internal Codex executor/path crates plus `tree-sitter` and `tree-sitter-bash`; shell invocation extraction alone imports the executor and Bash AST machinery. [See the crate dependencies](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/Cargo.toml#L19-L28) and [the invocation imports](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/apply-patch/src/invocation.rs#L1-L28).

The smallest viable Apache-2.0 source transplant is:

- patch-body `Hunk`/`UpdateFileChunk` grammar and parser;
- update derivation and tolerant line seeker;
- `SourceFile` line-ending representation;
- a reduced committed-delta/failure model;
- optionally the streaming body parser, only if SERAPH previews partial tool arguments.

Replace `PathUri`, `ExecutorFileSystem`, sandbox context, shell-command detection, summaries, and Codex runtime orchestration with SERAPH traits. Record copied-file provenance and Apache notices. Keep shell interception out of v0: SERAPH can expose a typed `edit.patch` capability directly.

## OpenCode: port the CAS and feedback order

OpenCode now has two relevant layers. The stable edit tool is broad and forgiving; a newer core patch path is narrower and makes concurrency semantics explicit.

The stable edit tool serializes cooperating edits by resolved path, then reads, matches, writes, optionally formats, recomputes the final diff, publishes file events, touches LSP, and returns diagnostics. [Lock and execution setup](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/edit.ts#L35-L88), [write/format/final-diff ordering](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/edit.ts#L123-L171), and [LSP feedback](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/edit.ts#L175-L211) are a useful integration contract. It explicitly preserves a UTF-8 BOM and converts requested text to the file's chosen LF/CRLF style, although that whole-file choice does not preserve mixed endings line by line. [See the BOM/ending path](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/edit.ts#L123-L155).

Its recovery ladder includes exact, line-trimmed, block-anchor/Levenshtein, whitespace, indentation, escape, boundary, context, and occurrence matchers. The final replacement normally requires a unique matched span and rejects disproportionate spans. [The ladder and final uniqueness check are here](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/edit.ts#L682-L728). However, block-anchor recovery can score multiple candidates and yield the single highest scorer above a 0.65 threshold, without rejecting a tie or near-tie. [See candidate selection](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/edit.ts#L329-L410). SERAPH should not port that policy unchanged: fuzzy recovery must return candidates or fail when confidence is ambiguous.

The newer `packages/core` patch parser is a compact TypeScript behavioral reference: it is strict about invalid lines, preserves BOM, and otherwise mirrors Codex's first-match exact/rstrip/trim/Unicode ladder. It normalizes reconstructed updates to LF. [Parser/derive](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/patch.ts#L25-L80) and [matching](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/patch.ts#L132-L195) show the seam.

More importantly, `FileMutation.writeIfUnchanged` takes expected bytes and compares them under the same canonical-target process-local mutex used for the write; create uses exclusive `wx`. [The interface and lock contract](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/file-mutation.ts#L54-L83), [exclusive create](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/file-mutation.ts#L124-L142), and [byte CAS](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/file-mutation.ts#L144-L157) are the best concurrency contract inspected. Its apply-patch tool still commits sequentially and reports the applied prefix; moves and atomic rollback are deliberately unsupported. [Preparation/commit path](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/tool/apply-patch.ts#L125-L189). The source itself records formatter, watcher, snapshot, LSP, multi-file rollback, and crash recovery as deferred work. [See the explicit TODO boundary](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/file-mutation.ts#L198-L207).

Port the contract, not the Effect/TypeScript code: a Rust keyed lock around `(read current bytes, compare expected, commit)` and the formatter/LSP ordering. OpenCode's separate Git-object snapshot service supports post-hoc restore but is a subprocess/storage subsystem, not a commit primitive. [Its interface and Git-backed state are visible here](https://github.com/anomalyco/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/snapshot/index.ts#L23-L75); defer it.

## Grok Build: borrow fail-closed anchors and the event seam

Grok Build contains an older Rust Codex patch port and an OpenCode-style edit port, but upstream Codex/OpenCode are better references for those mechanisms. Grok's unique mechanism is hashline editing.

Hashline read output gives a line number plus local/context fingerprint. The recommended candidate for benchmarking is `ChunkFingerprint`; shifted-anchor recovery searches a bounded ±15-line window and distinguishes exactly one match, multiple matches, and no match. [The three scheme tradeoffs](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/scheme.rs#L1-L17), [validation contract](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/scheme.rs#L23-L66), and [shift result/default radius](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/scheme.rs#L161-L190) are explicit.

All operations in a same-file batch are resolved against one pre-edit snapshot. One stale anchor or overlap returns no new content; valid operations are applied bottom-up. [The all-or-none derivation behavior is here](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/edit/apply.rs#L143-L244) and [overlap checks are here](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/edit/apply.rs#L670-L713). Stale references do not silently retarget: unique shifted content returns a fresh retry anchor, while multiple candidates produce an ambiguity error and bounded fresh context. [See stale recovery](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/edit/apply.rs#L575-L665). Success and failure both return fresh local anchors, explicitly allowing retry without another full read. [The tool contract states this](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/edit/mod.rs#L50-L64). That is the best token-efficiency idea in this editing survey.

The current disk boundary is weaker than the in-memory algorithm: it canonicalizes and reads, decodes invalid UTF-8 lossily, derives, then directly writes without CAS. [Read/derive/write path](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build_hashline/edit/mod.rs#L318-L404). This creates a race window and can corrupt non-UTF-8 input; reconstruction uses line strings rather than an explicit BOM/per-line-ending model. A capable FIFO per-path/exclusive lock manager exists, but source search at the pinned commit found it exported only from `editor_infra` and used by its own tests, not by hashline edit. [Its semantics are here](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/editor_infra/file_operation_lock.rs#L1-L115). SERAPH can copy the idea, but a simpler keyed lock is sufficient unless whole-workspace exclusivity is required.

Grok's cross-cutting LSP reminder is a strong boundary: structured successful mutation output becomes create/change/delete events, then pending diagnostics are drained with a timeout. [See the reminder](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/reminders/lsp_diagnostics.rs#L1-L95). SERAPH should model one `FileCommitted` event consumed independently by LSP, diff review, watcher, and journal projection.

Its review system is excellent but not a v0 transplant. The diff crate builds structured numbered hunks and conservatively refuses to stitch sequential hunks when coordinates/text cannot truthfully describe one result. [The data model](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager-diff/src/lib.rs#L1-L20) and [fail-closed stitcher](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager-diff/src/lib.rs#L168-L288) are reusable design references. Ratatui rendering performs cheap hunk-only first paint and upgrades to full-file syntax scope only below 2 MiB/50,000-line caps. [See the caps and progressive phases](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/edit.rs#L1-L69). However, `xai-grok-pager-diff` depends on the very large `xai-grok-tools` graph, ACP, and JSON, while `xai-grok-tools` includes LSP, network, image, PDF, archive, auth, sandbox, and multiple internal crates. [Diff crate dependencies](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager-diff/Cargo.toml#L9-L20) and [tool crate dependencies](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/Cargo.toml#L8-L94) rule out direct reuse.

Grok also has a separate prompt-level rewind system that captures file state before reads/writes, lazily loads historical points, and captures after-snapshots at prompt end. [See `FileStateTracker`](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-workspace/src/session/file_state.rs#L454-L585). Its restore orchestration explicitly permits partial domain rewind when Git and filesystem restoration diverge. [See `rewind_to`](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-workspace/src/session/checkpoint.rs#L367-L447). This is valuable product recovery, but it is far above the edit commit seam and should not be mistaken for per-call rollback.

The inspected hashline/edit source is also thousands of lines before integration, so hashline should be a measured follow-up. Benchmark total input/output tokens and retry rate against contextual Codex patches on repeated-line files, shifted files, and parallel-agent races. Adopt it only if anchors reduce total model-visible text after accounting for the anchors added to every read line.

## Prime Agent: preserve only the small behavioral lessons

Prime's edit tool matches multiple replacements against the same original file, requires each normalized match to be unique, rejects overlaps, and applies from the end so offsets remain stable. [See matching, uniqueness, overlap, and reverse application](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit-diff.ts#L172-L246). Its canonical-path promise queue is only 39 lines and allows different files to proceed in parallel. [See the queue](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/file-mutation-queue.ts#L1-L39). These are useful requirements, but OpenCode's byte CAS is a stronger model for a parallel multi-agent host.

Prime preserves UTF-8 BOM and chooses LF/CRLF based on the first line ending. [Ending/BOM helpers](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit-diff.ts#L11-L25) and [the commit path](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit.ts#L409-L424) are straightforward. Its fuzzy fallback is risky: once any edit needs fuzzy matching, the entire file becomes an NFKC/trailing-whitespace/typographic-punctuation-normalized base, so unrelated text can change. [The normalization](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit-diff.ts#L27-L43) and [whole-base switch](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit-diff.ts#L196-L218) make this explicit. Do not port that behavior.

The preview/result reconciliation is worth retaining as a UI invariant: Prime suppresses predicted diffs on failed execution and replaces a preview when the committed result diff differs. [See rendering behavior](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit.ts#L209-L235) and [failure suppression](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/edit.ts#L281-L319).

## Recommended standalone Rust design

### Minimal v0 module

```text
edit_patch        parse and derive; no I/O
    ↓ PreparedEdit { target, expected_bytes, next_bytes, metadata }
edit_commit       canonical locks + CAS + sequential explicit commit
    ↓ CommittedDelta | CommitFailure { committed_prefix, exact }
file_committed    event consumed by journal, diff view, LSP, watcher
```

The v0 API should be approximately:

```rust
struct PreparedEdit {
    target: CanonicalTarget,
    expected: ExpectedState, // Missing or exact byte digest + retained bytes/artifact
    next: NextState,         // Delete or exact bytes
    display: FileDelta,
}

enum CommitOutcome {
    Complete(Vec<CommittedDelta>),
    Partial { committed: Vec<CommittedDelta>, failure: CommitFailure, exact: bool },
}
```

Implementation rules:

1. Resolve and deduplicate targets; acquire all affected canonical-path locks in sorted order. This prevents same-process agent/tool races and lock-order deadlocks while preserving parallelism across disjoint paths.
2. Re-read under lock and compare exact expected bytes before each commit. Add must use create-new semantics; update/delete must reject missing or changed bytes. This is OpenCode's CAS contract in Rust.
3. Parse and derive all edits before the first mutation. Reject duplicate target instructions, overlapping ranges, invalid UTF-8, ambiguous tolerant matches, and unsupported symlink cases during preflight.
4. Preserve a UTF-8 BOM explicitly and use Codex's per-line ending representation. Never use lossy UTF-8 conversion at the write boundary.
5. Commit sequentially in v0 and report the exact prefix. Retain original/final bytes as artifact handles so review and later manual recovery do not reread mutable workspace state. Do not auto-rollback a partially failed batch until rollback failure semantics are designed.
6. Emit `FileCommitted` only from actual committed deltas. Formatter output, if enabled later, is another mutation inside the same per-file lock/CAS boundary; recompute the final delta before emitting. LSP diagnostics run after the event and are bounded summaries, not raw logs.
7. Render a simple `similar` unified diff in Ratatui first. Keep structured line/tag data so Grok-style numbered, wrapped, progressively highlighted rendering can be added without changing the commit engine.

### Smallest source transplant

For v0, extract only Codex's patch-body types/parser, update derivation/seeker, and `SourceFile` line-ending model into a SERAPH-local crate. Expect to adapt path and error types. Implement the lock/CAS/event shell natively in SERAPH; it is smaller and safer than importing OpenCode's Effect runtime or Grok's tool graph. Use `similar` for diff generation if it is already selected for the Ratatui review surface; otherwise keep diff generation behind a trait until the UI dependency is chosen.

Defer:

- shell-command interception and Tree-sitter Bash;
- OpenCode's heuristic fuzzy matcher ladder;
- Grok hashline until token benchmarks justify its surface;
- formatter orchestration;
- semantic/LSP edits (keep LSP diagnostics read-only after commit initially);
- Git-backed snapshots and whole-prompt rewind;
- multi-file rollback and crash recovery;
- Grok's full pager/highlighter transplant.

## Token-efficiency consequences

Editing efficiency is total task tokens, not shortest edit arguments in isolation.

- One multi-file patch call collapses many model/tool round trips and returns one compact status plus artifact handles.
- Exact committed deltas avoid rereading entire files to render review or reconstruct what happened.
- CAS failures should return a bounded fresh context/anchor snippet, not the full file. Grok demonstrates that a failed stale edit can often retry without another read.
- Tolerant matching can save a reread, but silent heuristic retargeting is costlier than failure because it creates repair work. Return ambiguity candidates.
- Hashline may reduce `oldText` and context tokens, but it adds anchors to read output. Measure end-to-end tokens and retries before adoption.
- Formatter and LSP output should be stored as artifacts and projected as counts plus the few relevant diagnostics.

## Final recommendation

Build **Codex parser/derive + SERAPH CAS commit layer + Grok-style fail-closed recovery/event UX**. Borrow OpenCode's concurrency and feedback ordering, and Prime's simple same-file batching/UI truthfulness. This is the smallest Rust-native Frankenstein that earns its code:

- Codex is the canonical edit language and committed-delta reference.
- SERAPH owns race safety and explicit failure semantics.
- Grok supplies fresh retry context and the post-commit LSP/event shape.
- OpenCode fuzzy matching and Grok hashline remain optional, benchmark-gated recovery modes.

That gives v0 parallel-agent safety, compact multi-edit orchestration, truthful review, and a clean upgrade path without importing any harness's chassis.
