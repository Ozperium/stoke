import os
import socket
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
LAUNCHER = HERE / "launch.py"
PYTHON = sys.executable
sys.path.insert(0, str(HERE))
from launch import _augmented_config, _validate_worker_python  # noqa: E402


class LauncherTests(unittest.TestCase):
    def test_existing_headroom_table_is_updated_without_duplication(self):
        original = (
            b'default_model = "user-provided"\n\n'
            b'[plugins.headroom]\n'
            b'enabled = true\n'
            b'url = "http://source.invalid/compress"\n'
            b'timeout_ms = 250\n'
            b'custom_setting = "preserved"\n'
        )
        result = _augmented_config(original, 4321)
        text = result.decode()
        self.assertEqual(text.count("[plugins.headroom]"), 1)
        self.assertIn('enabled = true', text)
        self.assertIn('url = "http://127.0.0.1:4321/compress"', text)
        self.assertIn("timeout_ms = 1000", text)
        self.assertIn('custom_setting = "preserved"', text)

    def test_unrelated_multiline_values_and_lookalikes_are_preserved(self):
        original = (
            b'title = """valid\n'
            b'# [plugins.headroom]\n'
            b'[plugins.headroom]\n'
            b'enabled = false\n'
            b'"""\n'
            b"literal = '''also\n"
            b"[plugins.headroom]\n"
            b"'''\n"
            b'# """ and [plugins.headroom] are comment text\n'
            b'owner = "unchanged"\n'
        )
        result = _augmented_config(original, 4321)
        parsed = tomllib.loads(result.decode())
        self.assertEqual(parsed["title"], "valid\n# [plugins.headroom]\n[plugins.headroom]\nenabled = false\n")
        self.assertEqual(parsed["literal"], "also\n[plugins.headroom]\n")
        self.assertEqual(parsed["owner"], "unchanged")
        self.assertTrue(parsed["plugins"]["headroom"]["enabled"])
        self.assertIn(b"enabled = true", result)

    def test_existing_headroom_header_whitespace_and_quoted_variants_update_once(self):
        for header in (
            b"[plugins.headroom ]",
            b"[ plugins . headroom ]",
            b'["plugins" . \'headroom\' ]',
        ):
            with self.subTest(header=header):
                original = header + b"\nenabled = false\ncustom = \"keep\"\n"
                result = _augmented_config(original, 4321)
                self.assertEqual(result.count(b"enabled ="), 1)
                self.assertEqual(result.count(b"url ="), 1)
                self.assertEqual(result.count(b"timeout_ms ="), 1)
                self.assertEqual(tomllib.loads(result.decode())["plugins"]["headroom"]["custom"], "keep")

    def test_unrelated_multiline_literal_and_basic_headers_do_not_trigger_rejection(self):
        original = b'basic = """line one\nline two"""\nliteral = \'\'\'line one\nline two\'\'\'\n'
        result = _augmented_config(original, None, enabled=False)
        parsed = tomllib.loads(result.decode())
        self.assertEqual(parsed["basic"], "line one\nline two")
        self.assertEqual(parsed["literal"], "line one\nline two")
        self.assertFalse(parsed["plugins"]["headroom"]["enabled"])

        with self.assertRaisesRegex(ValueError, "unsupported ambiguous TOML"):
            _augmented_config(b'plugins.headroom.enabled = true\n', 4321)

    def test_enabled_owns_augmented_config_and_stops_worker(self):
        with tempfile.TemporaryDirectory(prefix="headroom-launch-test-") as raw:
            directory = Path(raw)
            ready = directory / "ready"
            env_dump = directory / "worker-env"
            port_dump = directory / "worker-port"
            stoke_token = directory / "stoke-token"
            fake = directory / "fake-stoke.py"
            fake.write_text(
                "#!/usr/bin/env python3\n"
                "from pathlib import Path\n"
                "import os\n"
                "import time\n"
                f"Path({str(stoke_token)!r}).write_text(os.environ.get('STOKE_HEADROOM_TOKEN', ''))\n"
                "config = Path(os.environ['STOKE_CONFIG'])\n"
                "assert config.parent == Path.cwd()\n"
                "assert config.exists()\n"
                "text = config.read_text()\n"
                "assert '[plugins.headroom]' in text\n"
                "assert 'enabled = true' in text\n"
                "assert 'timeout_ms = 1000' in text\n"
                "assert 'http://127.0.0.1:' in text and '/compress' in text\n"
                f"Path({str(ready)!r}).write_text(str(config))\n"
                "time.sleep(30)\n"
            )
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            worker_python = directory / "worker-python-stub.py"
            worker_impl = directory / "worker-impl.py"
            worker_impl.write_text(
                "#!/usr/bin/env python3\n"
                "import sys\n"
                "import os\n"
                "from pathlib import Path\n"
                "from http.server import BaseHTTPRequestHandler, HTTPServer\n"
                "import json\n"
                "class Handler(BaseHTTPRequestHandler):\n"
                "    def do_GET(self):\n"
                "        valid = self.path == '/health' and self.headers.get('Authorization') == 'Bearer ' + os.environ['STOKE_HEADROOM_TOKEN']\n"
                "        self.send_response(200 if valid else 401)\n"
                "        self.send_header('Content-Length', '15')\n"
                "        self.end_headers()\n"
                "        self.wfile.write(b'{\"status\":\"ok\"}')\n"
                "    def log_message(self, *_args): pass\n"
                f"Path({str(env_dump)!r}).write_text('\\n'.join(sorted(os.environ)))\n"
                "ready_fd = int(sys.argv[sys.argv.index('--ready-fd') + 1])\n"
                "server = HTTPServer(('127.0.0.1', 0), Handler)\n"
                f"Path({str(port_dump)!r}).write_text(str(server.server_port))\n"
                "with os.fdopen(ready_fd, 'w') as ready:\n"
                "    ready.write(json.dumps({'port': server.server_port}) + '\\n')\n"
                "    ready.flush()\n"
                "server.serve_forever()\n"
            )
            worker_python.write_text(
                "#!/bin/sh\n"
                f"if [ \"$1\" = \"-c\" ]; then exec {sys.executable} \"$@\"; fi\n"
                f"exec {sys.executable} {worker_impl} \"$@\"\n"
            )
            worker_python.chmod(worker_python.stat().st_mode | stat.S_IEXEC)
            config = directory / "user.toml"
            original = 'default_model = "user-provided"\n'
            config.write_text(original)
            process = subprocess.Popen(
                [sys.executable, str(LAUNCHER), "--worker-python", str(worker_python),
                 "--stoke-bin", str(fake), "--config", str(config)],
                env={**os.environ, "OPENAI_API_KEY": "must-not-reach-worker"},
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.time() + 15
                while time.time() < deadline and not ready.exists():
                    time.sleep(0.05)
                self.assertTrue(ready.exists())
                self.assertEqual(config.read_text(), original)
                self.assertNotIn("OPENAI_API_KEY", env_dump.read_text())
                self.assertGreaterEqual(len(stoke_token.read_text()), 32)
            finally:
                process.terminate()
                process.wait(timeout=12)
            port = int(port_dump.read_text())
            with self.assertRaises(OSError):
                socket.create_connection(("127.0.0.1", port), timeout=1)
            self.assertFalse(list(directory.glob(".stoke-headroom-*.toml")))

    def test_worker_python_must_be_executable_and_report_a_version(self):
        with tempfile.TemporaryDirectory(prefix="headroom-python-test-") as raw:
            path = Path(raw) / "not-executable"
            path.write_text("#!/bin/sh\nexit 0\n")
            with self.assertRaisesRegex(ValueError, "executable"):
                _validate_worker_python(path)
            old_version = Path(raw) / "old-version"
            old_version.write_text("#!/bin/sh\nprintf '3.9\\n'\n")
            old_version.chmod(old_version.stat().st_mode | stat.S_IEXEC)
            with self.assertRaisesRegex(ValueError, "3.10"):
                _validate_worker_python(old_version)

    def test_worker_crash_does_not_leave_owned_config(self):
        with tempfile.TemporaryDirectory(prefix="headroom-crash-test-") as raw:
            directory = Path(raw)
            fake = directory / "fake-stoke"
            fake.write_text("#!/bin/sh\nexit 0\n")
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            worker = directory / "crash-python"
            worker.write_text(
                "#!/bin/sh\n"
                f"if [ \"$1\" = \"-c\" ]; then exec {sys.executable} \"$@\"; fi\n"
                "exit 17\n"
            )
            worker.chmod(worker.stat().st_mode | stat.S_IEXEC)
            config = directory / "config.toml"
            original = "default_model = 'x'\n"
            config.write_text(original)
            result = subprocess.run(
                [sys.executable, str(LAUNCHER), "--worker-python", str(worker),
                 "--stoke-bin", str(fake), "--config", str(config)],
                capture_output=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(config.read_text(), original)
            self.assertFalse(list(directory.glob(".stoke-headroom-*.toml")))

    def test_off_does_not_need_python_headroom(self):
        with tempfile.TemporaryDirectory(prefix="headroom-off-test-") as raw:
            directory = Path(raw)
            fake = directory / "fake-stoke"
            fake.write_text(
                "#!/bin/sh\n"
                "grep -F '[plugins.headroom]' \"$STOKE_CONFIG\" >/dev/null || exit 8\n"
                "grep -F 'enabled = false' \"$STOKE_CONFIG\" >/dev/null || exit 9\n"
                "test \"$(grep -Fc '[plugins.headroom]' \"$STOKE_CONFIG\")\" -eq 1 || exit 10\n"
                "exit 7\n"
            )
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            config = directory / "arbitrary-basename.toml"
            original = (
                'default_model = "user-provided"\n\n'
                '[plugins.headroom]\n'
                'enabled = true\n'
                'url = "http://127.0.0.1:9999/compress"\n'
            )
            config.write_text(original)
            result = subprocess.run(
                [sys.executable, str(LAUNCHER), "--off", "--stoke-bin", str(fake), "--config", str(config)],
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 7)
            self.assertEqual(config.read_text(), original)


if __name__ == "__main__":
    unittest.main()
