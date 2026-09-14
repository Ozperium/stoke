#!/usr/bin/env python3
"""Offline Headroom contract smoke: worker spy + provider spy, no live calls."""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen

GATEWAY_KEY = "gateway-test-key"
WORKER_TOKEN = "worker-test-token"
RAW_CREDENTIAL = "raw-gateway-credential-must-not-leak"

class SpyState:
    def __init__(self):
        self.lock = threading.Lock()
        self.worker = []
        self.provider = []

class SpyHandler(BaseHTTPRequestHandler):
    state: SpyState
    role: str

    def log_message(self, *_args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length)
        with self.state.lock:
            getattr(self.state, self.role).append({
                "headers": dict(self.headers), "body": body,
            })
        if self.role == "worker":
            incoming = json.loads(body)
            assert set(incoming) == {"outputs"}, incoming
            assert self.headers.get("authorization") == f"Bearer {WORKER_TOKEN}"
            output = incoming["outputs"][0]
            # Valid lexical JSON minification; all non-output bytes stay gateway-owned.
            replacement = output.replace(" ", "")
            payload = json.dumps({"outputs": [replacement]}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        request = json.loads(body)
        response = {"id": "spy", "output": request["input"][0]["output"], "usage": {"input_tokens": 1, "output_tokens": 1}}
        if request.get("stream"):
            payload = b'data: {"type":"response.completed"}\n\n'
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        else:
            payload = json.dumps(response).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)


def start_spy(state: SpyState, role: str):
    handler = type(f"{role}Handler", (SpyHandler,), {"state": state, "role": role})
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def wait_health(base: str, proc: subprocess.Popen):
    for _ in range(100):
        if proc.poll() is not None:
            raise RuntimeError(f"gateway exited early: {proc.returncode}")
        try:
            with urlopen(base + "/health", timeout=0.2) as response:
                if response.status == 200:
                    return
        except Exception:
            time.sleep(0.05)
    raise RuntimeError("gateway health timeout")


def call(base: str, body: dict, key: str = GATEWAY_KEY):
    request = Request(
        base + "/v1/responses",
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json", "x-stoke-key": key},
    )
    try:
        with urlopen(request, timeout=5) as response:
            return response.status, dict(response.headers), response.read()
    except HTTPError as error:
        return error.code, dict(error.headers), error.read()


def config(port: int, provider_port: int, worker_port: int, tier: str = "local"):
    return f'''[server]\nhost = "127.0.0.1"\nport = {port}\n\n[[providers]]\nname = "spy-provider"\ntype = "openai_compatible"\nbase_url = "http://127.0.0.1:{provider_port}/v1"\ntier = "{tier}"\nmodels = ["spy-model"]\n\n[plugins.headroom]\nenabled = true\nurl = "http://127.0.0.1:{worker_port}/compress"\ntimeout_ms = 1000\n'''


def main():
    binary = os.environ.get("STOKE_BINARY", "target/debug/stoke")
    if not os.path.isfile(binary):
        raise SystemExit(f"missing {binary}; run cargo build first")
    state = SpyState()
    worker = start_spy(state, "worker")
    provider = start_spy(state, "provider")
    procs = []
    logs = []
    try:
        with tempfile.TemporaryDirectory(prefix="stoke-headroom-smoke-") as directory:
            def launch(tier="local", with_token=True):
                with socket.socket() as probe:
                    probe.bind(("127.0.0.1", 0))
                    port = probe.getsockname()[1]
                cfg = os.path.join(directory, f"config-{port}.toml")
                with open(cfg, "w", encoding="utf-8") as handle:
                    handle.write(config(port, provider.server_address[1], worker.server_address[1], tier))
                env = {**os.environ, "STOKE_CONFIG": cfg, "STOKE_API_KEYS": GATEWAY_KEY}
                if with_token:
                    env["STOKE_HEADROOM_TOKEN"] = WORKER_TOKEN
                else:
                    env.pop("STOKE_HEADROOM_TOKEN", None)
                proc = subprocess.Popen([binary], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                procs.append(proc)
                base = f"http://127.0.0.1:{port}"
                wait_health(base, proc)
                return base

            body = {"model": "spy-model", "stream": False, "metadata": {"keep": True}, "tools": [{"name": "keep"}], "input": [{"type": "function_call_output", "call_id": "c1", "output": '{"a": 1, "secret": "output-only"}'}]}
            base = launch()
            before = len(state.worker), len(state.provider)
            status, headers, result = call(base, body)
            assert status == 200 and headers.get("x-stoke-headroom") == "compressed", (status, headers, result)
            assert len(state.worker) == before[0] + 1 and len(state.provider) == before[1] + 1
            assert b"metadata" not in state.worker[-1]["body"] and RAW_CREDENTIAL.encode() not in state.worker[-1]["body"]
            assert json.loads(state.provider[-1]["body"])["tools"] == body["tools"]

            streamed = dict(body, stream=True)
            status, headers, _ = call(base, streamed)
            assert status == 200 and headers.get("x-stoke-headroom") == "compressed", (status, headers)
            assert len(state.worker) == before[0] + 2 and len(state.provider) == before[1] + 2

            denied_before = len(state.worker), len(state.provider)
            status, _, _ = call(base, body, key="wrong-key")
            assert status == 401 and (len(state.worker), len(state.provider)) == denied_before

            no_token = launch(with_token=False)
            denied_before = len(state.worker), len(state.provider)
            status, headers, _ = call(no_token, body)
            assert status == 200 and headers.get("x-stoke-headroom") == "unavailable"
            assert len(state.worker) == denied_before[0] and len(state.provider) == denied_before[1] + 1

            denied = launch("cloud")
            denied_before = len(state.worker), len(state.provider)
            status, _, _ = call(denied, body)
            assert status == 403 and (len(state.worker), len(state.provider)) == denied_before

            for proc in procs:
                if proc.poll() is None:
                    proc.terminate()
            for proc in procs:
                stdout, stderr = proc.communicate(timeout=2)
                logs.extend([stdout, stderr])
            assert all(WORKER_TOKEN not in log and GATEWAY_KEY not in log and RAW_CREDENTIAL not in log for log in logs)
            print("headroom smoke: PASS (buffered+streaming, auth/admission deny, worker token, no payload/credential logs)")
    finally:
        for proc in procs:
            if proc.poll() is None:
                proc.terminate()
        worker.shutdown(); provider.shutdown()

if __name__ == "__main__":
    main()
