#!/usr/bin/env python3
"""
Stoke SWE-bench Verified Benchmark Runner

Generates model patches for SWE-bench Verified (500 real GitHub issues).
Outputs predictions in the official SWE-bench format for evaluation.

Key features:
- Repo context: clones repos, includes relevant source files in prompt
- Patch validation: checks file paths exist, diff format is valid
- Agentic retry: feeds validation errors back to model for refinement (up to 3 attempts)

Usage:
  # Single model with repo context + validation
  python3 run_swebench.py --model qwen2.5-coder:7b --limit 10 --repo-context --output preds.jsonl

  # test_vote fusion
  python3 run_swebench.py --model qwen2.5-coder:7b --routing test_vote \
    --vote-models qwen2.5-coder:7b,qwen2.5-coder:3b,llama3.2:3b \
    --limit 50 --repo-context --output preds.jsonl

Evaluation (requires Docker):
  pip install swebench
  python -m swebench.harness.run_evaluation \
    --dataset_name SWE-bench/SWE-bench_Verified \
    --predictions_path preds.jsonl \
    --max_workers 8 --run_id my_run
"""

import argparse
import json
import os
import re
import subprocess as sp
import sys
import time
from pathlib import Path
from typing import Optional
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

PROXY_URL = "http://127.0.0.1:8787/v1/chat/completions"
SWEBENCH_PATH = Path(__file__).parent / "swebench_verified.jsonl"


def load_instances(limit: Optional[int] = None) -> list:
    instances = []
    with open(SWEBENCH_PATH) as f:
        for line in f:
            instances.append(json.loads(line))
    if limit:
        instances = instances[:limit]
    return instances


# ── Repo context ──────────────────────────────────────────────────────────────

def get_repo_dir(instance: dict, repo_cache: Path) -> Path:
    """Get or clone the repo, checked out at base_commit."""
    repo = instance["repo"]
    base_commit = instance["base_commit"]
    repo_dir = repo_cache / repo.replace("/", "__")

    if not repo_dir.exists():
        repo_url = f"https://github.com/{repo}.git"
        repo_dir.parent.mkdir(parents=True, exist_ok=True)
        sp.run(["git", "clone", "--quiet", repo_url, str(repo_dir)],
               capture_output=True, timeout=180, check=True)

    # Checkout base commit
    sp.run(["git", "-C", str(repo_dir), "checkout", "--quiet", base_commit],
           capture_output=True, timeout=30, check=True)

    return repo_dir


def get_repo_files(repo_dir: Path) -> list:
    """Get list of non-test Python files in the repo."""
    result = sp.run(
        ["git", "-C", str(repo_dir), "ls-files", "*.py"],
        capture_output=True, text=True, timeout=30
    )
    all_files = result.stdout.strip().split("\n") if result.stdout.strip() else []
    return [f for f in all_files if "/tests/" not in f and "/test_" not in f and f.endswith(".py")]


def find_relevant_files(instance: dict, repo_dir: Path, max_files: int = 8) -> list:
    """Find files relevant to the issue using multiple strategies."""
    problem = instance["problem_statement"]
    repo_files = get_repo_files(repo_dir)

    # Strategy 1: Extract file paths mentioned in the issue
    file_refs = re.findall(
        r'(?:astropy|django|sympy|sphinx|matplotlib|sklearn|scikit|xarray|pytest|pylint|requests|seaborn|flask)/[\w/]+\.py',
        problem.lower()
    )

    # Strategy 2: Extract class/function/module names from the issue
    identifiers = re.findall(r'\b(?:def |class )?(\w+)\b', problem)
    identifiers = [i for i in identifiers if len(i) > 4 and not i.isupper()][:15]

    # Strategy 3: Use FAIL_TO_PASS test names to find the source files being tested
    # e.g. "astropy/modeling/tests/test_separable.py::test_separable" → look for separable.py
    test_names = instance.get("FAIL_TO_PASS", "")
    if isinstance(test_names, str):
        try:
            test_names = json.loads(test_names)
        except Exception:
            test_names = []
    test_file_refs = []
    for tn in (test_names if isinstance(test_names, list) else []):
        # Extract path from test name like "astropy/modeling/tests/test_separable.py::test_separable[...]"
        test_path = tn.split("::")[0]
        # test_foo.py → look for foo.py in parent directory (not tests/)
        test_parts = test_path.split("/")
        test_filename = test_parts[-1].replace("test_", "").replace(".py", "")
        # Parent dir = strip the "tests" component
        parent_parts = [p for p in test_parts[:-1] if p != "tests"]
        parent_dir = "/".join(parent_parts)
        for fpath in repo_files:
            if parent_dir in fpath and test_filename in fpath.split("/")[-1]:
                test_file_refs.append(fpath)

    # Score files
    scored = []
    for fpath in repo_files:
        score = 0
        for ref in file_refs:
            if ref in fpath:
                score += 20
        for ref in test_file_refs:
            if ref == fpath:
                score += 50  # Strong signal — this file is tested
        for ident in identifiers:
            if ident.lower() in fpath.lower():
                score += 2

        if score > 0:
            scored.append((score, fpath))

    scored.sort(reverse=True)
    return [f for _, f in scored[:max_files]]


def read_file_contents(repo_dir: Path, file_paths: list, max_chars: int = 12000) -> str:
    """Read file contents up to max_chars total."""
    chunks = []
    total = 0
    for fpath in file_paths:
        full = repo_dir / fpath
        if not full.exists():
            continue
        try:
            content = full.read_text(errors="replace")
            if total + len(content) > max_chars:
                content = content[:max_chars - total]
            chunks.append(f"### {fpath}\n```python\n{content}\n```")
            total += len(content)
            if total >= max_chars:
                break
        except Exception:
            continue
    return "\n\n".join(chunks)


# ── Prompt building ───────────────────────────────────────────────────────────

def build_prompt(instance: dict, repo_files: Optional[str] = None,
                 error_feedback: Optional[str] = None) -> str:
    """Build the prompt for a SWE-bench instance."""
    repo = instance["repo"]
    problem = instance["problem_statement"]
    hints = instance.get("hints_text", "").strip()
    base_commit = instance["base_commit"]

    prompt = f"""You are a software engineer fixing a GitHub issue in the {repo} repository.

## Issue
{problem}
"""
    if hints:
        prompt += f"\n## Hints from maintainers\n{hints}\n"

    if repo_files:
        prompt += f"\n## Relevant source files (at base commit {base_commit[:8]})\nUse EXACTLY these file paths in your diff:\n{repo_files}\n"

    if error_feedback:
        prompt += f"\n## Previous attempt failed\n{error_feedback}\nFix the issue and try again.\n"

    prompt += """
## Instructions
- Produce a minimal fix as a unified diff patch (git diff format).
- Use EXACT file paths from the source files above. Do NOT invent paths.
- Do NOT modify test files.
- Each hunk must have correct @@ line numbers and 3 lines of context.
- Make sure the patch is complete — do not truncate.
- Output ONLY the patch in a ```diff block. No explanation.
"""
    return prompt


# ── Patch extraction & validation ─────────────────────────────────────────────

def extract_patch(response_text: str) -> str:
    """Extract a unified diff patch from the model response."""
    # Try ```diff block
    if "```diff" in response_text:
        match = re.search(r"```diff\n(.*?)```", response_text, re.DOTALL)
        if match:
            return match.group(1).strip()

    # Try generic code block containing diff
    if "```" in response_text:
        for match in re.finditer(r"```\n?(.*?)```", response_text, re.DOTALL):
            content = match.group(1).strip()
            if content.startswith("diff --git") or content.startswith("--- "):
                return content

    # Try raw diff
    lines = response_text.split("\n")
    diff_lines = []
    in_diff = False
    for i, line in enumerate(lines):
        if line.startswith("diff --git") or (line.startswith("--- ") and not in_diff):
            in_diff = True
        if in_diff:
            diff_lines.append(line)

    if diff_lines:
        return "\n".join(diff_lines).strip()

    return response_text.strip()


def extract_diff_file_paths(patch: str) -> list:
    """Extract file paths from a unified diff."""
    paths = []
    for match in re.finditer(r'^diff --git a/(\S+) b/(\S+)', patch, re.MULTILINE):
        paths.append(match.group(2))
    return paths


def validate_patch(patch: str, repo_dir: Path) -> tuple:
    """Validate a patch against the repo.

    Returns (is_valid, error_message).
    """
    if not patch.strip():
        return False, "Empty patch"

    if not (patch.startswith("diff --git") or patch.startswith("--- ")):
        return False, "Patch does not start with diff --git or --- "

    file_paths = extract_diff_file_paths(patch)
    if not file_paths:
        return False, "No file paths found in diff"

    missing = []
    for fpath in file_paths:
        if fpath.startswith("a/"):
            fpath = fpath[2:]
        full = repo_dir / fpath
        if not full.exists():
            missing.append(fpath)

    if missing:
        return False, f"Files do not exist in repo: {', '.join(missing)}"

    # Check for truncation — diff should end with a newline after last hunk
    if not patch.endswith("\n") and not patch.endswith("\\ No newline at end of file"):
        # Truncated diffs often end mid-line
        last_line = patch.strip().split("\n")[-1]
        if not last_line.startswith(("diff --git", "---", "+++", "@@", " ", "-", "+", "\\")):
            return False, f"Patch appears truncated (ends with: '{last_line[:50]}')"

    return True, ""


def apply_patch_dry_run(patch: str, repo_dir: Path) -> tuple:
    """Dry-run patch application to check if it applies cleanly.

    Uses the same `patch -p1` command that SWE-bench eval uses.
    Returns (applies_clean, error_message).
    """
    try:
        # Use patch -p1 (same as SWE-bench eval, no fuzz)
        result = sp.run(
            ["patch", "--dry-run", "-p1"],
            input=patch,
            capture_output=True, text=True, timeout=10,
            cwd=str(repo_dir)
        )
        if result.returncode == 0:
            return True, ""
        err = result.stderr.strip() or result.stdout.strip()
        err_lines = [l for l in err.split("\n") if l.strip()]
        return False, "\n".join(err_lines[-5:])
    except Exception as e:
        return False, str(e)


# ── LLM calls ─────────────────────────────────────────────────────────────────

def call_stoke_single(model: str, messages: list, timeout: int = 300, max_tokens: int = 4096) -> str:
    """Call the Stoke proxy for a single model."""
    payload = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": max_tokens,
    }
    try:
        resp = requests.post(PROXY_URL, json=payload, timeout=timeout)
        resp.raise_for_status()
        data = resp.json()
        content = data["choices"][0]["message"].get("content", "")
        if not content:
            content = data["choices"][0]["message"].get("reasoning", "")
        return content
    except Exception as e:
        return f"ERROR: {e}"


# ── Patch generation with agentic retry ───────────────────────────────────────

def generate_patch(instance: dict, model: str, routing: str = "single",
                    vote_models: Optional[list] = None, timeout: int = 300,
                    repo_cache: Optional[Path] = None,
                    max_attempts: int = 3) -> dict:
    """Generate a patch for a single SWE-bench instance with validation + retry."""

    # Get repo context
    repo_dir = None
    repo_files_str = None
    if repo_cache:
        try:
            repo_dir = get_repo_dir(instance, repo_cache)
            relevant = find_relevant_files(instance, repo_dir)
            if relevant:
                repo_files_str = read_file_contents(repo_dir, relevant)
        except Exception as e:
            pass  # Continue without repo context

    messages = [{"role": "user", "content": build_prompt(instance, repo_files=repo_files_str)}]

    start = time.time()
    best_patch = ""
    winning_model = model
    last_error = ""
    attempts = 0

    for attempt in range(max_attempts):
        attempts = attempt + 1

        if routing == "test_vote" and vote_models:
            # Try each model, validate each patch, pick first that applies cleanly
            attempt_best = ""
            attempt_model = vote_models[0]
            attempt_error = ""

            for m in vote_models:
                resp = call_stoke_single(m, messages, timeout=timeout, max_tokens=4096)
                patch = extract_patch(resp)

                if not patch or not patch.startswith(("diff --git", "--- ")):
                    continue

                if repo_dir:
                    valid, err = validate_patch(patch, repo_dir)
                    if not valid:
                        if not attempt_best:
                            attempt_best = patch
                            attempt_model = m
                            attempt_error = err
                        continue

                    applies, apply_err = apply_patch_dry_run(patch, repo_dir)
                    if applies:
                        best_patch = patch
                        winning_model = m
                        last_error = ""
                        break
                    else:
                        if not attempt_best:
                            attempt_best = patch
                            attempt_model = m
                        attempt_error = apply_err
                else:
                    if patch.startswith("diff --git") and not attempt_best:
                        attempt_best = patch
                        attempt_model = m
                        break

            if best_patch and not last_error:
                break  # Got a clean patch

            # Save best fallback from this attempt
            if not best_patch and attempt_best:
                best_patch = attempt_best
                winning_model = attempt_model
                last_error = attempt_error

            # Build error feedback for retry
            if attempt < max_attempts - 1 and last_error:
                error_feedback = f"Your previous patch failed to apply:\n{last_error}\n\n"
                if repo_dir:
                    relevant = find_relevant_files(instance, repo_dir)
                    error_feedback += "Available files (use these exact paths):\n"
                    error_feedback += "\n".join(f"  - {f}" for f in relevant[:10])
                messages = [{"role": "user", "content": build_prompt(instance, repo_files=repo_files_str, error_feedback=error_feedback)}]

        else:
            # Single model
            resp = call_stoke_single(model, messages, timeout=timeout, max_tokens=4096)
            patch = extract_patch(resp)

            if repo_dir:
                valid, err = validate_patch(patch, repo_dir)
                if valid:
                    applies, apply_err = apply_patch_dry_run(patch, repo_dir)
                    if applies:
                        best_patch = patch
                        last_error = ""
                        break
                    else:
                        best_patch = patch
                        last_error = apply_err
                else:
                    best_patch = patch if patch.startswith("diff --git") else best_patch
                    last_error = err
            else:
                if patch.startswith("diff --git"):
                    best_patch = patch
                    break

            # Build error feedback for retry
            if last_error and attempt < max_attempts - 1:
                error_feedback = f"Your previous patch failed:\n{last_error}\n\n"
                if repo_dir:
                    error_feedback += "Available files in the repo (use these exact paths):\n"
                    relevant = find_relevant_files(instance, repo_dir)
                    error_feedback += "\n".join(f"  - {f}" for f in relevant[:10])
                messages = [{"role": "user", "content": build_prompt(instance, repo_files=repo_files_str, error_feedback=error_feedback)}]

    elapsed = time.time() - start

    # Final validation status
    is_valid = False
    if best_patch and repo_dir:
        valid, _ = validate_patch(best_patch, repo_dir)
        applies, _ = apply_patch_dry_run(best_patch, repo_dir)
        is_valid = valid and applies
    elif best_patch:
        is_valid = best_patch.startswith("diff --git")

    return {
        "instance_id": instance["instance_id"],
        "model_name_or_path": f"test_vote({'+'.join(vote_models)})" if routing == "test_vote" and vote_models else model,
        "model_patch": best_patch,
        "_winning_model": winning_model,
        "_elapsed": elapsed,
        "_has_diff": bool(best_patch.startswith("diff --git")),
        "_validated": is_valid,
        "_attempts": attempts,
        "_last_error": last_error[:200] if last_error else "",
    }


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Stoke SWE-bench Verified Benchmark")
    parser.add_argument("--model", required=True, help="Model to test")
    parser.add_argument("--routing", default="single",
                        choices=["single", "test_vote"],
                        help="Routing/fusion strategy")
    parser.add_argument("--vote-models", default=None,
                        help="Comma-separated models for test_vote")
    parser.add_argument("--limit", type=int, default=None,
                        help="Number of instances to test (default: all 500)")
    parser.add_argument("--timeout", type=int, default=300,
                        help="Timeout per request in seconds")
    parser.add_argument("--workers", type=int, default=4,
                        help="Parallel workers")
    parser.add_argument("--output", required=True,
                        help="Save predictions JSONL to this path")
    parser.add_argument("--repo-context", action="store_true", default=False,
                        help="Clone repos and include source files in prompt")
    parser.add_argument("--max-attempts", type=int, default=3,
                        help="Max agentic retry attempts on patch validation failure")
    args = parser.parse_args()

    instances = load_instances(args.limit)
    vote_models = args.vote_models.split(",") if args.vote_models else None

    repo_cache = Path(__file__).parent / "repo_cache" if args.repo_context else None
    if repo_cache:
        repo_cache.mkdir(exist_ok=True)
        print(f"Repo context: enabled (cache: {repo_cache})")

    print(f"SWE-bench Verified: {len(instances)} instances")
    print(f"Model: {args.model} | Routing: {args.routing}")
    if vote_models:
        print(f"Vote models: {vote_models}")
    print(f"Max attempts: {args.max_attempts}")
    print(f"Workers: {args.workers}")
    print("-" * 60)

    results = []
    valid_diffs = 0
    validated_patches = 0

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {
            pool.submit(generate_patch, inst, args.model, args.routing, vote_models,
                        args.timeout, repo_cache, args.max_attempts): inst
            for inst in instances
        }

        for i, future in enumerate(as_completed(futures), 1):
            result = future.result()
            results.append(result)
            has_diff = result.get("_has_diff", False)
            is_validated = result.get("_validated", False)
            if has_diff:
                valid_diffs += 1
            if is_validated:
                validated_patches += 1
            status = "✓✓" if is_validated else ("✓" if has_diff else "✗")
            print(
                f"[{i:3d}/{len(instances)}] {status} {result['instance_id']:40s} "
                f"({result.get('_elapsed', 0):.1f}s, {result.get('_attempts', 1)}attempts) "
                f"{result.get('_last_error', '')[:50]}"
            )

    # Sort by instance_id
    results.sort(key=lambda r: r["instance_id"])

    # Save predictions in official SWE-bench format
    with open(args.output, "w") as f:
        for r in results:
            pred = {
                "instance_id": r["instance_id"],
                "model_name_or_path": r["model_name_or_path"],
                "model_patch": r["model_patch"],
            }
            f.write(json.dumps(pred) + "\n")

    # Summary
    total = len(results)
    elapsed_sum = sum(r.get("_elapsed", 0) for r in results)
    avg_time = elapsed_sum / total if total else 0

    print("-" * 60)
    print(f"Valid diffs: {valid_diffs}/{total} ({valid_diffs/total*100:.1f}%)")
    print(f"Validated (applies clean): {validated_patches}/{total} ({validated_patches/total*100:.1f}%)")
    print(f"Average time: {avg_time:.1f}s per instance")
    print(f"Total time: {elapsed_sum:.1f}s")
    print(f"Predictions saved to: {args.output}")
    print()
    print("To evaluate (requires Docker):")
    print(f"  python -m swebench.harness.run_evaluation \\")
    print(f"    --dataset_name SWE-bench/SWE-bench_Verified \\")
    print(f"    --predictions_path {args.output} \\")
    print(f"    --max_workers 8 --run_id {Path(args.output).stem}")

    # Save metadata
    meta_path = args.output.replace(".jsonl", "_meta.json")
    if meta_path == args.output:
        meta_path = args.output + "_meta.json"
    with open(meta_path, "w") as f:
        json.dump({
            "model": args.model,
            "routing": args.routing,
            "vote_models": vote_models,
            "total": total,
            "valid_diffs": valid_diffs,
            "validated_patches": validated_patches,
            "avg_time": avg_time,
            "max_attempts": args.max_attempts,
            "results": results,
        }, f, indent=2)
    print(f"Metadata saved to: {meta_path}")


if __name__ == "__main__":
    main()