#!/usr/bin/env python3
"""Mock Ollama node for local smoke tests. Stdlib only.

Serves the three endpoints Stoke touches:
  GET  /api/tags             -> model inventory
  GET  /api/ps               -> warm (loaded) models
  POST /v1/chat/completions  -> canned OpenAI-compatible response

Usage: mock_ollama.py <port> <node_name> <warm:0|1>
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn

PORT, NAME, WARM = int(sys.argv[1]), sys.argv[2], sys.argv[3] == "1"
MODEL = sys.argv[4] if len(sys.argv) > 4 else "testmodel:latest"


class Handler(BaseHTTPRequestHandler):
    def _send(self, obj):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/api/tags":
            self._send({"models": [{"name": MODEL}]})
        elif self.path == "/api/ps":
            self._send({"models": [{"name": MODEL}] if WARM else []})
        elif self.path == "/v1/models":
            self._send({"data": [{"id": MODEL}]})
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path == "/api/show":
            self.rfile.read(int(self.headers.get("Content-Length", 0)))
            self._send({
                "model_info": {"testarch.context_length": 32768},
                "capabilities": ["completion", "tools"],
            })
            return
        if self.path == "/v1/chat/completions":
            raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            try:
                streaming = json.loads(raw).get("stream", False)
            except Exception:
                streaming = False
            if streaming:
                # Slow SSE stream (~2s) so tests can observe in-flight counts.
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for i in range(4):
                    chunk = {"id": "mock-1", "object": "chat.completion.chunk",
                             "created": 0, "model": MODEL,
                             "choices": [{"index": 0, "finish_reason": None,
                                          "delta": {"content": f"tok{i} "}}]}
                    self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                    self.wfile.flush()
                    time.sleep(0.5)
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
                return
            self._send({
                "id": "mock-1",
                "object": "chat.completion",
                "created": 0,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "finish_reason": "stop",
                    "message": {"role": "assistant",
                                "content": f"hello from {NAME}"},
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 3,
                          "total_tokens": 4},
            })
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *args):
        pass


class ThreadingHTTPServer(ThreadingMixIn, HTTPServer):
    daemon_threads = True


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
