#!/usr/bin/env python3
"""Native HTTP regressions for unchanged JS post-response output."""
import contextlib
import http.server
import json
import os
import pathlib
import socket
import subprocess
import tempfile
import threading
import time
from typing import Any
import urllib.error
import urllib.request


BIN = os.environ.get(
    "STOKE_BIN", str(pathlib.Path(__file__).resolve().parents[1] / "target/debug/stoke")
)


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_POST(self):
        _ = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        body = {
            "id": "fixture-response",
            "object": "chat.completion",
            "created": 1,
            "model": "fixture-model",
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": "fixture"},
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "input_tokens": 1,
                "output_tokens": 1,
            },
        }
        encoded = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


@contextlib.contextmanager
def daemon(script):
    with tempfile.TemporaryDirectory(
        prefix="stoke-response-", dir=os.environ.get("TMPDIR")
    ) as directory:
        path = pathlib.Path(directory)
        port = free_port()
        (path / "hook.js").write_text(script)
        (path / "stoke.toml").write_text(
            f'''default_model="fixture-model"
[server]
host="127.0.0.1"
port={port}
[[providers]]
name="fixture"
type="openai"
base_url="http://127.0.0.1:{provider.server_port}/v1"
tier="cloud"
models=["fixture-model"]
[[keys]]
key="tenant-a-fixture"
budget_usd=1.0
[pricing.models."fixture-model"]
input_per_1m=1.0
output_per_1m=1.0
[plugins]
scripts=["hook.js"]
'''
        )
        env = {
            key: value for key, value in os.environ.items() if not key.startswith("STOKE_")
        }
        env.update(
            HOME=directory,
            STOKE_API_KEYS="tenant-a-fixture",
            STOKE_LEDGER_PATH=str(path / "ledger.db"),
            STOKE_TEST_JS="1",
        )
        log = open(path / "daemon.log", "w+")
        proc = subprocess.Popen([BIN], cwd=directory, env=env, stdout=log, stderr=log)
        try:
            for _ in range(200):
                if proc.poll() is not None:
                    break
                try:
                    urllib.request.urlopen(
                        f"http://127.0.0.1:{port}/health", timeout=0.1
                    )
                    break
                except Exception:
                    time.sleep(0.02)
            yield port, proc, log
        finally:
            if proc.poll() is None:
                proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            log.close()


def request(port) -> tuple[int, Any]:
    body = json.dumps(
        {"model": "fixture-model", "messages": [{"role": "user", "content": "hello"}]}
    ).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=body,
        headers={"Content-Type": "application/json", "x-stoke-key": "tenant-a-fixture"},
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def check(name, script):
    with daemon(script) as (port, proc, log):
        assert proc.poll() is None, f"{name}: daemon failed to start"
        status, body = request(port)
        assert status == 200, (name, status, body)
        assert isinstance(body, dict), (name, status, body)
        assert body["id"] == "fixture-response", (name, body)
        assert body["choices"][0]["message"]["content"] == "fixture", (name, body)
        log.seek(0)
        assert "JS plugins loaded" in log.read(), f"{name}: JS plugin was not loaded"


check(
    "pre_only",
    "module.exports = { pre_request(input) { return input; } };",
)
check(
    "identity_post",
    "module.exports = { post_response(input) { return input; } };",
)
provider.shutdown()
provider.server_close()
print(json.dumps({"binary": BIN, "passed": 2, "failed": 0}))
