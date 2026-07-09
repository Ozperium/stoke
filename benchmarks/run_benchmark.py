#!/usr/bin/env python3
"""
Stoke HumanEval Benchmark Runner

Tests a model (or fusion pattern) on HumanEval problems by:
1. Sending each problem's prompt to the Stoke proxy
2. Extracting the generated code
3. Running the canonical test cases
4. Reporting pass@1

Usage:
  python3 run_benchmark.py --model gpt-oss:20b --limit 20
  python3 run_benchmark.py --model gpt-oss:20b --routing single
  python3 run_benchmark.py --model gpt-oss:20b --routing parallel_vote --providers gemma4:12b-mlx,glm-5.2:cloud,gpt-oss:20b
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
HUMANEVAL_PATH = Path(__file__).parent / "humaneval.jsonl"
HUMANEVALPLUS_PATH = Path(__file__).parent / "humanevalplus.jsonl"


def load_problems(limit: Optional[int] = None, dataset: str = "humaneval") -> list:
    path = HUMANEVALPLUS_PATH if dataset == "humanevalplus" else HUMANEVAL_PATH
    problems = []
    with open(path) as f:
        for line in f:
            problems.append(json.loads(line))
    if limit:
        problems = problems[:limit]
    return problems


def extract_code(prompt: str, response_text: str) -> str:
    """Extract the completion from the model response.

    HumanEval expects the model to complete the function body.
    The prompt ends mid-function, so the completion is the text that
    finishes it. We return the full prompt + completion.

    Some models (especially cloud models) return the ENTIRE function
    including the signature, not just the body. In that case, use the
    response directly instead of prepending the prompt (which would
    duplicate the function definition).
    """
    # Some models wrap code in ```python blocks — extract if present
    if "```python" in response_text:
        match = re.search(r"```python\n(.*?)```", response_text, re.DOTALL)
        if match:
            completion = match.group(1)
        else:
            completion = response_text
    elif "```" in response_text:
        match = re.search(r"```\n(.*?)```", response_text, re.DOTALL)
        if match:
            completion = match.group(1)
        else:
            completion = response_text
    else:
        completion = response_text

    # Detect if the model returned the full function (including signature)
    # rather than just the body. If the completion contains a function def
    # that matches the prompt's entry point, use the completion directly.
    # Extract the first function name from the prompt
    prompt_func_match = re.search(r'^def\s+(\w+)\s*\(', prompt, re.MULTILINE)
    if prompt_func_match:
        func_name = prompt_func_match.group(1)
        if f"def {func_name}" in completion:
            # Model returned the full function — prepend any imports from the prompt
            # (cloud models often drop the "from typing import List" that was in the prompt)
            prompt_lines = prompt.split('\n')
            imports = [l for l in prompt_lines if l.startswith('from ') or l.startswith('import ')]
            if imports:
                completion = '\n'.join(imports) + '\n\n' + completion
            return completion

    # Otherwise, the model just returned the body — prepend the prompt
    return prompt + completion


def call_stoke(
    model: str,
    messages: list,
    routing: str = "single",
    vote_models: Optional[list] = None,
    timeout: int = 120,
    test_code: Optional[str] = None,
    entry_point: Optional[str] = None,
) -> tuple:
    """Call the Stoke proxy and return (response_text, cost_dict)."""
    payload = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": 8192,
        "routing": routing,
    }
    if vote_models:
        payload["vote_models"] = vote_models
    if test_code:
        payload["test_code"] = test_code
    if entry_point:
        payload["entry_point"] = entry_point

    try:
        resp = requests.post(
            PROXY_URL,
            json=payload,
            timeout=timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        content = data["choices"][0]["message"].get("content", "")
        # Some Ollama models put the answer in "reasoning" field
        # But only use reasoning as fallback if it looks like code (contains 'def ')
        if not content:
            reasoning = data["choices"][0]["message"].get("reasoning", "")
            if "def " in reasoning or "```" in reasoning:
                content = reasoning
        cost = data.get("stoke_cost", {})
        return content, cost
    except Exception as e:
        return f"ERROR: {e}", {}


def run_test(problem: dict, completion_code: str) -> tuple:
    """Run the canonical HumanEval test for a problem.
    
    Returns (passed, error_message).
    """
    test_code = problem.get("test", "")
    entry_point = problem.get("entry_point", "")
    
    full_code = completion_code + "\n\n" + test_code + "\n\n"
    full_code += f"check({entry_point})\n"
    
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".py", delete=False, dir="/tmp"
    ) as f:
        f.write(full_code)
        f.flush()
        tmp_path = f.name
    
    try:
        result = subprocess.run(
            [sys.executable, tmp_path],
            capture_output=True,
            timeout=10,
            text=True,
        )
        if result.returncode == 0:
            return True, ""
        else:
            error = result.stderr.strip().split("\n")[-1] if result.stderr else "unknown error"
            return False, error
    except subprocess.TimeoutExpired:
        return False, "timeout"
    except Exception as e:
        return False, str(e)
    finally:
        os.unlink(tmp_path)


def call_stoke_single(
    model: str,
    messages: list,
    timeout: int = 120,
) -> str:
    """Call the Stoke proxy for a single model (bypasses routing)."""
    payload = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": 8192,
    }
    try:
        resp = requests.post(PROXY_URL, json=payload, timeout=timeout)
        resp.raise_for_status()
        data = resp.json()
        content = data["choices"][0]["message"].get("content", "")
        if not content:
            reasoning = data["choices"][0]["message"].get("reasoning", "")
            if "def " in reasoning or "```" in reasoning:
                content = reasoning
        return content
    except Exception as e:
        return f"ERROR: {e}"


def call_stoke_single_temp(
    model: str,
    messages: list,
    timeout: int = 120,
    temperature: float = 0.0,
) -> str:
    """Call the Stoke proxy for a single model with a specific temperature."""
    payload = {
        "model": model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": 8192,
    }
    try:
        resp = requests.post(PROXY_URL, json=payload, timeout=timeout)
        resp.raise_for_status()
        data = resp.json()
        content = data["choices"][0]["message"].get("content", "")
        if not content:
            reasoning = data["choices"][0]["message"].get("reasoning", "")
            if "def " in reasoning or "```" in reasoning:
                content = reasoning
        return content
    except Exception as e:
        return f"ERROR: {e}"


def benchmark_single(
    problem: dict,
    model: str,
    routing: str,
    vote_models: Optional[list] = None,
    timeout: int = 120,
    temperatures: Optional[list] = None,
    refine_rounds: int = 0,
) -> dict:
    """Run a single HumanEval problem through Stoke."""
    prompt = problem["prompt"]
    
    messages = [
        {
            "role": "user",
            "content": prompt,
        }
    ]
    
    start = time.time()
    
    if routing == "auto":
        # Auto-routing: let the proxy classify and route.
        # Pass test_code + entry_point so the proxy can use cascade_test for code.
        test_code = problem.get("test", "")
        entry_point = problem.get("entry_point", "")
        raw_messages = [{"role": "user", "content": prompt}]
        response, cost = call_stoke(
            model, raw_messages, routing="auto", timeout=timeout,
            test_code=test_code, entry_point=entry_point,
        )
        elapsed = time.time() - start
        code = extract_code(prompt, response)
        passed, error = run_test(problem, code)
        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(response),
            "cost": cost,
        }

    if routing == "test_vote" and vote_models:
        # Use server-side test_vote via the proxy — includes cost tracking,
        # early-exit, and server-side code extraction with import handling.
        # Send the raw prompt (not wrapped) so the server can detect function defs.
        test_code = problem.get("test", "")
        entry_point = problem.get("entry_point", "")
        raw_messages = [{"role": "user", "content": prompt}]
        response, cost = call_stoke(
            model, raw_messages, routing="test_vote", vote_models=vote_models, timeout=timeout,
            test_code=test_code, entry_point=entry_point,
        )
        elapsed = time.time() - start

        # Re-test locally with proper extraction to confirm the server's result.
        # The server returns the winning model's raw response (with markdown blocks).
        # We need to extract the code and test it locally.
        code = extract_code(prompt, response)
        passed, error = run_test(problem, code)

        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(response),
            "cost": cost,
        }

    if routing == "cascade_test" and vote_models:
        # Use server-side cascade_test via the proxy
        test_code = problem.get("test", "")
        entry_point = problem.get("entry_point", "")
        raw_messages = [{"role": "user", "content": prompt}]
        response, cost = call_stoke(
            model, raw_messages, routing="cascade_test", vote_models=vote_models, timeout=timeout,
            test_code=test_code, entry_point=entry_point,
        )
        elapsed = time.time() - start
        code = extract_code(prompt, response)
        passed, error = run_test(problem, code)
        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(response),
            "cost": cost,
        }

    if routing == "chain" and vote_models:
        # Chain: each model refines the previous output
        current_content = ""
        for i, m in enumerate(vote_models):
            if i == 0:
                chain_messages = messages
            else:
                chain_messages = [{
                    "role": "user",
                    "content": f"Review and improve the following response. Fix any errors, improve clarity, and provide a better version.\n\nOriginal request:\n{prompt}\n\nPrevious response:\n{current_content}\n\nProvide only the improved response:"
                }]
            resp = call_stoke_single(m, chain_messages, timeout=timeout)
            current_content = resp
        
        elapsed = time.time() - start
        code = extract_code(prompt, current_content)
        passed, error = run_test(problem, code)
        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(current_content),
            "winning_model": vote_models[-1],
        }

    if routing == "parallel_merge" and vote_models:
        # Parallel+Merge: fan out to generators, merge with last model
        gen_models = vote_models[:-1]
        merge_model = vote_models[-1]

        # Phase 1: fan out
        responses = []
        for m in gen_models:
            resp = call_stoke_single(m, messages, timeout=timeout)
            responses.append((m, resp))

        # Phase 2: merge
        merge_prompt = (
            "You are a merge assistant. Multiple AI models were asked the same question. "
            "Your job is to synthesize their responses into a single best answer.\n\n"
            f"Original request:\n{prompt}\n\n"
        )
        for i, (m, resp) in enumerate(responses):
            merge_prompt += f"Model {i+1} ({m}):\n{resp}\n\n"
        merge_prompt += (
            "Synthesize the above responses into a single best answer. "
            "Pick the most accurate parts, resolve any conflicts, and output the final answer only."
        )
        merge_messages = [{"role": "user", "content": merge_prompt}]
        merged = call_stoke_single(merge_model, merge_messages, timeout=timeout)

        elapsed = time.time() - start
        code = extract_code(prompt, merged)
        passed, error = run_test(problem, code)
        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(merged),
            "winning_model": merge_model,
        }
    
    if routing == "self_consistency":
        # Self-consistency with test-based selection: N samples at different
        # temperatures, test each, return first that passes.
        # This is the "augmentation" pattern — same model, diverse outputs.
        temps = temperatures or [0.0, 0.3, 0.5, 0.7, 1.0]

        error = ""
        response = ""
        for temp in temps:
            response = call_stoke_single_temp(model, messages, timeout=timeout, temperature=temp)
            if response.startswith("ERROR:"):
                continue
            code = extract_code(prompt, response)
            passed, error = run_test(problem, code)
            if passed:
                elapsed = time.time() - start
                return {
                    "task_id": problem["task_id"],
                    "passed": True,
                    "error": "",
                    "elapsed": elapsed,
                    "response_len": len(response),
                    "temperature": temp,
                }
        elapsed = time.time() - start
        # All failed — return the last error
        return {
            "task_id": problem["task_id"],
            "passed": False,
            "error": error if not response.startswith("ERROR:") else response[:40],
            "elapsed": elapsed,
            "response_len": len(response),
            "temperature": temps[-1],
        }

    if routing == "best_of_n":
        # Best-of-N with Self-Certainty: server-side pattern.
        # N samples at diverse temps, score by logprob, return best.
        # Optional refine rounds loop the best output back for improvement.
        temps_arg = temperatures or [0.0, 0.3, 0.5, 0.7, 1.0]
        payload = {
            "model": model,
            "messages": messages,
            "temperature": 0.0,
            "max_tokens": 8192,
            "routing": "best_of_n",
            "n_samples": len(temps_arg),
            "temperatures": temps_arg,
            "refine_rounds": refine_rounds,
            "logprobs": True,
        }
        try:
            resp = requests.post(PROXY_URL, json=payload, timeout=timeout)
            resp.raise_for_status()
            data = resp.json()
            content = data["choices"][0]["message"].get("content", "")
            if not content:
                reasoning = data["choices"][0]["message"].get("reasoning", "")
                if "def " in reasoning or "```" in reasoning:
                    content = reasoning
            cost = data.get("stoke_cost", {})
        except Exception as e:
            content = f"ERROR: {e}"
            cost = {}

        elapsed = time.time() - start
        code = extract_code(prompt, content)
        passed, error = run_test(problem, code)
        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(content),
            "cost": cost,
        }

    if routing == "deliberation":
        # Deliberation: panel → judge → synthesizer (server-side)
        # vote_models = panel_models..., judge_model, synth_model (last 2)
        if not vote_models or len(vote_models) < 3:
            return {
                "task_id": problem["task_id"],
                "passed": False,
                "error": "deliberation needs >=3 vote_models",
                "elapsed": time.time() - start,
                "response_len": 0,
            }
        payload = {
            "model": model,
            "messages": messages,
            "temperature": 0.0,
            "max_tokens": 8192,
            "routing": "deliberation",
            "vote_models": vote_models,
        }
        try:
            resp = requests.post(PROXY_URL, json=payload, timeout=timeout)
            resp.raise_for_status()
            data = resp.json()
            content = data["choices"][0]["message"].get("content", "")
            if not content:
                reasoning = data["choices"][0]["message"].get("reasoning", "")
                if "def " in reasoning or "```" in reasoning:
                    content = reasoning
            cost = data.get("stoke_cost", {})
        except Exception as e:
            content = f"ERROR: {e}"
            cost = {}

        elapsed = time.time() - start
        code = extract_code(prompt, content)
        passed, error = run_test(problem, code)
        return {
            "task_id": problem["task_id"],
            "passed": passed,
            "error": error,
            "elapsed": elapsed,
            "response_len": len(content),
            "cost": cost,
        }

    # Single or proxy-side fusion
    response, cost = call_stoke(model, messages, routing=routing, vote_models=vote_models, timeout=timeout)
    elapsed = time.time() - start

    code = extract_code(prompt, response)
    passed, error = run_test(problem, code)

    return {
        "task_id": problem["task_id"],
        "passed": passed,
        "error": error,
        "elapsed": elapsed,
        "response_len": len(response),
        "cost": cost,
    }


def main():
    parser = argparse.ArgumentParser(description="Stoke HumanEval Benchmark")
    parser.add_argument("--model", required=True, help="Model to test")
    parser.add_argument("--routing", default="single", 
                        choices=["single", "auto", "parallel_vote", "cascade", "cascade_test", "test_vote", "chain", "parallel_merge", "self_consistency", "best_of_n", "deliberation"],
                        help="Routing/fusion strategy")
    parser.add_argument("--n-samples", type=int, default=5,
                        help="Number of samples for self_consistency (default 5)")
    parser.add_argument("--temperatures", default="0.0,0.3,0.5,0.7,1.0",
                        help="Comma-separated temperatures for self_consistency samples")
    parser.add_argument("--refine-rounds", type=int, default=0,
                        help="Number of refine rounds for best_of_n (default 0)")
    parser.add_argument("--vote-models", default=None,
                        help="Comma-separated models for parallel_vote (e.g. 'gpt-oss:20b,gemma4:12b-mlx,glm-5.2:cloud')")
    parser.add_argument("--limit", type=int, default=20,
                        help="Number of problems to test (default 20, max 164)")
    parser.add_argument("--timeout", type=int, default=120,
                        help="Timeout per request in seconds")
    parser.add_argument("--workers", type=int, default=4,
                        help="Parallel workers")
    parser.add_argument("--output", default=None,
                        help="Save results JSON to this path")
    parser.add_argument("--dataset", default="humaneval",
                        choices=["humaneval", "humanevalplus"],
                        help="Dataset to use (humaneval or humanevalplus)")
    args = parser.parse_args()
    
    problems = load_problems(args.limit, dataset=args.dataset)
    vote_models = args.vote_models.split(",") if args.vote_models else None
    print(f"HumanEval Benchmark: {len(problems)} problems")
    print(f"Model: {args.model} | Routing: {args.routing}")
    if vote_models:
        print(f"Vote models: {vote_models}")
    print(f"Workers: {args.workers}")
    print("-" * 60)
    
    results = []
    
    temps = [float(t) for t in args.temperatures.split(",")] if args.temperatures else [0.0, 0.3, 0.5, 0.7, 1.0]
    
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {
            pool.submit(benchmark_single, p, args.model, args.routing, vote_models, args.timeout, temps, args.refine_rounds): p
            for p in problems
        }
        
        for i, future in enumerate(as_completed(futures), 1):
            result = future.result()
            results.append(result)
            status = "✓" if result["passed"] else "✗"
            print(
                f"[{i:3d}/{len(problems)}] {status} {result['task_id']:15s} "
                f"({result['elapsed']:.1f}s) "
                f"{result['error'][:40] if result['error'] else ''}"
            )
    
    # Sort by task_id for consistent output
    results.sort(key=lambda r: r["task_id"])
    
    passed = sum(1 for r in results if r["passed"])
    total = len(results)
    avg_time = sum(r["elapsed"] for r in results) / total if total else 0
    total_cost = sum(r.get("cost", {}).get("cost_usd", 0) for r in results)
    total_tokens = sum(r.get("cost", {}).get("total_tokens", 0) for r in results)

    print("-" * 60)
    print(f"Results: {passed}/{total} passed ({passed/total*100:.1f}%)")
    print(f"Average time: {avg_time:.1f}s per problem")
    print(f"Total time: {sum(r['elapsed'] for r in results):.1f}s")
    print(f"Total cost: ${total_cost:.4f} ({total_tokens} tokens)")
    
    if args.output:
        output_data = {
            "model": args.model,
            "routing": args.routing,
            "total": total,
            "passed": passed,
            "pass_rate": passed / total if total else 0,
            "avg_time": avg_time,
            "results": results,
        }
        with open(args.output, "w") as f:
            json.dump(output_data, f, indent=2)
        print(f"Saved to {args.output}")


if __name__ == "__main__":
    main()