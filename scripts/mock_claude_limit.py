#!/usr/bin/env python3
"""Mock claude_subscription upstream (Anthropic-shaped /v1/messages).

Modes via argv:
  limit          — always return the qualified 429 usage_limit_reached refusal
  ok             — serve an Anthropic message normally
Also exposes GET /count (how many POSTs reached this mock) and /mode to flip
behavior at runtime between "limit" and "ok" between requests.
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn

PORT = int(sys.argv[1])
MODE = sys.argv[2] if len(sys.argv) > 2 else "limit"

LOCK = threading.Lock()
CALLS = 0
MODE = {"value": MODE}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/count":
            with LOCK:
                self._json({"calls": CALLS, "mode": MODE["value"]})
        elif self.path.startswith("/mode/"):
            with LOCK:
                MODE["value"] = self.path.split("/mode/")[1]
            self._json({"mode": MODE["value"]})
        else:
            self._json({}, 404)

    def do_POST(self):
        global CALLS
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if "/v1/messages" not in self.path:
            self._json({}, 404)
            return
        with LOCK:
            CALLS += 1
        if MODE["value"] == "limit":
            self._json({
                "type": "error",
                "error": {"type": "usage_limit_reached", "message": "flat plan exhausted"},
            }, 429)
            return
        req = json.loads(raw or b"{}")
        self._json({
            "id": "msg-1", "type": "message", "role": "assistant",
            "model": req.get("model", "claude-fixture"),
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 10},
        })

    def log_message(self, *args):
        pass


class Threaded(ThreadingMixIn, HTTPServer):
    daemon_threads = True


Threaded(("127.0.0.1", PORT), Handler).serve_forever()
