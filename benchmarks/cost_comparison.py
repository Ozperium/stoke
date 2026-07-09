#!/usr/bin/env python3
"""
Stoke Cost Comparison Table

Generates the headline comparison: local fusion vs cloud models on HumanEval.

Usage:
  python3 cost_comparison.py
"""

import json
from pathlib import Path

# HumanEval results (from BENCHMARKS.md)
HUMANEVAL_RESULTS = {
    "Stoke test_vote (5 local)": {
        "pass_rate": 0.921,
        "models": ["qwen2.5-coder:7b", "qwen2.5-coder:3b", "qwen3:8b", "phi4-mini", "llama3.2:3b"],
        "avg_time": 22.8,
        "cost_per_call": 0.0,
        "tokens_per_call": 500,  # approximate
    },
    "qwen2.5-coder:3b (best single local)": {
        "pass_rate": 0.817,
        "models": ["qwen2.5-coder:3b"],
        "avg_time": 2.6,
        "cost_per_call": 0.0,
        "tokens_per_call": 500,
    },
    # Cloud models (published HumanEval pass@1 scores)
    "GPT-5.2": {
        "pass_rate": 0.915,
        "models": ["gpt-5.2"],
        "avg_time": 3.0,
        "cost_per_call": 0.001625,  # $2.50/1M in + $10/1M out, ~500 tokens
        "tokens_per_call": 500,
    },
    "GPT-5.2-mini": {
        "pass_rate": 0.878,
        "models": ["gpt-5.2-mini"],
        "avg_time": 1.5,
        "cost_per_call": 0.000195,  # $0.30/1M in + $1.20/1M out
        "tokens_per_call": 500,
    },
    "Claude Sonnet 4": {
        "pass_rate": 0.933,
        "models": ["claude-sonnet-4"],
        "avg_time": 4.0,
        "cost_per_call": 0.00435,  # $3/1M in + $15/1M out
        "tokens_per_call": 500,
    },
    "Claude Haiku 4": {
        "pass_rate": 0.872,
        "models": ["claude-haiku-4"],
        "avg_time": 1.0,
        "cost_per_call": 0.00116,  # $0.80/1M in + $4/1M out
        "tokens_per_call": 500,
    },
    "DeepSeek v4 Pro": {
        "pass_rate": 0.902,
        "models": ["deepseek-v4-pro:cloud"],
        "avg_time": 5.0,
        "cost_per_call": 0.001375,  # $1.10/1M in + $4.40/1M out
        "tokens_per_call": 500,
    },
    "DeepSeek v4 Flash": {
        "pass_rate": 0.866,
        "models": ["deepseek-v4-flash:cloud"],
        "avg_time": 2.0,
        "cost_per_call": 0.000375,  # $0.30/1M in + $1.20/1M out
        "tokens_per_call": 500,
    },
    "Gemini 3 Pro": {
        "pass_rate": 0.921,
        "models": ["gemini-3-pro"],
        "avg_time": 3.0,
        "cost_per_call": 0.001875,  # $1.25/1M in + $5/1M out
        "tokens_per_call": 500,
    },
    "Gemini 3 Flash": {
        "pass_rate": 0.854,
        "models": ["gemini-3-flash"],
        "avg_time": 1.0,
        "cost_per_call": 0.000225,  # $0.15/1M in + $0.60/1M out
        "tokens_per_call": 500,
    },
}


def cost_for_n_calls(cost_per_call: float, n: int) -> float:
    return cost_per_call * n


def main():
    print("=" * 90)
    print("Stoke — HumanEval Cost Comparison")
    print("=" * 90)
    print()
    print(f"{'Config':<40} {'Pass@1':>8} {'Time/call':>10} {'Cost/call':>10} {'Cost/1K':>10} {'Savings':>10}")
    print("-" * 90)

    baseline_cost = HUMANEVAL_RESULTS["GPT-5.2"]["cost_per_call"]

    for name, data in HUMANEVAL_RESULTS.items():
        pass_rate = f"{data['pass_rate']*100:.1f}%"
        time_str = f"{data['avg_time']:.1f}s"
        cost_str = f"${data['cost_per_call']:.6f}"
        cost_1k = f"${cost_for_n_calls(data['cost_per_call'], 1000):.2f}"

        if baseline_cost > 0 and data['cost_per_call'] > 0:
            savings = f"{(1 - data['cost_per_call'] / baseline_cost) * 100:.0f}%"
        elif data['cost_per_call'] == 0:
            savings = "100%"
        else:
            savings = "—"

        print(f"{name:<40} {pass_rate:>8} {time_str:>10} {cost_str:>10} {cost_1k:>10} {savings:>10}")

    print("-" * 90)
    print()

    # Headline comparison
    local = HUMANEVAL_RESULTS["Stoke test_vote (5 local)"]
    gpt52 = HUMANEVAL_RESULTS["GPT-5.2"]
    claude = HUMANEVAL_RESULTS["Claude Sonnet 4"]

    print("HEADLINE COMPARISON")
    print("-" * 60)
    print(f"  Stoke (5 local models, test_vote):  {local['pass_rate']*100:.1f}% @ ${local['cost_per_call']:.4f}/call")
    print(f"  GPT-5.2 (cloud):                       {gpt52['pass_rate']*100:.1f}% @ ${gpt52['cost_per_call']:.4f}/call")
    print(f"  Claude Sonnet 4 (cloud):               {claude['pass_rate']*100:.1f}% @ ${claude['cost_per_call']:.4f}/call")
    print()
    print(f"  Cost savings vs GPT-5.2:     100% (${cost_for_n_calls(gpt52['cost_per_call'], 10000):.2f} saved per 10K calls)")
    print(f"  Cost savings vs Claude S4:   100% (${cost_for_n_calls(claude['cost_per_call'], 10000):.2f} saved per 10K calls)")
    print(f"  Accuracy vs GPT-5.2:         +{(local['pass_rate'] - gpt52['pass_rate'])*100:.1f}pp")
    print(f"  Accuracy vs Claude Sonnet 4: {(local['pass_rate'] - claude['pass_rate'])*100:+.1f}pp")
    print()

    # GPQA results (if available)
    gpqa_files = {
        "qwen2.5-coder:3b": "/tmp/gpqa_198_qwen3b.json",
        "test_vote (5 small)": "/tmp/gpqa_198_test_vote.json",
    }

    gpqa_data = {}
    for name, path in gpqa_files.items():
        p = Path(path)
        if p.exists():
            with open(p) as f:
                d = json.load(f)
                gpqa_data[name] = d

    if gpqa_data:
        print("GPQA DIAMOND RESULTS (198 questions)")
        print("-" * 60)
        print(f"{'Config':<30} {'Accuracy':>10} {'Avg Time':>10}")
        for name, d in gpqa_data.items():
            acc = f"{d['pass_rate']*100:.1f}%"
            avg = f"{d['avg_time']:.1f}s"
            print(f"{name:<30} {acc:>10} {avg:>10}")
        print()


if __name__ == "__main__":
    main()