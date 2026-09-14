import http.client
import json
import socket
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from http.server import HTTPServer  # noqa: E402
from worker import (  # noqa: E402
    Handler,
    MAX_BODY_BYTES,
    MAX_OUTPUTS,
    compress_one,
    process_payload,
)


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.server.headroom_token = "test-token"
        self.server.socket_read_timeout = 0.2
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        connection.request(method, path, body=body, headers=headers or {})
        response = connection.getresponse()
        result = (response.status, response.read())
        connection.close()
        return result

    def test_health_and_compress_require_the_exact_bearer_token(self):
        self.assertEqual(self.request("GET", "/health")[0], 401)
        self.assertEqual(self.request("GET", "/health", headers={"Authorization": "Bearer wrong"})[0], 401)
        self.assertEqual(self.request("GET", "/health", headers={"Authorization": "Bearer test-token"})[0], 200)
        self.assertEqual(
            self.request(
                "POST", "/compress", body=b'{"outputs": []}',
                headers={"Authorization": "Bearer wrong"},
            )[0], 401,
        )

    def test_http_envelope_rejects_duplicates_lengths_transfer_encoding_and_limits(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        connection.putrequest("POST", "/compress")
        connection.putheader("Authorization", "Bearer test-token")
        connection.putheader("Content-Length", "15")
        connection.putheader("Content-Length", "15")
        connection.endheaders(b'{"outputs": []}')
        self.assertEqual(connection.getresponse().status, 400)
        connection.close()

        for header, value in (("Content-Length", "nope"), ("Transfer-Encoding", "chunked")):
            status, _ = self.request(
                "POST", "/compress", body=b'{"outputs": []}',
                headers={"Authorization": "Bearer test-token", header: value},
            )
            self.assertEqual(status, 400)
        duplicate = b'{"outputs": [], "outputs": ["plain"]}'
        status, _ = self.request(
            "POST", "/compress", body=duplicate,
            headers={"Authorization": "Bearer test-token"},
        )
        self.assertEqual(status, 400)
        too_many = json.dumps({"outputs": ["x"] * (MAX_OUTPUTS + 1)}).encode()
        status, _ = self.request(
            "POST", "/compress", body=too_many,
            headers={"Authorization": "Bearer test-token"},
        )
        self.assertEqual(status, 413)
        status, _ = self.request(
            "POST", "/compress", body=b"{}",
            headers={"Authorization": "Bearer test-token", "Content-Length": str(MAX_BODY_BYTES + 1)},
        )
        self.assertEqual(status, 413)

    def test_partial_body_times_out_without_exposing_payload_exception(self):
        sock = socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2)
        sock.sendall(
            b"POST /compress HTTP/1.1\r\nHost: localhost\r\n"
            b"Authorization: Bearer test-token\r\nContent-Length: 15\r\n\r\n{}"
        )
        response = sock.recv(4096)
        self.assertIn(b" 400 ", response)
        self.assertNotIn(b"payload", response.lower())
        sock.close()

    def test_fixture_is_lossless_and_reduced(self):
        source = (ROOT / "fixtures" / "structured_report.json").read_text()
        result = compress_one(source)
        self.assertNotEqual(result, source)
        self.assertLess(len(result.encode()), len(source.encode()))
        self.assertEqual(json.loads(result), json.loads(source))
        self.assertEqual(_non_ws_lexemes(result), _non_ws_lexemes(source))

    def test_controls_preserve_strings_precision_and_bypass_duplicates_prose(self):
        quoted = '{"text": "quoted  \\n  whitespace", "rows": [{"id": 1}, {"id": 2}]}'
        precision = '{"value": 1.2300, "rows": [{"id": 1}, {"id": 2}]}'
        duplicate = '{"a": 1, "a": 2, "rows": [{"id": 1}, {"id": 2}]}'
        prose = "ordinary prose  exact  spacing."
        self.assertNotEqual(compress_one(quoted), quoted)
        self.assertNotEqual(compress_one(precision), precision)
        self.assertEqual(compress_one(duplicate), duplicate)
        self.assertEqual(compress_one(prose), prose)
        self.assertEqual(_non_ws_lexemes(compress_one(precision)), _non_ws_lexemes(precision))

    def test_envelope_preserves_outer_metadata_and_order(self):
        inner = '{"rows": [{"id": 1}, {"id": 2}], "note": "x  y"}'
        output = json.dumps(
            {"id": "opaque", "metadata": {"x": 1}, "output": inner},
            separators=(",", ":"),
        )
        result = process_payload({"outputs": [output, "plain"]})
        self.assertEqual(len(result["outputs"]), 2)
        outer = json.loads(result["outputs"][0])
        self.assertEqual(outer["id"], "opaque")
        self.assertEqual(outer["metadata"], {"x": 1})
        self.assertEqual(json.loads(outer["output"]), json.loads(inner))
        self.assertEqual(result["outputs"][1], "plain")

    def test_payload_shape_is_strict(self):
        for value in ({}, {"outputs": "x"}, {"outputs": [1]}):
            with self.assertRaises(ValueError):
                process_payload(value)


def _non_ws_lexemes(text):
    out = []
    in_string = escaped = False
    for char in text:
        if in_string:
            out.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
            out.append(char)
        elif not char.isspace():
            out.append(char)
    return "".join(out)


if __name__ == "__main__":
    unittest.main()
