#!/usr/bin/env python3
"""
Stoke Cost Simulation — projects real-world savings for teams.

No LLM calls needed — uses our benchmark data to calculate costs.

Usage:
  python3 cost_simulation.py
  python3 cost_simulation.py --team-size 50 --requests 10000 --model gpt-5.2
  python3 cost_simulation.py --scenarios
"""

import argparse
import json
from dataclasses import dataclass
from pathlib import Path

# Model pricing per 1M tokens (from our cost.rs)
MODEL_PRICING = {
    "gpt-5.2": {"input": 2.50, "output": 10.00},
    "gpt-5.2-mini": {"input": 0.30, "output": 1.20},
    "claude-sonnet-4": {"input": 3.00, "output": 15.00},
    "claude-haiku-4": {"input": 0.80, "output": 4.00},
    "gemini-3-pro": {"input": 1.25, "output": 5.00},
    "gemini-3-flash": {"input": 0.15, "output": 0.60},
    "deepseek-v4-pro:cloud": {"input": 1.10, "output": 4.40},
    "deepseek-v4-flash:cloud": {"input": 0.30, "output": 1.20},
    "kimi-k2.6:cloud": {"input": 0.60, "output": 2.50},
    "glm-5.2:cloud": {"input": 0.50, "output": 2.00},
    "minimax-m3:cloud": {"input": 0.30, "output": 1.20},
    "qwen3.6-plus": {"input": 0.40, "output": 1.60},
    # Local models = $0
    "qwen2.5-coder:3b": {"input": 0.0, "output": 0.0},
    "qwen2.5-coder:7b": {"input": 0.0, "output": 0.0},
    "gemma4:e4b": {"input": 0.0, "output": 0.0},
    "gemma4:12b-mlx": {"input": 0.0, "output": 0.0},
}

# Benchmark results (from our actual runs)
BENCHMARK_RESULTS = {
    "gpt-5.2_direct": {"accuracy": 95.0, "cost_per_call": None, "source": "published"},
    "claude_sonnet_direct": {"accuracy": 92.0, "cost_per_call": None, "source": "published"},
    "stoke_local_cascade": {"accuracy": 92.1, "cost_per_call": 0.0, "source": "HumanEval 164"},
    "stoke_local_testvote": {"accuracy": 92.1, "cost_per_call": 0.0, "source": "HumanEval 164"},
    "stoke_cloud_consensus": {"accuracy": 100.0, "cost_per_call": 0.0035, "source": "HumanEval 164"},
    "stoke_local_plus_cloud_fallback": {"accuracy": 92.1, "cost_per_call": 0.00002, "source": "estimated"},
    "stoke_humanevalplus_local_testvote": {"accuracy": 87.2, "cost_per_call": 0.0, "source": "HumanEval+ 164"},
    "stoke_humanevalplus_cloud_consensus": {"accuracy": 92.1, "cost_per_call": 0.0033, "source": "HumanEval+ 164"},
    "stoke_humanevalplus_gemma4_selfcons": {"accuracy": 90.2, "cost_per_call": 0.0, "source": "HumanEval+ 164"},
    "stoke_humanevalplus_3b_single": {"accuracy": 65.2, "cost_per_call": 0.0, "source": "HumanEval+ 164"},
}

# Average tokens per request (realistic agent workloads)
# Code generation: ~3K prompt (code context) + ~500 completion
# Chat/reasoning: ~500 prompt + ~300 completion
# Weighted average for mixed workloads
AVG_PROMPT_TOKENS = 2000
AVG_COMPLETION_TOKENS = 500


@dataclass
class Scenario:
    name: str
    team_size: int
    requests_per_month: int
    current_model: str
    stoke_config: str
    cache_hit_rate: float  # 0.0 - 1.0
    cloud_fallback_rate: float  # 0.0 - 1.0, fraction of requests that hit cloud


def cost_per_call(model: str, prompt_tokens: float = 0.0, completion_tokens: float = 0.0) -> float:
    """Calculate cost per API call for a model."""
    pt = prompt_tokens if prompt_tokens > 0 else AVG_PROMPT_TOKENS
    ct = completion_tokens if completion_tokens > 0 else AVG_COMPLETION_TOKENS
    pricing = MODEL_PRICING.get(model, {"input": 0.50, "output": 2.00})
    return (pt * pricing["input"] + ct * pricing["output"]) / 1_000_000


def monthly_cost(model: str, requests: int, cache_hit_rate: float = 0.0) -> float:
    """Calculate monthly cost for a model with optional cache hit rate."""
    per_call = cost_per_call(model)
    effective_calls = requests * (1 - cache_hit_rate)
    return per_call * effective_calls


def stoke_monthly_cost(
    requests: int,
    config: str,
    cache_hit_rate: float = 0.0,
    cloud_fallback_rate: float = 0.0,
    cloud_model: str = "deepseek-v4-flash:cloud",
) -> float:
    """Calculate Stoke monthly cost based on configuration."""
    effective_calls = requests * (1 - cache_hit_rate)
    
    if config == "local_only":
        # 100% local = $0
        return 0.0
    elif config == "local_plus_cloud_fallback":
        # Most requests go to local ($0), some fall back to cloud
        local_calls = effective_calls * (1 - cloud_fallback_rate)
        cloud_calls = effective_calls * cloud_fallback_rate
        return 0.0 * local_calls + cost_per_call(cloud_model) * cloud_calls
    elif config == "cloud_consensus":
        # All requests use cloud consensus (4 models)
        # From benchmark: $0.0035 per problem
        return effective_calls * 0.0035
    elif config == "auto_budget":
        # Auto-routing in budget mode: local first, cloud only on failures
        # Assume 5% cloud fallback rate for code, 0% for chat
        return effective_calls * cloud_fallback_rate * cost_per_call(cloud_model)
    else:
        return 0.0


def print_comparison(scenario: Scenario):
    """Print a cost comparison table for a scenario."""
    print(f"\n{'='*70}")
    print(f"  {scenario.name}")
    print(f"  Team: {scenario.team_size} engineers · {scenario.requests_per_month:,} requests/month")
    print(f"{'='*70}\n")
    
    # Current cost (direct API, no cache)
    current_per_call = cost_per_call(scenario.current_model)
    current_monthly = monthly_cost(scenario.current_model, scenario.requests_per_month)
    
    # Stoke costs
    local_only = stoke_monthly_cost(scenario.requests_per_month, "local_only", scenario.cache_hit_rate)
    local_cloud = stoke_monthly_cost(
        scenario.requests_per_month, "local_plus_cloud_fallback",
        scenario.cache_hit_rate, scenario.cloud_fallback_rate
    )
    cloud_consensus = stoke_monthly_cost(scenario.requests_per_month, "cloud_consensus", scenario.cache_hit_rate)
    auto_budget = stoke_monthly_cost(
        scenario.requests_per_month, "auto_budget",
        scenario.cache_hit_rate, scenario.cloud_fallback_rate
    )
    
    configs = [
        (f"Direct {scenario.current_model}", current_monthly, current_per_call, "Baseline"),
        ("Stoke local only", local_only, 0.0, "92.1% HumanEval"),
        ("Stoke local + cloud fallback", local_cloud, local_cloud / scenario.requests_per_month if scenario.requests_per_month else 0, "92.1%+ HumanEval"),
        ("Stoke auto (budget mode)", auto_budget, auto_budget / scenario.requests_per_month if scenario.requests_per_month else 0, "Routes by task type"),
        ("Stoke cloud consensus", cloud_consensus, 0.0035, "100% HumanEval"),
    ]
    
    print(f"  {'Config':<35} {'Monthly Cost':>12} {'Per Call':>10} {'Quality':>20}")
    print(f"  {'-'*35} {'-'*12} {'-'*10} {'-'*20}")
    
    for name, monthly, per_call, quality in configs:
        savings = ""
        if monthly < current_monthly and current_monthly > 0:
            pct = (1 - monthly / current_monthly) * 100
            savings = f" (-{pct:.0f}%)"
        print(f"  {name:<35} ${monthly:>10,.2f} ${per_call:>8.5f} {quality:>20}{savings}")
    
    print(f"\n  Annual savings (local + cloud fallback): ${(current_monthly - local_cloud) * 12:,.0f}")
    print(f"  Annual savings (auto budget mode):       ${(current_monthly - auto_budget) * 12:,.0f}")


def print_scenarios():
    """Print pre-built scenarios."""
    scenarios = [
        Scenario(
            name="Small Startup (5 engineers)",
            team_size=5,
            requests_per_month=2_000,
            current_model="gpt-5.2",
            stoke_config="local_plus_cloud_fallback",
            cache_hit_rate=0.20,
            cloud_fallback_rate=0.05,
        ),
        Scenario(
            name="Mid-size SaaS (50 engineers)",
            team_size=50,
            requests_per_month=50_000,
            current_model="gpt-5.2",
            stoke_config="local_plus_cloud_fallback",
            cache_hit_rate=0.30,
            cloud_fallback_rate=0.05,
        ),
        Scenario(
            name="Enterprise (200 engineers)",
            team_size=200,
            requests_per_month=500_000,
            current_model="claude-sonnet-4",
            stoke_config="local_plus_cloud_fallback",
            cache_hit_rate=0.35,
            cloud_fallback_rate=0.03,
        ),
        Scenario(
            name="Heavy AI usage (10 engineers, agentic)",
            team_size=10,
            requests_per_month=100_000,
            current_model="gpt-5.2",
            stoke_config="auto_budget",
            cache_hit_rate=0.25,
            cloud_fallback_rate=0.10,
        ),
    ]
    
    for s in scenarios:
        print_comparison(s)
    
    # Summary table
    print(f"\n{'='*70}")
    print(f"  SUMMARY: Annual savings across scenarios")
    print(f"{'='*70}\n")
    print(f"  {'Scenario':<40} {'Current/yr':>12} {'Stoke/yr':>12} {'Savings':>12}")
    print(f"  {'-'*40} {'-'*12} {'-'*12} {'-'*12}")
    
    for s in scenarios:
        current = monthly_cost(s.current_model, s.requests_per_month) * 12
        stoke = stoke_monthly_cost(
            s.requests_per_month, "local_plus_cloud_fallback",
            s.cache_hit_rate, s.cloud_fallback_rate
        ) * 12
        savings = current - stoke
        print(f"  {s.name:<40} ${current:>10,.0f} ${stoke:>10,.0f} ${savings:>10,.0f}")


def main():
    parser = argparse.ArgumentParser(description="Stoke Cost Simulation")
    parser.add_argument("--scenarios", action="store_true", help="Run pre-built scenarios")
    parser.add_argument("--team-size", type=int, default=50, help="Number of engineers")
    parser.add_argument("--requests", type=int, default=50000, help="Requests per month")
    parser.add_argument("--model", default="gpt-5.2", help="Current model")
    parser.add_argument("--cache-rate", type=float, default=0.30, help="Cache hit rate (0-1)")
    parser.add_argument("--cloud-fallback", type=float, default=0.05, help="Cloud fallback rate (0-1)")
    args = parser.parse_args()
    
    if args.scenarios:
        print_scenarios()
    else:
        s = Scenario(
            name=f"Custom: {args.team_size} engineers, {args.requests:,} req/month",
            team_size=args.team_size,
            requests_per_month=args.requests,
            current_model=args.model,
            stoke_config="local_plus_cloud_fallback",
            cache_hit_rate=args.cache_rate,
            cloud_fallback_rate=args.cloud_fallback,
        )
        print_comparison(s)


if __name__ == "__main__":
    main()