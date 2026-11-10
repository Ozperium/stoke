"""CI and release workflows must share one complete main verification."""
import json
import os
from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github/workflows"
NATIVE_CHECKS = (
    "build + test + smoke",
    "optional Headroom — macOS arm64",
    "JS plugin chains (V8 feature)",
)


class WorkflowTests(unittest.TestCase):
    def test_release_is_the_only_automatic_main_ci_caller(self):
        ci = (WORKFLOWS / "ci.yml").read_text()
        triggers = ci.split("concurrency:", 1)[0]
        self.assertNotRegex(triggers, r"(?m)^  push:")
        self.assertIn("  pull_request:", triggers)
        self.assertIn("  workflow_dispatch:", triggers)
        self.assertIn("  workflow_call:", triggers)
        release = (WORKFLOWS / "release.yml").read_text()
        self.assertIn("  push:\n    branches: [main]", release)
        self.assertIn("  verify:\n    uses: ./.github/workflows/ci.yml", release)
        self.assertIn("    needs: verify", release)
        for name in NATIVE_CHECKS:
            self.assertIn(f"    name: {name}", ci)

    def promotion_gate(self, checks):
        source = (WORKFLOWS / "promote-nightly.yml").read_text()
        match = re.search(r"(?m)^          for NAME in .*?^          done$", source, re.S)
        if match is None:
            self.fail("Missing promotion check loop")
        script = "set -euo pipefail\n" + match.group(0)
        return subprocess.run(
            ["bash", "-c", script],
            env={"PATH": os.environ["PATH"], "CHECKS": json.dumps({"check_runs": checks})},
            capture_output=True,
            text=True,
        ).returncode

    def test_promotion_accepts_successful_release_verification_checks(self):
        checks = [{"name": f"verify / {name}", "status": "completed", "conclusion": "success"} for name in NATIVE_CHECKS]
        self.assertEqual(self.promotion_gate(checks), 0)

    def test_promotion_rejects_missing_failed_or_unprefixed_checks(self):
        checks = [{"name": f"verify / {name}", "status": "completed", "conclusion": "success"} for name in NATIVE_CHECKS]
        self.assertNotEqual(self.promotion_gate(checks[:-1]), 0)
        for index in range(len(checks)):
            with self.subTest(check=NATIVE_CHECKS[index]):
                failed = [dict(check) for check in checks]
                failed[index]["conclusion"] = "failure"
                self.assertNotEqual(self.promotion_gate(failed), 0)
        standalone = [dict(check, name=name) for check, name in zip(checks, NATIVE_CHECKS)]
        self.assertNotEqual(self.promotion_gate(standalone), 0)


if __name__ == "__main__":
    unittest.main()
