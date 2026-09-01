# SERAPH

Stateful Execution Runtime for Agentic Programmatic Harness.

SERAPH is an independent Rust and Ratatui agent harness that programs a persistent Python execution environment while keeping capabilities, artifacts, workflows, and agent coordination outside the model context until needed.

The first executable spine is available now:

```sh
cargo run -- exec \
  'matches = await caps.search("file")' \
  'fs = await caps.load(matches[0]["id"])' \
  'text, meta = await asyncio.gather(fs.read_text(path="README.md"), fs.metadata(path="README.md"))' \
  'emit({"heading": text["text"].splitlines()[0], "bytes": meta["size_bytes"]})'
```

Python state persists between cells. Capabilities execute in Rust and remain outside model context until code explicitly calls `emit()`.

Set `SERAPH_PYTHON` to select a Python 3.11+ interpreter.
