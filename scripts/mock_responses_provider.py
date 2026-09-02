#!/usr/bin/env python3
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

capture_path = os.environ.get("CAPTURE_FILE")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        del format, args

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        if capture_path:
            with open(capture_path, "w", encoding="utf-8") as capture:
                json.dump({
                    "path": self.path,
                    "authorization": self.headers.get("authorization"),
                    "openai_beta": self.headers.get("openai-beta"),
                    "originator": self.headers.get("originator"),
                    "headers": {key.lower(): value for key, value in self.headers.items()},
                    "body": body,
                }, capture)

        if self.path != "/v1/responses":
            self.send_error(404)
            return

        if body.get("stream"):
            events = [
                ("response.created", {
                    "type": "response.created",
                    "response": {"id": "resp_stream", "object": "response", "status": "in_progress"},
                }),
                ("response.output_item.added", {
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "id": "msg_test",
                        "type": "message",
                        "status": "in_progress",
                        "role": "assistant",
                        "content": [],
                    },
                }),
                ("response.content_part.added", {
                    "type": "response.content_part.added",
                    "item_id": "msg_test",
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                }),
                ("response.output_text.delta", {
                    "type": "response.output_text.delta",
                    "item_id": "msg_test",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "mock stream",
                }),
                ("response.output_text.done", {
                    "type": "response.output_text.done",
                    "item_id": "msg_test",
                    "output_index": 0,
                    "content_index": 0,
                    "text": "mock stream",
                }),
                ("response.content_part.done", {
                    "type": "response.content_part.done",
                    "item_id": "msg_test",
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "mock stream", "annotations": []},
                }),
                ("response.output_item.done", {
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "id": "msg_test",
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "mock stream", "annotations": []}],
                    },
                }),
                ("response.completed", {
                    "type": "response.completed",
                    "response": {
                        "id": "resp_stream",
                        "object": "response",
                        "status": "completed",
                        "model": body.get("model", "gpt-test"),
                        "output": [{
                            "id": "msg_test",
                            "type": "message",
                            "status": "completed",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "mock stream", "annotations": []}],
                        }],
                        "usage": {"input_tokens": 13, "output_tokens": 8, "total_tokens": 21},
                    },
                }),
            ]
            payload = "".join(
                f"event: {event}\ndata: {json.dumps(data)}\n\n"
                for event, data in events
            ).encode()
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        response = {
            "id": "resp_test",
            "object": "response",
            "status": "completed",
            "model": body.get("model", "gpt-test"),
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "mock response"}],
            }],
            "usage": {"input_tokens": 13, "output_tokens": 8, "total_tokens": 21},
        }
        payload = json.dumps(response).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
