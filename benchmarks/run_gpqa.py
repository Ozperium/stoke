#!/usr/bin/env python3
"""
Stoke GPQA Diamond Benchmark Runner

Tests a model (or fusion pattern) on GPQA Diamond questions by:
1. Downloading the dataset from HuggingFace
2. Formatting each as multiple-choice (A/B/C/D, shuffled)
3. Sending to the Stoke proxy
4. Parsing the model's A/B/C/D answer and comparing to correct
5. Reporting accuracy

Usage:
  python3 run_gpqa.py --model qwen3.6:35b --limit 10
  python3 run_gpqa.py --model qwen3.6:35b --routing single
  python3 run_gpqa.py --model qwen3.6:35b --routing test_vote --vote-models 'qwen2.5-coder:7b,qwen3:8b,gemma4:12b'
"""

import argparse
import json
import os
import random
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

PROXY_URL = "http://127.0.0.1:8787/v1/chat/completions"
GPQA_REPO = "Idavidrein/GPQA"
GPQA_SUBSET = "gpqa_diamond"
GPQA_CACHE_DIR = Path(__file__).parent / ".cache_gpqa"


# ---------------------------------------------------------------------------
# Dataset loading
# ---------------------------------------------------------------------------

def _try_datasets_lib() -> Optional[list]:
    """Try to load GPQA via the `datasets` library."""
    try:
        import datasets as ds  # type: ignore
    except ImportError:
        return None

    ds_path = GPQA_CACHE_DIR / "gpqa_diamond"
    if ds_path.exists():
        ds_path.unlink()
    dataset = ds.load_dataset(GPQA_REPO, GPQA_SUBSET, cache_dir=str(ds_path))
    # dataset is a DatasetDict; use the train split
    rows = dataset["train"]
    return [
        {
            "question": row["Question"],
            "correct_answer": row["Correct Answer"],
            "incorrect_answers": row["Incorrect Answers"],
        }
        for row in rows
    ]


def _try_hf_url() -> Optional[list]:
    """Download GPQA Diamond from ungated HuggingFace mirror (Wanfq/gpqa)."""
    import csv
    import urllib.request

    cache_path = GPQA_CACHE_DIR / "gpqa_diamond.csv"
    if not cache_path.exists():
        url = "https://huggingface.co/datasets/Wanfq/gpqa/raw/main/gpqa_diamond.csv"
        print("Downloading GPQA Diamond from HuggingFace ...")
        os.makedirs(GPQA_CACHE_DIR, exist_ok=True)
        try:
            urllib.request.urlretrieve(url, str(cache_path))
        except Exception:
            return None

    if not cache_path.exists():
        return None

    rows = []
    with open(cache_path, newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for row in reader:
            question = (row.get("Question") or "").strip()
            correct = (row.get("Correct Answer") or "").strip()
            distractors = [
                (row.get("Incorrect Answer 1") or "").strip(),
                (row.get("Incorrect Answer 2") or "").strip(),
                (row.get("Incorrect Answer 3") or "").strip(),
            ]
            if not question or not correct:
                continue
            rows.append({
                "question": question,
                "correct_answer": correct,
                "incorrect_answers": distractors,
            })

    return rows if rows else None


def load_gpqa(limit: Optional[int] = None) -> list:
    """Load GPQA Diamond dataset.

    Tries three backends in order:
      1. `datasets` library (HuggingFace datasets)
      2. HuggingFace Hub direct URL (CSV)
      3. bundled fallback JSON inside benchmarks/gpqa.jsonl
    """
    # 1. HF URL (CSV) — ungated mirror, no auth needed
    rows = _try_hf_url()
    if rows:
        print(f"[hf-url] loaded {len(rows)} questions from CSV")
        if limit:
            return rows[:limit]
        return rows

    # 3. Bundled fallback – check for local JSONL alongside this script
    jsonl_path = Path(__file__).parent / "gpqa_diamond.jsonl"
    if jsonl_path.exists():
        rows = []
        with open(jsonl_path) as f:
            for line in f:
                row = json.loads(line.strip())
                # Accept both formats – wide column names or short keys
                question = row.get("Question") or row.get("question", "")
                correct = row.get("Correct Answer") or row.get("correct_answer", "")
                wrong = row.get("Incorrect Answers") or row.get("incorrect_answers", [])
                # Wrong might be a string, int list, or dict with keys 1/3/4
                if isinstance(wrong, str):
                    distractors = [wrong.split(",")[0], wrong.split(",")[1], wrong.split(",")[2]]
                elif isinstance(wrong, int):
                    distractors = [str(wrong)]
                else:
                    distractors = (
                        wrong.get(1, wrong.get("1", "")),
                        wrong.get(3, wrong.get("3", "")),
                        wrong.get(4, wrong.get("4", "")),
                    )
                    distractors = [d.strip() for d in distractors if d]
                rows.append({"question": question, "correct_answer": correct, "incorrect_answers": distractors})
        if rows:
            print(f"[jsonl-fallback] loaded {len(rows)} questions")
            if limit:
                return rows[:limit]
            return rows

    raise RuntimeError(
        "GPQA Diamond dataset not found.\n"
        "Install it with: pip install datasets\n"
        "Or place a local gpqa_diamond.jsonl in benchmarks/\n"
        "Or ensure network access for HuggingFace URL download."
    )


# ---------------------------------------------------------------------------
# Question formatting
# ---------------------------------------------------------------------------

def format_question(item: dict) -> dict:
    """Shuffle choices and return structured question data."""
    correct = item["correct_answer"]
    distractors = item["incorrect_answers"]
    all_choices = [correct] + distractors[:3]
    random.shuffle(all_choices)

    # Ensure we have exactly 4
    while len(all_choices) < 4:
        all_choices.append("")

    correct_idx = all_choices.index(correct)
    letters = ["A", "B", "C", "D"]
    answer_letter = letters[correct_idx]

    prompt = (
        f"Answer the following multiple-choice question. "
        f"Respond with ONLY the letter (A, B, C, or D) of the correct answer, nothing else.\n\n"
        f"Question: {item['question']}\n\n"
        f"A) {all_choices[0]}\n"
        f"B) {all_choices[1]}\n"
        f"C) {all_choices[2]}\n"
        f"D) {all_choices[3]}"
    )

    return {
        "question": item["question"],
        "prompt": prompt,
        "choices": ["A", "B", "C", "D"],
        "choice_texts": all_choices[:4],
        "correct_letter": answer_letter,
        "correct_text": correct,
    }


# ---------------------------------------------------------------------------
# Answer parsing
# ---------------------------------------------------------------------------

def parse_answer(response_text: str) -> Optional[str]:
    """Extract A/B/C/D from the model's response."""
    text = response_text.strip()
    # Direct letter
    if text and text[0] in "ABCD":
        return text[0].upper()
    # Uppercase word
    for letter in ("A", "B", "C", "D"):
        if text.upper().startswith(letter):
            return letter
    # "Answer: X" or similar patterns
    match = __import__("re").search(r"[ABCD]", text.upper())
    if match:
        return match.group(0)
    return None


# ---------------------------------------------------------------------------
# API calls
# ---------------------------------------------------------------------------

def call_stoke(
    model: str,
    messages: list,
    routing: str = "single",
    vote_models: Optional[list] = None,
    timeout: int = 120,
) -> str:
    """Call the Stoke proxy and return the response text."""
    payload = {
        "model": model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": 8192,
        "routing": routing,
    }
    if vote_models:
        payload["vote_models"] = vote_models

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
        if not content:
            content = data["choices"][0]["message"].get("reasoning", "")
        return content
    except Exception as e:
        return f"ERROR: {e}"


def call_stoke_single(
    model: str,
    messages: list,
    timeout: int = 120,
    temperature: float = 0.0,
) -> str:
    """Call the Stoke proxy for a single model (bypasses routing)."""
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
            content = data["choices"][0]["message"].get("reasoning", "")
        return content
    except Exception as e:
        return f"ERROR: {e}"


# ---------------------------------------------------------------------------
# Per-question benchmark
# ---------------------------------------------------------------------------

def benchmark_single(
    qitem: dict,
    question: dict,
    model: str,
    routing: str,
    vote_models: Optional[list],
    timeout: int = 120,
) -> dict:
    """Run a single GPQA question through Stoke."""
    messages = [
        {
            "role": "user",
            "content": question["prompt"],
        }
    ]

    start = time.time()

    if routing == "test_vote" and vote_models:
        # Majority-vote across models (no executable test for GPQA)
        votes = {}
        for m in vote_models:
            resp = call_stoke_single(m, messages, timeout=timeout)
            parsed = parse_answer(resp)
            votes[parsed] = votes.get(parsed, 0) + 1
            print(f"    {m}: '{parsed}'")

        # Pick majority winner
        if votes:
            winner = max(votes, key=lambda k: votes[k])
            won_by = votes[winner]
        else:
            winner = None
            won_by = 0

        elapsed = time.time() - start
        correct_letter = question["correct_letter"]
        passed = winner == correct_letter if winner else False

        return {
            "question": question["question"],
            "correct": correct_letter,
            "predicted": str(winner),
            "passed": passed,
            "votes": votes,
            "elapsed": elapsed,
            "winning_model": vote_models[0],
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
                    "content": (
                        "Review and improve the following response. Fix any errors, "
                        "improve clarity, and provide a better version.\n\n"
                        f"Original request:\n{question['prompt']}\n\n"
                        f"Previous response:\n{current_content}\n\n"
                        "Provide only the improved response:"
                    )
                }]
            resp = call_stoke_single(m, chain_messages, timeout=timeout)
            current_content = resp

        elapsed = time.time() - start
        letter = parse_answer(current_content)
        correct_letter = question["correct_letter"]
        passed = letter == correct_letter if letter else False

        return {
            "question": question["question"],
            "correct": correct_letter,
            "predicted": str(letter),
            "passed": passed,
            "elapsed": elapsed,
            "winning_model": vote_models[-1],
        }

    if routing == "parallel_merge" and vote_models:
        # Parallel+Merge: fan out to generators, merge with last model
        gen_models = vote_models[:-1]
        merge_model = vote_models[-1]

        responses = []
        for m in gen_models:
            resp = call_stoke_single(m, messages, timeout=timeout)
            responses.append((m, resp))

        merge_prompt = (
            "You are a merge assistant. Multiple AI models were asked the same question. "
            "Your job is to synthesize their responses into a single best answer.\n\n"
            f"Original request:\n{question['prompt']}\n\n"
        )
        for i, (m, resp) in enumerate(responses):
            merge_prompt += f"Model {i+1} ({m}):\n{resp}\n\n"
        merge_prompt += (
            "Synthesize the above responses into a single best answer. "
            "Respond with ONLY the letter (A, B, C, or D) of the correct answer."
        )
        merge_messages = [{"role": "user", "content": merge_prompt}]
        merged = call_stoke_single(merge_model, merge_messages, timeout=timeout)

        elapsed = time.time() - start
        letter = parse_answer(merged)
        correct_letter = question["correct_letter"]
        passed = letter == correct_letter if letter else False

        return {
            "question": question["question"],
            "correct": correct_letter,
            "predicted": str(letter),
            "passed": passed,
            "elapsed": elapsed,
            "winning_model": merge_model,
        }

    if routing == "cascade_test" and vote_models:
        # No tests for GPQA — same as sequential majority vote
        votes = {}
        for m in vote_models:
            resp = call_stoke_single(m, messages, timeout=timeout)
            parsed = parse_answer(resp)
            votes[parsed] = votes.get(parsed, 0) + 1

        winner = max(votes, key=lambda k: votes[k]) if votes else None
        elapsed = time.time() - start
        correct_letter = question["correct_letter"]
        passed = winner == correct_letter if winner else False

        return {
            "question": question["question"],
            "correct": correct_letter,
            "predicted": str(winner),
            "passed": passed,
            "votes": votes,
            "elapsed": elapsed,
            "winning_model": "cascade",
        }

    if routing == "self_consistency" and vote_models:
        # Self-consistency: same model, N samples at temp>0, majority vote
        target_model = vote_models[0]
        n_samples = 5
        sc_temp = 0.7

        votes = {}
        for _ in range(n_samples):
            resp = call_stoke_single(target_model, messages, timeout=timeout, temperature=sc_temp)
            parsed = parse_answer(resp)
            votes[parsed] = votes.get(parsed, 0) + 1

        winner = max(votes, key=lambda k: votes[k]) if votes else None
        elapsed = time.time() - start
        correct_letter = question["correct_letter"]
        passed = winner == correct_letter if winner else False

        return {
            "question": question["question"],
            "correct": correct_letter,
            "predicted": str(winner),
            "passed": passed,
            "votes": votes,
            "elapsed": elapsed,
            "winning_model": f"{target_model} (x{n_samples})",
        }

    # Single or proxy-side fusion
    response = call_stoke(model, messages, routing=routing, vote_models=vote_models, timeout=timeout)
    elapsed = time.time() - start

    letter = parse_answer(response)
    correct_letter = question["correct_letter"]
    passed = letter == correct_letter if letter else False

    return {
        "question": question["question"],
        "correct": correct_letter,
        "predicted": str(letter),
        "passed": passed,
        "response": response,
        "elapsed": elapsed,
        "winning_model": model,
    }


# ---------------------------------------------------------------------------
# Main CLI
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Stoke GPQA Diamond Benchmark")
    parser.add_argument("--model", required=True, help="Model to test")
    parser.add_argument(
        "--routing",
        default="single",
        choices=["single", "parallel_vote", "cascade", "test_vote", "chain", "parallel_merge", "cascade_test", "self_consistency"],
        help="Routing/fusion strategy",
    )
    parser.add_argument(
        "--vote-models",
        default=None,
        help="Comma-separated models for parallel_vote (e.g. 'qwen2.5-coder:7b,qwen3:8b,gemma4:12b')",
    )
    parser.add_argument("--limit", type=int, default=198, help="Number of questions to test (default 198)")
    parser.add_argument("--timeout", type=int, default=120, help="Timeout per request in seconds")
    parser.add_argument("--workers", type=int, default=4, help="Parallel workers")
    parser.add_argument(
        "--shuffle-seed", type=int, default=42, help="Random seed for shuffling choices (default: 42)"
    )
    parser.add_argument("--output", default=None, help="Save results JSON to this path")
    args = parser.parse_args()

    random.seed(args.shuffle_seed)

    # Load dataset
    raw_items = load_gpqa(limit=args.limit)
    questions = [format_question(item) for item in raw_items]
    vote_models = args.vote_models.split(",") if args.vote_models else None

    print(f"GPQA Diamond Benchmark: {len(questions)} questions")
    print(f"Model: {args.model} | Routing: {args.routing}")
    if vote_models:
        print(f"Vote models: {vote_models}")
    print(f"Workers: {args.workers} | Seed: {args.shuffle_seed}")
    print("-" * 60)

    results = []

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {
            pool.submit(benchmark_single, qitem, q, args.model, args.routing, vote_models, args.timeout): i
            for i, (qitem, q) in enumerate(zip(raw_items, questions))
        }

        for i, future in enumerate(as_completed(futures), 1):
            result = future.result()
            results.append(result)
            status = "OK" if result["passed"] else ("ERR" if result.get("predicted") == "ERROR" or result.get("predicted") == "None" else "FAIL")
            print(
                f"[{i:3d}/{len(questions)}] {status} Q{i:3d} | correct={result['correct']} "
                f"pred={'|'.join(str(k)+':'+str(v) for k,v in result.get('votes', {result['predicted']:1}).items())}"
            )

    # Sort by index for consistent output
    results.sort(key=lambda r: len(r["question"]))

    passed = sum(1 for r in results if r["passed"])
    total = len(results)
    avg_time = sum(r["elapsed"] for r in results) / total if total else 0

    print("-" * 60)
    print(f"Results: {passed}/{total} correct ({passed/total*100:.1f}%)")
    print(f"Average time: {avg_time:.1f}s per question")
    print(f"Total time: {sum(r['elapsed'] for r in results):.1f}s")

    if args.output:
        output_data = {
            "model": args.model,
            "routing": args.routing,
            "total": total,
            "passed": passed,
            "pass_rate": passed / total if total else 0,
            "avg_time": avg_time,
            "shuffle_seed": args.shuffle_seed,
            "results": results,
        }
        with open(args.output, "w") as f:
            json.dump(output_data, f, indent=2)
        print(f"Saved to {args.output}")


if __name__ == "__main__":
    main()
