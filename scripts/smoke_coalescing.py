"""Cold-overlap wire smoke for named-route exact response coalescing.

The provider delay creates a real overlap; the assertion is upstream call count
plus a distinct `stoke_cache=coalesced` marker, not a warm-cache hit.
"""
import json
import base64
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from urllib.request import Request, urlopen
from urllib.error import URLError

binary = os.environ.get("STOKE_BIN")
if not binary:
    raise SystemExit("STOKE_BIN must point at the built stoke binary")
binary = str(Path(binary).resolve())
provider_calls = []
provider_started = threading.Event()
lock = threading.Lock()


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
        self.send_json({"data": [{"id": "coalesce-fixture"}]})

    def do_POST(self):
        length = int(self.headers["Content-Length"])
        body = json.loads(self.rfile.read(length))
        if self.path != "/v1/chat/completions":
            self.send_json({})
            return
        with lock:
            provider_calls.append(body)
            provider_started.set()
        # Hold the leader in the provider so the second request must overlap it.
        time.sleep(0.35)
        self.send_json({
            "id": "coalesce-mock",
            "object": "chat.completion",
            "created": 0,
            "model": "coalesce-fixture",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "barrier answer"},
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def post(base, key, path, messages, headers=None, max_tokens=32):
    request_headers = {"Authorization": "Bearer " + key, "Content-Type": "application/json"}
    request_headers.update(headers or {})
    body = {
        "model": "ignored-by-route",
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": False,
        "messages": messages,
    }
    req = Request(base + path, data=json.dumps(body).encode(), headers=request_headers)
    with urlopen(req, timeout=5) as response:
        return json.load(response)


def get(base, key, path):
    req = Request(base + path, headers={"Authorization": "Bearer " + key})
    with urlopen(req, timeout=5) as response:
        return json.load(response)


def read_decision_feed(base, key, events, ready):
    credentials = base64.b64encode(("stoke:" + key).encode()).decode()
    req = Request(base + "/ui/events", headers={"Authorization": "Basic " + credentials})
    try:
        with urlopen(req, timeout=1) as response:
            ready.set()
            deadline = time.monotonic() + 4
            while time.monotonic() < deadline:
                try:
                    line = response.readline()
                except TimeoutError:
                    continue
                if not line:
                    return
                events.append(line.decode(errors="replace"))
                if "Coalesced deterministic response reused" in events[-1]:
                    return
    except (URLError, TimeoutError):
        return


server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="stoke-coalescing-") as temp:
        work = Path(temp)
        gateway_port = free_port()
        key = secrets.token_hex(16)
        (work / "stoke.toml").write_text(f'''[server]
host = "127.0.0.1"
port = {gateway_port}

[[providers]]
name = "fixture"
base_url = "http://127.0.0.1:{server.server_port}/v1"
models = ["coalesce-fixture"]
tier = "cloud"

[pricing.models."coalesce-fixture"]
input_per_1m = 1.0
output_per_1m = 1.0

[[routes]]
name = "joined"
path = "/v1/joined/completions"
model = "coalesce-fixture"
routing = "single"
stream = false
coalesce = true
[routes.response_cache]
mode = "exact"
ttl_secs = 60

[[routes]]
name = "plain"
path = "/v1/plain/completions"
model = "coalesce-fixture"
routing = "single"
stream = false
[routes.response_cache]
mode = "exact"
ttl_secs = 60
''')
        other_key = secrets.token_hex(16)
        env = dict(os.environ, STOKE_API_KEYS=key + "," + other_key, STOKE_LEDGER_PATH=str(work / "ledger.db"))
        env.pop("STOKE_DEV", None)
        with (work / "gateway.log").open("w") as log:
            process = subprocess.Popen([binary], cwd=work, env=env, stdout=log, stderr=subprocess.STDOUT)
            try:
                base = f"http://127.0.0.1:{gateway_port}"
                for _ in range(50):
                    try:
                        with urlopen(base + "/health", timeout=1):
                            break
                    except (URLError, TimeoutError):
                        if process.poll() is not None:
                            raise RuntimeError((work / "gateway.log").read_text())
                        time.sleep(0.1)
                else:
                    raise RuntimeError("gateway did not become healthy")

                cold = [{"role": "user", "content": "cold overlap"}]
                feed_events = []
                feed_ready = threading.Event()
                feed_thread = threading.Thread(
                    target=read_decision_feed,
                    args=(base, key, feed_events, feed_ready),
                    daemon=True,
                )
                feed_thread.start()
                assert feed_ready.wait(2), "decision feed did not open"
                with ThreadPoolExecutor(max_workers=2) as pool:
                    futures = [pool.submit(post, base, key, "/v1/joined/completions", cold) for _ in range(2)]
                    assert provider_started.wait(2), "provider barrier was not reached"
                    results = [future.result() for future in futures]
                feed_thread.join(timeout=2)
                assert len(provider_calls) == 1, f"cold overlap made {len(provider_calls)} provider calls"
                assert sum(r.get("stoke_cache") == "coalesced" for r in results) == 1, results
                assert all(r["choices"][0]["message"]["content"] == "barrier answer" for r in results)
                feed_text = "".join(feed_events)
                assert "Cache hit" in feed_text, feed_text
                assert "Coalesced deterministic response reused" in feed_text, feed_text
                budget = get(base, key, "/v1/budget")
                spend = budget["keys"][0]["spend_usd"]
                assert abs(spend - 0.000002) < 1e-9, f"joined usage charged incorrectly: {budget}"

                # Distinct full identity and scope never join the same flight.
                post(base, key, "/v1/joined/completions", cold, max_tokens=33)
                # The second key changes the A1 scope even with the same request.
                post(base, other_key, "/v1/joined/completions", cold)
                assert len(provider_calls) == 3, f"identity-distinct call count={len(provider_calls)}"

                # Coalescing is opt-in: an exact route without the flag must
                # dispatch both cold identical requests, even when they overlap.
                plain = [{"role": "user", "content": "plain cold"}]
                with ThreadPoolExecutor(max_workers=2) as pool:
                    plain_futures = [
                        pool.submit(post, base, key, "/v1/plain/completions", plain, max_tokens=40)
                        for _ in range(2)
                    ]
                    plain_results = [future.result() for future in plain_futures]
                assert len(provider_calls) == 5, f"coalescing-off call count={len(provider_calls)}"
                assert not any(r.get("stoke_cache") == "coalesced" for r in plain_results)

                # Header bypass remains normal dispatch and cannot populate exact cache.
                bypass = [{"role": "user", "content": "bypass"}]
                post(base, key, "/v1/joined/completions", bypass, {"Cache-Control": "no-store"})
                post(base, key, "/v1/joined/completions", bypass, {"Cache-Control": "no-store"})
                assert len(provider_calls) == 7, f"header-bypass call count={len(provider_calls)}"

                print(json.dumps({
                    "cold_overlap": {"requests": 2, "provider_calls": 1, "coalesced_markers": 1},
                    "metered_spend_usd": spend,
                    "distinct_identity_calls": 2,
                    "coalescing_off_calls": 2,
                    "header_bypass_calls": 2,
                    "measurement": "deterministic delayed local mock; not production frequency or savings",
                }, indent=2))
            finally:
                process.terminate()
                process.wait(timeout=5)
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
