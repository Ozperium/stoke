#!/usr/bin/env python3
"""Loopback regressions for capped routing and model-aware stream fan-out."""
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
import urllib.error
import urllib.request

BIN = os.environ.get(
    "STOKE_BIN",
    str(pathlib.Path(__file__).resolve().parents[1] / "target/debug/stoke"),
)


class Provider:
    def __init__(self, mode):
        self.mode = mode
        self.calls = 0
        self.paths = []
        self.lock = threading.Lock()
        self.two_calls = threading.Event()

        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format, *args):
                pass

            def do_GET(self):
                if self.path.endswith("/api/tags"):
                    body = {"models": [{"name": "priced-model"}, {"name": "unpriced-model"}]}
                elif self.path.endswith("/api/ps"):
                    body = {"models": [{"name": "priced-model"}, {"name": "unpriced-model"}]}
                elif self.path.endswith("/api/show"):
                    body = {"details": {}}
                else:
                    body = {"data": []}
                raw = json.dumps(body).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                body = json.loads(self.rfile.read(length) or b"{}")
                if self.path.endswith("/api/show"):
                    raw = b'{"details": {}}'
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                    return
                with owner.lock:
                    owner.calls += 1
                    owner.paths.append((self.path, body))
                    calls = owner.calls
                    if calls >= 2:
                        owner.two_calls.set()
                if owner.mode == "malformed":
                    raw = b"not-json"
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                    return
                if owner.mode == "error":
                    raw = b"upstream failure"
                    self.send_response(503)
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                    return

                if body.get("stream"):
                    # Let a hedge/race establish both calls before either response
                    # wins; a single-leg fix proceeds after a bounded wait.
                    owner.two_calls.wait(0.35)
                    wire = (
                        b'data: {"choices":[{"delta":{"content":"fixture"}}]}\n\n'
                        b'data: [DONE]\n\n'
                    )
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(wire)))
                    self.end_headers()
                    self.wfile.write(wire)
                    return

                response = {
                    "id": "fixture",
                    "object": "chat.completion",
                    "created": 1,
                    "model": body.get("model", "fixture-model"),
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "fixture"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1},
                }
                raw = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def port(self):
        return self.server.server_port

    def close(self):
        self.server.shutdown()
        self.server.server_close()


@contextlib.contextmanager
def daemon(provider_a, provider_b, *, cap, key, routing="single", hedge=True):
    with tempfile.TemporaryDirectory(prefix="stoke-routing-", dir=os.environ.get("TMPDIR")) as directory:
        root = pathlib.Path(directory)
        port = free_port()
        config = f'''default_model="priced-model"\nrouting="{routing}"\n[server]\nhost="127.0.0.1"\nport={port}\n[auto_route]\nhedge={str(hedge).lower()}\n[[providers]]\nname="provider-a"\ntype="openai_compatible"\nbase_url="http://127.0.0.1:{provider_a.port}/v1"\ntier="local"\nmodels=["priced-model", "unpriced-model"]\n[[providers]]\nname="provider-b"\ntype="openai_compatible"\nbase_url="http://127.0.0.1:{provider_b.port}/v1"\ntier="local"\nmodels=["priced-model", "unpriced-model"]\n[[keys]]\nkey="{key}"\nbudget_usd={cap}\n[pricing.models."priced-model"]\ninput_per_1m=1.0\noutput_per_1m=1.0\n'''
        (root / "stoke.toml").write_text(config)
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("STOKE_") and k not in {"OLLAMA_HOST", "OPENAI_API_KEY", "ANTHROPIC_API_KEY"}
        }
        env.update(
            HOME=directory,
            STOKE_LEDGER_PATH=str(root / "ledger.db"),
            STOKE_API_KEYS=key,
        )
        log = open(root / "daemon.log", "w+")
        proc = subprocess.Popen([BIN], cwd=directory, env=env, stdout=log, stderr=log)
        try:
            for _ in range(200):
                if proc.poll() is not None:
                    raise AssertionError(f"daemon exited early: {(root / 'daemon.log').read_text()}")
                try:
                    urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=0.1).close()
                    break
                except Exception:
                    time.sleep(0.02)
            else:
                raise AssertionError("daemon did not become healthy")
            time.sleep(0.2)
            yield port
        finally:
            if proc.poll() is None:
                proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            log.close()


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def request(port, *, model="priced-model", stream=False, key="cap"):
    body = {
        "model": model,
        "messages": [{"role": "user", "content": "routing fixture"}],
        "max_tokens": 16,
        "stream": stream,
    }
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "x-stoke-key": key},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def budget(port, key):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/budget",
        headers={"x-stoke-key": key},
    )
    with urllib.request.urlopen(req, timeout=2) as response:
        return json.loads(response.read())


def run_case(name, fn):
    try:
        fn()
        return {"test": name, "ok": True}
    except Exception as error:
        return {"test": name, "ok": False, "error": repr(error)}


def finite_nonstream_is_single_dispatch():
    first, second = Provider("malformed"), Provider("valid")
    try:
        with daemon(first, second, cap=1.0, key="cap") as port:
            status, _ = request(port)
            assert status >= 400, (status, first.calls, second.calls)
            assert (first.calls, second.calls) == (1, 0), (first.calls, second.calls, first.paths, second.paths)
            stats = budget(port, "cap")["keys"][0]
            assert stats["reserved_usd"] == 0, stats
            assert stats["spend_usd"] > 0, stats
    finally:
        first.close()
        second.close()


def unlimited_nonstream_retains_failover():
    first, second = Provider("malformed"), Provider("valid")
    try:
        with daemon(first, second, cap=0.0, key="unlimited") as port:
            status, _ = request(port, key="unlimited")
            assert status == 200, status
            assert (first.calls, second.calls) == (1, 1), (first.calls, second.calls)
    finally:
        first.close()
        second.close()


def finite_stream_is_single_dispatch():
    first, second = Provider("error"), Provider("valid")
    try:
        with daemon(first, second, cap=1.0, key="cap") as port:
            status, _ = request(port, stream=True)
            assert status >= 400, (status, first.calls, second.calls)
            assert (first.calls, second.calls) == (1, 0), (first.calls, second.calls, first.paths, second.paths)
    finally:
        first.close()
        second.close()


def finite_priced_local_hedge_has_one_leg():
    first, second = Provider("valid"), Provider("valid")
    try:
        with daemon(first, second, cap=1.0, key="cap", hedge=True) as port:
            status, _ = request(port, stream=True)
            assert status == 200, status
            assert first.calls + second.calls == 1, (first.calls, second.calls)
    finally:
        first.close()
        second.close()


def unlimited_priced_local_hedge_has_one_leg():
    first, second = Provider("valid"), Provider("valid")
    try:
        with daemon(first, second, cap=0.0, key="unlimited", hedge=True) as port:
            status, _ = request(port, stream=True, key="unlimited")
            assert status == 200, status
            assert first.calls + second.calls == 1, (first.calls, second.calls)
    finally:
        first.close()
        second.close()


def unlimited_unpriced_owned_hedge_retains_two_legs():
    first, second = Provider("valid"), Provider("valid")
    try:
        with daemon(first, second, cap=0.0, key="unlimited", hedge=True) as port:
            status, _ = request(port, model="unpriced-model", stream=True, key="unlimited")
            assert status == 200, status
            assert first.calls + second.calls == 2, (first.calls, second.calls)
    finally:
        first.close()
        second.close()


def unlimited_priced_stream_race_has_one_leg():
    first, second = Provider("valid"), Provider("valid")
    try:
        with daemon(first, second, cap=0.0, key="unlimited", routing="stream_race", hedge=False) as port:
            status, _ = request(port, key="unlimited", stream=True)
            assert status == 200, status
            assert first.calls + second.calls == 1, (first.calls, second.calls)
    finally:
        first.close()
        second.close()


if __name__ == "__main__":
    if not pathlib.Path(BIN).is_file():
        raise SystemExit(f"missing STOKE_BIN: {BIN}")
    results = [
        run_case("finite_nonstream_is_single_dispatch", finite_nonstream_is_single_dispatch),
        run_case("unlimited_nonstream_retains_failover", unlimited_nonstream_retains_failover),
        run_case("finite_stream_is_single_dispatch", finite_stream_is_single_dispatch),
        run_case("finite_priced_local_hedge_has_one_leg", finite_priced_local_hedge_has_one_leg),
        run_case("unlimited_priced_local_hedge_has_one_leg", unlimited_priced_local_hedge_has_one_leg),
        run_case("unlimited_unpriced_owned_hedge_retains_two_legs", unlimited_unpriced_owned_hedge_retains_two_legs),
        run_case("unlimited_priced_stream_race_has_one_leg", unlimited_priced_stream_race_has_one_leg),
    ]
    print(json.dumps({"passed": all(result["ok"] for result in results), "results": results}, sort_keys=True))
    raise SystemExit(0 if all(result["ok"] for result in results) else 1)
