#!/usr/bin/env python3
"""Foreground local launcher for Stoke plus the optional Headroom worker.

The launcher itself is stdlib-only. `--off` therefore starts plain Stoke without
loading Python Headroom dependencies.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import select
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_UPSTREAM_SOURCE = str(HERE / "upstream")


def _validate_worker_python(python: Path) -> Path:
    python = python.expanduser()
    if not python.is_file() or not os.access(python, os.X_OK):
        raise ValueError(f"Headroom Python is not an executable: {python}")
    try:
        result = subprocess.run(
            [str(python), "-c", "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise ValueError("Headroom Python failed its startup check") from exc
    if result.returncode != 0:
        raise ValueError("Headroom Python failed its startup check")
    version = result.stdout.strip().split(".")
    if len(version) != 2 or version[0] != "3" or not version[1].isdigit() or int(version[1]) < 10:
        raise ValueError("Headroom Python 3.10 or newer is required")
    return python


def _read_ready(proc: subprocess.Popen[bytes], ready_fd: int) -> int:
    deadline = time.monotonic() + 60
    data = bytearray()
    try:
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                raise RuntimeError(f"Headroom worker exited with {proc.returncode}")
            readable, _, _ = select.select([ready_fd], [], [], 0.1)
            if not readable:
                continue
            chunk = os.read(ready_fd, 4096)
            if not chunk:
                break
            data.extend(chunk)
            if b"\n" in data:
                line, remainder = bytes(data).split(b"\n", 1)
                if remainder.strip():
                    raise RuntimeError("invalid Headroom worker ready message")
                try:
                    ready = json.loads(line.decode("ascii"))
                except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
                    raise RuntimeError("invalid Headroom worker ready message") from exc
                if not isinstance(ready, dict) or set(ready) != {"port"} or isinstance(ready["port"], bool):
                    raise RuntimeError("invalid Headroom worker ready message")
                port = ready["port"]
                if not isinstance(port, int) or not (1 <= port <= 65535):
                    raise RuntimeError("invalid Headroom worker port")
                return port
    finally:
        os.close(ready_fd)
    raise RuntimeError("Headroom worker closed its ready pipe")


def _wait_health(proc: subprocess.Popen[bytes], port: int, token: str) -> None:
    deadline = time.monotonic() + 60
    url = f"http://127.0.0.1:{port}/health"
    last: Exception | None = None
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"Headroom worker exited with {proc.returncode}")
        try:
            request = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
            with urllib.request.urlopen(request, timeout=0.5) as response:
                if response.status == 200 and response.read(256) == b'{"status":"ok"}':
                    return
        except Exception as exc:  # startup race only
            last = exc
        time.sleep(0.05)
    raise RuntimeError(f"worker health timeout: {last}")


_TARGET_KEYS = {"enabled", "url", "timeout_ms"}
_BARE_KEY = re.compile(r"^[A-Za-z0-9_-]+$")
_DOTTED_KEY = re.compile(r"^[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)+$")
_HEADROOM_TABLE = re.compile(
    r'^\[\s*(?:plugins|"plugins"|\'plugins\')\s*\.\s*'
    r'(?:headroom|"headroom"|\'headroom\')\s*\]$'
)
_HEADROOM_ARRAY = re.compile(
    r'^\[\[\s*(?:plugins|"plugins"|\'plugins\')\s*\.\s*'
    r'(?:headroom|"headroom"|\'headroom\')\s*\]\]$'
)
_HEADROOM_DOTTED = re.compile(
    r'^\s*(?:plugins|"plugins"|\'plugins\')\s*\.\s*'
    r'(?:headroom|"headroom"|\'headroom\')'
    r'(?:\s*\.\s*(?:[A-Za-z0-9_-]+|"[^"]+"|\'[^\']+\'))*\s*='
)


def _without_comment(line: str) -> str:
    """Return TOML code before a comment, respecting quoted strings."""
    quote: str | None = None
    escaped = False
    for index, char in enumerate(line):
        if quote == '"':
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif quote == "'":
            if char == quote:
                quote = None
        elif char in ('"', "'"):
            quote = char
        elif char == "#":
            return line[:index]
    return line


def _assignment_key(code: str) -> str | None:
    """Get a simple assignment key; return None for non-simple keys."""
    quote: str | None = None
    escaped = False
    for index, char in enumerate(code):
        if quote == '"':
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif quote == "'":
            if char == quote:
                quote = None
        elif char in ('"', "'"):
            quote = char
        elif char == "=":
            key = code[:index].strip()
            if _BARE_KEY.fullmatch(key) or _DOTTED_KEY.fullmatch(key):
                return key
            if len(key) >= 2 and key[0] == key[-1] and key[0] in ('"', "'"):
                return key[1:-1]
            return None
    return None


def _masked_toml_lines(text: str) -> list[str]:
    """Hide multiline-string contents before looking for tables or keys."""
    lines = text.splitlines(keepends=True)
    masked_lines: list[str] = []
    multiline: str | None = None
    for line in lines:
        output = list(line)
        index = 0
        quote: str | None = None
        while index < len(line):
            if multiline is not None:
                if line.startswith(multiline, index):
                    escaped = False
                    if multiline == '"""':
                        slashes = 0
                        previous = index - 1
                        while previous >= 0 and line[previous] == "\\":
                            slashes += 1
                            previous -= 1
                        escaped = slashes % 2 == 1
                    for position in range(index, min(index + 3, len(output))):
                        output[position] = " "
                    index += 3
                    if not escaped:
                        multiline = None
                    continue
                output[index] = " "
                index += 1
                continue
            if quote is not None:
                if quote == '"' and line[index] == "\\":
                    index += 2
                elif line[index] == quote:
                    quote = None
                    index += 1
                else:
                    index += 1
                continue
            if line[index] == "#":
                for position in range(index, len(output)):
                    output[position] = " "
                break
            if line.startswith('"""', index):
                for position in range(index, min(index + 3, len(output))):
                    output[position] = " "
                multiline = '"""'
                index += 3
                continue
            if line.startswith("'''", index):
                for position in range(index, min(index + 3, len(output))):
                    output[position] = " "
                multiline = "'''"
                index += 3
                continue
            if line[index] in ('"', "'"):
                quote = line[index]
            index += 1
        masked_lines.append("".join(output))
    return masked_lines


def _canonical_header(header: str) -> tuple[str, ...]:
    body = header[1:-1].strip()
    if body.startswith("["):
        body = body[1:-1].strip()
    pattern = r'"([^"]*)"|\'([^\']*)\'|([A-Za-z0-9_-]+)'
    return tuple(
        match.group(1) if match.group(1) is not None else
        match.group(2) if match.group(2) is not None else match.group(3)
        for match in re.finditer(pattern, body)
    )


def _validated_config(data: bytes, *, source: bool = False) -> bytes:
    try:
        text = data.decode("utf-8")
        tomllib.loads(text)
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        label = "config" if source else "generated config"
        raise ValueError(f"invalid TOML {label}: {exc}") from exc
    return data


def _augmented_config(original: bytes, port: int | None, enabled: bool = True) -> bytes:
    """Make a temporary TOML copy without duplicating or guessing tables."""
    _validated_config(original, source=True)
    text = original.decode("utf-8")

    lines = text.splitlines(keepends=True)
    code_lines = _masked_toml_lines(text)
    headers: list[tuple[int, str]] = []
    for index, line in enumerate(code_lines):
        code = line.strip()
        if code.startswith("[[") and code.endswith("]]" ):
            headers.append((index, code))
        elif code.startswith("[") and code.endswith("]"):
            headers.append((index, code))

    headroom_headers = [index for index, header in headers if _HEADROOM_TABLE.fullmatch(header)]
    if len(headroom_headers) > 1:
        raise ValueError("unsupported ambiguous TOML: duplicate [plugins.headroom] tables")
    for index, header in headers:
        path = _canonical_header(header)
        if len(path) > 2 and path[:2] == ("plugins", "headroom") and not headroom_headers:
            raise ValueError("unsupported ambiguous TOML: child of implicit plugins.headroom table")

    # These forms can conflict with a child table and cannot be safely rewritten
    # with a line-preserving stdlib launcher.
    current_header: tuple[str, ...] = ()
    header_by_index = dict(headers)
    for index, code_line in enumerate(code_lines):
        code = code_line.strip()
        header = header_by_index.get(index)
        if header is not None:
            current_header = _canonical_header(header)
            if _HEADROOM_ARRAY.fullmatch(header):
                raise ValueError("unsupported ambiguous TOML: [plugins.headroom] is an array of tables")
            continue
        key = _assignment_key(code)
        if _HEADROOM_DOTTED.match(code) or (
            key and (key == "plugins.headroom" or key.startswith("plugins.headroom."))
        ):
            raise ValueError("unsupported ambiguous TOML: dotted plugins.headroom key")
        if current_header == () and key == "plugins":
            raise ValueError("unsupported ambiguous TOML: plugins is an inline/root key")
        if current_header in ((), ("plugins",)) and key == "headroom":
            raise ValueError("unsupported ambiguous TOML: plugins.headroom is not a normal table")

    values: dict[str, str] = {"enabled": "true" if enabled else "false"}
    if enabled:
        if port is None:
            raise ValueError("internal error: enabled Headroom config needs a worker port")
        values.update({
            "url": f'"http://127.0.0.1:{port}/compress"',
            "timeout_ms": "1000",
        })

    if not headroom_headers:
        suffix = "\n[plugins.headroom]\n" + "\n".join(
            f"{key} = {value}" for key, value in values.items()
        ) + "\n"
        return _validated_config(
            original + (b"\n" if original and not original.endswith(b"\n") else b"") + suffix.encode("utf-8")
        )

    start = headroom_headers[0] + 1
    end = len(lines)
    for index, _header in headers:
        if index > headroom_headers[0]:
            end = index
            break
    seen: set[str] = set()
    for index in range(start, end):
        code = code_lines[index]
        key = _assignment_key(code)
        if key in _TARGET_KEYS:
            if key in seen:
                raise ValueError(f"unsupported ambiguous TOML: duplicate {key} in [plugins.headroom]")
            seen.add(key)
            if key not in values:
                continue
            ending = "\r\n" if lines[index].endswith("\r\n") else "\n" if lines[index].endswith("\n") else ""
            indent = lines[index][: len(lines[index]) - len(lines[index].lstrip())]
            lines[index] = f"{indent}{key} = {values[key]}{ending}"
    for key, value in values.items():
        if key not in seen:
            if end and lines[end - 1] and not lines[end - 1].endswith(("\n", "\r")):
                lines[end - 1] += "\n"
            lines.insert(end, f"{key} = {value}\n")
            end += 1
    return _validated_config("".join(lines).encode("utf-8"))


def _stop(proc: subprocess.Popen[bytes] | None) -> None:
    if proc is None or proc.poll() is not None:
        return
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="run Stoke with optional local Headroom")
    parser.add_argument("--stoke-bin", required=True, type=Path)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--worker-python", type=Path, default=Path(sys.executable))
    parser.add_argument("--worker-port", type=int, default=0)
    parser.add_argument("--off", action="store_true", help="run plain Stoke; do not import Headroom")
    args = parser.parse_args(argv)
    config = args.config.expanduser().resolve()
    binary = args.stoke_bin.expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"executable Stoke binary not found: {binary}")
    if not config.is_file():
        parser.error(f"config not found: {config}")

    worker: subprocess.Popen[bytes] | None = None
    stoke: subprocess.Popen[bytes] | None = None
    temp_name: str | None = None
    token: str | None = None
    ready_read: int | None = None
    ready_write: int | None = None
    def forward(signum: int, _frame: object) -> None:
        # Do not wait re-entrantly from a signal handler; the finally block
        # below owns waits and escalation.
        if stoke is not None and stoke.poll() is None:
            stoke.terminate()
        if worker is not None and worker.poll() is None:
            worker.terminate()
        raise SystemExit(128 + signum)

    old_int = signal.signal(signal.SIGINT, forward)
    old_term = signal.signal(signal.SIGTERM, forward)
    try:
        port = None
        if not args.off:
            if not (0 <= args.worker_port <= 65535):
                parser.error("--worker-port must be between 0 and 65535")
            try:
                python = _validate_worker_python(args.worker_python)
            except ValueError as exc:
                parser.error(str(exc))
            token = secrets.token_urlsafe(32)
            ready_read, ready_write = os.pipe()
            worker_env = {
                "PATH": os.environ.get("PATH", ""),
                "PYTHONUNBUFFERED": "1",
                "STOKE_HEADROOM_TOKEN": token,
                "HEADROOM_UPSTREAM_SOURCE": os.environ.get(
                    "HEADROOM_UPSTREAM_SOURCE", DEFAULT_UPSTREAM_SOURCE
                ),
                "HEADROOM_LOSSLESS_ONLY": "1",
                "HEADROOM_OFFLINE": "1",
                "HEADROOM_TELEMETRY": "0",
                "DO_NOT_TRACK": "1",
                "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1",
            }
            try:
                worker = subprocess.Popen(
                    [str(python), str(HERE / "worker.py"), "--host", "127.0.0.1",
                     "--port", str(args.worker_port), "--ready-fd", str(ready_write)],
                    cwd=str(HERE), env=worker_env, pass_fds=(ready_write,),
                )
            except OSError:
                os.close(ready_read)
                os.close(ready_write)
                ready_read = ready_write = None
                raise
            os.close(ready_write)
            ready_write = None
            assert ready_read is not None
            port = _read_ready(worker, ready_read)
            ready_read = None
            assert token is not None
            _wait_health(worker, port, token)
        try:
            augmented = _augmented_config(config.read_bytes(), port, enabled=not args.off)
        except ValueError as exc:
            parser.error(str(exc))
        with tempfile.NamedTemporaryFile(
            mode="wb", prefix=".stoke-headroom-", suffix=".toml", dir=config.parent, delete=False
        ) as owned:
            owned.write(augmented)
            temp_name = owned.name
        stoke_env = os.environ.copy()
        stoke_env["STOKE_CONFIG"] = temp_name
        if not args.off:
            assert token is not None
            stoke_env["STOKE_HEADROOM_TOKEN"] = token
        stoke = subprocess.Popen([str(binary)], cwd=str(config.parent), env=stoke_env)
        return stoke.wait()
    finally:
        signal.signal(signal.SIGINT, old_int)
        signal.signal(signal.SIGTERM, old_term)
        _stop(stoke)
        _stop(worker)
        for fd in (ready_read, ready_write):
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass
        if temp_name:
            try:
                Path(temp_name).unlink()
            except FileNotFoundError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
