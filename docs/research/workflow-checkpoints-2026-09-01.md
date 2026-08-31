# Workflow persistence and checkpoint mechanisms

Date: 2026-09-01  
Ticket: [#4](https://github.com/Matari-Audio/SERAPH/issues/4)  
Scope: persistent execution state, workflow replay, task coordination, artifacts, usage, and crash recovery for a standalone Rust/Ratatui SERAPH host with a Python execution sidecar.

## Recommendation

SERAPH should not transplant any candidate's persistence subsystem whole. The reusable combination is:

1. **Rust owns one transactional state authority.** Put sessions, workflow nodes, task claims, agent topology, messages, artifact metadata, usage events, and checkpoint manifests in one SQLite database. Serialize mutations through one Rust state actor. This replaces the candidates' independent JSONL, JSON, and registry files with one commit boundary.
2. **Python owns only executable namespace state.** Run a sidecar process over framed stdio. Adapt Prime's per-variable `dill` snapshot/restore code, but write immutable generation files and return a hash plus manifest to Rust. Rust validates and publishes the snapshot reference in SQLite.
3. **Use two replay identities.** For dynamic sequential `agent()` calls, port Qwen's rolling-prefix hash. For an explicit task DAG, hash the node's semantic inputs and dependency result hashes so an unrelated branch does not invalidate. Reuse only a committed successful result whose environment and side-effect receipt still validate.
4. **Use a durable shared task graph, not a chat todo list.** Borrow Qwen/Claude's owner, status, and dependency semantics. Implement claiming as a conditional SQLite transaction with leases, not per-file locks.
5. **Persist raw usage events.** Borrow Codex's per-response identity and full token dimensions. A cache hit spends zero new tokens but retains the original result's historical cost and provenance. Do not copy Qwen's in-memory output-token-only budget.
6. **Define checkpoint honestly.** A SERAPH checkpoint can atomically publish references to host state and already-staged blobs. It cannot atomically undo arbitrary filesystem, process, or network side effects. Such calls need idempotency keys, receipts, validation, and explicit `uncertain` recovery.

No reference provides a whole-machine atomic checkpoint. Prime has the best Python namespace continuity, Qwen and Claude have the best deterministic workflow replay, Qwen and Claude have the best shared task-board behavior, and Codex has the most useful Rust-native event/topology/usage seams.

## Source baseline and licensing

| Project | Pinned evidence | License consequence |
| --- | --- | --- |
| Prime Agent | [`9f5edc1`](https://github.com/PrimeIntellect-ai/prime-agent/tree/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09), 2026-08-31 | MIT. Its Python snapshot code can be adapted with copyright and license notice retained. |
| Qwen Code | [`bd59085`](https://github.com/QwenLM/qwen-code/tree/bd5908531160e3c68556cda5ee01b3a86a2dc1f1), 2026-08-31 | Apache-2.0. Port algorithms or code with license/NOTICE obligations and modification notices. |
| Codex | [`2c3bf4e`](https://github.com/openai/codex/tree/2c3bf4ea793aa5c590932553d242a287380e9cec), 2026-08-31 | Apache-2.0. Rust code is legally reusable, but the state crates are too coupled to transplant wholesale. |
| Claude Code | Public repo [`f275fa2`](https://github.com/anthropics/claude-code/tree/f275fa282e76c5e5456912268f2c367a7f4f4797), docs and changelog current on 2026-09-01 | [All rights reserved; commercial terms](https://github.com/anthropics/claude-code/blob/f275fa282e76c5e5456912268f2c367a7f4f4797/LICENSE.md). Use official behavior as a specification only; do not copy its distributed implementation. |

This is an engineering reading of repository licenses, not legal advice.

## Recovery matrix

| State | Prime | Qwen | Codex | Claude Code | SERAPH decision |
| --- | --- | --- | --- | --- | --- |
| Conversation/event history | Session JSONL | Transcript plus workflow files | Canonical rollout JSONL | Session JSONL | SQLite event log; prompt context is a projection |
| Executable kernel | Persistent Python process; per-variable `dill` snapshot | None | Fresh V8 isolate per cell; JSON `store()` only in process | No public kernel | Python sidecar plus immutable namespace snapshot |
| Workflow progress | No deterministic workflow journal | Started/result JSONL and prefix replay | No equivalent | Saved per-agent results and prefix replay | Node journal in SQLite; prefix and DAG keys |
| Shared task graph | No first-class durable board | Durable JSON task files, dependencies, claims | `update_plan` is not a durable shared board | Durable local task list, dependencies, claims | SQLite tasks, dependencies, owners, leases |
| Child topology | Durable append-only spawn ledger plus child metadata | Team files and session state | SQLite spawn edges and lazy metadata restore | Ephemeral team config; durable session/task state | SQLite agents/edges; lazy process hydration |
| Artifacts | Session artifact directory, mostly ad hoc | Versioned artifact events/snapshots and managed-copy references | Bounded JSON thread-artifact rows | Shareable UI artifacts, not execution blobs | Content-addressed blob store plus typed metadata |
| Usage | Parent/child attribution in session log | Terminal workflow summary and in-memory output counter | Per-response, turn, and thread token records | Per-agent totals in workflow UI | Append-only provider response ledger, deduplicated |
| Rewind | Conversation branches; kernel restores latest snapshot | Replay unchanged workflow prefix | Conversation rollback/revert, not side effects | File-tool snapshots and conversation rewind | Checkpoint frontier plus explicit effect receipts |
| Atomic whole-machine checkpoint | No | No | No | No | SERAPH-specific staged blobs plus one DB publish transaction |

## Prime Agent: reusable Python state, weak global checkpoint

Prime serializes each top-level user name independently with `dill`, skips private/bootstrap names, and reports oversized or unpicklable values instead of losing the whole snapshot. Its host defaults to a 256 MiB aggregate and 16 MiB per variable and explicitly calls out open files, sockets, and GPU tensors as non-restorable ([snapshot contract](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/kernel/state-snapshot.ts#L1-L47)). This is the right failure model for SERAPH: restored names and failed names must be visible state, never an implied complete restore.

The Python implementation serializes each value into its own bytes value, stages payload and manifest temp files, replaces the payload and then the manifest, and restores all successfully decoded values after deserializing them into a staging map ([snapshot](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.py#L608-L778), [restore](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.py#L781-L818)). The host serializes executions and state operations, snapshots after successful cells, debounces ordinary snapshots, and drains the execution queue for a final snapshot on dispose ([queue](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/kernel/repl-manager.ts#L719-L805), [state API and final flush](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/kernel/repl-manager.ts#L1328-L1488)). Compaction preserves the live kernel and injects only its surviving names; a resumed session prewarms and restores before the first turn ([compaction notice](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/agent-session.ts#L7307-L7381), [runtime rebuild](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/agent-session.ts#L9078-L9177)).

The snapshot is not transactionally safe as a pair. `os.replace(payload)` can succeed and `os.replace(manifest)` can fail, leaving mismatched generations. Neither staged file nor parent directory is explicitly `fsync`ed. A Python background thread can mutate objects while they are being serialized; the code handles a deleted name but cannot make an arbitrary object graph immutable. Therefore the transplantable seam is the **per-variable serialization and staged restore**, not the fixed filenames or claimed checkpoint boundary.

Prime's child registry is stronger. A daemon-owned append-only ledger records spawn, rename, and delete operations; it uses small append writes, `fsync`, torn-tail repair, bounded replay, and last-writer-wins reconstruction ([model and assumptions](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/modes/daemon/rlm-ledger.ts#L24-L118), [durable append and replay](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/modes/daemon/rlm-ledger.ts#L713-L842)). Per-child JSON is explicitly display/hydration metadata, not topology ([child metadata](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/modes/daemon/rlm-subagent-display.ts#L5-L86)). In SERAPH, the same separation belongs in normalized SQLite tables rather than another ledger file.

Prime persists child usage separately from the parent response while also recording the aggregate, which is useful provenance ([usage record](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/session-manager.ts#L148-L159), [attribution append](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/session-manager.ts#L1484-L1507)). Its session JSONL and artifact directory are separate stores, however, and ordinary session appends are not `fsync`ed ([paths](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/session-manager.ts#L281-L305), [append path](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/session-manager.ts#L1353-L1392)).

### Prime transplant verdict

- **Adapt nearly directly:** per-variable `dill` serialization, size caps, explicit skipped/restored reports, staged restore.
- **Reimplement in Rust:** protocol framing, immutable generation naming, hashing, lifecycle, topology, and checkpoint publication.
- **Do not copy:** fixed `kernel-state.dill`/`.json` replacement scheme or independent JSONL registries.
- **Runtime cost:** Python plus `dill`; no `pyo3` is needed with a sidecar. This keeps Rust builds portable and contains interpreter crashes.

## Qwen Code: best open workflow replay and task-board reference

Qwen's journal is a compact, high-value algorithm. Every dispatch appends `started` then `result`. A key is a SHA-256 chain over the previous key, prompt, and canonical semantic options. The first missing or changed call invalidates the suffix. Arguments seed the chain, and `Date.now()`/`Math.random()` are forbidden so call order is reproducible ([journal design and key derivation](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-journal.ts#L7-L31), [canonical options and args seed](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-journal.ts#L67-L153)). Replay retains the last completed result and all prior started attempts. Journal appends are serialized but deliberately fire-and-forget; load failure silently becomes an empty cache ([replay and I/O](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-journal.ts#L155-L208)).

On resume, a cache hit consumes neither an agent slot nor new token budget. The first miss records a new `started` marker and makes the remaining suffix live; a successful JSON result is appended for later reuse ([dispatch replay](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-orchestrator.ts#L1649-L1767), [live result](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-orchestrator.ts#L1847-L1902)). This resumes the **program by re-executing it from the start**, not by snapshotting a JavaScript VM stack.

The terminal workflow snapshot is only a recent-history projection. The live registry is in-memory; the snapshot is written only for terminal runs, with plain `writeFile`, best effort, separately from the journal ([snapshot contract](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/workflow-snapshot.ts#L7-L66), [write path](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/workflow-snapshot.ts#L102-L145)). Settlement writes that snapshot before draining the journal, so the two files are not one atomic state ([settlement order](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-runner.ts#L403-L438)). A crash may lose recently completed cache entries, causing safe but expensive reruns.

Qwen's budget counts only output tokens, lives for one in-memory `run()`, and is a soft gate that can overshoot under concurrency ([budget model](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-budget.ts#L7-L35), [counter](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-budget.ts#L98-L156)). A resumed run creates a fresh counter. It is useful as an admission pattern, not an authoritative usage ledger.

The task board is directly aligned with SERAPH's desired coordination. Each task has `pending`, `in_progress`, or `completed`, an owner, and `blocks`/`blockedBy` edges ([task model](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/types.ts#L101-L129)). One JSON file per task is guarded by an in-process mutex and cross-process `proper-lockfile`; creation claims IDs with `O_EXCL` and publishes complete JSON through temp-and-rename ([storage and locks](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L7-L29), [creation](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L241-L317)). Claims re-read under lock and transition only unowned pending work; completion unblocks dependent files ([claim](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L846-L921), [unblock](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L800-L825)).

Qwen also has the most useful open artifact state model: typed retention, restore and storage states, managed-copy content references with hashes and sizes, sequenced event records, compacted snapshots, tombstones, and stale-event rejection ([artifact types](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/services/session-artifact-persistence.ts#L11-L145), [rebuild](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/services/session-artifact-persistence.ts#L266-L389)). This is a metadata/replay reference, not an atomic content store.

### Qwen transplant verdict

- **Port:** rolling-prefix key derivation, canonical option projection, `started`/`result` state machine, deterministic-script restrictions, and task model semantics.
- **Improve:** journal writes must participate in the Rust state transaction; a persistence error must be visible, not silently converted into a cache miss.
- **Do not transplant wholesale:** the JavaScript orchestrator, ~1,000-line filesystem task module, `proper-lockfile`, or terminal JSON snapshot layer. They solve Node/process constraints that the Rust state actor and SQLite already remove.
- **Runtime cost:** the algorithms need only stable JSON, SHA-256, and SQLite. A JavaScript VM is not required for persistence and should be chosen separately as an orchestration-language decision.

## Codex: best Rust-native event, topology, and usage seams

Codex's rollout recorder is a canonical JSONL event stream with explicit create/resume modes, queued add/persist/flush/shutdown commands, deferred file creation for new sessions, and retryable pending items after I/O failure ([recorder API](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/rollout/src/recorder.rs#L77-L137), [open and writer lifecycle](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/rollout/src/recorder.rs#L829-L988)). It persists model/tool items, compaction markers, turn context, inter-agent communications, world state, and token usage records ([persistence policy](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/rollout/src/policy.rs#L9-L24)). This is excellent audit history, but replay reconstructs conversational state rather than deterministic workflow nodes or executable computation.

Its multi-agent graph uses durable SQLite `thread_spawn_edges` keyed by child thread ([migration](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/state/migrations/0021_thread_spawn_edges.sql#L1-L8)). On root resume, Codex loads open descendants and restores their metadata without reopening every runtime ([lazy restore](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/agent/control/spawn.rs#L155-L225)). That is the correct SERAPH topology model: durable identity, lazy process residency.

Codex also makes message wake semantics explicit. `send_message` queues communication while `followup_task` wakes an idle target; both hydrate the target if needed. `interrupt_agent` is separate and reports the previous status ([message modes](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/message_tool.rs#L1-L142), [interrupt](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/interrupt_agent.rs#L32-L103)). Inter-agent communication is a durable rollout item, while UI collaboration begin/end events are transient. `PlanUpdate` is also transient; a completed plan item may persist in paginated history, but this is not a shared claimable board ([event policy](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/rollout/src/policy.rs#L89-L201)).

The usage record is the best candidate shape: provider response identity plus raw usage and cumulative turn/thread usage, including input, cache read, cache write, output, and reasoning tokens ([types](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/protocol/src/protocol.rs#L2206-L2239), [accumulation](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/state/session.rs#L156-L191)). Its shared rollout budget is only in memory and starts at zero, so the record is reusable but the recovery semantics are not ([budget](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/rollout_budget.rs#L16-L64)).

Codex's code-mode runtime is not a persistent kernel. Each cell creates a fresh V8 isolate and receives a clone of JSON stored values; successful completion commits new values to an in-memory session map, while cancellation rejects the commit ([isolate and state](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-runtime/src/runtime/mod.rs#L168-L225), [session store](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-runtime/src/session_runtime/mod.rs#L39-L73), [commit](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-runtime/src/session_runtime/mod.rs#L273-L291)). No source seam persists those values across CLI restart.

The new thread-artifact table stores bounded JSON metadata with a stable identity key and deterministic paging, not arbitrary blobs ([model](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/state/src/model/thread_artifact.rs#L4-L45), [schema](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/state/migrations/0051_thread_artifacts.sql#L1-L12)). At the pinned revision it has no non-test runtime attachment API outside the state model, so it is an emerging seam rather than a ready artifact subsystem.

### Codex transplant verdict

- **Borrow directly in Rust:** event vocabulary, durable spawn-edge model, lazy hydration, queue-versus-wake messaging, interrupt separation, and token usage dimensions.
- **Do not import the full crates:** `codex-state`, rollout, protocol, and core are workspace-coupled and would bring far more than SERAPH needs.
- **Do not mistake:** rollout replay for workflow replay, conversation rollback for side-effect rollback, `update_plan` for a shared task board, or code-mode `store()` for kernel persistence.
- **Runtime cost:** a small local schema is cheaper than Codex's SQLx state stack. For v0, a single-writer actor around a lightweight SQLite driver is sufficient; async database pooling is unnecessary for one local process.

## Claude Code: strongest current behavioral specification, no reusable core source

Claude's current dynamic workflow documentation independently confirms Qwen's design. Intermediate results live in script variables, not conversation context; `Date.now()`, `Math.random()`, and no-argument `new Date()` throw; per-agent results make a run resumable; completed unchanged agents return saved results; the first changed, failed, or incomplete agent reruns together with the suffix ([workflow model and restrictions](https://code.claude.com/docs/en/workflows#edit-a-saved-script), [resume rules](https://code.claude.com/docs/en/workflows#resume-after-a-pause)). Results survive under the session directory and can be replayed after `claude --resume`; a fresh session has no prior results. The workflow program itself has no filesystem or shell access, but its agents do, so cached results do not prove external side effects still hold ([runtime limits](https://code.claude.com/docs/en/workflows#behavior-and-limits)).

Claude teams provide the exact user-facing coordination behavior SERAPH wants: independent contexts, automatic messages, final/idle notifications, and a shared task list with pending/in-progress/completed states, dependencies, explicit assignment, and self-claim. Claims use file locking. The task directory persists for resumed sessions, while runtime team config is deleted at session end ([assignment and claims](https://code.claude.com/docs/en/agent-teams#assign-and-claim-tasks), [architecture and retention](https://code.claude.com/docs/en/agent-teams#architecture), [communication](https://code.claude.com/docs/en/agent-teams#context-and-communication)). This is a useful product contract; Qwen is the open implementation reference.

Cross-session messaging is a separate local rendezvous layer. Live sessions register endpoints on disk and communicate over per-session Unix sockets or Windows named pipes; the receiving agent reads a message between tool calls, or starts a new turn when idle. It transports text, not the sender's history or files, and does not transfer user authority ([delivery](https://code.claude.com/docs/en/cross-session-messaging#message-delivery), [transport](https://code.claude.com/docs/en/cross-session-messaging#message-sessions-on-other-machines), [authority](https://code.claude.com/docs/en/cross-session-messaging#how-a-session-treats-an-incoming-message)). This informs SERAPH's messaging API, but endpoint discovery is not durable workflow state.

Claude checkpoints are intentionally narrower than whole-machine snapshots. They retain the 100 most recent pre-prompt file snapshots and conversation points, but only edits made through direct file tools are restorable. Bash changes, most subagent edits, external concurrent changes, symlinks, and hard links are not rolled back ([checkpoint behavior and limits](https://code.claude.com/docs/en/checkpointing)). Agent SDK file checkpointing likewise restores file-tool changes but not conversation state ([SDK contract](https://code.claude.com/docs/en/agent-sdk/file-checkpointing#how-checkpointing-works)).

Claude's external session store API is also instructive about failure semantics: the CLI writes local JSONL first, mirrors batches best-effort, retries, may redeliver a batch, and requires deduplication by entry UUID. File checkpoints cannot be mirrored through that API ([dual write and failures](https://code.claude.com/docs/en/agent-sdk/session-storage#dual-write-architecture), [mirror semantics](https://code.claude.com/docs/en/agent-sdk/session-storage#mirror-writes-are-best-effort)). SERAPH should avoid this dual-authority design locally.

### Claude transplant verdict

- **Specify behavior from:** workflow replay, task/team UX, cross-session delivery, and explicit rewind limitations.
- **Copy no implementation:** the public core is proprietary and unavailable for source-level verification.
- **Do not promise:** that workflow replay restores side effects, that file rewind covers shell/subagent edits, or that cross-session messages carry context or consent.

## SERAPH persistence model

### One authoritative database

Use a single SQLite database in WAL mode, written through one Rust actor. The minimum logical tables are:

| Table | Purpose |
| --- | --- |
| `sessions`, `events` | Session identity and append-only audit events with monotonic sequence |
| `agents`, `agent_edges`, `messages` | Durable topology, lifecycle, queue/wake delivery, acknowledgements |
| `workflow_runs`, `workflow_nodes`, `node_attempts` | Script identity, node keys, status, result reference, attempt/error history |
| `tasks`, `task_dependencies` | Shared work board, owner/lease, dependency graph, terminal result |
| `artifacts`, `artifact_refs` | Typed metadata and references to immutable content blobs |
| `usage_events` | Deduplicated provider response usage and cost dimensions |
| `kernel_snapshots` | Python version, serializer version, blob hash, restored/skipped manifest |
| `checkpoints` | Atomic frontier over the rows and immutable blobs above |

The model-facing API can still expose only `pending`, `in_progress`, and `completed`. Internally tasks and nodes need `pending`, `leased`, `running`, `completed`, `failed`, `cancelled`, and `uncertain`. A lease deadline and attempt ID prevent a crashed agent from owning `in_progress` forever.

Task claim should be a single transaction: claim only a pending, unowned, unblocked row; set owner, lease, attempt, and start time; return the row. Completion should atomically attach the result, mark the task complete, release the lease, and expose newly unblocked dependents. This yields Qwen/Claude behavior without `proper-lockfile`, per-task files, or polling every file.

### Immutable artifact store

Keep large bytes outside SQLite under a content-addressed path such as `blobs/sha256/<digest>`. To publish a blob:

1. stream to a same-filesystem temporary file while hashing;
2. flush and `fsync` the file;
3. rename/link it to its digest path without clobbering an existing digest;
4. `fsync` the parent directory where supported;
5. insert the artifact metadata and checkpoint reference in one SQLite transaction.

A crash before step 5 leaves an unreferenced blob that garbage collection can remove. A committed database reference never points to a partially written blob. Metadata should retain Qwen's useful concepts: kind, storage, retention, status, content hash, size, provenance, and tombstone. The model receives a typed handle and small projection, never the raw large payload by default.

### Python sidecar checkpoint protocol

Prefer a sidecar over embedded CPython for v0. The wire contract needs only:

```text
execute(request_id, code)
interrupt(request_id)
list_names(request_id)
snapshot(request_id, generation, max_total, max_variable)
restore(request_id, blob_path, expected_sha256)
shutdown(request_id)
```

`snapshot` must run behind the sidecar's execution queue. It writes an immutable bundle, returns its hash, Python/dill versions, saved names, skipped names/reasons, and byte count. Rust stages that bundle before publishing the `kernel_snapshots` and `checkpoints` rows. `restore` validates the hash, decodes each name into a staging map, applies all successful names, and reports failures. Host capability clients, child handles, open descriptors, locks, sockets, subprocesses, and other live resources are recreated from durable Rust IDs, never pickled.

The checkpoint guarantee should say: “serializable top-level Python names captured after prior submitted cells completed.” It must not claim a memory-consistent snapshot of arbitrary Python background threads. If strict determinism is needed, prohibit background threads in checkpointable workflows or require user-defined snapshot adapters.

### Workflow node identity and invalidation

For dynamic sequential programs, start with Qwen's safe rule:

```text
seed = H(engine_version, workflow_source_digest, canonical_args)
key[n] = H(key[n-1], canonical_prompt[n], semantic_options[n])
```

Semantic options must include model/provider identity, effort, agent type/instructions, output schema, selected capability versions, isolation/working directory, and a workspace input fingerprint. Labels, UI phase names, and scheduler timeouts should not invalidate a pure result unless they change behavior.

For explicit DAG nodes, use:

```text
node_key = H(
  engine_version,
  workflow_source_digest,
  stable_node_id,
  canonical_inputs,
  semantic_options,
  dependency_result_hashes,
  workspace_input_fingerprint
)
```

This preserves completed independent branches when one branch changes. Reuse requires all of:

- a committed `completed` node with a valid result artifact;
- exact node-key match;
- successful validation of any environment or side-effect receipt;
- no explicit `non_cacheable` or `force_rerun` policy;
- compatible serializer/schema versions.

`running`, `leased`, `failed`, cancelled, missing-result, and crash-ambiguous attempts rerun. A result journal entry is committed in the same database transaction as node completion and usage attribution. Replay never increments new usage; UI shows “0 new / N historical tokens.”

### Usage ledger

Persist one row per provider response, unique on provider plus response ID (or a host-generated request id when the provider has none):

- session, workflow, node, task, agent, turn, root-turn, and attempt IDs;
- input, cache-read input, cache-write input, output, and reasoning tokens;
- provider/model, price schedule ID, amount/currency when known;
- observed, estimated, or unknown provenance.

Compute session/tree/workflow totals from these raw rows or transactionally maintained projections. Budget admission reads durable totals plus reservations for in-flight requests. Parallel dispatches reserve estimated capacity before starting and reconcile on completion, preventing the large soft-gate overshoot present in Qwen. If a crash occurs after the provider charges but before usage is recorded, the attempt is `uncertain`; do not fabricate zero usage.

## Checkpoint transaction and recovery

A checkpoint is a durable **frontier**, not a copy of the whole machine. At an explicit safe point:

1. stop admitting new workflow nodes and task claims for the target session;
2. let completed host mutations commit; mark still-external in-flight attempts as `uncertain` rather than pretending they stopped;
3. request and stage a Python namespace snapshot if a kernel exists;
4. stage any new artifact blobs and workspace delta/commit reference;
5. in one SQLite transaction, record the kernel blob, workflow/node frontier, task/lease state, agent topology, delivered-message sequence, artifact references, usage sequence, workspace reference, and checkpoint row;
6. resume admission.

On restart, SQLite supplies the last committed checkpoint and all later committed events. Rust restores topology metadata without eagerly spawning every child, restores the Python snapshot on first kernel use, expires stale task leases, reconciles in-flight attempts, and replays only valid nodes. Unreferenced staged blobs are garbage-collected.

The checkpoint should contain schema/version hashes for every subsystem. A restore with an unsupported workflow engine, Python/dill, node-result schema, or capability version must fail closed for that component and rerun/rebuild it while preserving the rest.

## External side effects and replay

No candidate solves distributed transactionality. SERAPH should classify capability calls:

| Class | Examples | Replay rule |
| --- | --- | --- |
| Pure/read | parse artifact, search immutable snapshot | Safe to rerun; cache by inputs |
| Host-transactional | task claim, artifact attach, exact patch commit | Commit with idempotency key in SQLite |
| Verifiable | write file, create worktree, run deterministic build | Record receipt and current-state validator; reuse only if validation passes |
| Idempotent external | API create/update with provider idempotency key | Persist request key and receipt before considering complete |
| Non-idempotent/irreversible | email, publish, payment, arbitrary shell/network | Never auto-replay after ambiguity; surface `uncertain` for user or compensating action |

Filesystem rewind should be based on an exact Git tree/commit, patch transaction, or content-addressed per-path snapshot owned by SERAPH. It must not imply that shell commands, other processes, or concurrent sessions were reversed. Agent result reuse and side-effect reuse are distinct decisions.

## Dependency and code-reuse budget

| Candidate seam | Reuse form | Cost | Decision |
| --- | --- | --- | --- |
| Prime `repl.py` per-variable snapshot | Adapt MIT Python functions | Python + `dill`; small protocol surface | **Use**, with immutable blobs and Rust publication |
| Qwen workflow journal | Port ~200 lines of Apache-2.0 algorithm to Rust | SHA-256 + stable JSON already broadly useful | **Use** |
| Qwen task implementation | Copy Node filesystem module | `proper-lockfile`, `async-mutex`, Node-specific failure modes | **Do not copy**; reproduce semantics in SQLite |
| Qwen artifact accumulator | Port types/replay ideas | Moderate schema work | **Use concepts**, simplify for v0 |
| Codex state/rollout crates | Import workspace crates | SQLx/Tokio/protocol/core coupling | **Do not import** |
| Codex schemas and small Rust patterns | Reimplement locally under Apache-2.0 notice where copied | Low | **Use selectively** |
| Claude core | Copy implementation | Proprietary and not publicly inspectable | **Never copy** |
| Embedded Python via `pyo3` | Link CPython into Rust | Build/link/platform complexity, interpreter lifecycle in host | **Defer**; sidecar first |
| JavaScript VM solely for replay | Embed V8/QuickJS | Large dependency and attack/maintenance surface | **Not required by this subsystem** |

The simplest v0 state stack is Rust, a lightweight SQLite driver, Serde JSON for opaque payloads, SHA-256 for identities, and a Python sidecar with `dill`. Ratatui reads projections from the Rust state actor; it never parses sidecar files.

## V0 acceptance boundary

The persistence slice is ready when it can demonstrate these behaviors without claiming more:

- a serializable Python namespace survives process restart, with skipped values reported;
- 39 completed unchanged workflow nodes resume without new model calls while three unfinished nodes run;
- changing args, prompt, semantic options, dependency output, or workspace fingerprint invalidates exactly the required nodes;
- a task claim survives restart, stale leases recover, dependencies unblock once, and two agents cannot claim the same task;
- child identities/topology survive without eagerly starting child runtimes;
- every provider response is counted once across parent, child, workflow, and replay views;
- a committed checkpoint never references a partial blob;
- a crash at each pre-publication stage recovers to either the previous checkpoint or the new one, never a mixed manifest;
- ambiguous external effects surface as `uncertain` and are not silently repeated;
- restoring a checkpoint reports which conversation, kernel, workflow, tasks, artifacts, usage, workspace state, and external effects were restored, replayed, validated, skipped, or left uncertain.

## Bottom line

Build the checkpoint system around **Rust transactional metadata plus immutable blobs**, not around Python pickles or JSONL files. Use Prime's Python namespace serializer inside a narrow sidecar, Qwen's deterministic result-keying and task semantics, and Codex's durable Rust event/topology/usage shapes. Treat Claude Code as the current UX and behavioral specification. This yields resumable work and token-free result reuse without pretending that arbitrary external side effects can be rewound.
