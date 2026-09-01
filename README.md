# SERAPH

Stateful Execution Runtime for Agentic Programmatic Harness.

SERAPH is a Rust agent harness using Grok Build's actual Apache-2.0 pager UI with a SERAPH ACP backend. It programs a persistent Python execution environment while keeping capabilities, artifacts, workflows, and agent coordination outside the model context until needed.

Install on macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/Matari-Audio/SERAPH/main/install.sh | sh
```

The installer downloads a prebuilt, checksummed release for Apple Silicon,
Intel Mac, Linux x64, or Linux ARM64. It does not require Rust, Cargo, Git, npm,
or a source checkout. Node.js 22.19+ and Python 3.11+ remain runtime requirements.
Re-run the same command to update atomically; previous releases remain under
`~/.local/share/seraph/releases`. For development, use `npm ci && cargo run`.

Prime/Pi's production `ModelRuntime` owns ChatGPT login, locked credential storage, and token refresh in `~/.seraph/auth.json`. SERAPH passes only short-lived access tokens to an isolated installed `codex app-server`; Codex never receives Pi's refresh token. On first launch, press `l` on Grok's welcome screen and complete Pi's browser login. Grok's composer, dashboard, settings, themes, shortcuts, animations, and terminal renderer are used directly rather than recreated.

SERAPH's dynamic tools provide the persistent Python kernel, reversible exact edits, shared SQLite task coordination, parallel child agents, interruption, follow-ups, waits, and inter-agent mailboxes. Tool execution is projected into Grok's native tool-call cards instead of dumping raw output into the conversation.

The model can call SERAPH's persistent Python kernel as `seraph.python`. The standalone kernel path remains available:

```sh
cargo run -- exec \
  'matches = await caps.search("file")' \
  'fs = await caps.load(matches[0]["id"])' \
  'text, meta = await asyncio.gather(fs.read_text(path="README.md"), fs.metadata(path="README.md"))' \
  'emit({"heading": text["text"].splitlines()[0], "bytes": meta["size_bytes"]})'
```

Python state persists between cells. Capabilities execute in Rust and remain outside model context until code explicitly calls `emit()`.

Set `SERAPH_PYTHON` to select a Python 3.11+ interpreter.
