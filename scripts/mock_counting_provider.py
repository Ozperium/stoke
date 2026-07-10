#!/usr/bin/env python3
"""Mock OpenAI-compatible provider that counts what the gateway asks of it.

The counters are the whole point. `calls` distinguishes "Stoke refused before
spending" from "Stoke spent and then errored", which look identical from the
client's side. `stream_options_seen` proves whether Stoke asked the provider to
report token usage — you cannot bill a stream from a measurement you never
requested.

Usage: mock_counting_provider.py <port> [--no-usage]
  --no-usage   stream without ever reporting usage, so the gateway must estimate

  GET  /count                -> {"calls": N, "stream_options_seen": M}
  POST /v1/chat/completions  -> 1000 prompt + 1000 completion tokens,
                                streamed as SSE when the request sets stream:true
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn

PORT = int(sys.argv[1])
REPORT_USAGE = "--no-usage" not in sys.argv[2:]

LOCK = threading.Lock()
CALLS = 0
STREAM_OPTIONS_SEEN = 0

USAGE = {"prompt_tokens": 1000, "completion_tokens": 1000, "total_tokens": 2000}


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
            self._json({"calls": CALLS, "stream_options_seen": STREAM_OPTIONS_SEEN})
        elif self.path.startswith("/api/tags") or self.path.startswith("/api/ps"):
            self._json({"models": []})
        else:
            self._json({}, 404)

    def do_POST(self):
        global CALLS, STREAM_OPTIONS_SEEN
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if "chat/completions" not in self.path:
            self._json({}, 404)
            return
        try:
            req = json.loads(raw or b"{}")
        except ValueError:
            req = {}

        with LOCK:
            CALLS += 1
            if "stream_options" in req:
                STREAM_OPTIONS_SEEN += 1

        if req.get("stream"):
            self._stream(req)
        else:
            self._json({
                "id": "mock-1", "object": "chat.completion", "created": 0,
                "model": req.get("model", "mock"),
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "42"}}],
                "usage": USAGE,
            })

    def _stream(self, req):
        model = req.get("model", "mock")
        wants_usage = bool(req.get("stream_options", {}).get("include_usage"))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        def frame(obj):
            payload = ("data: " + json.dumps(obj) + "\n\n").encode()
            self.wfile.write(hex(len(payload))[2:].encode() + b"\r\n" + payload + b"\r\n")
            self.wfile.flush()

        for tok in ("4", "2"):
            frame({"id": "mock-1", "object": "chat.completion.chunk", "created": 0,
                   "model": model,
                   "choices": [{"index": 0, "delta": {"content": tok}}],
                   "usage": None})

        # Report usage only if asked, and only if this instance is willing to.
        if wants_usage and REPORT_USAGE:
            frame({"id": "mock-1", "object": "chat.completion.chunk", "created": 0,
                   "model": model, "choices": [], "usage": USAGE})

        done = b"data: [DONE]\n\n"
        self.wfile.write(hex(len(done))[2:].encode() + b"\r\n" + done + b"\r\n")
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def log_message(self, *args):
        pass


class Threaded(ThreadingMixIn, HTTPServer):
    daemon_threads = True


Threaded(("127.0.0.1", PORT), Handler).serve_forever()
