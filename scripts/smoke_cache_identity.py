"""Verify exact cache identity against a local mock provider.

The mock returns role labels only; this is a wire identity check, not inference
or a token/cost benchmark. STOKE_BIN must point at the built gateway binary.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import URLError
from urllib.request import Request, urlopen


binary = os.environ.get("STOKE_BIN")
if not binary:
    raise SystemExit("STOKE_BIN must point at the built stoke binary")
calls = []


class Mock(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def send_json(self, value):
        encoded = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def do_GET(self):
        self.send_json({"models": [{"name": "cache-fixture"}]})

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path != "/v1/chat/completions":
            self.send_json({})
            return
        calls.append(request)
        answer = json.dumps([message["role"] for message in request["messages"]])
        self.send_json(
            {
                "id": "mock",
                "object": "chat.completion",
                "created": 0,
                "model": "cache-fixture",
                "choices": [
                    {
                        "index": 0,
                        "finish_reason": "stop",
                        "message": {"role": "assistant", "content": answer},
                    }
                ],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            }
        )


server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="stoke-cache-identity-") as temp:
        work = Path(temp)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        key = secrets.token_hex(16)
        (work / "stoke.toml").write_text(
            f'''[server]
            host="127.0.0.1"
            port={port}
            [[providers]]
            name="fixture"
            base_url="http://127.0.0.1:{server.server_port}/v1"
            models=["cache-fixture"]
            tier="local"
            '''
        )
        env = dict(os.environ, STOKE_API_KEYS=key, STOKE_LEDGER_PATH=str(work / "ledger.db"))
        env.pop("STOKE_DEV", None)
        env.pop("STOKE_SEMANTIC_CACHE", None)
        with (work / "gateway.log").open("w") as log:
            process = subprocess.Popen(
                [binary], cwd=work, env=env, stdout=log, stderr=subprocess.STDOUT
            )
            try:
                base = f"http://127.0.0.1:{port}"
                for _ in range(50):
                    if process.poll() is not None:
                        raise RuntimeError((work / "gateway.log").read_text())
                    try:
                        with urlopen(base + "/health", timeout=1):
                            break
                    except (URLError, TimeoutError):
                        time.sleep(0.1)
                else:
                    raise RuntimeError("gateway did not become healthy")

                def ask(messages, **settings):
                    body = {
                        "model": "cache-fixture",
                        "temperature": 0,
                        "max_tokens": 64,
                        "stream": False,
                        "messages": messages,
                        **settings,
                    }
                    req = Request(
                        base + "/v1/chat/completions",
                        data=json.dumps(body).encode(),
                        headers={
                            "Authorization": "Bearer " + key,
                            "Content-Type": "application/json",
                        },
                    )
                    return json.load(urlopen(req, timeout=5))

                identical_messages = [{"role": "user", "content": "repeat"}]
                first = ask(identical_messages)
                identical = ask(identical_messages)
                assert identical.get("stoke_cache") == "hit", "identical request missed exact cache"
                assert len(calls) == 1, "identical request contacted the provider twice"

                role_a = [
                    {"role": "system", "content": "alpha"},
                    {"role": "user", "content": "beta"},
                ]
                role_b = [{"role": "user", "content": "alpha\nbeta"}]
                role_result = ask(role_a)
                role_result_b = ask(role_b)
                assert role_result_b.get("stoke_cache") != "hit", (
                    "role boundary reused cache: " + json.dumps(role_result_b)
                )
                assert len(calls) == 3, "role-distinct request did not reach provider"

                setting_result = ask(identical_messages, max_tokens=128)
                assert setting_result.get("stoke_cache") != "hit", "setting change reused cache"
                assert len(calls) == 4, "setting-distinct request did not reach provider"

                receipt = {
                    "identical_cache_status": identical.get("stoke_cache"),
                    "identical_provider_calls_after_pair": 1,
                    "role_answers": [
                        first["choices"][0]["message"]["content"],
                        role_result["choices"][0]["message"]["content"],
                        role_result_b["choices"][0]["message"]["content"],
                    ],
                    "final_provider_calls": len(calls),
                    "negative_cases": ["role/message-boundary", "max_tokens"],
                    "measurement": "mock wire identity check, not a token/cost benchmark",
                }
                print(json.dumps(receipt, indent=2))
            finally:
                process.terminate()
                process.wait(timeout=5)
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
