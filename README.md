# SERAPH

Stateful Execution Runtime for Agentic Programmatic Harness.

SERAPH is an independent Rust and Ratatui agent harness that programs a persistent Python execution environment while keeping capabilities, artifacts, workflows, and agent coordination outside the model context until needed.

Run the Codex-backed terminal chat:

```sh
cargo run
```

SERAPH uses the installed `codex app-server` for ChatGPT authentication, model discovery, and streaming. Existing Codex login state is reused without exposing tokens to SERAPH; when signed out, press `L` to start the official browser login. Use `<` and `>` on an empty composer to change reasoning effort, `↓` or `Ctrl+G` to open All Agents, `?` for help, and `Ctrl+C` to quit.

The dock above the composer uses Grok Build's Dock V2 renderer and GrokNight primitives. Press `Tab` on an empty composer to focus it, navigate with arrows, and press `Enter` to collapse a section or open a subagent. Task state is read from the project SQLite board, so claims and completions made by child-agent processes appear without entering the conversation context.

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
