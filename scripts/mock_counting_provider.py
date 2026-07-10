#!/usr/bin/env python3
"""Mock OpenAI-compatible provider that counts how many calls it receives.

The count is the whole point: it distinguishes "Stoke refused before spending"
from "Stoke spent and then errored", and it measures fan-out width.

Usage: mock_counting_provider.py <port>
  GET  /count                -> {"calls": N}
  POST /v1/chat/completions  -> canned response, 1000 prompt + 1000 completion tokens
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn

PORT = int(sys.argv[1])
LOCK = threading.Lock()
CALLS = 0


class Handler(BaseHTTPRequestHandler):
    def _send(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/count":
            self._send({"calls": CALLS})
        elif self.path.startswith("/api/tags"):
            self._send({"models": []})
        elif self.path.startswith("/api/ps"):
            self._send({"models": []})
        else:
            self._send({}, 404)

    def do_POST(self):
        global CALLS
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if "chat/completions" not in self.path:
            self._send({}, 404)
            return
        with LOCK:
            CALLS += 1
        self._send({
            "id": "mock-1",
            "object": "chat.completion",
            "created": 0,
            "model": "mock",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "42"},
            }],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 1000, "total_tokens": 2000},
        })

    def log_message(self, *args):
        pass


class Threaded(ThreadingMixIn, HTTPServer):
    daemon_threads = True


Threaded(("127.0.0.1", PORT), Handler).serve_forever()
