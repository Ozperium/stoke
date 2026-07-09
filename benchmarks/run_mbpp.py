#!/usr/bin/env python3
"""
Stoke MBPP Benchmark Runner

Tests a model (or fusion pattern) on MBPP problems (974 tasks).
MBPP problems are standalone: given a task description, generate a complete Python function.

Usage:
  python3 run_mbpp.py --model qwen2.5-coder:3b --limit 20
  python3 run_mbpp.py --model qwen2.5-coder:3b --routing test_vote --vote-models qwen2.5-coder:3b,qwen2.5-coder:7b,llama3.2:3b,phi4-mini
  python3 run_mbpp.py --model qwen2.5-coder:3b --routing cascade_test --vote-models qwen2.5-coder:3b,qwen2.5-coder:7b
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

PROXY_URL = "http://127.0.0.1:8787/v1/chat/completions"
MBPP_PATH = Path(__file__).parent / "mbpp.jsonl"


def load_problems(limit: Optional[int] = None) -> list:
    problems = []
    with open(MBPP_PATH) as f:
        for line in f:
            problems.append(json.loads(line))
    # MBPP: task_ids 11-510 are the test set (500 problems)
    # Filter to test set only
    problems = [p for p in problems if 11 <= p["task_id"] <= 510]
    if limit:
        problems = problems[:limit]
    return problems


def build_prompt(problem: dict) -> str:
    """Build a chat prompt for MBPP. Unlike HumanEval (completion), MBPP is instruction-based."""
    task = problem["text"]
    tests = problem.get("test_list", [])
    test_str = "\n".join(tests) if tests else ""

    prompt = f"""You are an expert Python programmer. Write a complete Python function to solve the following task.

Task: {task}

Your code should pass these tests:
{test_str}

Write only the Python code, no explanations. Use ```python to wrap your code."""

    return prompt


def extract_code(response_text: str) -> str:
    """Extract Python code from the model response."""
    if "```python" in response_text:
        match = re.search(r"```python\n(.*?)```", response_text, re.DOTALL)
        if match:
            return match.group(1).strip()
    if "```" in response_text:
        match = re.search(r"```\n(.*?)```", response_text, re.DOTALL)
        if match:
            return match.group(1).strip()
    return response_text.strip()


def run_tests(code: str, problem: dict, timeout: int = 10) -> tuple:
    """Run the generated code against MBPP test cases.
    Returns (passed: bool, error: str)
    """
    test_list = problem.get("test_list", [])
    if not test_list:
        return False, "No tests available"

    setup = problem.get("test_setup_code", "")

    # Build the test script
    test_script = f"{setup}\n\n{code}\n\n"
    for test in test_list:
        test_script += f"\n{test}"

    try:
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False, dir="/tmp") as f:
            f.write(test_script)
            f.flush()
            result = subprocess.run(
            ["python3", f.name],
            capture_output=True,
            text=True,
            timeout=timeout,
            )
            os.unlink(f.name)
            if result.returncode == 0:
                return True, ""
            else:
                error = result.stderr.strip().split("\n")[-1] if result.stderr else "Unknown error"
                return False, error
    except subprocess.TimeoutExpired:
        return False, "timeout"
    except Exception as e:
        return False, str(e)


def call_stoke(
    model: str,
    messages: list,
    routing: str = "single",
    vote_models: list = None,
    timeout: int = 120,
) -> tuple:
    """Call the Stoke proxy. Returns (response_text, cost_dict)."""
    body = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": 1024,
        "routing": routing,
    }
    if vote_models:
        body["vote_models"] = vote_models

    # For test_vote / cascade_test: include the test code and entry point
    # MBPP tests are assert statements — we need to wrap them in a function for the proxy
    if routing in ("test_vote", "cascade_test") and vote_models:
        # We'll pass test_code separately — the proxy expects test_code and entry_point
        # For MBPP, tests are assert statements, not a function. We adapt:
        # The proxy runs: python -c "test_code; entry_point(args); print('PASS')"
        # We'll set test_code to the assert statements and entry_point to a dummy
        pass  # test_code and entry_point are set per-problem in the caller

    try:
        resp = requests.post(PROXY_URL, json=body, timeout=timeout)
        resp.raise_for_status()
        data = resp.json()
        content = (
            data.get("choices", [{}])[0]
            .get("message", {})
            .get("content", "")
        )
        cost = data.get("stoke_cost", {})
        return content, cost
    except Exception as e:
        return "", {"cost_usd": 0, "error": str(e)}


def call_stoke_fusion(
    model: str,
    messages: list,
    routing: str,
    vote_models: list,
    problem: dict,
    timeout: int = 120,
) -> tuple:
    """Call Stoke with fusion patterns that need test_code/entry_point."""
    test_list = problem.get("test_list", [])
    test_code = "\n".join(test_list)

    # MBPP doesn't have a single entry_point — the tests call functions directly.
    # We adapt: wrap tests in a __main__ block that prints PASS
    full_test = f"{test_code}\nprint('PASS')"

    body = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": 1024,
        "routing": routing,
        "vote_models": vote_models,
        "test_code": full_test,
        "entry_point": "PASS",  # dummy — the test_code prints PASS directly
    }

    try:
        resp = requests.post(PROXY_URL, json=body, timeout=timeout)
        resp.raise_for_status()
        data = resp.json()
        content = (
            data.get("choices", [{}])[0]
            .get("message", {})
            .get("content", "")
        )
        cost = data.get("stoke_cost", {})
        return content, cost
    except Exception as e:
        return "", {"cost_usd": 0, "error": str(e)}


def benchmark_single(problem: dict, model: str, routing: str, vote_models: list, timeout: int):
    """Run benchmark on a single problem."""
    prompt = build_prompt(problem)
    messages = [{"role": "user", "content": prompt}]

    start = time.time()

    if routing in ("test_vote", "cascade_test") and vote_models:
        response_text, cost = call_stoke_fusion(
            model, messages, routing, vote_models, problem, timeout
        )
    else:
        response_text, cost = call_stoke(model, messages, routing, vote_models, timeout)

    elapsed = time.time() - start
    code = extract_code(response_text)
    passed, error = run_tests(code, problem)

    return {
        "task_id": problem["task_id"],
        "passed": passed,
        "error": error if not passed else "",
        "elapsed": round(elapsed, 1),
        "code": code[:500],
        "cost": cost.get("cost_usd", 0),
    }


def main():
    parser = argparse.ArgumentParser(description="MBPP benchmark runner for Stoke")
    parser.add_argument("--model", default="qwen2.5-coder:3b", help="Model to test")
    parser.add_argument("--routing", default="single",
                        choices=["single", "test_vote", "cascade_test", "self_consistency", "auto"],
                        help="Fusion pattern")
    parser.add_argument("--vote-models", default="",
                        help="Comma-separated list of models for fusion (e.g. qwen2.5-coder:3b,qwen2.5-coder:7b)")
    parser.add_argument("--limit", type=int, default=None, help="Number of problems (default: 500 = full test set)")
    parser.add_argument("--workers", type=int, default=4, help="Parallel workers")
    parser.add_argument("--timeout", type=int, default=120, help="Timeout per problem (seconds)")
    parser.add_argument("--output", default="results_mbpp.json", help="Output file")

    args = parser.parse_args()
    vote_models = [m.strip() for m in args.vote_models.split(",") if m.strip()]

    problems = load_problems(args.limit)
    total = len(problems)
    print(f"MBPP Benchmark: {total} problems")
    print(f"Model: {args.model} | Routing: {args.routing}")
    if vote_models:
        print(f"Vote models: {vote_models}")
    print("-" * 60)

    results = []
    passed_count = 0
    start_time = time.time()

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {
            pool.submit(benchmark_single, p, args.model, args.routing, vote_models, args.timeout): p
            for p in problems
        }
        for i, future in enumerate(as_completed(futures), 1):
            result = future.result()
            results.append(result)
            if result["passed"]:
                passed_count += 1
                marker = "✓"
            else:
                marker = "✗"
            print(f"[{i}/{total}] {marker} MBPP/{result['task_id']:>4}   ({result['elapsed']:.1f}s) {result['error'][:50] if not result['passed'] else ''}")

    total_time = time.time() - start_time
    pass_rate = passed_count / total * 100

    print("-" * 60)
    print(f"Results: {passed_count}/{total} passed ({pass_rate:.1f}%)")
    print(f"Total time: {total_time:.1f}s")
    print(f"Saved to {args.output}")

    output = {
        "summary": {
            "passed": passed_count,
            "total": total,
            "pass_rate": round(pass_rate, 1),
            "total_time": round(total_time, 1),
            "model": args.model,
            "routing": args.routing,
            "vote_models": vote_models,
        },
        "results": results,
    }

    with open(args.output, "w") as f:
        json.dump(output, f, indent=2)


if __name__ == "__main__":
    main()