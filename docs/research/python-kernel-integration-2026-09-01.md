# Python kernel integration for SERAPH

**Date:** 2026-09-01
**Question:** Which Python runtime should a standalone Rust/Ratatui SERAPH use for a persistent, async, token-efficient execution kernel?

## Recommendation

Use **CPython in a dedicated subprocess**, extracting the small MIT-licensed execution core from Prime Agent and replacing Prime's TypeScript manager with a Rust host.

The v0 boundary should be:

```text
model -> one compact `python` capability -> Rust kernel manager
                                           | framed, versioned RPC
                                           v
                                      CPython runtime shim
                                      - persistent __main__ dict
                                      - one asyncio loop
                                      - top-level await
                                      - streamed output/display
                                      - host_request/reply
                                      - per-name dill snapshots
```

Do not adopt Prime's MCP, CLI, Bash, provisioning, or TypeScript host code. The reusable donor is essentially `repl.py`, its protocol contract, and its failure-handling ideas. Prime's runtime package declares Python 3.11+, `mcp`, and `tyro`, but the executor itself is almost entirely standard library; `dill` is loaded only when snapshotting. SERAPH can remove the Prime-specific imports and make `dill` the sole optional kernel dependency. [Prime runtime package](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/pyproject.toml) [Prime REPL source](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.py)

This is the best v0 choice because it simultaneously provides normal CPython package compatibility, persistent async state, strong cancellation escalation, streamed results, cheap Rust callbacks, and restart after a kernel crash. PyO3 and RustPython place the interpreter in SERAPH's failure domain. Jupyter adds mature remote/notebook interoperability but no standard snapshot or typed host-capability protocol.

## Decision matrix

| Option | Persistent async namespace | Rust callbacks | Hard cancellation / crash recovery | Normal Python packages | Snapshot basis | Build and runtime cost | v0 verdict |
|---|---|---|---|---|---|---|---|
| Prime-derived CPython subprocess | Already implemented | Duplex `host_request` / `host_reply` | Best: interrupt, then terminate/restart | Native CPython and venvs | Existing per-name `dill` | Small Python shim plus Rust process manager | **Choose** |
| New subprocess framed RPC | Must recreate Prime's hard parts | Straightforward | Best | Native CPython | Must design | Small, but needless reinvention if Prime code is reusable | Merge with Prime extraction |
| Jupyter / ipykernel | Mature IPython namespace and top-level await | Custom Comm target or custom messages | Mature process interrupt/restart; state still lost | Native CPython | Not standardized | Five ZMQ channels, connection/auth machinery, large Python stack | Optional later adapter |
| PyO3 embedded CPython | Easy `PyDict`; async REPL is custom work | Best in-process API | Weak: native hang/crash shares Rust process | Native CPython, subject to linked distribution | Still needs `dill` or custom logic | CPython linking, cross-build and shipping work | Reject for v0 |
| RustPython embedded | Easy VM scope | Native Rust modules | No process boundary; custom cancellation | Pure-Python subset is plausible; drop-in compatibility is unproven | No Prime/dill guarantee | Large Rust graph, Rust 1.95, evolving runtime | Reject for general kernel |
| xeus-python | Mature Jupyter kernel | Custom Jupyter extension | Process-level | Native CPython | Not standardized | CMake/C++/xeus/ZeroMQ/pybind/conda stack | Wrong dependency direction |

No wire encoding choice materially changes model token cost: RPC bytes are outside the prompt. JSON is preferable initially because it is inspectable and already proven by Prime; output projection, artifact handles, and schema disclosure determine token usage.

## What to transplant from Prime

Prime's protocol version 3 runs one persistent `__main__` module on one `asyncio` event loop. Cells are compiled with `ast.PyCF_ALLOW_TOP_LEVEL_AWAIT`; a trailing expression is compiled separately, awaited when it yields a coroutine, rendered with `repr`, and stored as `_`. Background tasks survive between cells. [Protocol and execution contract](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.md) [Compiler and executor](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.py#L477-L547)

Transplant these seams:

- `_compile_cell`, `_run_codes`, and the real `types.ModuleType("__main__")` namespace.
- The persistent event loop and FIFO execution/state-operation queue.
- `host_request(data)`: it emits an id, stores an `asyncio.Future`, and waits for a matching reply. Replies bypass the FIFO, because queueing a reply behind the waiting execute request deadlocks.
- The private protocol file descriptor and separate stdout/stderr capture. Python writes carry a cell id through `contextvars`; C extensions, subprocesses, and raw file-descriptor writes are captured as unattributed stream events and cannot corrupt control frames.
- Request-correlated `stdout`, `stderr`, rich `display`, trailing `result`, structured `error`, and exactly one terminal `done` event.
- Targeted interrupt bookkeeping, including interrupts that arrive before a queued cell begins or during its final `repr`/output drain.
- Owner watchdog, EOF shutdown, protocol handshake/version check, and host-side respawn/restore/rebootstrap logic. Prime's manager starts `python -m rlm.repl`, rejects malformed frames, replaces a corrupted child, restores once, and reruns bootstrap code. [Prime Rust-host analogue source seam](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/packages/coding-agent/src/core/kernel/repl-manager.ts#L252-L545)

Do not transplant:

- `rlm.bash`, MCP, skills, agent recursion, provider access, or Prime's package bootstrap.
- TypeScript lifecycle code verbatim. Re-express its state machine in Rust and keep Python ignorant of credentials and capability implementations.
- Prime-specific snapshot file naming and its two-file commit behavior without strengthening it.

Prime is MIT licensed, so source reuse is compatible with a permissive SERAPH codebase, subject to retaining the MIT copyright/license notice for copied or substantially derived code. [Prime license](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/LICENSE)

## Rust host contract

The Rust side should own the child process, session paths, capability registry, request validation, artifacts, token projections, snapshot generations, timeouts, and usage accounting. The Python side should own only live Python objects and execution semantics.

Use a small, versioned envelope:

```text
host -> kernel: hello, execute, interrupt, host_reply, snapshot, restore,
                list_names, shutdown
kernel -> host: ready, stdout, stderr, display, result, host_request,
                snapshot_result, error, done
```

Every request and event needs a request id. `host_request` additionally needs its own call id and a typed capability/method name. Rust should validate argument/result size and schema before dispatch. Capability calls should return JSON-sized values or artifact handles; large bytes, logs, tables, ASTs, and tool transcripts stay in Rust's artifact store.

Prime uses newline-delimited JSON on a private stdout duplicate. Preserve that for v0, but add a maximum frame length, maximum JSON nesting/decode limits, and bounded per-request output. A later length-prefixed binary transport would improve large binary transfer, not model token efficiency; artifacts are the correct binary path.

The Python bootstrap should expose only a tiny stable surface, for example `cap.call(...)`, `artifact.get(...)`, `display(...)`, and perhaps generated ergonomic proxy objects. A single cell can then call many Rust capabilities locally and reduce the result before the model receives anything.

## Cancellation and failure semantics

Prime's Unix path sends SIGINT to the loop thread. It raises `KeyboardInterrupt` into synchronous Python execution when possible and cancels an await-suspended task otherwise. On Windows, where `signal.pthread_kill` is unavailable, it can cancel async work but cannot reliably break synchronous blocked Python. Prime documents this limitation explicitly. [Prime interrupt contract](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.md#interrupt)

SERAPH should define three levels:

1. **Cooperative:** target the active request; cancel its asyncio task or deliver SIGINT.
2. **Process interrupt:** send the platform-appropriate control signal and wait a short bounded grace period.
3. **Hard recovery:** terminate the child/process group, mark the active request `outcome_unknown`, start a clean kernel, restore the last committed snapshot, then rebootstrap capability proxies.

The third level is why subprocess CPython wins. `PyErr_SetInterrupt()` in embedded CPython only simulates SIGINT and is acted on when Python next checks signals; it is not a hard stop for a native extension or deadlock. [CPython interrupt API](https://docs.python.org/3/c-api/exceptions.html#c.PyErr_SetInterrupt) In-process async exception injection is not a safe general recovery mechanism, and a CPython or C-extension crash terminates the Rust host.

After EOF, malformed protocol, unexpected exit, or hard kill, never claim the interrupted cell failed cleanly: filesystem/network/tool side effects may already have happened. Report `outcome_unknown` and restore only kernel memory. Capability implementations need their own idempotency keys or effect journal if replay is ever added.

## Snapshots and recovery

Prime serializes each eligible top-level name independently with `dill` recurse mode. One unpickleable or oversized value does not block the others; the manifest records saved, skipped, pruned, byte count, Python version, and timestamp. Restore deserializes each value independently into a staging dict before applying it. This is a strong v0 basis. [Prime snapshot implementation](https://github.com/PrimeIntellect-ai/prime-agent/blob/9f5edc192cfe3d4737205a2f551d2b6b6e34fe09/prime-agent-runtime/src/rlm/repl.py#L608-L845)

Limitations SERAPH must make explicit:

- `dill` is best-effort state capture, not a transaction over live Python objects. Threads and background tasks can mutate values during serialization.
- Open files, sockets, subprocesses, event loops, locks, native handles, imported modules, and capability proxies should be skipped and reconstructed by bootstrap.
- Code/package/Python-version changes can invalidate old pickles. Store runtime version, Python ABI/version, bootstrap hash, environment identity, and schema version with every generation.
- Prime atomically replaces the payload and manifest **individually**, but not as one pair. A crash or failure between the two replacements can mismatch them; it also does not establish a durable `fsync` boundary.

SERAPH should publish snapshots by generation: write payload and manifest into a new generation directory, flush as appropriate, then atomically replace one small `CURRENT` pointer. The manifest must hash the payload. Recovery accepts only a generation whose manifest, payload hash, runtime/Python compatibility, and completion marker agree. Keep the prior generation until the new pointer commits.

Snapshot after a successful cell or explicit checkpoint, not during arbitrary execution. The host should record the snapshot generation associated with a completed request. A cell followed by an uncommitted crash restores the preceding generation and reports the cell outcome as unknown.

## Why not Jupyter as the internal protocol

Jupyter provides a mature process boundary, IPython top-level await, rich display, completion/introspection, interrupt, remote kernels, and notebook compatibility. `ipykernel` delegates execution to IPython's `should_run_async` / `run_cell_async`, which provides established auto-await behavior. [ipykernel execution](https://github.com/ipython/ipykernel/blob/dfb3467ba8939b7183d0fd6367ca7d538858b871/ipykernel/ipkernel.py#L374-L475) [IPython async cells](https://github.com/ipython/ipython/blob/19f9ae0a863c21cff52fa07c74b18fc5b482d9c3/IPython/core/interactiveshell.py#L3324-L3445)

Its cost is substantial for a local one-host/one-kernel runtime:

- A connection or registration file, authentication key, message signatures, and five ZeroMQ channels: shell, IOPub, stdin, heartbeat, and control. [Jupyter kernel connection](https://github.com/jupyter/jupyter_client/blob/978361b3785dcd9cba6c733f4555e833e88fc0df/docs/kernels.rst#connection-files) [Client channels](https://github.com/jupyter/jupyter_client/blob/978361b3785dcd9cba6c733f4555e833e88fc0df/jupyter_client/client.py#L75-L95)
- Multipart messages with routing identities, delimiter, HMAC, four JSON dictionaries, and optional buffers. [Jupyter wire protocol](https://github.com/jupyter/jupyter_client/blob/978361b3785dcd9cba6c733f4555e833e88fc0df/docs/messaging.rst#the-wire-protocol)
- `ipykernel`, IPython, `jupyter_client`, PyZMQ, Tornado, traitlets, and related runtime dependencies.
- No standardized namespace checkpoint. Kernel restart restarts the process; SERAPH still needs its own persistence layer.
- No typed request/reply capability bridge. Jupyter Comms are symmetric one-way messages whose `data` schema is defined by each Comm pair, so SERAPH would recreate call ids, errors, cancellation, and typing on top. [Jupyter Comms](https://github.com/jupyter/jupyter_client/blob/978361b3785dcd9cba6c733f4555e833e88fc0df/docs/messaging.rst#custom-messages)

Jupyter is the right **later adapter** if SERAPH needs remote kernels, existing kernelspecs, notebooks, widgets, or third-party Jupyter frontends. The maintained Rust route is `runtimed`: `jupyter-protocol` supplies message types and `jupyter-zmq-client` supplies a native async client over the Rust `zeromq` crate, with Tokio or async-dispatcher runtime features. [runtimed overview](https://github.com/runtimed/runtimed/blob/4ec1526a5624de7eb632d996759e83dd3f3d9bac/README.md) [jupyter-zmq-client dependencies](https://github.com/runtimed/runtimed/blob/4ec1526a5624de7eb632d996759e83dd3f3d9bac/crates/jupyter-zmq-client/Cargo.toml)

The relevant Jupyter components are BSD-3-Clause. `jupyter-protocol` 2.0.2 and `jupyter-zmq-client` 1.0.1 are also BSD-3-Clause. These are legally easy to add later, but operationally much larger than SERAPH's required internal contract. [jupyter_client license](https://github.com/jupyter/jupyter_client/blob/978361b3785dcd9cba6c733f4555e833e88fc0df/LICENSE) [runtimed crate licenses](https://github.com/runtimed/runtimed/blob/4ec1526a5624de7eb632d996759e83dd3f3d9bac/crates/jupyter-protocol/Cargo.toml)

## Why not embed CPython with PyO3

PyO3 gives excellent direct callbacks: Rust can expose `#[pyfunction]` and `#[pymodule]`, and `pyo3-async-runtimes` converts Rust futures to Python awaitables and Python coroutines to Rust futures. Its current async integration supports Tokio and Python's default asyncio loop. [PyO3 async guide](https://github.com/PyO3/pyo3/blob/1c0a5202b2c29928ce4c547f00dce293b55e0b95/guide/src/async-await.md) [pyo3-async-runtimes](https://github.com/PyO3/pyo3-async-runtimes/blob/4f53cebd55a65b973c6c437d10599356a3ad5e82/README.md)

It does not supply a model-facing REPL. SERAPH would still need the persistent namespace, top-level-await compiler, result/display capture, event-loop ownership, background-task behavior, output attribution, snapshots, and cancellation semantics already present in Prime.

Embedding also worsens recovery:

- Python and native-extension crashes share the Ratatui host process.
- A stuck native extension cannot be escalated to process kill without killing SERAPH.
- PyO3 warns that `with_embedded_python_interpreter` should be called only once per process because many C modules do not initialize correctly in a second interpreter; it initializes and finalizes the process interpreter. [PyO3 lifecycle warning](https://github.com/PyO3/pyo3/blob/1c0a5202b2c29928ce4c547f00dce293b55e0b95/src/interpreter_lifecycle.rs#L27-L85)

Shipping is also coupled to a chosen CPython build. Dynamic embedding links `libpython` and requires distributing/finding that shared library. Static embedding is Unix-oriented, lacks first-class PyO3 support, and requires compatible C/Rust toolchains and exported symbols for compiled extensions. Cross-compiling an embedded interpreter needs target `libpython` and sysconfig data such as `PYO3_CROSS_LIB_DIR`. [PyO3 embedding and cross-compilation](https://github.com/PyO3/pyo3/blob/1c0a5202b2c29928ce4c547f00dce293b55e0b95/guide/src/building-and-distribution.md#embedding-python-in-rust)

PyO3 is MIT OR Apache-2.0. `pyo3-async-runtimes` 0.29 is Apache-2.0 and adds runtime-bridge complexity. Revisit PyO3 only for an opt-in low-latency trusted mode after the subprocess semantics are stable; do not make it the only kernel. [PyO3 package metadata](https://github.com/PyO3/pyo3/blob/1c0a5202b2c29928ce4c547f00dce293b55e0b95/Cargo.toml) [async-runtimes package metadata](https://github.com/PyO3/pyo3-async-runtimes/blob/4f53cebd55a65b973c6c437d10599356a3ad5e82/Cargo.toml)

## Why not RustPython

RustPython exposes a clean Rust embedding API: build an interpreter, create/reuse a scope, compile and execute code, and register native Rust modules with `#[pymodule]`. [Embedding example](https://github.com/RustPython/RustPython/blob/5494a3acbc57a1dce759dace973d7e9653325df8/examples/hello_embed.rs) [Rust callback example](https://github.com/RustPython/RustPython/blob/5494a3acbc57a1dce759dace973d7e9653325df8/examples/call_between_rust_and_python.rs)

It is not the right general-purpose agent kernel today:

- The project describes itself as still in development and “not totally production-ready.” Its goal is a clean independent implementation rather than CPython compatibility hacks. [RustPython status](https://github.com/RustPython/RustPython/blob/5494a3acbc57a1dce759dace973d7e9653325df8/README.md)
- `pip` can be installed with SSL support, but that does not establish compatibility with the large CPython C-extension ecosystem that agents expect. The current tree has an optional `capi` effort; relying on arbitrary NumPy/Pandas/PyTorch/database/browser wheels would require separate compatibility proof.
- SERAPH would need to recreate Prime's persistent top-level-await semantics, output attribution, snapshot format, event-loop integration, and robust cancel/restart boundary.
- Embedded RustPython still shares the Rust process. Rust ownership eliminates CPython linking, not interpreter bugs or unbounded user code.
- The current workspace is version 0.5.0 with Rust 1.95 and a broad VM/compiler/stdlib dependency graph; default binary features include threading, stdlib, importlib, SSL, and host environment. [RustPython Cargo features](https://github.com/RustPython/RustPython/blob/5494a3acbc57a1dce759dace973d7e9653325df8/Cargo.toml)

RustPython code is MIT licensed. It remains attractive for a future restricted, portable, or WASM kernel where pure-Python compatibility is acceptable; it should not define SERAPH's default Python experience. [RustPython package metadata](https://github.com/RustPython/RustPython/blob/5494a3acbc57a1dce759dace973d7e9653325df8/Cargo.toml)

## Other maintained options

`xeus-python` is a maintained BSD-3-Clause native Jupyter kernel, but its documented source build requires CMake, xeus/xeus-zmq, nlohmann_json, pybind11, pybind11_json, and xeus-python-shell; its maintainers recommend conda-forge and describe PyPI wheels as experimental. It provides Jupyter benefits while moving SERAPH away from its Rust core and still lacks SERAPH checkpoint/capability semantics. [xeus-python README and dependencies](https://github.com/jupyter-xeus/xeus-python/blob/ab188126dad0b559315228f06f4ad3135dd31b0e/README.md)

Astral's `python-build-standalone` is not a kernel but is a useful **distribution input**. It publishes highly redistributable CPython installations for Windows, macOS, and Linux; `uv` can manage those distributions. Dynamic/install-only builds can load compiled extensions, while fully static builds cannot load arbitrary `.so` extensions. SERAPH should initially accept a configured Python 3.11+ executable/venv, then optionally provision a pinned standalone CPython without changing the kernel protocol. [python-build-standalone overview](https://github.com/astral-sh/python-build-standalone/blob/981b6fdf9f44c970be399eddbcb1ab0ecf9f1266/docs/index.rst) [distribution variants](https://github.com/astral-sh/python-build-standalone/blob/981b6fdf9f44c970be399eddbcb1ab0ecf9f1266/docs/running.rst)

Bundling a Python distribution creates a separate license inventory: CPython and bundled libraries have their own licenses, and the standalone distribution metadata/archive carries those texts. Its docs call out GPL-3 components such as readline and GDBM, with GDBM disabled globally. This must be reviewed when SERAPH ships Python, not when it merely launches a user-selected interpreter. [Standalone Python licensing](https://github.com/astral-sh/python-build-standalone/blob/981b6fdf9f44c970be399eddbcb1ab0ecf9f1266/docs/running.rst#licensing)

## Token-facing ergonomics

The best kernel is the one the model barely has to discuss.

- Expose one stable `python` execution schema, not Python's package inventory, namespace contents, or host capability schemas on every inference.
- Load capability proxies into the live namespace after discovery. A later cell can reuse `github`, `fs`, or `agents` without resending their full tool schemas.
- Return stream/result/error summaries under bounded budgets. Store overflow and rich/binary output as artifacts and return handles plus compact metadata.
- Keep Python variables in the kernel. Do not echo assignments, large `repr`s, or namespace snapshots into conversation.
- Let one generated Python program invoke many host capabilities, parallelize with asyncio, aggregate results, and return only the reduction.
- Snapshot names and restore diagnostics are control-plane data; inject them only when recovery affects the current reasoning.

Subprocess IPC adds microseconds-to-milliseconds, but saves far more by eliminating inference/tool round trips and by keeping raw data outside context. PyO3's lower call overhead is not a token advantage. Jupyter's internal verbosity is also not itself a token cost if hidden, but its larger implementation surface buys no v0 model-facing capability.

## Acceptance boundary for the donor

Before copying Prime code into product code, pin the donor commit, preserve its MIT notice, and record a source-to-source inventory. The minimal extracted Python package should contain only:

- the REPL executor/protocol module;
- a SERAPH-owned capability bridge;
- optional snapshot support (`dill`);
- no Prime branding, provider logic, MCP, Bash helper, or agent implementation.

The Rust manager should be written against a SERAPH protocol specification, not against incidental Python implementation details. That allows a later Jupyter, PyO3, or RustPython backend to implement the same logical kernel interface without changing the model-facing tool.

## Bottom line

**Use Prime's Python brain, not Prime's host.** A Prime-derived CPython sidecar plus a Rust lifecycle/capability/artifact manager is the smallest route to the desired semantics and the only option here that combines ordinary Python packages with a recoverable failure boundary. Strengthen framing, snapshot generations, output limits, and outcome-unknown semantics; defer Jupyter to interoperability, PyO3 to an optional trusted mode, and RustPython to constrained portable execution.

## Source snapshot

Research inspected these primary-source revisions on 2026-09-01:

- Prime Agent `9f5edc192cfe3d4737205a2f551d2b6b6e34fe09`
- PyO3 `1c0a5202b2c29928ce4c547f00dce293b55e0b95`
- pyo3-async-runtimes `4f53cebd55a65b973c6c437d10599356a3ad5e82`
- RustPython `5494a3acbc57a1dce759dace973d7e9653325df8`
- jupyter_client `978361b3785dcd9cba6c733f4555e833e88fc0df`
- IPython `19f9ae0a863c21cff52fa07c74b18fc5b482d9c3`
- ipykernel `dfb3467ba8939b7183d0fd6367ca7d538858b871`
- runtimed `4ec1526a5624de7eb632d996759e83dd3f3d9bac`
- xeus-python `ab188126dad0b559315228f06f4ad3135dd31b0e`
- python-build-standalone `981b6fdf9f44c970be399eddbcb1ab0ecf9f1266`

Context7 was also queried for current PyO3 embedding and `pyo3-async-runtimes` API behavior; pinned upstream source above is used for durable citations.
