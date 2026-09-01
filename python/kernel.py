"""SERAPH's persistent CPython execution sidecar.

Derived from Prime Agent's ``src/rlm/repl.py`` at commit
9f5edc192cfe3d4737205a2f551d2b6b6e34fe09. See THIRD_PARTY_NOTICES.md.
"""

from __future__ import annotations

import ast
import asyncio
import codecs
import contextvars
import inspect
import io
import json
import linecache
import os
import platform
import struct
import sys
import threading
import time
import traceback
import types
import uuid
from typing import Any

PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 1024 * 1024

_protocol_fd = -1
_write_lock = threading.Lock()
_loop: asyncio.AbstractEventLoop | None = None
_pending_host: dict[str, asyncio.Future[dict[str, Any]]] = {}
_host_closed = False
_current_cell: contextvars.ContextVar[str | None] = contextvars.ContextVar(
    "_current_cell", default=None
)


def _send(event: dict[str, Any]) -> bool:
    accepted = True
    try:
        payload = json.dumps(
            event, separators=(",", ":"), allow_nan=False, default=repr
        ).encode()
    except BaseException:
        accepted = False
        payload = b""
    if len(payload) > MAX_FRAME_BYTES or not accepted:
        accepted = False
        payload = json.dumps(
            {
                "event": "error",
                "id": event.get("id"),
                "ename": "ProtocolFrameRejected",
                "evalue": f"event could not fit in {MAX_FRAME_BYTES} bytes",
                "traceback": [],
            },
            separators=(",", ":"),
        ).encode()
    frame = struct.pack(">I", len(payload)) + payload
    with _write_lock:
        view = memoryview(frame)
        while view:
            view = view[os.write(_protocol_fd, view) :]
    return accepted


def emit(value: Any) -> None:
    """Project one JSON value to the model-facing result."""
    json.dumps(value, allow_nan=False)
    if not _send({"event": "display", "id": _current_cell.get(), "data": value}):
        raise ValueError("emitted value exceeds the protocol frame limit")


async def host_request(method: str, params: dict[str, Any]) -> Any:
    if _loop is None:
        raise RuntimeError("kernel is not serving")
    if _host_closed:
        raise RuntimeError("host connection is closed")
    request_id = uuid.uuid4().hex
    future: asyncio.Future[dict[str, Any]] = _loop.create_future()
    _pending_host[request_id] = future
    try:
        if not _send(
            {
                "event": "host_request",
                "id": request_id,
                "data": {"method": method, "params": params},
            }
        ):
            raise ValueError("host request exceeds the protocol frame limit")
        reply = await future
    finally:
        _pending_host.pop(request_id, None)
    if not reply.get("ok"):
        raise RuntimeError(reply.get("error", "host request failed"))
    return reply.get("value")


def _fail_pending_host_requests() -> None:
    global _host_closed
    _host_closed = True
    for future in _pending_host.values():
        if not future.done():
            future.set_exception(RuntimeError("host connection is closed"))


class CapabilityProxy:
    def __init__(self, handle: dict[str, Any]) -> None:
        self._handle = handle
        self._operations = frozenset(handle["operations"])

    def __repr__(self) -> str:
        return f"CapabilityProxy({self._handle['id']}@{self._handle['version']})"

    def __getattr__(self, operation: str) -> Any:
        if operation.startswith("_") or operation not in self._operations:
            raise AttributeError(operation)

        async def call(**kwargs: Any) -> Any:
            return await host_request(
                "capability.call",
                {
                    "handle": self._handle,
                    "operation": operation,
                    "args": kwargs,
                },
            )

        return call


class Capabilities:
    async def search(self, query: str, limit: int = 5) -> list[dict[str, Any]]:
        return await host_request(
            "capability.search", {"query": query, "limit": limit}
        )

    async def load(self, capability_id: str) -> CapabilityProxy:
        handle = await host_request("capability.load", {"id": capability_id})
        return CapabilityProxy(handle)


class _Pump:
    def __init__(self, read_fd: int, write_fd: int, stream: str) -> None:
        self._read_fd = read_fd
        self._token_fd = os.dup(write_fd)
        self._stream = stream
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self._lock = threading.Lock()
        self._watch: tuple[bytes, threading.Event] | None = None
        self._buffer = b""
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def drain(self) -> None:
        token = b"\xff<drain:" + uuid.uuid4().hex.encode() + b">\xff"
        seen = threading.Event()
        with self._lock:
            self._watch = (token, seen)
        try:
            os.write(self._token_fd, token)
            while not seen.wait(0.1):
                if not self._thread.is_alive():
                    return
        finally:
            with self._lock:
                self._watch = None

    def _run(self) -> None:
        while chunk := os.read(self._read_fd, 65536):
            self._feed(chunk)

    def _feed(self, chunk: bytes) -> None:
        data = self._buffer + chunk
        self._buffer = b""
        with self._lock:
            watch = self._watch
        if watch is None:
            self._emit(data)
            return
        token, seen = watch
        index = data.find(token)
        if index >= 0:
            self._emit(data[:index])
            self._finish_decode()
            seen.set()
            data = data[index + len(token) :]
        hold = 0
        for size in range(min(len(data), len(token) - 1), 0, -1):
            if data.endswith(token[:size]):
                hold = size
                break
        if hold:
            self._buffer = data[-hold:]
            data = data[:-hold]
        self._emit(data)

    def _emit(self, data: bytes) -> None:
        if text := self._decoder.decode(data):
            for start in range(0, len(text), 65536):
                _send(
                    {
                        "event": self._stream,
                        "id": None,
                        "text": text[start : start + 65536],
                    }
                )

    def _finish_decode(self) -> None:
        if text := self._decoder.decode(b"", final=True):
            _send({"event": self._stream, "id": None, "text": text})
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")


class _TaggedBuffer(io.RawIOBase):
    def __init__(self, fallback_fd: int) -> None:
        self._fallback_fd = fallback_fd

    def write(self, data: Any) -> int:
        view = memoryview(data).cast("B")
        total = len(view)
        while view:
            view = view[os.write(self._fallback_fd, view) :]
        return total

    def fileno(self) -> int:
        return self._fallback_fd

    def writable(self) -> bool:
        return True


class _TaggedWriter(io.TextIOBase):
    def __init__(self, stream: str, fallback_fd: int) -> None:
        self._stream = stream
        self._fallback_fd = fallback_fd
        self._buffer = _TaggedBuffer(fallback_fd)

    def write(self, text: str) -> int:
        if not isinstance(text, str):
            raise TypeError(f"write() argument must be str, not {type(text).__name__}")
        for start in range(0, len(text), 65536):
            _send(
                {
                    "event": self._stream,
                    "id": _current_cell.get(),
                    "text": text[start : start + 65536],
                }
            )
        return len(text)

    def fileno(self) -> int:
        return self._fallback_fd

    def writable(self) -> bool:
        return True

    @property
    def buffer(self) -> _TaggedBuffer:
        return self._buffer

    @property
    def encoding(self) -> str:
        return "utf-8"


def _compile_cell(code: str, filename: str) -> tuple[list[types.CodeType], bool]:
    linecache.cache[filename] = (len(code), None, code.splitlines(keepends=True), filename)
    tree = ast.parse(code, filename)
    trailing: ast.Expression | None = None
    if tree.body and isinstance(tree.body[-1], ast.Expr):
        trailing = ast.Expression(tree.body.pop().value)
    flags = ast.PyCF_ALLOW_TOP_LEVEL_AWAIT
    compiled = []
    if tree.body:
        compiled.append(compile(tree, filename, "exec", flags=flags, dont_inherit=True))
    if trailing is not None:
        compiled.append(
            compile(trailing, filename, "eval", flags=flags, dont_inherit=True)
        )
    return compiled, trailing is not None


async def _execute(request: dict[str, Any], namespace: dict[str, Any]) -> None:
    cell_id = request["id"]
    token = _current_cell.set(cell_id)
    existing_tasks = asyncio.all_tasks()
    error_event: dict[str, Any] | None = None
    try:
        codes, has_trailing = _compile_cell(request["code"], f"<{cell_id}>")
        value: Any = None
        for code in codes:
            value = eval(code, namespace)
            if code.co_flags & inspect.CO_COROUTINE:
                value = await value
        if has_trailing and value is not None:
            namespace["_"] = value
    except BaseException as error:
        error_event = _error_event(cell_id, error)
    finally:
        current = asyncio.current_task()
        detached = [
            task
            for task in asyncio.all_tasks()
            if task not in existing_tasks and task is not current
        ]
        for task in detached:
            task.cancel()
        if detached:
            await asyncio.gather(*detached, return_exceptions=True)
        _drain_output()
        if error_event is not None:
            _send(error_event)
        _send(
            {
                "event": "done",
                "id": cell_id,
                "status": "error" if error_event is not None else "ok",
            }
        )
        _current_cell.reset(token)


def _safe_str(error: BaseException) -> str:
    try:
        return str(error)
    except BaseException:
        return "<exception str() failed>"


def _bound_text(text: str, max_bytes: int) -> str:
    return text.encode("utf-8", "replace")[:max_bytes].decode("utf-8", "replace")


def _error_event(cell_id: str, error: BaseException) -> dict[str, Any]:
    try:
        trace = "".join(
            traceback.format_exception(type(error), error, error.__traceback__)
        )
    except BaseException:
        trace = f"{type(error).__name__}: {_safe_str(error)}\n"
    return {
        "event": "error",
        "id": cell_id,
        "ename": type(error).__name__,
        "evalue": _bound_text(_safe_str(error), 64 * 1024),
        "traceback": [_bound_text(trace, 256 * 1024)],
    }


def _drain_output() -> None:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except (OSError, ValueError, AttributeError):
            pass
    _pump_out.drain()
    _pump_err.drain()


def _resolve_host_reply(request_id: str, data: dict[str, Any]) -> None:
    assert _loop is not None

    def deliver() -> None:
        future = _pending_host.get(request_id)
        if future is not None and not future.done():
            future.set_result(data)

    _loop.call_soon_threadsafe(deliver)


async def _serve(queue: asyncio.Queue[dict[str, Any]], namespace: dict[str, Any]) -> None:
    while True:
        request = await queue.get()
        kind = request["type"]
        if kind == "execute":
            await _execute(request, namespace)
        elif kind == "shutdown":
            _send({"event": "done", "id": request["id"], "status": "ok"})
            return


def _read_exact(fd: int, size: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < size:
        chunk = os.read(fd, size - len(chunks))
        if not chunk:
            return b""
        chunks.extend(chunk)
    return bytes(chunks)


def _read_requests(stdin_fd: int, queue: asyncio.Queue[dict[str, Any]]) -> None:
    assert _loop is not None
    while header := _read_exact(stdin_fd, 4):
        size = struct.unpack(">I", header)[0]
        if size > MAX_FRAME_BYTES:
            _send(
                {
                    "event": "error",
                    "id": None,
                    "ename": "ProtocolError",
                    "evalue": f"frame exceeds {MAX_FRAME_BYTES} bytes",
                    "traceback": [],
                }
            )
            break
        payload = _read_exact(stdin_fd, size)
        if not payload:
            break
        try:
            request = json.loads(payload)
            kind = request.get("type")
            if kind == "host_reply":
                _resolve_host_reply(request["id"], request["data"])
            elif kind in {"execute", "shutdown"}:
                if kind == "shutdown":
                    _loop.call_soon_threadsafe(_fail_pending_host_requests)
                _loop.call_soon_threadsafe(queue.put_nowait, request)
            else:
                raise ValueError(f"unknown request type: {kind!r}")
        except BaseException as error:
            _send(
                {
                    "event": "error",
                    "id": None,
                    "ename": "ProtocolError",
                    "evalue": f"{type(error).__name__}: {error}",
                    "traceback": [],
                }
            )
    _loop.call_soon_threadsafe(_fail_pending_host_requests)
    _loop.call_soon_threadsafe(
        queue.put_nowait, {"type": "shutdown", "id": "eof"}
    )


def _owner_watchdog(owner: int, initial_parent: int) -> None:
    while os.getppid() == initial_parent:
        try:
            os.kill(owner, 0)
        except ProcessLookupError:
            break
        time.sleep(1)
    os._exit(1)


_pump_out: _Pump
_pump_err: _Pump


def _setup_fds() -> int:
    global _protocol_fd, _pump_out, _pump_err
    _protocol_fd = os.dup(1)
    os.set_inheritable(_protocol_fd, False)
    out_read, out_write = os.pipe()
    err_read, err_write = os.pipe()
    os.dup2(out_write, 1)
    os.dup2(err_write, 2)
    os.close(out_write)
    os.close(err_write)
    sys.stdout = _TaggedWriter("stdout", os.dup(1))
    sys.stderr = _TaggedWriter("stderr", os.dup(2))
    stdin_fd = os.dup(0)
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)
    os.close(devnull)
    sys.stdin = open(os.devnull, "r")
    _pump_out = _Pump(out_read, 1, "stdout")
    _pump_err = _Pump(err_read, 2, "stderr")
    return stdin_fd


def main() -> None:
    global _loop
    stdin_fd = _setup_fds()
    owner = int(os.environ.get("SERAPH_KERNEL_OWNER_PID", os.getppid()))
    threading.Thread(
        target=_owner_watchdog, args=(owner, os.getppid()), daemon=True
    ).start()

    user_module = types.ModuleType("__main__")
    user_module.__dict__.update(
        {"__builtins__": __builtins__, "asyncio": asyncio, "caps": Capabilities(), "emit": emit}
    )
    sys.modules["__main__"] = user_module

    _loop = asyncio.new_event_loop()
    asyncio.set_event_loop(_loop)
    queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
    threading.Thread(target=_read_requests, args=(stdin_fd, queue), daemon=True).start()
    _send(
        {
            "event": "ready",
            "protocol": PROTOCOL_VERSION,
            "python": platform.python_version(),
        }
    )
    _loop.run_until_complete(_serve(queue, user_module.__dict__))
    _loop.close()


if __name__ == "__main__":
    main()
