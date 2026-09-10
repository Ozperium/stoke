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
binary = str(Path(binary).resolve())
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
        finish_reason = "length" if request.get("max_tokens") == 1 else "stop"
        self.send_json(
            {
                "id": "mock",
                "object": "chat.completion",
                "created": 0,
                "model": "cache-fixture",
                "choices": [
                    {
                        "index": 0,
                        "finish_reason": finish_reason,
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
            [[routes]]
            name="exact"
            path="/v1/exact/completions"
            model="cache-fixture"
            routing="single"
            stream=false
            [routes.response_cache]
            mode="exact"
            ttl_secs=60
            [[routes]]
            name="off"
            path="/v1/off/completions"
            model="cache-fixture"
            routing="single"
            stream=false
            [routes.response_cache]
            mode="off"
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

                def ask(messages, path="/v1/chat/completions", headers=None, **settings):
                    body = {
                        "model": "cache-fixture",
                        "temperature": 0,
                        "max_tokens": 64,
                        "stream": False,
                        "messages": messages,
                        **settings,
                    }
                    request_headers = {
                        "Authorization": "Bearer " + key,
                        "Content-Type": "application/json",
                    }
                    request_headers.update(headers or {})
                    req = Request(
                        base + path,
                        data=json.dumps(body).encode(),
                        headers=request_headers,
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

                exact_messages = [{"role": "user", "content": "policy exact"}]
                exact_first = ask(exact_messages, "/v1/exact/completions")
                exact_second = ask(exact_messages, "/v1/exact/completions")
                assert exact_second.get("stoke_cache") == "hit", "exact route missed full-identity hit"
                assert len(calls) == 5, "exact route contacted provider twice"

                off_messages = [{"role": "user", "content": "policy off"}]
                off_first = ask(off_messages, "/v1/off/completions")
                off_second = ask(off_messages, "/v1/off/completions")
                assert off_first.get("stoke_cache") != "hit" and off_second.get("stoke_cache") != "hit"
                assert len(calls) == 7, "off route unexpectedly reused a response"

                bypass_messages = [{"role": "user", "content": "policy bypass"}]
                bypass = ask(bypass_messages, "/v1/exact/completions", {"Cache-Control": "No-Store"})
                after_bypass = ask(bypass_messages, "/v1/exact/completions")
                after_bypass_hit = ask(bypass_messages, "/v1/exact/completions")
                assert bypass.get("stoke_cache") != "hit"
                assert after_bypass.get("stoke_cache") != "hit", "bypass populated exact cache"
                assert after_bypass_hit.get("stoke_cache") == "hit"
                assert len(calls) == 9, "header bypass did not force exactly one provider call"

                incomplete_messages = [{"role": "user", "content": "policy incomplete"}]
                incomplete = ask(incomplete_messages, "/v1/exact/completions", max_tokens=1)
                incomplete_again = ask(incomplete_messages, "/v1/exact/completions", max_tokens=1)
                assert incomplete.get("stoke_cache") != "hit" and incomplete_again.get("stoke_cache") != "hit"
                assert len(calls) == 11, "non-cacheable completion populated exact cache"

                receipt = {
                    "identical_cache_status": identical.get("stoke_cache"),
                    "identical_provider_calls_after_pair": 1,
                    "role_answers": [
                        first["choices"][0]["message"]["content"],
                        role_result["choices"][0]["message"]["content"],
                        role_result_b["choices"][0]["message"]["content"],
                    ],
                    "final_provider_calls": len(calls),
                    "policy_routes": {"exact_calls": 1, "off_calls": 2, "header_bypass_calls": 2, "incomplete_calls": 2},
                    "negative_cases": ["role/message-boundary", "max_tokens", "no-store", "truncated_completion"],
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
