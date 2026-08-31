# Lazy capabilities and artifacts for SERAPH

Research date: 2026-09-01
Question: Which current harness mechanisms are reusable for a tiny model bootstrap, lazy capabilities, retained Python clients, artifact handles, programmatic reduction, context projection, and honest token accounting?

## Decision

SERAPH should expose one stable model-facing execution tool and put discovery inside its persistent Python runtime:

```python
matches = caps.search("GitHub pull requests", limit=5)
github = caps.load(matches[0].id)
prs = await github.search_pull_requests(state="open")
emit(summarize(prs))
```

The model-facing tool list and system-prefix do not change when 10 or 10,000 capabilities are installed. `caps.search()` queries a host-side index; `caps.load()` returns a Python proxy retained in the kernel; raw results stay in Python or the artifact store; only `emit()` crosses back into model context. This is smaller and more cache-stable than installing newly selected provider tools.

Implement the host in Rust, keep CPython as a sidecar, and do not embed a JavaScript/V8 runtime in v0. Take behavior from the projects below, not their application architecture.

## Method and pinned sources

I inspected the source at these exact revisions, plus current first-party Claude documentation. Source links below are immutable commit permalinks.

| System | Revision inspected | License | Relevant runtime |
| --- | --- | --- | --- |
| Kimi Code | [`630a11d`](https://github.com/MoonshotAI/kimi-code/tree/630a11db51ab0ac422cae6a10580b62c1ae8e05f) (2026-09-01) | [MIT](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/LICENSE) | TypeScript/Node |
| Codex | [`2c3bf4e`](https://github.com/openai/codex/tree/2c3bf4ea793aa5c590932553d242a287380e9cec) (2026-08-31) | [Apache-2.0](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/LICENSE) | Rust plus V8 for Code Mode |
| Prime Agent | [`9f5edc1`](https://github.com/PrimeIntellect-ai/prime-agent/tree/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09) (2026-08-31) | [MIT, retaining Pi and Prime notices](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/LICENSE) | TypeScript host plus CPython |
| Qwen Code | [`bd59085`](https://github.com/QwenLM/qwen-code/tree/bd5908531160e3c68556cda5ee01b3a86a2dc1f1) (2026-08-31) | [Apache-2.0](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/LICENSE) | TypeScript/Node plus JS workflow VM |
| OpenCode | [`0428492`](https://github.com/sst/opencode/tree/04284921ac8f657555b5a182f5ff055f471543e4) (2026-08-31) | [MIT](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/LICENSE) | TypeScript/Bun plus owned JS interpreter |
| Pi | [`853a80d`](https://github.com/badlogic/pi-mono/tree/853a80d26c90a14c1886f0ebb8ffaae133ca2185) (2026-08-28) | [MIT](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/LICENSE) | TypeScript/Node |

The older `MoonshotAI/kimi-cli` repository was also checked at `cbc15c0`, but current development and the mechanisms discussed here are in `MoonshotAI/kimi-code`. Treating the two as one codebase gives stale conclusions.

## Comparative result

`O(N)` below means prompt material grows with all installed tools, skills, or namespaces even when none is used.

| System | What the model initially sees | What happens on selection | Unused-capability prompt cost | Cache behavior | Large-result behavior |
| --- | --- | --- | --- | --- | --- |
| Kimi Code | `select_tools` plus every deferred tool **name** announcement | Full schemas are inserted into history | `O(N names)` | Loaded schemas land in the conversation suffix | Spill file plus preview; not a typed artifact store |
| Codex native tool search | Small BM25 search tool; deferred metadata remains host-side when source listing is omitted | Matching schemas arrive as `tool_search_output` | Effectively `O(1)` | New definitions are conversation items rather than a rewritten prefix | No general artifact store |
| Codex Code Mode | One `exec` plus enabled declarations; deferred declarations may be omitted | All nested tools remain callable inside V8; raw results stay in code unless emitted | `O(enabled schemas)`; deferred metadata is available to code | Stable outer tool, subject to enabled declaration set | Strong programmatic reduction; session JSON `store/load`, not blobs |
| Prime Agent | One persistent `ipython` tool, plus all visible skill metadata and enabled MCP server names | Python client discovers and caches MCP schemas | `O(skills + servers)` | Stable one-tool surface | Kernel objects and output spill files; durable namespace snapshots |
| Qwen Code | Search tool plus every deferred name and truncated description | Rebuilds active tool declarations | `O(N names + descriptions)` | Rebuild can invalidate tool-prefix cache | Aggregate output finalizer and session-artifact metadata |
| OpenCode CodeMode | One `execute`, all namespace counts, and up to 2,000 estimated tokens of signatures | In-program search returns complete signatures | `O(namespaces) + bounded catalog` | Stable outer tool | Intermediate structured values stay in interpreter; outer result is bounded |
| Pi | Normal active tools and all visible skill metadata | Extensions can add tools at a result boundary; supported providers serialize definitions at that transcript point | `O(active tools + skills)` before extension-specific discovery | Native late loading preserves the prior prefix | Conventional tool results/compaction; no general artifact store |
| Claude API | Search/code-execution tools; deferred schemas excluded from model context although all are sent to the API | `tool_reference` expands inline; programmatic calls run from Python | Effectively `O(1)` model context, but `O(N schemas)` request payload | Deferred expansion leaves the cached prefix untouched | Intermediate programmatic results do not enter Claude context |
| Proposed SERAPH | One Python execution contract and constant bootstrap | Search/load stays inside Python; no provider tool-list mutation | `O(1)` with respect to installed capabilities | Stable tools, system prefix, and provider cache key | Opaque, durable artifact references; explicit `emit()` projection |

The important distinction is between **transport cost**, **provider input tokens**, and **visible prompt text**. Anthropic can accept every schema over the wire while excluding deferred ones from Claude's context. A local SERAPH index can avoid all three costs until selection.

## Source findings

### 1. Kimi Code: useful schema deferral, but the catalog still leaks into context

Kimi's current dynamic-tool service is gated by the model capability `dynamically_loaded_tools`, ordinary tool-use support, and an experimental flag. It withholds MCP tools and tools marked `disclosure: deferred`; `select_tools` adds exact names; `drainPendingToolSchemas()` then inserts full definitions; loaded state is reconstructed from tool-bearing context messages. Compaction and context splicing explicitly reconcile pending/loaded state. See [`toolSelectService.ts`](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/packages/agent-core-v2/src/agent/toolSelect/toolSelectService.ts) and the flag description in [`flag.ts`](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/packages/agent-core-v2/src/agent/toolSelect/flag.ts).

This is not zero-cost disclosure. `renderLoadableToolsAnnouncement()` emits an XML block containing **every loadable exact name**, and tells the model to fold additions/removals across conversation history. Schemas are hidden, but catalog-name tokens scale linearly. See [`dynamicTools.ts`](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/packages/agent-core-v2/src/agent/toolSelect/dynamicTools.ts).

Kimi's large-result path is a good minimum safety net: output over the configured threshold is atomically saved, then replaced with a bounded head/tail preview and `output_path` instructions. The default model-facing threshold is 50,000 characters and retention is capped at 10,000,000 characters unless the producer already supplied a spill path. It is a path-bearing text spill, not an opaque typed or content-addressed artifact. See [`toolResultTruncationService.ts`](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/packages/agent-core-v2/src/agent/toolResultTruncation/toolResultTruncationService.ts) and [`toolContract.ts`](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/packages/agent-core-v2/src/tool/toolContract.ts).

Worth porting:

- rebuild loaded capability state from durable events rather than trusting an in-memory set;
- fail explicitly when a loaded capability disappears;
- reuse a producer-owned spill instead of copying the output again.

Do not port the all-name announcement.

### 2. Codex: the best host-side search seam and the strongest local reduction boundary

Codex's native deferred-tool search is the closest existing implementation to SERAPH's desired catalog. A tiny `query`/`limit` tool searches deferred metadata with a cached BM25 engine, and returns matching `LoadableToolSpec` definitions as a Responses API `tool_search_output`. Output schemas are stripped and the definitions remain marked deferred. See [`tool_search_spec.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/tool_search_spec.rs), [`tool_search.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/handlers/tool_search.rs), and the loadable representation in [`tools/src/tool_search.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/tools/src/tool_search.rs).

The searchable catalog can remain entirely host-side. A source listing can be omitted when world state already advertises sources; otherwise source descriptions, not every tool, are exposed. The handler cache reuses its index while immutable and dynamic sources are unchanged. This is the retrieval behavior to reproduce in Rust.

Code Mode adds a different seam. The model writes JavaScript into one `exec` call. A fresh V8 isolate gets nested `tools.*` functions plus `text`, `store`, `load`, and other helpers. Deferred nested declarations can be omitted from the exec description while the runtime still receives all nested definitions and `ALL_TOOLS` metadata. See [`description.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-protocol/src/description.rs), [`execute_handler.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/code_mode/execute_handler.rs), and [`globals.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-runtime/src/runtime/globals.rs).

The critical token property is explicit in the tool-context code: nested history receives a token budget, while Code Mode retains the raw result. JavaScript can call many tools and only values passed through `text()` become outer model content. See [`context.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/core/src/tools/context.rs). `store/load` retain serializable JSON between fresh cells and commit changes only after successful execution; they are not durable arbitrary-object or blob storage. See [`callbacks.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-runtime/src/runtime/callbacks.rs).

Worth porting:

- host-side BM25 retrieval with a cache keyed by catalog generation;
- raw nested results available to code, with an explicit model-output function;
- commit session state only after a successful cell;
- separate nested execution accounting from outer context accounting.

Do not embed Codex's V8 runtime in v0. Its crate directly depends on V8 with sandbox support and ICU data, a large second language runtime beside CPython. See [`code-mode-runtime/Cargo.toml`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/code-mode-runtime/Cargo.toml).

### 3. Prime Agent: the retained Python client and kernel-state reference

Prime's default model surface is a persistent `ipython` tool. Python variables, imports, handles, and computed data survive calls, making Python the exploratory workspace and orchestration language rather than just a calculator. See [`ipython.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/tools/ipython.ts), the default tool choice in [`system-prompt.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/system-prompt.ts), and the model-facing Python orchestration guidance in [`rlm.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/prompts/rlm.ts).

Its `McpIntegration` is the exact retained-client behavior SERAPH needs. It lazily lists tools on first use, caches their schemas in the Python object, synthesizes async methods through `__getattr__`, and forwards each invocation to the host. The broader MCP registry retains connection generations and tool listings. See [`mcp_base.py`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/mcp_base.py) and [`mcp.py`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/mcp.py).

Prime snapshots each serializable Python variable with `dill`, subject to per-variable and total limits, and skips unpicklable values. This is useful for executable workspace recovery, but live transport clients and secrets should be snapshotted only as capability IDs and rehydrated by the Rust host. See [`state-snapshot.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/kernel/state-snapshot.ts).

Prime does not satisfy zero-cost disclosure by itself. Its prompt lists enabled MCP server names, and its skills formatter emits every visible skill's name, type, import/location, and description before the skill body is used. See [`system-prompt.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/system-prompt.ts) and [`skills.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/skills.ts).

Worth porting:

- persistent Python namespace and async proxy ergonomics;
- host-owned credentials, connections, policy, and accounting;
- per-variable snapshot with explicit skipped-variable reporting.

Implement a clean Rust/Python RPC boundary; do not transplant Prime's TypeScript host.

### 4. Qwen Code: recovery and finalization are stronger than its disclosure economics

Qwen hides deferred MCP and low-frequency built-in schemas behind a search tool. Exact `select:` lookup and keyword search reveal definitions, and the registry can preload all deferred tools when its rough JSON-character/token estimate fits a configured fraction of context. See [`tool-search.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/tools/tool-search.ts) and [`tool-registry.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/tools/tool-registry.ts).

The hidden catalog is not hidden from the prompt: the environment reminder includes every deferred name and a truncated description, grouped by built-in or MCP server. See [`environmentContext.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/core/environmentContext.ts). Qwen's own design note also records that rebuilding the declarations with `setTools()` can invalidate the prompt-cache prefix, unlike Claude's inline `tool_reference` expansion. See [`toolsearch-preload-threshold.md`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/docs/design/toolsearch-preload-threshold.md).

Two adjacent mechanisms are worth taking:

- The shared finalizer allocates one aggregate character budget across all model-facing tool text, persists an oversized result once, reuses producer spill paths, preserves head/tail previews, and applies the same bounded representation to replay/resume. See [`tool-response-finalizer.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/tools/tool-response-finalizer.ts) and [`truncation.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/tools/truncation.ts).
- Its deterministic workflow journal reuses the longest unchanged prefix of completed `agent()` calls and resumes the rest, while forbidding `Date.now()` and `Math.random()` in the workflow sandbox. See [`workflow-journal.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/agents/runtime/workflow-journal.ts).

Qwen also has real session-artifact metadata: kinds, storage modes, retention, restore state, event/snapshot records, stable identity keys, limits, and validation. However, production source at this revision only validates a `managed_copy` content reference; no non-test writer constructing that reference was found. Its ordinary large-tool-output path remains a file pointer, and its artifact system primarily registers declared session outputs rather than turning every raw value into an opaque content-addressed object. See [`session-artifact-persistence.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/services/session-artifact-persistence.ts) and [`record-artifact.ts`](https://github.com/QwenLM/qwen-code/blob/bd5908531160e3c68556cda5ee01b3a86a2dc1f1/packages/core/src/tools/record-artifact.ts).

### 5. OpenCode: a lightweight interpreter and good output retention, still not zero-cost

OpenCode's current experimental CodeMode exposes one `execute` tool backed by an owned tree-walking JavaScript interpreter, not V8. Nested MCP calls run eagerly with up to eight concurrent calls; complete structured intermediate values remain inside the interpreter; only the outer result is model-facing. See [`code-mode.ts`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/opencode/src/tool/code-mode.ts) and the runtime design in [`codemode.md`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/codemode/codemode.md).

Its disclosure is bounded, not free. The default inline signature budget is 2,000 estimated tokens; every namespace and its tool count is always printed even at budget zero; an in-program search returns complete signatures for the rest. See [`tool-runtime.ts`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/codemode/src/tool-runtime.ts) and [`README.md`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/codemode/README.md).

OpenCode's v2 tool-output store is a useful retention baseline: over 2,000 lines or 50 KiB it writes a managed file, returns a bounded head/tail preview, carries the output path separately, and expires managed outputs after seven days. It is still a path, not an opaque artifact reference. See [`tool-output-store.ts`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/tool-output-store.ts).

Skills remain `O(N)`: the system context renders every permitted skill name and description, then the skill tool injects the body on demand. See [`guidance.ts`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/skill/guidance.ts) and [`skill.ts`](https://github.com/sst/opencode/blob/04284921ac8f657555b5a182f5ff055f471543e4/packages/core/src/tool/skill.ts).

Worth porting:

- supervised eager calls and an outer-only model-output boundary;
- deterministic, in-program catalog search;
- one managed-output cleanup policy.

The interpreter is a good option only if SERAPH later needs a deterministic workflow DSL. It does not justify a second exploratory language in v0.

### 6. Pi: cache-friendly late tools are now a real reusable primitive

Pi now carries `addedToolNames` on a tool result. Its coding-agent extension wrapper detects tools activated during an extension-tool call and attaches the newly active names to that result. See [`wrapper.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/core/extensions/wrapper.ts) and the contract in [`types.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/types.ts).

Provider adapters then split tools into immediate and transcript-loaded sets. Anthropic emits `tool_reference` blocks; OpenAI Responses uses message-anchored `additional_tools` or client `tool_search_output`; unsupported providers fall back to the normal full tool list. This preserves the old cached prefix because the definition is introduced at the historical result boundary. See [`deferred-tools.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/utils/deferred-tools.ts), [`anthropic-messages.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/api/anthropic-messages.ts), and [`openai-responses-shared.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/api/openai-responses-shared.ts).

This is a late-binding transport primitive, not a complete zero-cost catalog. An already-visible extension tool must decide what to activate, and Pi still prints every visible skill name, description, and location in the system prompt. See [`system-prompt.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/agent/src/harness/system-prompt.ts).

Worth porting later: a durable “capability became available at event X” record and provider-specific inline expansion. For v0, retaining a proxy inside Python avoids provider-specific schema installation entirely.

### 7. Claude API: confirms both principles, but does not remove host-side payload cost

Anthropic's current tool-search API accepts up to 10,000 deferred tools. The client still sends every definition on every request, but the service excludes deferred definitions from Claude's initial context, returns matches as `tool_reference` blocks, expands them inline, and leaves the cached prefix untouched. Anthropic reports that a representative multiserver catalog can consume about 55,000 definition tokens and that tool search commonly cuts this by over 85 percent. See the official [tool search documentation](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool) and [prompt caching documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).

The current API's programmatic-tool calling is Python, not JavaScript: Claude writes Python in its code-execution container, tool results resume inside that program, intermediate results are excluded from Claude's context, and only the final code result is counted as model input/output. Anthropic reports roughly 38 percent fewer billed input tokens on one 75-tool benchmark, but also reports about 8 percent higher cost for workloads with only one or two sequential calls. See the official [programmatic tool calling documentation](https://platform.claude.com/docs/en/agents-and-tools/tool-use/programmatic-tool-calling).

This independently validates SERAPH's central architecture: programmatic reduction wins when there are many calls or large reducible results, and it has fixed overhead that should not be forced onto trivial calls.

## Artifact contract SERAPH actually needs

None of the inspected open harnesses combines Prime's live object namespace, Codex/OpenCode's explicit output boundary, and a durable opaque content store. SERAPH should.

### Minimal v0 representation

```text
ArtifactRef {
  id            opaque stable ID
  digest        BLAKE3 or SHA-256 of stored bytes
  size_bytes
  media_type
  kind          text | json | table | binary | log
  created_by    execution/capability call ID
}
```

The model receives only a compact representation such as `ArtifactRef(log_01, 18.7 MiB, text/plain)`. Absolute paths, credentials, and provider response objects do not enter the transcript.

Minimum operations:

- `artifacts.put(value)` and automatic spill from capability calls;
- `meta(ref)`, `read(ref, offset, limit)`, and `grep(ref, query, limit)`;
- Python-native parsing/reduction after bounded reads;
- `emit(value)` as the only intentional model projection.

Use one SQLite database for capability metadata, artifact metadata, provenance, and reference/retention state, with blobs stored by digest under the SERAPH data directory. SQLite FTS5 can index capability names/descriptions and text artifacts without adding a separate search service. The database is authoritative; Python holds only proxy IDs.

An automatic capability call should behave as follows:

1. Host executes the operation and measures bytes while streaming.
2. Small structured output becomes a Python value.
3. Large output is written once to the blob store and Python receives an `ArtifactRef`.
4. A bounded preview is available on request, not automatically emitted.
5. Parallel consumers share the same immutable ref.
6. Snapshotting serializes the ref ID, never a live socket, credential, or blob.

This subsumes Kimi/Qwen/OpenCode spill files while preserving Qwen's “persist once, reuse the producer artifact” rule.

## Smallest Rust/Python implementation

### Model-facing surface

Keep one stable freeform tool, for example `python`, whose fixed description explains only:

- the namespace persists;
- `caps.search`, `caps.load`, `artifacts`, and `emit` are pre-imported;
- tool/capability results remain out of model context unless emitted;
- independent awaits may run with `asyncio.gather`.

Do not list installed capability, MCP-server, or skill names in that description. Ordinary direct tools can be added later only when evidence shows they improve accuracy enough to repay their permanent schema cost.

### Rust host modules

```text
KernelSupervisor   CPython process/cell protocol, snapshot and recovery
CapabilityCatalog  SQLite/FTS index, schema records, catalog generation
CapabilityHost     generic call(id, operation, args), connection pooling
ArtifactStore      digest blobs, metadata, read/grep, retention
Projection         emit-only model result plus bounded errors
UsageLedger        provider usage, execution bytes, estimates and hashes
```

These should be deep interfaces, not one module per donor project. Capability implementations may be loaded dynamically, but they all cross the same generic host RPC.

### Python bootstrap

The Python package should stay tiny:

```python
class Capabilities:
    async def search(self, query: str, limit: int = 5): ...
    async def load(self, capability_id: str): ...

class CapabilityProxy:
    def __getattr__(self, operation: str): ...  # async generic host call

class Artifacts:
    async def meta(self, ref): ...
    async def read(self, ref, offset=0, limit=65536): ...
    async def grep(self, ref, query, limit=50): ...

def emit(value): ...
```

`caps.load()` caches the proxy in Python and the connection/schema generation in Rust. If the host restarts, the proxy's stable capability ID rehydrates its host state. Methods can expose signatures through Python introspection after selection without adding provider tool schemas.

### Parallelism

Python uses `asyncio.gather`; Rust dispatches calls as Tokio tasks under per-capability and session semaphores. Results settle into Python variables or artifact refs. Only the final emitted reduction becomes one tool result. This gives Codex/OpenCode-style parallel nested execution without a second JS runtime.

### Skills

Index skill frontmatter in the same host catalog. `caps.search(kind="skill")` returns a few candidates; loading a skill returns its body as an artifact or bounded Python string. Never render the global skill list into the system prompt. This is the clearest area where SERAPH can exceed Prime, OpenCode, and Pi immediately.

## Prompt caching and token accounting

### Cache rule

Keep the following byte-stable across turns whenever policy has not changed:

1. model-facing tool definitions;
2. system/bootstrap instructions;
3. provider cache key/session affinity;
4. the old transcript prefix.

Capability search results, schemas, and clients live in Python/host state, not in `tools[]`. This avoids Qwen's declaration-list rewrite and avoids even the inline selected-schema tokens used by provider-native tool search. A compact search result costs tokens only if the program deliberately emits it.

### Honest accounting

The inspected harnesses normalize aggregate provider counters, not exact semantic prompt-section attribution:

- Kimi records non-cached input, output, cache read, and cache creation in [`usage.ts`](https://github.com/MoonshotAI/kimi-code/blob/630a11db51ab0ac422cae6a10580b62c1ae8e05f/packages/kosong/src/usage.ts).
- Codex records input, cached input, cache-write input, output, reasoning output, and totals in [`protocol.rs`](https://github.com/openai/codex/blob/2c3bf4ea793aa5c590932553d242a287380e9cec/codex-rs/protocol/src/protocol.rs).
- Prime records input/output/cache read/cache write and cost in [`types.ts`](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/ai/src/types.ts).
- Pi normalizes the same provider categories in its [`Usage` contract](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/types.ts).

SERAPH should call these values **provider exact** only at the aggregate request level. Per-section figures should be labeled **tokenizer estimate** unless a provider returns them explicitly. Provider framing, tool serialization, hidden service transformations, and model-specific tokenizers make an exact local decomposition impossible in the general case.

Record for every request:

- hash and byte length of system, tool-schema, history, memory, and emitted-artifact projections;
- local tokenizer estimate for each section and tokenizer identity;
- provider input/output/cache-read/cache-write/reasoning counters;
- capability calls, raw bytes, retained bytes, emitted bytes, and artifact reuse;
- child-agent usage separately from parent context usage.

Measure dynamic disclosure with controlled A/B request pairs, not by pretending an estimated section sum is provider truth. The profiler can then truthfully say, for example: “7,840 estimated schema tokens stayed host-side; provider reported 416 new input tokens; 18.7 MiB raw output produced a 612-token projection.”

## Licensing and reuse boundary

All inspected open-source donors are permissively licensed: Kimi Code, Prime Agent, OpenCode, and Pi are MIT; Codex and Qwen Code are Apache-2.0. Direct copying is legally possible only with the relevant copyright/license notices and Apache notice/patent obligations preserved. This research is not legal advice.

For a standalone Rust repository, behavioral reimplementation is preferable:

- port Codex's host-side retrieval and explicit output boundary as behavior;
- port Prime's Python proxy ergonomics and snapshot rules as behavior;
- port Qwen's persist-once/finalization invariants as behavior;
- port Pi's event-anchored capability activation as a later provider adapter feature;
- reuse no Claude Code implementation: the cited behavior is a proprietary hosted/API feature documented publicly, not donor source.

OpenCode's `packages/codemode` could technically be reused under MIT, but doing so would pull TypeScript/Bun and a second language runtime into a Rust/CPython v0. It is a reference, not the smallest dependency.

## Acceptance criteria for SERAPH v0

1. Installing 10,000 never-used capabilities changes the steady model prompt by **zero bytes**.
2. One selected capability costs only the intentionally emitted search result/instructions, never the global catalog.
3. Loaded proxies survive kernel cells; checkpoint restoration rehydrates IDs without serializing connections or credentials.
4. Fifty nested calls can execute locally and produce one bounded model result.
5. Raw nested outputs never enter conversation history by default.
6. Large results are stored once and shared by opaque reference.
7. The tool/system prefix remains byte-identical when capabilities are searched or loaded.
8. Provider-reported totals and local estimates are visibly distinguished.
9. A missing/reconfigured capability fails with an explicit generation/version error rather than silently calling a different implementation.
10. The direct-call path remains available for trivial operations where programmatic overhead is not economical.

## Bottom line

The best Frankenstein is:

- **Prime** for persistent Python and retained lazy clients;
- **Codex** for host-side retrieval and raw-result-to-explicit-output separation;
- **Qwen** for persist-once output finalization and resumable deterministic work;
- **OpenCode** for supervised in-program parallelism and managed output retention;
- **Pi/Claude** for cache-preserving event-anchored late definitions when a provider adapter eventually needs them;
- **SERAPH's own artifact store and Python-internal discovery** for the part none of them supplies.

That combination makes unused capabilities genuinely free in model context, keeps the provider prefix stable, avoids a second embedded language runtime, and spends tokens only on selected metadata and emitted reductions.
