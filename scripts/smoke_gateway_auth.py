#!/usr/bin/env python3
"""Offline auth/destination smoke for a Stoke candidate binary.

Default mode exercises a debug binary with a loopback subscription override:
malformed gateway headers -> 401, gateway-owned OAuth with a missing store ->
503, and x-stoke-key + distinct Authorization -> successful chat through a
local mock provider. --verify-release-pins checks a production binary refuses
an unpinned subscription base at boot, without credentials or inference.
"""
import argparse, http.client, json, os, shutil, socket, subprocess, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

class Mock(BaseHTTPRequestHandler):
    calls = []
    def do_GET(self):
        raw = json.dumps({"models": [{"name": "gpt-test"}]}).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n)
        if self.path != "/api/show":
            Mock.calls.append((self.path, dict(self.headers), body))
        if self.path.endswith("/chat/completions"):
            payload = {"id":"mock-chat","object":"chat.completion","created":0,"model":"gpt-test","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}
        else:
            payload = {"id":"mock-response","object":"response","output":[]}
        raw = json.dumps(payload).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
    def log_message(self, *_): pass

def wait_health(port):
    for _ in range(100):
        try:
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=.2); c.request("GET", "/health"); r = c.getresponse()
            if r.status == 200: return
        except OSError: pass
        time.sleep(.05)
    raise RuntimeError("gateway did not become healthy")

def request(port, path, headers, body):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=3); c.request("POST", path, body=json.dumps(body), headers={"content-type":"application/json", **headers}); r=c.getresponse(); return r.status, r.read()

def malformed_request(port):
    status, _ = request(port, "/v1/chat/completions", {"x-stoke-key": "Bearer gateway-key", "authorization": "Bearer gateway-key"}, {"model":"gpt-test", "messages":[]})
    assert status == 401, status
    for path in ("/v1/chat/completions", "/v1/responses", "/v1/messages"):
        for name in ("x-stoke-key", "x-api-key"):
            status, _ = request(port, path, {name: "\xff", "authorization": "Bearer gateway-key"}, {"model": "gpt-test", "messages": []})
            assert status == 401, (path, name, status)
    return status


def verify_release(binary):
    binary = os.path.abspath(binary)
    with tempfile.TemporaryDirectory(prefix="stoke-release-pin-") as d:
        cfg=os.path.join(d,"stoke.toml")
        open(cfg,"w").write('''[server]\nhost="127.0.0.1"\nport=0\n[[providers]]\nname="bad"\ntype="codex_subscription"\nbase_url="http://203.0.113.7:43123"\ntier="subscription"\n''')
        env={**os.environ, "HOME":d, "STOKE_API_KEYS":"unused", "STOKE_TEST_SUBSCRIPTION_BASES":"http://203.0.113.7:43123"}
        p=subprocess.run([binary,"--version"], cwd=d, env=env, capture_output=True, text=True)
        if p.returncode != 0: raise AssertionError(p.stderr)
        try:
            p=subprocess.run([binary], cwd=d, env=env, capture_output=True, text=True, timeout=5)
        except subprocess.TimeoutExpired as e:
            raise AssertionError("production binary stayed up with an unpinned subscription base") from e
        assert p.returncode != 0, "production binary accepted an unpinned subscription base"
        assert "base_url" in (p.stderr+p.stdout), p.stderr
        print("release pin verification: ok")

def smoke(binary):
    binary = os.path.abspath(binary)
    with tempfile.TemporaryDirectory(prefix="stoke-auth-smoke-") as d:
        provider=ThreadingHTTPServer(("127.0.0.1",0), Mock); threading.Thread(target=provider.serve_forever,daemon=True).start(); pp=provider.server_address[1]
        port=pp+1
        open(os.path.join(d,"stoke.toml"),"w").write(f'''[server]\nhost="127.0.0.1"\nport={port}\nrouting="single"\ndefault_model="gpt-test"\n[pricing]\nunpriced="free"\n[[providers]]\nname="mock"\ntype="openai_compatible"\nbase_url="http://127.0.0.1:{pp}/v1"\napi_key_env="MOCK_KEY"\nmodels=["gpt-test"]\ntier="local"\n[[providers]]\nname="codex"\ntype="codex_subscription"\nbase_url="http://127.0.0.1:{pp}"\nmodels=["gpt-sub"]\ntier="subscription"\n[[keys]]\nkey="gateway-key"\n''')
        env={**os.environ,"HOME":d,"STOKE_API_KEYS":"gateway-key","MOCK_KEY":"mock-api-key","STOKE_TEST_SUBSCRIPTION_BASES":f"http://127.0.0.1:{pp}"}
        p=subprocess.Popen([binary],cwd=d,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True); 
        try:
            wait_health(port)
            assert malformed_request(port)==401
            status,_=request(port,"/v1/responses",{"x-stoke-key":"gateway-key","authorization":"Bearer gateway-key"},{"model":"gpt-sub","input":"x"})
            assert status==503, status
            status,_=request(port,"/v1/chat/completions",{"x-stoke-key":"gateway-key","authorization":"Bearer client-oauth"},{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]})
            assert status==200, status
            call=Mock.calls[-1]; assert call[0].endswith("/chat/completions"); assert call[1].get("authorization")=="Bearer mock-api-key"; assert "x-stoke-key" not in {k.lower() for k in call[1]}
            print("gateway auth smoke: malformed=401 missing-store=503 dual-header-chat=200")
        finally:
            p.terminate(); p.wait(timeout=3); provider.shutdown()

if __name__ == "__main__":
    ap=argparse.ArgumentParser(); ap.add_argument("binary"); ap.add_argument("--verify-release-pins",action="store_true"); a=ap.parse_args()
    (verify_release if a.verify_release_pins else smoke)(a.binary)
