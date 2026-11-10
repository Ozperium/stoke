"""Fail-closed native CI selection for PRs and the rolling nightly pipeline."""
import json
import os
from pathlib import Path
import re
import subprocess

WEBSITE_FILES = {"README.md", "CHANGELOG.md", "AGENTS.md", "scripts/build_guides.py", "scripts/check_landing.py"}


def website_only(path):
    return path.startswith(("landing/", "docs/")) or path in WEBSITE_FILES


def commit_id(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value) or value == "0" * 40:
        raise ValueError("Expected a nonzero full Git commit ID")
    return value


def native_required(event_name, event, repo=Path(".")):
    if event_name == "pull_request":
        pull = event["pull_request"]
        base = commit_id(pull["base"]["sha"])
        head = commit_id(pull["head"]["sha"])
        revisions = [f"{base}...{head}"]
    elif event_name == "push" and event.get("ref") == "refs/heads/main":
        head = commit_id(event["after"])
        # Compare against the published product, not push.before: a docs push may
        # supersede an in-progress native build whose changes still need to ship.
        nightly = subprocess.run(
            ["git", "rev-parse", "--verify", "refs/tags/nightly^{commit}"],
            cwd=repo, capture_output=True, text=True,
        )
        if nightly.returncode:
            return True
        base = commit_id(nightly.stdout.strip())
        ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", base, head], cwd=repo)
        if ancestor.returncode:
            return True
        revisions = [base, head]
    else:
        # Manual runs are the explicit full-verification override.
        return True
    changed = subprocess.check_output(
        ["git", "diff", "--name-only", "--no-renames", "-z", *revisions, "--"], cwd=repo,
    )
    paths = [os.fsdecode(path) for path in changed.split(b"\0") if path]
    return not paths or any(not website_only(path) for path in paths)


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    required = native_required(os.environ["GITHUB_EVENT_NAME"], event)
    value = str(required).lower()
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"native={value}\n")
    print(f"Native verification and packaging required: {value}")


if __name__ == "__main__":
    main()
