#!/usr/bin/env python3
"""Offline, lossless-only Headroom worker for Stoke output strings."""
from __future__ import annotations

import argparse
import hmac
import importlib.metadata
import json
import os
import socket
import sys
import types
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any

# Bootstrap only the reviewed upstream modules. The official wheel supplies the
# native extension; the one local patch supplies the reviewed envelope adapter.
# Namespace package paths avoid Headroom's broad eager __init__, keeping this
# worker's optional environment dependency-free.
HERE = Path(__file__).resolve().parent
PATCH_ROOT = HERE / "headroom_patches"
UPSTREAM = os.environ.get("HEADROOM_UPSTREAM_SOURCE")
try:
    WHEEL_ROOT = Path(str(importlib.metadata.distribution("headroom-ai").locate_file("headroom")))
except importlib.metadata.PackageNotFoundError as exc:
    raise RuntimeError("headroom-ai 0.37.0 must be installed in the worker venv") from exc

_PACKAGE_ROOTS = [PATCH_ROOT / "headroom"]
if UPSTREAM:
    _PACKAGE_ROOTS.append(Path(UPSTREAM) / "headroom")
_PACKAGE_ROOTS.append(WHEEL_ROOT)
_PACKAGE_ROOTS = [path for path in _PACKAGE_ROOTS if path.is_dir()]
if not _PACKAGE_ROOTS:
    raise RuntimeError("no Headroom package sources are available")


def _namespace_package(name: str, paths: list[Path]) -> None:
    module = types.ModuleType(name)
    module.__path__ = [str(path) for path in paths if path.is_dir()]
    module.__package__ = name
    sys.modules[name] = module


_namespace_package("headroom", _PACKAGE_ROOTS)
_namespace_package(
    "headroom.transforms",
    [_root / "transforms" for _root in _PACKAGE_ROOTS],
)
_namespace_package(
    "headroom.ccr",
    [_root / "ccr" for _root in _PACKAGE_ROOTS],
)

# Keep the optional dependency in offline/no-telemetry mode before importing
# any upstream modules that inspect process configuration at import time.
os.environ.setdefault("HEADROOM_LOSSLESS_ONLY", "1")
os.environ.setdefault("HEADROOM_DISABLE_KOMPRESS", "1")
os.environ.setdefault("HEADROOM_DISABLE_KOMPRESS_FALLBACK", "1")
os.environ.setdefault("HEADROOM_OFFLINE", "1")
os.environ.setdefault("HEADROOM_TELEMETRY", "0")
os.environ.setdefault("DO_NOT_TRACK", "1")
os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("HEADROOM_JSON_OUTPUT_ENVELOPE", "1")

# These imports are intentionally confined to this optional worker. The launcher
# never imports this module, especially on --off.
from headroom.config import CCRConfig  # noqa: E402
from headroom.transforms.recursive_json import route_embedded_json  # noqa: E402
from headroom.transforms.smart_crusher import (  # noqa: E402
    SmartCrusher,
    SmartCrusherConfig,
)

class _DuplicateKey(ValueError):
    pass


class _PayloadTooLarge(ValueError):
    pass


MAX_BODY_BYTES = 1024 * 1024
MAX_OUTPUTS = 128
SOCKET_READ_TIMEOUT = 5.0


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result = dict(pairs)
    if len(result) != len(pairs):
        raise _DuplicateKey("duplicate JSON key")
    return result


def _reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON number: {value}")


_DECODER = json.JSONDecoder(
    object_pairs_hook=_unique_object,
    parse_constant=_reject_constant,
)

# One actual upstream compressor, not a json.dumps replacement. Compaction is
# disabled deliberately: upstream's lossless-only Rust path emits minified JSON
# while retaining every non-whitespace lexeme and bypasses row-dropping/markers.
_CRUSHER = SmartCrusher(
    SmartCrusherConfig(min_tokens_to_crush=0, lossless_only=True),
    ccr_config=CCRConfig(enabled=False, inject_retrieval_marker=False),
    with_compaction=False,
)


def _has_routable_array(value: Any) -> bool:
    if isinstance(value, list):
        if len(value) >= 2 and sum(isinstance(item, dict) for item in value) >= 0.8 * len(value):
            return True
        return any(_has_routable_array(item) for item in value)
    if isinstance(value, dict):
        return any(_has_routable_array(item) for item in value.values())
    return False


def _non_whitespace_lexemes(text: str) -> str:
    """Remove whitespace only outside JSON strings, preserving all other chars."""
    chars: list[str] = []
    in_string = escaped = False
    for char in text:
        if in_string:
            chars.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
            chars.append(char)
        elif not char.isspace():
            chars.append(char)
    return "".join(chars)


def _compress_json(text: str) -> str:
    try:
        value = _DECODER.decode(text)
    except (ValueError, TypeError, RecursionError):
        return text
    if not _has_routable_array(value):
        return text
    try:
        result = _CRUSHER.crush(text, lossless_only=True)
    except Exception:
        return text
    candidate = result.compressed
    if (
        not result.was_modified
        or len(candidate.encode("utf-8")) >= len(text.encode("utf-8"))
        or _non_whitespace_lexemes(candidate) != _non_whitespace_lexemes(text)
        or "<<ccr:" in candidate
    ):
        return text
    return candidate


def compress_one(text: str) -> str:
    """Compress a supported JSON output, or return the exact original string."""
    # The reviewed adapter handles JSON-in-string envelopes and replaces only
    # the JSON string value; all outer metadata bytes remain untouched.
    try:
        envelope = route_embedded_json(text, _compress_json, tok=lambda s: len(s.encode("utf-8")))
    except Exception:
        envelope = None
    return envelope if envelope is not None else _compress_json(text)


def process_payload(payload: Any) -> dict[str, list[str]]:
    if not isinstance(payload, dict) or set(payload) != {"outputs"}:
        raise ValueError("expected object with exactly an outputs field")
    outputs = payload["outputs"]
    if not isinstance(outputs, list) or any(not isinstance(item, str) for item in outputs):
        raise ValueError("outputs must be an array of strings")
    if len(outputs) > MAX_OUTPUTS:
        raise _PayloadTooLarge("too many outputs")
    return {"outputs": [compress_one(item) for item in outputs]}


class Handler(BaseHTTPRequestHandler):
    server_version = "stoke-headroom/1"

    def log_message(self, *_args: Any) -> None:
        return

    def setup(self) -> None:
        super().setup()
        self.connection.settimeout(getattr(self.server, "socket_read_timeout", SOCKET_READ_TIMEOUT))

    def _authorized(self) -> bool:
        values = self.headers.get_all("Authorization", [])
        if len(values) != 1:
            return False
        presented = values[0]
        if not presented.startswith("Bearer "):
            return False
        expected = getattr(self.server, "headroom_token", "")
        return bool(expected) and hmac.compare_digest(presented[7:], expected)

    def _bad(self, status: int, error: str) -> None:
        self._send(status, {"error": error})

    def _content_length(self) -> int:
        values = self.headers.get_all("Content-Length", [])
        if len(values) != 1:
            raise ValueError("invalid content length")
        value = values[0]
        if not value or not value.isascii() or not value.isdigit():
            raise ValueError("invalid content length")
        length = int(value, 10)
        if length > MAX_BODY_BYTES:
            raise OverflowError("request too large")
        return length

    def _read_body(self, length: int) -> bytes:
        chunks: list[bytes] = []
        remaining = length
        while remaining:
            chunk = self.rfile.read(remaining)
            if not chunk:
                raise ValueError("incomplete request body")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)

    def _send(self, status: int, value: Any) -> None:
        raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if status == 200 and len(raw) > MAX_BODY_BYTES:
            status = 413
            raw = b'{"error":"response too large"}'
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/health":
            if self._authorized():
                self._send(200, {"status": "ok"})
            else:
                self._bad(401, "unauthorized")
        else:
            self._send(404, {"error": "not_found"})

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/compress":
            self._send(404, {"error": "not_found"})
            return
        if not self._authorized():
            self._bad(401, "unauthorized")
            return
        try:
            if self.headers.get_all("Transfer-Encoding", []):
                raise ValueError("transfer encoding is not supported")
            length = self._content_length()
            payload = _DECODER.decode(self._read_body(length).decode("utf-8"))
            self._send(200, process_payload(payload))
        except (OverflowError, _PayloadTooLarge):
            self._bad(413, "request too large")
        except Exception:
            # Payload/parser details must not become an HTTP response or log.
            self._bad(400, "bad request")


def main() -> int:
    parser = argparse.ArgumentParser(description="local lossless Headroom worker")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=9101)
    parser.add_argument("--ready-fd", type=int, default=None)
    args = parser.parse_args()
    if args.host != "127.0.0.1":
        parser.error("only numeric loopback 127.0.0.1 is supported")
    token = os.environ.get("STOKE_HEADROOM_TOKEN", "")
    if not token:
        parser.error("STOKE_HEADROOM_TOKEN is required")
    if not (0 <= args.port <= 65535):
        parser.error("--port must be between 0 and 65535")
    server = HTTPServer((args.host, args.port), Handler)
    server.headroom_token = token
    server.socket_read_timeout = SOCKET_READ_TIMEOUT
    actual_port = int(server.server_port)
    if args.ready_fd is not None:
        try:
            os.write(args.ready_fd, json.dumps({"port": actual_port}, separators=(",", ":")).encode("ascii") + b"\n")
        finally:
            os.close(args.ready_fd)
    else:
        print(json.dumps({"port": actual_port}, separators=(",", ":")), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
