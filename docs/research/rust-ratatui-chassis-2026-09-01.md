# Reusable Rust and Ratatui chassis components

**Research date:** 2026-09-01  
**Issue:** [#5 — Map reusable Rust and Ratatui chassis components](https://github.com/Matari-Audio/SERAPH/issues/5)

## Conclusion

SERAPH should own a small Ratatui/Tokio chassis. It should not fork Codex, Grok Build, Crush, VT Code, or another terminal agent.

The useful source-level transplants are narrow:

- the redraw-coalescing algorithm in Codex's `FrameRequester` and `FrameRateLimiter`;
- the producer-task-to-`Stream` ownership pattern in Forgecode's 40-line `MpscStream`, strengthened with a cancellation token and typed terminal event;
- the net-turn diff model in Codex and the conservative hunk stitching in Grok Build, after replacing their product-specific input types;
- data shapes and failure semantics, not crates, from Grok's synchronous command registry and chat-state persistence boundary.

Everything else is better treated as architectural evidence. Codex's TUI is about 294K Rust lines, Grok's pager about 480K, VT Code's UI crate about 59K, and each assumes its own agent protocol, configuration, authentication, tools, and session model. Pulling any one in would make that product's historical decisions SERAPH's architecture.

The recommended v0 is one application state owner, one typed event channel, demand-driven coalesced rendering, a small normalized provider stream, supervised background tasks, and canonical append-only JSONL sessions. That gives SERAPH the useful behavior without importing another harness.

## Evidence base and licensing

All source observations below are pinned to the inspected revision rather than a moving branch.

| Project | Inspected revision | License at revision | Reuse status |
| --- | --- | --- | --- |
| Ratatui | [`ratatui-v0.30.0`](https://github.com/ratatui/ratatui/tree/ratatui-v0.30.0) (`dcd9a10`) | MIT OR Apache-2.0 | Safe foundation |
| Codex | [`4ac20a7`](https://github.com/openai/codex/tree/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456) | Apache-2.0 | Small attributed ports are compatible |
| Grok Build | [`bb7f39d`](https://github.com/xai-org/grok-build/tree/bb7f39d5858cbf5e00de639367f59debbdcb0138) | Apache-2.0 | Small attributed ports are compatible |
| Crush | [`559ec80`](https://github.com/charmbracelet/crush/tree/559ec80922fecf3baa0b7599230f4c91067440de) (`v0.92.0`) | FSL-1.1-MIT | **Do not copy current code** into a competing harness; use behavior only. Each version becomes MIT two years after that version's publication, not immediately ([license](https://github.com/charmbracelet/crush/blob/559ec80922fecf3baa0b7599230f4c91067440de/LICENSE.md)) |
| Forgecode | [`6ed5d37`](https://github.com/tailcallhq/forgecode/tree/6ed5d37b6b45a2b6220877fd9aec5ba4c4b7f3c0) | Apache-2.0 | Tiny streaming helper is portable |
| Goose | [`5345d05`](https://github.com/block/goose/tree/5345d05176f76532e99cb6bef20372920c80f06b) | Apache-2.0 | Provider/extension contracts are useful evidence; crates are too broad |
| VT Code | [`8cfcd5c`](https://github.com/vinhnx/VTCode/tree/8cfcd5cc72f711cd3ac3f6db62014671e1d946dc) | MIT OR Apache-2.0 | Useful comparison; UI crate is not a small chassis |
| Shai | [`f076f61`](https://github.com/ovh/shai/tree/f076f6128a826a96a28e47d0d674284b1552905e) | Apache-2.0 | Do not adopt its unpinned Ratatui fork dependency |

Apache-derived source must retain the applicable copyright and license notices. The recommendation below often says “reimplement the seam” because a short SERAPH-native implementation is clearer than preserving imports and data models from a much larger work; that is an engineering judgment, not a way to avoid license obligations when code is actually copied.

## Decision matrix

| Concern | Best evidence | Exact seam | SERAPH action |
| --- | --- | --- | --- |
| Terminal init/restore | Ratatui | `ratatui::init`/`try_init`, `restore`/`try_restore` | Use directly |
| Render loop | Ratatui + Codex | `Terminal::draw` buffer diff; `FrameRequester`/limiter | Use Ratatui; port only coalescing logic |
| Terminal input | Grok + Codex | dedicated reader to MPSC; pause/recreate source for subprocesses | Implement a small SERAPH reader controller |
| App events | Codex + Crush | typed app messages and lossy-vs-must-deliver distinction | Define a small closed `UiEvent` enum; do not copy either bus |
| Provider streaming | Goose + Forgecode + Codex | one required `stream` method; owned producer task; cancellation on consumer drop | Define SERAPH-owned trait and normalized events |
| Commands | Grok | synchronous metadata/dispatch returning typed actions | Reimplement a much smaller registry |
| Background work | Grok | `Effect` -> `JoinSet` -> typed completion | Use `JoinSet` + `CancellationToken` directly |
| Session persistence | Codex + Grok | canonical append log; single persistence owner; explicit flush/ack | Implement one JSONL writer task and small store trait |
| Diff/review | Codex + Grok | net committed delta; conservative hunk stitch | Port decoupled algorithms/data only |
| Extensions | Goose + Grok ACP | process/protocol boundary and runtime catalogs | Keep registries internal; add process protocols later |

## 1. Ratatui shell and event/render loop

Ratatui already owns the low-level terminal contract. Its v0.30 initialization helpers enable raw mode, enter the alternate screen, install a panic hook, and provide paired restoration; the official async example uses `tokio::select!` for input and background work ([init source](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/ratatui/src/init.rs), [async example](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/examples/apps/async-github/src/main.rs)). `Terminal::draw` already computes and flushes the buffer delta, so SERAPH does not need a rendering engine beneath widgets ([terminal source](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/ratatui-core/src/terminal/terminal.rs)). Render `&App` as a widget so drawing borrows state rather than cloning it.

Use one state-owning application loop. Producers send events; only the loop mutates UI state and calls `draw`. A suitable minimal event set is:

```rust
enum UiEvent {
    Terminal(crossterm::event::Event),
    Provider { turn: TurnId, event: ProviderEvent },
    TaskFinished { id: TaskId, result: TaskResult },
    SessionPersisted { revision: u64, result: io::Result<()> },
    Redraw,
    Shutdown,
}
```

This is deliberately smaller than Codex's 1,422-line product-specific `AppEvent` bus ([source](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/tui/src/app_event.rs)). A closed enum gives the compiler ownership of routing and prevents UI components from acquiring service handles.

### Input cancellation and terminal handoff

Do not place `EventStream::next()` directly beside many always-ready branches and assume cancellation behavior. Grok documents an observed idle-input failure and routes blocking crossterm reads from a dedicated thread into an MPSC receiver, whose `recv()` is cancellation-safe ([event-loop workaround](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager/src/app/event_loop.rs#L1692-L1699)). The upstream crossterm cancellation-safety question remains open, so Grok's diagnosis is product evidence rather than an upstream guarantee ([crossterm #936](https://github.com/crossterm-rs/crossterm/issues/936)).

Codex solves the adjacent subprocess problem by owning the event source in an `EventBroker` that can drop and recreate it when an editor or child process needs stdin, then round-robins draw and terminal polling to avoid starvation ([broker](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/tui/src/tui/event_stream.rs#L1-L180), [fair polling](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/tui/src/tui/event_stream.rs#L289-L317)).

For v0, implement a smaller `TerminalInput` controller:

- one dedicated reader thread and bounded or unbounded MPSC into the app loop;
- explicit `pause()` that stops and joins the reader before giving stdin to a child;
- `resume()` that creates a fresh reader;
- terminal restoration in both normal shutdown and the panic hook.

Do not transplant Codex's 619-line broker or Grok's 6,270-line event loop. Their complexity is largely suspension, platform workarounds, ACP, voice, auth, multiple screen modes, and product-specific fairness policy.

### Demand-driven redraw

Provider token streams can otherwise redraw once per token. Codex's `FrameRequester` accepts immediate or delayed deadlines, coalesces them in a task, and clamps emissions to 120 FPS ([requester](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/tui/src/tui/frame_requester.rs#L1-L124), [limiter](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/tui/src/tui/frame_rate_limiter.rs#L1-L37)). The production algorithm is small and Apache-compatible.

Port the algorithm, not the surrounding TUI. SERAPH can initially cap at 60 FPS and coalesce a dirty flag until the next frame deadline. Idle sessions then draw zero frames, while bursts of provider events remain smooth.

### What not to take from Grok's terminal layer

`xai-ratatui-inline` is a separate Apache-2.0 crate with only `anstyle-parse`, `crossterm`, `ratatui`, and `unicode-width` dependencies ([manifest](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-ratatui-inline/Cargo.toml)). That looks attractive until its custom frame conversion asserts equal sizes and uses `unsafe transmute` between its private frame and Ratatui's private layout ([source](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-ratatui-inline/src/terminal.rs#L112-L143)). This is version-fragile. Start with Ratatui's native fullscreen/inline viewport support; revisit scrollback-preserving output only after a concrete v0 requirement proves the standard terminal insufficient.

## 2. Provider and streaming boundary

The best contract is Goose's shape, not its crate. `goose-provider-types` makes `stream` the only required provider operation and separates static provider metadata from the runtime provider ([README](https://github.com/block/goose/blob/5345d05176f76532e99cb6bef20372920c80f06b/crates/goose-provider-types/README.md#the-provider-trait), [trait](https://github.com/block/goose/blob/5345d05176f76532e99cb6bef20372920c80f06b/crates/goose-provider-types/src/base.rs#L474-L506)). However, that “types” crate is roughly 29K lines and directly depends on `reqwest`, `rmcp`, regex, canonical model data, permissions, images, and caching ([manifest](https://github.com/block/goose/blob/5345d05176f76532e99cb6bef20372920c80f06b/crates/goose-provider-types/Cargo.toml)). It is not a lean core dependency.

Forgecode's `MpscStream<T>` is the smallest reusable implementation found: about 40 production lines, depending only on Tokio and `futures`; it spawns a producer, exposes the receiver as a `Stream`, and aborts the producer on drop ([source](https://github.com/tailcallhq/forgecode/blob/6ed5d37b6b45a2b6220877fd9aec5ba4c4b7f3c0/crates/forge_stream/src/mpsc_stream.rs#L1-L40), [manifest](https://github.com/tailcallhq/forgecode/blob/6ed5d37b6b45a2b6220877fd9aec5ba4c4b7f3c0/crates/forge_stream/Cargo.toml)). Codex independently uses a bounded MPSC `ResponseStream` and cancels its mapper when the consumer drops ([source](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/core/src/client_common.rs#L108-L127)).

SERAPH should combine those behaviors in its own small API:

```rust
trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, ProviderError>>;
    fn stream(&self, request: ModelRequest)
        -> BoxFuture<'_, Result<ProviderStream, ProviderError>>;
}

enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta { id: String, json: String },
    ToolCallReady(ToolCall),
    Usage(TokenUsage),
    Completed(FinishReason),
}
```

The stream should use a bounded channel for backpressure, own its producer task, and cancel that task on drop. Require one terminal `Completed` or error event. Provider adapters translate wire-specific SSE/WebSocket chunks into these events; neither UI nor session code sees OpenAI/Anthropic DTOs.

Do not transplant Codex's `ModelClient`: its 2,733 lines deliberately encode OpenAI Responses API, WebSocket prewarming, sticky routing, auth, telemetry, and transport fallback ([source overview](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/core/src/client.rs#L1-L24), [turn-scoped session](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/core/src/client.rs#L302-L345)). Forgecode's `ProviderService` is also tied to Forge domain DTOs ([trait](https://github.com/tailcallhq/forgecode/blob/6ed5d37b6b45a2b6220877fd9aec5ba4c4b7f3c0/crates/forge_app/src/services.rs#L158-L180)), and Shai's otherwise small trait leaks `openai_dive` response types ([source](https://github.com/ovh/shai/blob/f076f6128a826a96a28e47d0d674284b1552905e/shai-llm/src/provider.rs#L1-L70)).

## 3. Commands and supervised background work

Grok's key command design is good: command lookup and argument metadata are synchronous; work that needs I/O returns a typed `CommandResult::Action`, leaving execution to the app/effect layer ([result type](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager/src/slash/command.rs#L1-L70), [trait](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager/src/slash/command.rs#L219-L281)). Its registry supports runtime commands and aliases and fails closed when required tools are not known ([registry](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager/src/slash/registry.rs#L1-L170)).

Reimplement only this shape:

```rust
struct CommandSpec { name: &'static str, aliases: &'static [&'static str], help: &'static str }
enum CommandOutcome { Action(AppAction), Submit(String), Message(String) }
```

A `HashMap<&str, CommandId>` and a small match/handler table are enough for v0. Do not copy Grok's 463-line trait, 1,329-line registry, or command-specific app contexts; those abstractions pay for ACP commands, workflows, product entitlements, dynamic visibility, and rich completion.

Model every asynchronous command as an effect. Grok's event loop spawns `Effect`s into a `JoinSet` and routes typed `TaskResult` completions back as actions ([effect boundary](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager/src/app/actions.rs#L1314-L1339)). SERAPH can do the same directly with:

- a root `CancellationToken` and child tokens per turn/task;
- a `JoinSet<(TaskId, TaskResult)>` owned by the app runtime;
- no detached `tokio::spawn` for work whose completion matters;
- cancel, drain, flush session writes, restore terminal, then exit;
- `spawn_blocking` for filesystem scans, subprocess waits, or other blocking work.

Crush reinforces an important reliability distinction: ordinary updates may be lossy under backpressure, but terminal run-completion, tool-result, error, and cancel events must be delivered; its fan-in waits for subscribers and flushes buffered streaming updates before database shutdown ([event contract](https://github.com/charmbracelet/crush/blob/559ec80922fecf3baa0b7599230f4c91067440de/internal/pubsub/events.go#L41-L66), [shutdown/fan-in](https://github.com/charmbracelet/crush/blob/559ec80922fecf3baa0b7599230f4c91067440de/internal/app/app.go#L588-L668)). Copy the invariant, not the FSL code: progress/redraw may coalesce, but completion and persistence acknowledgements may not disappear.

## 4. Sessions and persistence

Codex now has a clean conceptual boundary: `ThreadStore` owns canonical history appends and metadata updates, while its local implementation writes JSONL as the canonical history and treats SQLite as queryable metadata/projection ([README](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/thread-store/README.md)). Its writer has explicit append, persist, flush, and shutdown commands with acknowledgements ([recorder](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/rollout/src/recorder.rs#L77-L143)). The projection is ordered after the durable JSONL write so SQLite may lag but cannot get ahead of canonical history ([live writer](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/thread-store/src/local/live_writer.rs#L283-L347)).

That invariant is excellent. The implementation is not a v0 transplant: `codex-thread-store` alone pulls protocol, rollout, state, SQLx, zstd, git, telemetry, project, search, migration, and pagination crates ([manifest](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/thread-store/Cargo.toml)).

Grok's `xai-chat-state` similarly puts mutable conversation state and a `ChatPersistence` implementation under one actor, so persistence needs no shared locks. Its trait distinguishes append, rewrite, destructive rewrite with backup/ack, and flush ([actor](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-chat-state/src/actor/mod.rs#L28-L114), [persistence contract](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-chat-state/src/persistence.rs#L1-L63)). The crate is about 15K lines and depends on Grok sampling, compaction, and token-estimation types, so again the seam is better than the crate.

For SERAPH v0:

- one session directory per UUID;
- `events.jsonl` as canonical append-only history;
- a small atomically replaced `meta.json` for title, timestamps, current model, and last durable revision;
- one writer task exclusively owning `BufWriter<File>`;
- commands `Append`, `Flush(oneshot)`, and `Shutdown(oneshot)`;
- monotonic event revision and explicit schema version on every record;
- recovery that accepts complete records and reports/ignores only a torn final line;
- no SQLite until session listing/search measurements justify a projection.

Keep session events, kernel snapshots, workflow journals, and artifacts as separate typed records or stores. Do not serialize a giant mutable application snapshot as the source of truth.

## 5. Diff and review rendering

Ratatui should render structured review data; it should not parse ANSI output from another subsystem.

Codex's `TurnDiffTracker` consumes exact committed patch deltas, maintains baseline/current content without rereading the filesystem, follows renames, caches rendered revisions, and invalidates itself if the input delta is not exact ([source](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/core/src/turn_diff_tracker.rs#L47-L181)). This is the correct semantic layer for “what this turn changed.” The 403-line implementation depends on Codex patch and path types; port its state model after SERAPH's own committed-delta type exists.

Grok's `xai-grok-pager-diff` has good conservative presentation behavior: overlapping hunks are stitched only when post-state coordinates and text agree, and ambiguous shapes remain separate rather than displaying false history ([algorithm](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager-diff/src/lib.rs#L168-L288)). Do not depend on the crate as published in the workspace: despite being presented as an extracted diff crate, its manifest imports `xai-grok-tools`, ACP, JSON, and tracing merely to decode Grok tool-call shapes ([manifest](https://github.com/xai-org/grok-build/blob/bb7f39d5858cbf5e00de639367f59debbdcb0138/crates/codegen/xai-grok-pager-diff/Cargo.toml)).

The smallest useful transplant is:

- local `DiffLine { old_line, new_line, kind, text }` and `DiffHunk` types;
- `similar` for line diffs;
- the decoupled conservative stitch algorithm with attribution;
- a Ratatui review widget that virtualizes/wraps only visible rows.

Rendering should consume committed `FileDelta`s from the editing transaction layer, not provider prose or raw tool JSON. Keep previews explicitly provisional and replace them with the committed delta after execution.

## 6. Extension seams

SERAPH v0 needs extension points, not an extension runtime.

Define registries for providers, commands, capabilities/tools, artifact renderers, and lifecycle observers. Registration returns metadata and a typed handler; discovery/catalog data stays outside the model prompt until selected. These registries are ordinary Rust composition at startup, not dynamically loaded Rust libraries with an unstable ABI.

Goose demonstrates why a full MCP extension manager is not a chassis primitive: its manager is 4,281 lines, holds MCP clients, secrets-resolved config, provider/session/scheduler context, resources, caches, and capability negotiation ([manager](https://github.com/block/goose/blob/5345d05176f76532e99cb6bef20372920c80f06b/crates/goose/src/agents/extension_manager.rs#L124-L212)). Grok's ACP split likewise proves a process/protocol boundary can keep pager and agent independent, but the pager's ACP session machinery is product-scale, not needed to launch v0.

When external extensions become necessary, use a versioned subprocess protocol (ACP/MCP/JSON-RPC) so failures, upgrades, and permissions stay outside the host process. Do not stabilize a plugin ABI before there is a real third-party extension.

## Screened alternatives and rejection reasons

- **VT Code:** its Apache/MIT `vtcode-ui` crate has a useful separate input task and pause/resume controller, but the crate directly includes images, clipboard, panic UI, syntax highlighting, fuzzy matching, custom widgets, project config, and unstable Ratatui features ([manifest](https://github.com/vinhnx/VTCode/blob/8cfcd5cc72f711cd3ac3f6db62014671e1d946dc/crates/codegen/vtcode-ui/Cargo.toml), [event channel](https://github.com/vinhnx/VTCode/blob/8cfcd5cc72f711cd3ac3f6db62014671e1d946dc/crates/codegen/vtcode-ui/src/tui/core_tui/runner/events.rs#L1-L80)). It confirms the reader-channel design but is not lighter than implementing it.
- **Shai:** small and Apache-2.0, but both workspace and CLI depend on an unpinned Git branch of a Ratatui fork for viewport resizing ([manifest](https://github.com/ovh/shai/blob/f076f6128a826a96a28e47d0d674284b1552905e/shai-cli/Cargo.toml#L7-L33)). Do not inherit that supply-chain and compatibility risk.
- **Crush:** its Bubble Tea update/subscription model, SQLite session service, and parent/child session records are useful product evidence ([sessions](https://github.com/charmbracelet/crush/blob/559ec80922fecf3baa0b7599230f4c91067440de/internal/session/session.go#L19-L121)), but it is Go rather than Rust/Ratatui and its current FSL terms prohibit the relevant source reuse.
- **CodeWhale and other Codex-shaped Rust agents:** no inspected candidate exposed a smaller, cleaner licensed chassis than Ratatui plus the narrow seams above. A renamed or lightly altered full harness fork compounds provenance and dependency risk without providing a durable module boundary.

## Smallest viable Rust dependency set

Start with:

- `ratatui` 0.30 and `crossterm` 0.29;
- `tokio` with only required runtime, sync, signal, process, filesystem, and time features;
- `tokio-util` for `CancellationToken`;
- `futures-core`/`futures-util` only if provider streams need the standard `Stream` trait;
- `serde` and `serde_json` for the journal and provider/tool wire models;
- `thiserror` for stable subsystem errors;
- `uuid` for session/turn/task IDs;
- `similar` only when diff review lands.

Avoid Codex's current patched crossterm fork and unstable Ratatui feature set unless a reproduced bug requires them: its workspace pins crossterm to an OpenAI Git revision, and the TUI enables scrolling regions plus unstable backend/rendered-line/widget features ([workspace pins](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/Cargo.toml#L398-L402), [patches](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/Cargo.toml#L597-L602), [TUI features](https://github.com/openai/codex/blob/4ac20a7f748a8a12cae0eb5019a26d13fdc2d456/codex-rs/tui/Cargo.toml#L73-L112)). Keep syntax highlighting, image/clipboard support, SQLite, ACP, MCP, and dynamic plugins out of the chassis until a shipped feature needs each dependency.

## Recommendation

Build a SERAPH-native chassis around official Ratatui. The only near-term code port worth preserving recognizably is Codex's redraw coalescer/rate limiter; Forgecode's stream wrapper is small enough either to port with attribution or reimplement with stronger cancellation. Treat the provider trait, command/effect split, session writer, diff tracker, and registries as SERAPH domain interfaces informed by the sources above.

This boundary also serves token efficiency: provider chunks, tool progress, logs, and session history remain typed runtime state; the TUI renders them and the model receives only the selected projection. Reusing a large harness would import its context and protocol assumptions—the opposite of SERAPH's central advantage.
