# Multi-agent coordination mechanisms for SERAPH

**Research date:** 2026-09-01
**Scope:** Codex, Claude Code, Grok Build, Prime Agent, and Qwen Code
**Target:** a standalone Rust/Ratatui SERAPH host, not a fork of any donor

## Decision

SERAPH should combine four mechanisms, not adopt one harness's agent model:

1. **A durable shared task graph derived from Qwen Code.** It is the best source-available implementation of task creation, dependency edges, atomic claiming, ownership, completion unblocking, crash requeue, and change-driven auto-claim.
2. **A small agent-control protocol derived from Codex MultiAgent V2.** Hierarchical stable names, bounded context forking, `send_message`, `followup_task`, `interrupt_agent`, `list_agents`, and event-driven `wait_agent` are clean primitives and already implemented in Rust.
3. **A session-wide messaging plane derived from Claude Code's behavior.** Independent chats need peer discovery, unambiguous addressing, delivery receipts, safe-point delivery, wake-on-idle, and a token-free one-shot idle subscription. Claude's implementation is proprietary, so this is a behavior specification, not reusable code.
4. **A coordinator-owned child lifecycle and usage ledger derived from Grok Build.** Its Rust code distinguishes admitted, queued, running, finalizing, completed, failed, and cancelled work; treats uncertain message admission honestly; scopes cancellation to a child, parent prompt, parent session, or workflow; and separately accounts for cached reads, cache creation, reasoning, cost, and incomplete child usage.

The shared task graph and message plane must live outside model context. Agents receive compact task/message deltas and query the full board only when needed. This is more token-efficient than repeatedly injecting a shared checklist, polling every child, or forwarding transcripts between agents.

## Source baseline and reuse boundary

| System | Inspected revision | License at that revision | Reuse consequence |
|---|---|---|---|
| Codex | [`2c3bf4e`](https://github.com/openai/codex/tree/2c3bf4ea793aa5c590932553d242a287380e9cec) (2026-08-31) | [Apache-2.0](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/LICENSE) | Rust algorithms and types can be adapted with attribution, but most modules depend on Codex's session machinery. Port the seams, not whole crates. |
| Claude Code | [`f275fa2`](https://github.com/anthropics/claude-code/tree/f275fa282e76c5e5456912268f2c367a7f4f4797) (2026-08-31) | [Anthropic commercial terms; not open source](https://github.com/anthropics/claude-code/blob/f275fa282e76c5e5456912268f2c367a7f4f4797/LICENSE.md) | Copy documented behavior and public schemas only. Do not copy distributed implementation code. |
| Grok Build | [`bb7f39d`](https://github.com/xai-org/grok-build/tree/bb7f39d5858cbf5e00de639367f59debbdcb0138) (2026-08-31) | [Apache-2.0](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/LICENSE) | Rust code is reusable with attribution. The useful subagent modules are tightly coupled to internal crates, so extract state-machine ideas first. |
| Prime Agent | [`9f5edc1`](https://github.com/PrimeIntellect-ai/prime-agent/tree/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09) (2026-09-01 checkout) | [MIT](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/LICENSE) | The Python runtime and TypeScript host patterns are permissively reusable, but SERAPH should keep coordination authoritative in Rust. |
| Qwen Code | [`bd59085`](https://github.com/QwenLM/qwen-code/tree/bd5908531160e3c68556cda5ee01b3a86a2dc1f1) (2026-08-31) | [Apache-2.0](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/LICENSE) | The TypeScript task state machine can be ported to Rust. Its file-locking layer should become coordinator-owned SQLite transactions rather than a literal port. |

The official Qwen repository contains no translated Chinese page for the multi-agent coordination feature; the English first-party page is the sole product specification. Chinese design notes elsewhere in the repository do not add multi-agent behavior beyond the source examined here.

## Capability matrix

| Mechanism | Codex | Claude Code | Grok Build | Prime Agent | Qwen Code |
|---|---|---|---|---|---|
| Shared tasks with owners | No shared board | Yes, when Task tools are available | Per-session plan, not a team board | Goal state, not a team board | **Yes** |
| Dependencies and automatic unblock | No | Yes | No shared graph | No | **Yes, with cycle checks** |
| Atomic self-claim | No | File-locked claim | No shared claim | No | **Yes, cross-process locked** |
| Crash requeue | N/A | Not documented as a board invariant | Child lifecycle survives as a job result | Retained children; no shared claims | **Yes** |
| Peer discovery | Hierarchical live tree | Local, remote, subagent, teammate roster | Parent-owned children only | Parent/sibling/child family roster | Team roster plus local session peers |
| Peer-to-peer team messages | Any known V2 agent path | Yes | Parent to owned active child | Parent/sibling/child only | Yes, plus broadcast |
| Cross-session messages | No general independent-session surface found | **Yes, local and remote** | No | Yes through daemon family/root peers | Yes, same machine |
| Safe-point delivery | Queued input | Between tool calls | Queue; internal steer mode exists | Steering queue | Between tool rounds |
| Wake idle agent | `followup_task` | Idle message starts a turn | Completion auto-wake; child follow-up is queue-only publicly | Idle message delivery; schedules/heartbeats | Idle teammate flush; peer input enqueues work |
| Idle subscription without polling | `wait_agent` waits current session activity | **`notify_when_idle`** | Coordinator wait/query | Heartbeats/schedules, no peer idle subscription | Activity waiters inside team runtime |
| Explicit interrupt/cancel | **`interrupt_agent`** | UI/task stop; cooperative teammate shutdown | **Child/prompt/session/workflow scopes** | Delete/cancel child, stop session | Abort teammate/task stop |
| Durable topology/recovery | **Persisted spawn edges and lazy reload** | Tasks persist; in-process teammates do not resume | Child attempt store and resume | **Daemon sessions, kernel and child registry** | Task files persist; team runtime is in-process |
| Worktree isolation | Environment/workspace support, not a shared merge protocol | Subagent worktrees; manual parallel sessions | **Full child worktree lifecycle** | Advises clean worktree; no built-in team merge protocol | Read-only workers plus at most one coordinated writer worktree |
| Explicit child token ledger | Usage exists, less coordination-specific | Usage per agent; cache TTL controls | **Most complete accounting** | Child usage attributed separately | Runtime usage plus fork cache reuse |

## Findings by system

### Qwen Code: transplant the task graph

Qwen's experimental Agent Team is the closest implementation of the user's desired “agents know what is done and do not duplicate it” behavior. The product flow exposes separate teammates, a shared list, direct messages, and Agent View. `/coordinate` uses up to three independent workstreams, enforces read-only investigation workers, optionally places one writer in a leader-owned Git worktree, and keeps merge authority with the leader ([first-party feature page](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/docs/users/features/multi-agent-coordination.md)).

The task storage is unusually rigorous:

- Each task is a JSON record with `pending | in_progress | completed`, `owner`, `blocks`, `blockedBy`, and metadata. Same-process mutexes and `proper-lockfile` protect cross-process writers ([task storage and locks](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L8-L25)).
- Creation atomically reserves a numeric ID with `O_CREAT | O_EXCL`, then replaces the empty placeholder with a complete JSON file so readers never observe partial JSON ([atomic creation](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L241-L311)).
- Claiming locks the task, accepts only an unowned pending task, writes owner and `in_progress`, and optionally serializes claims per agent so one idle agent cannot accidentally own two jobs ([claim protocol](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L846-L921)).
- Completing a task removes its ID from dependents' `blockedBy` arrays. Dependency updates are mirrored in both directions and `task_update` rejects cycles ([completion unblocking](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L780-L825), [cycle detection seam](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/tools/task-update.ts#L346-L365)).
- When an agent terminates, owned in-progress work is atomically returned to pending without overwriting a completion or reassignment that won the race ([release/requeue](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/tasks.ts#L940-L1020)).
- Task updates wake an auto-claim scan. Idle agents concurrently attempt the first unowned, unblocked task; the locked claim decides the winner ([auto-claim](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/TeamManager.ts#L2116-L2163), [change-driven scan](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/TeamManager.ts#L2258-L2291)).
- Idle completion automatically reports a final answer to the leader unless the teammate already sent an explicit report. A terminal teammate's tasks are requeued and its pending inbox is dropped rather than silently accepted ([lifecycle bridge](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/TeamManager.ts#L1764-L1859)).

Team messages are priority queued and flushed immediately to an idle teammate; broadcast uses `allSettled` and reports partial failure ([message routing](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/team/TeamManager.ts#L743-L900)). Qwen also has same-machine peer discovery and delivery over short-lived Unix sockets. Names are not assumed unique: peers carry a stable short ref, reachability is probed, and ambiguous names are refused ([peer directory](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/ipc/peer-directory.ts#L18-L30), [peer send](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/ipc/peer-send.ts#L217-L340)). The receiver has `accept | hold | refuse` ingress policy and bounded held-message state ([inbound gate](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/ipc/inbound-gate.ts#L8-L40)).

**Do not port its persistence literally.** JSON-per-task plus two layers of file locks is appropriate for cooperating Node processes without a central owner. SERAPH already needs a Rust coordinator. A single writer actor backed by SQLite/WAL gives atomic task-and-edge transitions, durable receipts, indexed queries, and simpler recovery.

### Codex: transplant the control protocol

Codex MultiAgent V2 has no shared task board. `task_name` is a hierarchical agent identity, while `update_plan` remains session-local and is now opt-in; there is no owner/dependency/claim protocol in the multi-agent tool surface. What Codex does have is the cleanest minimal control API:

- `spawn_agent` creates a canonical hierarchical `AgentPath`, recursively supports children, and starts the child asynchronously ([spawn handler](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs#L93-L179)).
- `fork_turns` accepts `none`, `all`, or a positive number. This is a direct token-control mechanism: independent workers need no transcript; context-dependent workers can receive only the last N real turns ([fork argument](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs#L280-L322), [rollout truncation](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/thread_rollout_truncation.rs#L261-L310)).
- `send_message` and `followup_task` share one route. Queue-only delivery does not wake an idle runtime; trigger-turn delivery loads a cold child if needed and starts a new turn. Keeping “add information to current work” separate from “assign more work and wake it” avoids ambiguous message semantics ([shared delivery modes](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/message_tool.rs#L1-L24), [load and dispatch](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/message_tool.rs#L52-L129)).
- `interrupt_agent` cancels another spawned agent, refuses root/self targets, and returns the previous status ([interrupt contract](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/interrupt_agent.rs#L32-L103)).
- `list_agents` exposes the live hierarchy with optional path-prefix filtering ([discovery](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/list_agents.rs#L31-L55)).
- `wait_agent` subscribes to mailbox/steer activity and sleeps on a watch channel rather than spending model turns polling child status ([event-driven wait](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs#L39-L116), [activity outcomes](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs#L180-L204)).
- Child status changes are converted into compact `<subagent_notification>` context fragments rather than requiring the parent to read a transcript ([notification projection](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/context/subagent_notification.rs)).
- Spawn edges and V2 identities are persisted; a later message can lazily reload a cold agent. The resume code validates parent ownership and restores descendants without eagerly reopening every runtime ([cold metadata restore](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/agent/control/spawn.rs#L156-L208), [lazy child load](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/agent/control/spawn.rs#L297-L374)).

For SERAPH, keep these APIs but make the task graph separate from agent identity. A task can move between agents after failure; an agent path should remain stable across many tasks.

### Claude Code: copy behavior, not code

Claude currently unifies subagent, teammate, and independent-session messaging behind `ListAgents` and `SendMessage`. Independent sessions discover each other locally or through Remote Control. On the same machine each session registers an inbox socket (Unix domain socket on macOS/Linux, named pipe on Windows), and the listing includes a short ref when names collide ([official cross-session architecture](https://code.claude.com/docs/en/cross-session-messaging#the-sessions-inbox-socket)). A receiving active session reads a message **between tool calls**, never in the middle of a running tool; an idle receiver starts a new turn ([delivery semantics](https://code.claude.com/docs/en/cross-session-messaging#message-delivery)).

The most valuable unique mechanism is `notify_when_idle`: a main session can subscribe once to a local session's next idle/exit event. Subscribing alone starts no watched-session turn and spends no watched-session tokens; if the target is already idle, the notice is immediate. The subscription is one-shot and expires after 12 hours ([official idle notification](https://code.claude.com/docs/en/cross-session-messaging#get-a-notice-when-another-session-goes-idle)). SERAPH should generalize this to `watch(agent_or_task, event, once=true)`.

Agent teams add a shared task list with `pending`, `in progress`, and `completed` states, dependency blocking, leader assignment, teammate self-claim, and file locking ([official task behavior](https://code.claude.com/docs/en/agent-teams#assign-and-claim-tasks)). Mailboxes are per-agent JSON files; malformed entries are removed without blocking valid entries, and a send succeeds only after the mailbox write succeeds ([official team architecture](https://code.claude.com/docs/en/agent-teams#architecture)). Task files persist for session resume, but in-process teammate runtimes do **not** survive `/resume` or `/rewind` ([official limitations](https://code.claude.com/docs/en/agent-teams#limitations)). That split is a warning for SERAPH: durable tasks without durable agent topology leave stale owners, so recovery must atomically reconcile both.

There is an important 2026 qualification. Since Claude Code v2.1.233, `TaskCreate`, `TaskGet`, `TaskUpdate`, and `TaskList` are omitted by default on Opus 4.8, Sonnet 5, Fable 5, Mythos 5, and later family versions because their schemas and reminders consume context. Users can opt back in, and teammates without the tools coordinate only through messages ([official task-tool availability](https://code.claude.com/docs/en/tools-reference#task-tool-availability); [current changelog](https://github.com/anthropics/claude-code/blob/f275fa282e76c5e5456912268f2c367a7f4f4797/CHANGELOG.md#L555-L565)). SERAPH should not react by removing durable tasks. It should keep task operations outside the steady prompt, disclose their tiny schema only when coordination is active, and project task deltas without repeating the full board.

Claude's current subagents can be named and resumed through `SendMessage`; a completed subagent auto-resumes in the background under the same ID. Fork subagents inherit the main conversation and prompt cache, whereas fresh subagents use a separate cache ([official resume behavior](https://code.claude.com/docs/en/sub-agents#resume-subagents), [fork comparison](https://code.claude.com/docs/en/sub-agents#fork-the-conversation)). This supports a two-mode SERAPH spawn contract: `fresh(prompt)` for independent work and `fork(turns=N|all)` only when shared context or cache economics justify it.

Token costs remain the weakness. Agent-team cost scales approximately linearly with active teammates because every teammate has an independent context. In-process teammate requests use a five-minute cache bucket by default; `subagentPromptCacheTtl=1h` trades higher cache-write price for longer reuse ([official token guidance](https://code.claude.com/docs/en/agent-teams#token-usage)). The implementation is proprietary, so none of its mailbox or session code should enter SERAPH.

### Grok Build: reuse Rust lifecycle and accounting ideas

Grok's `task` is a child-job API, not a shared team task list. A coordinator actor owns all lifecycle transitions and reply channels; callers interact through an abstract `SubagentBackend` ([task module boundary](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build/task/mod.rs#L1-L23)). Each request records the parent session and parent prompt, supports transcript/tool-state resume, foreground or background completion surfacing, optional parent-context fork, model/tool/runtime overrides, worktree isolation, and a cancellation token ([request contract](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build/task/types.rs#L64-L119)).

Its messaging surface is deliberately narrower than Codex or Qwen: the root parent may send a follow-up only to an active child it owns. The public tool builds a `Queue` operation; `Steer` exists in the internal protocol but is not exposed by that model tool ([public send tool](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build/send_subagent_message.rs#L134-L205), [queue/steer protocol](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build/task/active_message.rs#L9-L75)). Admission is bounded to 32 KiB and distinguishes definite rejection from uncertain admission instead of encouraging unsafe retries ([message outcomes](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build/send_subagent_message.rs#L19-L68)).

Cancellation is unusually well scoped: coordinator requests can target one child, every child from a parent prompt, a parent session, or a workflow run ([cancel targets](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-tools/src/implementations/grok_build/task/types.rs#L572-L605)). This is better than a single “stop agent” boolean and should be copied into SERAPH's cancellation tree.

Grok also has the best accounting model. `UsageTotals` separately records input, output, cached-read, cache-creation, reasoning, model calls, API duration, and cost. Child usage folds into the session without inflating the main loop's turn count, and an `incomplete` bit survives uncertain cancellation/drain paths ([usage ledger](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-chat-state/src/usage.rs#L1-L43), [child fold](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-chat-state/src/usage.rs#L106-L152)). SERAPH needs this exact honesty: an interrupted child whose final provider usage is unknown must not silently become zero-cost.

For writing agents, Grok creates worktrees preserving the source working tree, registers them as subagent worktrees, snapshots results to a ref, and has explicit reclaim decisions ([creation seam](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-shell/src/agent/subagent/handle_request.rs#L499-L532), [snapshot/reclaim seam](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-shell/src/agent/subagent/handle_request.rs#L2016-L2088)). This is more useful to SERAPH than Grok's parent-only messaging.

### Prime Agent: keep the programmatic and durable-agent lessons

Prime's current RLM call returns at **admission**, not completion. A child has an independent `AgentSession` and sends requested results explicitly through `agent_message` or files. The parent-scoped child registry survives compaction, kernel restart, and parent restoration; completed daemon-backed children remain addressable ([RLM programming model](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/docs/rlm.md#L50-L106)). Daemon workers own the root session, kernel, schedules, queue, and descendants after the TUI detaches, and recover JSONL transcripts plus session artifacts after restart ([long-running architecture](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/docs/long-running-agents.md#L1-L68)).

Agent discovery is intentionally family-scoped: parent, siblings, and direct children are visible; deeper relatives require relay through the intermediate agent ([family roster](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/agent-messages.ts#L217-L250), [reach policy](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/agent-messages.ts#L303-L329)). Messages are bounded, queue-capped, rate-limited, and have delivered/queued receipts ([message limits](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/agent-messages.ts#L331-L365), [receipts and limiter](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/agent-messages.ts#L450-L507)).

Prime does not provide a team task graph. Its persistent goal is one session objective with continuation and budget state, not claimable shared work. Its unique value is that the model can program agent creation and fan-in from a persistent Python namespace while credentials, provider calls, lifecycle, and policy remain in the host. Python cells are serialized, but independent child sessions run concurrently ([runtime architecture](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/docs/rlm-runtime.md#L80-L109)).

Prime's source does not state that Python wins a token benchmark. It chooses Python because it supplies a familiar stateful REPL, a large data/text ecosystem, top-level async calls, importable skills, and namespace serialization through `dill`. JavaScript in Claude/Qwen workflows serves a different purpose: promises and a constrained VM make orchestration programs easy to run and journal deterministically. **Coordination should therefore be language-neutral.** Keep the authoritative registry, tasks, messages, and accounting in Rust; expose the same typed capability to a Python living kernel and a deterministic workflow VM later. Syntax choice is a second-order token effect compared with context inheritance, schemas, polling, and transcript forwarding.

## SERAPH v0 coordination contract

### Rust host model

Use one coordinator actor as the authority, with SQLite/WAL persistence and an append-only event table. Every mutation goes through the actor; SQLite is recovery state, not a second competing writer.

```rust
struct AgentId(Uuid);
struct TaskId(Uuid);
struct MessageId(Uuid);

enum AgentStatus { Starting, Running, Idle, Completed, Failed, Interrupted, Stopped }
enum TaskStatus { Pending, Claimed, Running, Completed, Failed, Cancelled }
enum Delivery { NextSafePoint, WhenIdle, WakeIfIdle }
enum CancelScope { Agent(AgentId), SpawnTurn(TurnId), Session(SessionId), Workflow(RunId) }
```

Persist at least:

- `agents`: stable ID, hierarchical path, parent, session, model, status, current turn, workspace, heartbeat, last event;
- `tasks`: subject, structured description/artifact handle, state, owner, claim generation, result artifact, timestamps;
- `task_edges`: `blocker -> dependent`, with a cycle check before commit;
- `messages`: sender, recipient, delivery mode, state, receipt, bounded body/artifact handle;
- `subscriptions`: watcher, target, event kind, one-shot/expiry;
- `workspaces`: checkout/worktree path, writer lease, base revision, result ref/patch;
- `usage`: call owner, model, input/output/reasoning/cache-read/cache-write/cost, complete flag;
- `events`: monotonically ordered lifecycle facts used by Ratatui and recovery.

### Model-facing APIs

Keep the default surface small:

```text
agents.spawn(task, name, context = none|all|last_n, model?, workspace?)
agents.list(prefix?)
agents.send(target, message, delivery = next_safe_point)
agents.follow_up(target, task)          # queues and wakes an idle target
agents.interrupt(target)
agents.wait(event_filter, timeout)      # host wait, no inference polling

tasks.create(subject, description, blocked_by?, owner?)
tasks.list(status?, owner?, ready_only?)
tasks.claim(id)
tasks.complete(id, result_artifact?)
tasks.fail(id, reason, retryable?)
tasks.update(id, ...)
```

Task claim must be one transaction:

1. verify `pending`, unowned, all blockers completed, and claimant not already at its configured work limit;
2. increment `claim_generation`, set owner and `claimed`;
3. append `TaskClaimed` event;
4. enqueue the task prompt to that agent;
5. commit.

On agent death, requeue only tasks whose `(owner, claim_generation)` still match the dead lease. This preserves a completion or reassignment that raced with failure, the same invariant Qwen implements with a locked reread.

Completion must mark the task completed, store a compact result artifact, unblock dependents, and append one event in the same transaction. Interested agents receive a small notification such as `task #12 completed by /root/api; unblocked #15`, not the whole result or board.

### Messaging and cross-chat behavior

Because SERAPH has a daemon/coordinator, it does not need Claude/Qwen's per-session socket mesh. Every local session registers one channel with the coordinator. Address by stable ID or `name [short-ref]`; never guess between duplicate names.

Use three distinct delivery semantics:

- `NextSafePoint`: make the message visible after the current tool batch and before the next model call; never interrupt a running tool.
- `WhenIdle`: retain it until the current turn and queued steering finish.
- `WakeIfIdle`: `NextSafePoint` while busy, but start a new turn if idle. This is the useful cross-chat default.

Return `accepted`, `delivered`, `held`, `refused`, `expired`, or `uncertain`, with a message ID. An uncertain outcome must not be retried automatically without a deduplication key. Bound message size and queue length. Plain text should travel in an authority-labeled envelope; large results travel as artifact handles.

Add `watch(target, idle|terminal|task_completed, once=true, expires_at)` early. It eliminates the common token-burning loop where one agent repeatedly asks whether another is done. Completion and idle events should wake the watcher only when the watcher is itself idle; otherwise they join its safe-point queue.

### Workspaces

Associate workspace ownership with tasks, not agents forever:

- Read-only agents may share the source checkout.
- At most one agent holds a writer lease for a checkout.
- Parallel writers get isolated worktrees from a pinned base revision.
- Completion stores a patch/ref and worktree state; the leader or a dedicated integrator is the only merge authority.
- A worktree is never deleted merely because a child stopped. Reclaim it only after its changes are snapshotted or proven empty.

This combines Qwen's intentionally narrow single-writer coordination with Grok's recoverable worktree lifecycle.

## Token-efficiency rules

The implementation language is not the main token variable. These rules are:

1. **Fresh by default.** Spawn independent work with no parent history and a concise task/artifact list. Use a fork only when prior conversation is actually required.
2. **Bound forks.** Support Codex-style `last_n` real user turns. Never implicitly copy a 100k-token parent transcript.
3. **Exploit cache identity deliberately.** A full-history fork can be cheaper than a fresh agent only when model, system prompt, tool definitions, and prefix remain identical enough to hit the provider cache. Record cache-read and cache-write tokens so this is measured, not assumed.
4. **Keep volatile state late.** Stable instructions and tool schemas form the cached prefix. Task deltas, messages, and current artifacts belong in a late context projection, not the system prompt.
5. **Lazy-load coordination tools.** The base prompt needs at most a tiny `coordination` capability description. Load task/message schemas when a team exists or the model asks for them.
6. **Events, not polls.** Host waits, completion notifications, and one-shot subscriptions cost no inference tokens while idle.
7. **Artifacts, not transcripts.** Child result = short summary + typed artifact handles + usage. Do not paste its tool log or entire answer into every peer.
8. **Delta visibility.** Peers need “claimed/completed/unblocked” events. The full shared board is fetched on demand and rendered in Ratatui outside model context.
9. **Parallel tool execution is one runtime operation.** A model-facing program should issue independent capability calls concurrently and receive compact structured reductions; it should not require one inference round per tool.
10. **Account for all descendants.** Separate parent-context tokens from child usage, cache reads/writes, workflow calls, and incomplete/cancelled usage. Surface cost per task and per agent.

## What is uniquely worth transplanting

| Priority | Donor | Mechanism | SERAPH treatment |
|---|---|---|---|
| P0 | Qwen | Locked claims, dependency graph, completion unblock, crash requeue, auto-claim | Port the state machine to Rust/SQLite. |
| P0 | Codex | Hierarchical `AgentPath`; `fork_turns`; separate message/follow-up/interrupt/list/wait | Adapt the Rust API and semantics. |
| P0 | Grok | Coordinator-owned lifecycle, cancellation scopes, honest message/usage uncertainty | Reuse/adapt Apache Rust patterns behind a smaller module boundary. |
| P0 | Claude | Safe-point cross-session delivery and one-shot `notify_when_idle` | Implement from public behavior only. |
| P1 | Qwen | Unambiguous `name [ref]` peer routing and accept/hold/refuse ingress | Port behavior; central daemon replaces socket mesh. |
| P1 | Grok | Worktree snapshot/ref/reclaim lifecycle | Adapt after the basic writer lease exists. |
| P1 | Prime | Durable agent registry across detach/restart; model-facing programmatic capability | Reimplement in Rust; optionally reuse MIT Python runtime pieces later. |
| P1 | Claude/Qwen | Cache-sharing fork distinct from a fresh agent | Implement as an explicit spawn mode and verify through usage telemetry. |
| Skip v0 | Claude/Qwen | Per-session mailbox/socket files | Unnecessary with one SERAPH coordinator. |
| Skip v0 | Claude | tmux/iTerm teammate processes and remote Anthropic relay | Product shell complexity, not core coordination. |
| Skip v0 | Grok | Full fast-worktree crate family and every lifecycle edge | Too coupled for v0; begin with ordinary Git worktrees and durable refs. |
| Skip v0 | Prime | Python as coordination authority | Python may be the programmatic client; Rust remains the authority. |

## Acceptance criteria for the first usable version

SERAPH v0 multi-agent coordination is real when all of these hold:

- two agents cannot both successfully claim one task;
- one agent's completion atomically makes its dependents claimable;
- every viewer sees owner/status changes without injecting the entire board into each model prompt;
- an agent crash requeues only work it still owns;
- a busy recipient receives a message at a safe point, while an idle recipient can be woken;
- a parent can interrupt a child, and session cancellation cannot accidentally kill an unrelated prior-session child;
- agent topology, tasks, receipts, and worktree ownership recover after SERAPH restarts;
- parallel writers never share an unleased checkout;
- the usage view separates parent, each child, cache reads/writes, and incomplete usage;
- waiting for another agent consumes zero model tokens until a relevant event arrives.

That design gives SERAPH the shared awareness the user wants without turning coordination bookkeeping into permanent prompt overhead.
