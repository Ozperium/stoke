#!/usr/bin/env python3
"""Production output-webhook transport regression checks."""
import contextlib
import http.server
import json
import os
import pathlib
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

BIN = os.environ.get(
    "STOKE_BIN",
    str(pathlib.Path(__file__).resolve().parents[1] / "target/debug/stoke"),
)


class JsonServer:
    def __init__(self, handler):
        self.requests = []
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format, *args):
                pass

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length)
                owner.requests.append((self.path, body))
                handler(self, body)

            def do_GET(self):
                if self.path == "/health":
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"ok")
                    return
                self.send_response(404)
                self.end_headers()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.server.server_port

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def provider_response(server, body):
    payload = {
        "id": "fixture",
        "object": "chat.completion",
        "created": 1,
        "model": "fixture-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "fixture"}}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
    }
    raw = json.dumps(payload).encode()
    server.send_response(200)
    server.send_header("Content-Type", "application/json")
    server.send_header("Content-Length", str(len(raw)))
    server.end_headers()
    server.wfile.write(raw)


def redirect_response(server, sink_url, body):
    server.send_response(307)
    server.send_header("Location", sink_url)
    server.send_header("Content-Length", "0")
    server.end_headers()


def sink_response(server, body):
    raw = b'{"response":{"redirected":true}}'
    server.send_response(200)
    server.send_header("Content-Type", "application/json")
    server.send_header("Content-Length", str(len(raw)))
    server.end_headers()
    server.wfile.write(raw)


@contextlib.contextmanager
def daemon(provider, redirector):
    with tempfile.TemporaryDirectory(prefix="stoke-webhook-", dir=os.environ.get("TMPDIR")) as directory:
        root = pathlib.Path(directory)
        port = _free_port()
        config = f'''default_model="fixture-model"
[server]
host="127.0.0.1"
port={port}
[[providers]]
name="fixture"
type="openai"
base_url="http://127.0.0.1:{provider.port}/v1"
tier="cloud"
models=["fixture-model"]
[[keys]]
key="tenant-a-fixture"
budget_usd=1.0
[pricing.models."fixture-model"]
input_per_1m=1.0
output_per_1m=1.0
[plugins]
post_response=["http://127.0.0.1:{redirector.port}/start"]
'''
        (root / "stoke.toml").write_text(config)
        env = {key: value for key, value in os.environ.items() if not key.startswith("STOKE_")}
        env.update(
            HOME=directory,
            STOKE_LEDGER_PATH=str(root / "ledger.db"),
            STOKE_API_KEYS="tenant-a-fixture",
            STOKE_ADMIN_KEY="operator-fixture",
        )
        log = (root / "daemon.log").open("w+")
        proc = subprocess.Popen([BIN], cwd=directory, env=env, stdout=log, stderr=log)
        try:
            for _ in range(200):
                if proc.poll() is not None:
                    break
                try:
                    urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=0.1)
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


def _free_port():
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def request(port):
    body = json.dumps(
        {"model": "fixture-model", "messages": [{"role": "user", "content": "hello"}], "max_tokens": 100}
    ).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=body,
        headers={"Content-Type": "application/json", "x-stoke-key": "tenant-a-fixture"},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def main():
    sink = JsonServer(sink_response)
    redirector = JsonServer(lambda server, body: redirect_response(server, f"http://127.0.0.1:{sink.port}/sink", body))
    provider = JsonServer(provider_response)
    receipt = {}
    try:
        with daemon(provider, redirector) as (port, proc, log):
            status, raw = request(port)
            receipt = {
                "gateway_status": status,
                "provider_requests": len(provider.requests),
                "redirect_requests": len(redirector.requests),
                "target_requests": len(sink.requests),
                "target_bodies": [body.decode(errors="replace") for _, body in sink.requests],
            }
            if proc.poll() is not None:
                log.seek(0)
                raise AssertionError(f"daemon exited: {log.read()}")
            assert status >= 500, receipt
            assert len(provider.requests) == 1, receipt
            assert len(redirector.requests) == 1, receipt
            redirect_payload = json.loads(redirector.requests[0][1])
            assert redirect_payload["key_id"].startswith("sha256:"), receipt
            assert "tenant-a-fixture" not in redirector.requests[0][1].decode(), receipt
            assert not sink.requests, receipt
        print(json.dumps({"ok": True, **receipt}, sort_keys=True))
    except Exception as error:
        print(json.dumps({"ok": False, "error": repr(error), **receipt}, sort_keys=True))
        return 1
    finally:
        provider.close()
        redirector.close()
        sink.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
