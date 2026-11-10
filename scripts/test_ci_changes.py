"""Exercise CI selection against real Git histories, not mocked diff output."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

from scripts.ci_changes import native_required, website_only

ROOT = Path(__file__).resolve().parents[1]


class ChangeSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="stoke-ci-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q", "-b", "main")
        self.write("src/main.rs", "fn main() {}\n")
        self.write("landing/index.html", "hello\n")
        self.write("scripts/ci_changes.py", (ROOT / "scripts/ci_changes.py").read_text())
        self.baseline = self.commit()
        self.git("tag", "nightly")

    def git(self, *args):
        return subprocess.check_output(
            ["git", "-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", "-c", "core.hooksPath=/dev/null", *args],
            cwd=self.repo, text=True,
        ).strip()

    def write(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def push(self, head, before=None):
        return {"ref": "refs/heads/main", "after": head, "before": before or self.baseline}

    def test_allowlist_excludes_embedded_assets_integrations_and_unknown_files(self):
        for path in ("landing/index.html", "landing/install.sh", "docs/ci.md", "README.md", "scripts/build_guides.py", "scripts/check_landing.py"):
            with self.subTest(path=path):
                self.assertTrue(website_only(path))
        for path in ("src/main.rs", "src/prompt.md", "assets/dashboard.css", "integrations/headroom/README.md", "Cargo.lock", ".github/workflows/ci.yml", "scripts/smoke.sh", "new-build-input", "landing"):
            with self.subTest(path=path):
                self.assertFalse(website_only(path))

    def test_website_only_push_skips_native_work(self):
        self.write("landing/index.html", "new website\n")
        self.assertFalse(native_required("push", self.push(self.commit()), self.repo))

    def test_unreleased_native_change_is_not_hidden_by_a_later_docs_push(self):
        self.write("src/main.rs", "fn main() { println!(\"new\"); }\n")
        native = self.commit()
        self.write("docs/ci.md", "docs\n")
        head = self.commit()
        self.assertTrue(native_required("push", self.push(head, before=native), self.repo))
        self.git("tag", "-f", "nightly", native)
        self.assertFalse(native_required("push", self.push(head, before=native), self.repo))

    def test_native_deletion_and_rename_into_docs_require_checks(self):
        self.git("mv", "src/main.rs", "landing/main.rs")
        self.assertTrue(native_required("push", self.push(self.commit()), self.repo))

    def test_missing_nightly_and_empty_diff_fail_closed(self):
        self.assertTrue(native_required("push", self.push(self.baseline), self.repo))
        self.git("tag", "-d", "nightly")
        self.write("docs/ci.md", "docs\n")
        self.assertTrue(native_required("push", self.push(self.commit()), self.repo))

    def test_pr_uses_merge_base_not_unrelated_base_branch_changes(self):
        self.write("src/main.rs", "main-only native change\n")
        base = self.commit()
        self.git("checkout", "-qb", "website-pr", self.baseline)
        self.write("landing/index.html", "PR website\n")
        head = self.commit()
        event = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        self.assertFalse(native_required("pull_request", event, self.repo))
        self.write("assets/dashboard.css", "embedded asset\n")
        event["pull_request"]["head"]["sha"] = self.commit()
        self.assertTrue(native_required("pull_request", event, self.repo))

    def test_dispatch_unknown_events_and_non_main_pushes_fail_closed(self):
        for name in ("workflow_dispatch", "workflow_call", "unexpected"):
            self.assertTrue(native_required(name, {}, self.repo))
        event = self.push(self.baseline)
        event["ref"] = "refs/heads/feature"
        self.assertTrue(native_required("push", event, self.repo))

    def test_invalid_commit_ids_are_rejected(self):
        for value in ("HEAD", "-h", "0" * 40, "a" * 39, "a" * 40 + "\n"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                native_required("push", self.push(value), self.repo)

    def test_cli_writes_the_real_github_output(self):
        self.write("docs/ci.md", "docs\n")
        event = self.repo / "event.json"
        output = self.repo / "output"
        event.write_text(json.dumps(self.push(self.commit())))
        env = dict(os.environ, GITHUB_EVENT_NAME="push", GITHUB_EVENT_PATH=str(event), GITHUB_OUTPUT=str(output))
        result = subprocess.run([sys.executable, str(ROOT / "scripts/ci_changes.py")], cwd=self.repo, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), "native=false\n")

    def select_with_workflow_shell(self, base, head):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        body = workflow.split("        id: filter\n", 1)[1].split("        run: |\n", 1)[1].split("\n      - ", 1)[0]
        event = self.repo / "event.json"
        output = self.repo / "output"
        event.write_text(json.dumps({"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}))
        env = dict(os.environ, GITHUB_EVENT_NAME="pull_request", GITHUB_EVENT_PATH=str(event), GITHUB_OUTPUT=str(output), PR_BASE_SHA=base, RUNNER_TEMP=str(self.repo / "runner-temp"))
        Path(env["RUNNER_TEMP"]).mkdir()
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", textwrap.dedent(body)], cwd=self.repo, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return output.read_text()

    def test_pr_cannot_disable_native_checks_by_replacing_its_selector(self):
        self.write("src/main.rs", "untrusted native change\n")
        self.write("scripts/ci_changes.py", 'import os\nwith open(os.environ["GITHUB_OUTPUT"], "a") as output:\n    output.write("native=false\\n")\n')
        self.assertEqual(self.select_with_workflow_shell(self.baseline, self.commit()), "native=true\n")

    def test_trusted_selector_allows_docs_and_bootstraps_missing_base_safely(self):
        self.write("docs/ci.md", "docs\n")
        self.assertEqual(self.select_with_workflow_shell(self.baseline, self.commit()), "native=false\n")

    def test_base_without_selector_forces_all_native_checks(self):
        self.git("rm", "scripts/ci_changes.py")
        base = self.commit()
        self.write("docs/ci.md", "docs\n")
        self.assertEqual(self.select_with_workflow_shell(base, self.commit()), "native=true\n")


if __name__ == "__main__":
    unittest.main()
