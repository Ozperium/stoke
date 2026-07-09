# Stoke Fusion Patterns — Research & Taxonomy

> **Note:** This is research documentation for routing policies that exist in
> the Stoke codebase. Fusion/ensembling is not a product pillar and Stoke does
> not market it. The findings below — ensemble patterns can preserve quality at
> lower cost, but they don't add intelligence — are what led to Stoke's
> enforcement-first focus.

## Academic Foundation

Our patterns are grounded in peer-reviewed research. This document maps each pattern to its academic origin and explains the mechanism abstractly — not bound to any benchmark.

---

## Pattern Taxonomy (Academic Classification)

The [LLM Ensemble Survey (Chen et al., 2025)](https://arxiv.org/abs/2502.18036) defines three categories:

1. **Ensemble Before Inference** — route to the best model before generating
2. **Ensemble During Inference** — collaborate at token/step level during generation
3. **Ensemble After Inference** — generate full responses, then aggregate

Stoke's codebase implements patterns from the before- and after-inference categories; during-inference patterns are documented here for the taxonomy only.

---

## Patterns (8 total)

### 1. Single (Baseline)
**Academic category:** None — direct inference.

One model handles the request. No fusion.

### 2. Parallel Vote (Ensemble After — Majority)
**Academic origin:** [Self-Consistency (Wang et al., ICLR 2023)](https://arxiv.org/abs/2203.11171)

Multiple models generate responses independently. The most common response wins (majority vote on content).

**When it works:** When models have complementary error patterns and the correct answer is likely to be produced by at least one model. Self-consistency showed +17.9% on GSM8K, +11.0% on SVAMP.

**When it fails:** When models converge on the same wrong answer (confirmed by [Debate or Vote, NeurIPS 2025](https://arxiv.org/abs/2508.17536) — debate gains are "primarily from majority voting" not from the debate itself). Also fails for code where correct implementations differ syntactically.

**Status:** implemented in `router.rs` as `parallel_vote`, but not currently exposed on the request path (a request asking for it falls through to single-model routing).

### 3. Self-Consistency (Ensemble After — Same-Model Voting)
**Academic origin:** [Self-Consistency (Wang et al., ICLR 2023)](https://arxiv.org/abs/2203.11171)

Same model, N samples at temperature > 0, majority vote. Different from parallel_vote (which uses different models). Exploits the insight that correct reasoning paths converge — multiple valid approaches to the same problem tend to agree on the answer.

**When it works:** Math, reasoning, multi-step problems where chain-of-thought varies but converges. The original paper showed +17.9% on GSM8K.

**When it fails:** When the model is fundamentally wrong (all samples inherit the same bias).

**Our implementation:** Same model, N samples at configurable temperature, majority vote.

### 4. Cascade (Ensemble Before — Sequential Fallback)
**Academic origin:** [RouteLLM (Ong et al., 2024)](https://arxiv.org/abs/2406.18665) — cost-aware routing

Try models in order. If a model fails (HTTP error, timeout, bad response), fall through to the next. This is OpenRouter's core pattern — provider failover.

**When it works:** Cost optimization (try cheap model first, expensive model only if needed), resilience (provider outages).

**Our implementation:** Sequential provider list, first success wins.

### 5. Cascade+Test (Ensemble After — Sequential with Validation)
**Academic origin:** Novel — combines cascade failover with test-based selection.

Try each model sequentially. After each response, validate it against a test or verifier. Stop on first pass. This is the sequential version of test_vote — cheaper (no parallel fan-out) but slower (sequential).

**When it works:** Code generation with unit tests, any task with an executable verifier. In our experiments this matched test_vote's accuracy at a fraction of the latency, since models run sequentially and stop on first pass.

**When it fails:** When no model can solve the problem (same ceiling as test_vote).

**Our implementation:** Sequential models, test each, stop on first pass, return last result as fallback.

### 6. Test Vote (Ensemble After — Parallel with Validation)
**Academic origin:** [Best-of-N (Snell et al., 2024)](https://arxiv.org/abs/2408.03314) + [Self-Consistency](https://arxiv.org/abs/2203.11171)

Fan out to N models in parallel. Test each candidate against a verifier. Return the first that passes. This is best-of-N where the verifier is an executable test rather than a reward model.

**When it works:** Code generation with unit tests. In our experiments this matched cascade_test's accuracy; marginal cost per request was local electricity only.

**When it fails:** When no model in the set can solve the problem (same ceiling as cascade_test). Also wastes compute — all models run even if the first one passes.

**Academic insight:** [Snell et al.](https://arxiv.org/abs/2408.03314) showed that best-of-N is suboptimal — a "compute-optimal" strategy that adapts N to prompt difficulty is 4x more efficient. Cascade+Test is a step toward this (sequential = adaptive N).

**Our implementation:** Parallel fan-out, test each as it arrives, early-exit on first pass.

### 7. Chain / Refine (Ensemble During — Sequential Refinement)
**Academic origin:** [RecursiveMAS (Yang et al., 2026)](https://arxiv.org/abs/2604.25917) — recursive latent-space refinement; [DMAD (Liu et al., ICLR 2025)](https://openreview.net/forum?id=qsKo9mdGNu) — diverse multi-agent debate

Model A generates, Model B reviews and refines. The output of each model feeds as context to the next. This is the text-level version of RecursiveMAS's latent-space recursion — we do it in text space (decode between steps) while RecursiveMAS does it in latent space (no decoding until final round).

**When it works:** Draft→refine pipelines, prose improvement, SWE-bench (small model drafts patch, large model refines).

**When it fails:** When the refiner degrades working code (a failure mode we observed directly). [Debate or Vote (NeurIPS 2025)](https://arxiv.org/abs/2508.17536) proved debate is a martingale — it doesn't improve correctness in expectation, only variance reduction through voting helps.

**Status:** not currently implemented in the codebase.

### 8. Parallel+Merge (Ensemble After — Aggregation)
**Academic origin:** [Mixture-of-Agents (Wang et al., 2024)](https://arxiv.org/abs/2406.04692) — layered aggregation

Fan out to N generator models. A merge model synthesizes all responses into one. This is Mixture-of-Agents (MoA) — the paper showed open-source models beating GPT-4 Omni on AlpacaEval 2.0 (65.1% vs 57.5%).

**Key insight from MoA paper:** "LLMs tend to generate better responses when presented with outputs from other models, even if those outputs are of lower quality." The aggregator doesn't just pick — it synthesizes.

**When it works:** Text generation, summarization, multi-perspective synthesis. MoA showed SOTA on AlpacaEval 2.0, MT-Bench, FLASK.

**When it fails:** Code (merging different implementations produces invalid syntax). In our experiments the merge model overrode correct answers from a stronger general model — merging degraded accuracy on reasoning tasks.

**Status:** not currently implemented in the codebase.

---

## What We're Missing (Based on Research)

### 9. Best-of-N with Verifier (Not yet implemented)
**Academic origin:** [Best-of-N + Process Reward Models (Snell et al., 2024)](https://arxiv.org/abs/2408.03314); [Self-Certainty (Kang et al., 2025)](https://arxiv.org/abs/2502.18581)

Generate N samples, score each with a verifier (reward model or self-certainty), pick the best. Different from test_vote (which uses binary pass/fail) — this uses a graded score.

**Self-Certainty** is the reward-free version: use the model's own logprob distribution to estimate confidence. No external reward model needed. Scales with N like reward models but without the compute overhead.

**Why it matters:** This is the pattern that would improve GPQA scores — it selects the best reasoning path without needing an executable test.

### 10. Mixture-of-Agents Layered (Not yet implemented)
**Academic origin:** [MoA (Wang et al., 2024)](https://arxiv.org/abs/2406.04692)

Our parallel_merge is single-layer MoA. The full MoA is multi-layer: each layer's agents see all outputs from the previous layer. Deeper = better (up to diminishing returns).

### 11. Cost-Aware Auto-Routing (Not yet implemented)
**Academic origin:** [RouteLLM (Ong et al., 2024)](https://arxiv.org/abs/2406.18665)

Classify prompt difficulty, route to cheap model for easy, expensive model for hard. RouteLLM showed 2x cost savings while maintaining quality.

---

## Key Research Insights

1. **Voting > Debate** — [Debate or Vote (NeurIPS 2025)](https://arxiv.org/abs/2508.17536) proved debate is a martingale; gains come from ensembling, not interaction. This validates our parallel_vote and self_consistency patterns.

2. **Diversity matters** — [DMAD (ICLR 2025)](https://openreview.net/forum?id=qsKo9mdGNu) showed that diverse reasoning approaches beat persona-based diversity. For us: use different model families, not just different prompts.

3. **Adaptive compute** — [Snell et al.](https://arxiv.org/abs/2408.03314) showed compute-optimal scaling (adapting N to difficulty) is 4x more efficient than fixed best-of-N. Our cascade_test is a step toward this.

4. **Collaborativeness** — [MoA](https://arxiv.org/abs/2406.04692) found models improve when seeing other models' outputs, even inferior ones. This is why parallel_merge works for text.

5. **RecursiveMAS** — [+8.3% avg accuracy, 2.4x speedup, -75.6% tokens](https://recursivemas.github.io/) via latent-space recursion. The four collaboration patterns they identify map to ours: Sequential (chain), Mixture (parallel_merge), Distillation (chain with teacher→student), Deliberation (debate — implemented in `router.rs` as `deliberation`, not exposed on the request path).

---

## Revised Pattern Naming (Abstract, Not Benchmark-Specific)

| # | Pattern | Mechanism | Academic Origin |
|---|---|---|---|
| 1 | **Direct** | Single model inference | — |
| 2 | **Consensus** | Parallel models, majority vote | Self-Consistency (ICLR 2023) |
| 3 | **Self-Consensus** | Same model, N samples, majority vote | Self-Consistency (ICLR 2023) |
| 4 | **Fallback** | Sequential providers, first success | RouteLLM (2024) |
| 5 | **Validated Fallback** | Sequential models + verifier, first pass | Best-of-N (2024) + cascade |
| 6 | **Validated Consensus** | Parallel models + verifier, first pass | Best-of-N (2024) |
| 7 | **Refine** *(planned)* | Sequential refinement (draft → improve) | RecursiveMAS (2026), DMAD (ICLR 2025) |
| 8 | **Synthesize** *(planned)* | Parallel generation + merge model | Mixture-of-Agents (2024) |
| 9 | **Best-of-N** *(planned)* | N samples + reward model/self-certainty | Snell et al. (2024), Self-Certainty (2025) |
| 10 | **Layered Synthesize** *(planned)* | Multi-layer MoA | MoA (2024) |
| 11 | **Auto-Route** *(planned)* | Prompt classification → model selection | RouteLLM (2024) |

## References

1. Wang, X. et al. "Self-Consistency Improves Chain of Thought Reasoning in Language Models." ICLR 2023.
2. Wang, J. et al. "Mixture-of-Agents Enhances Large Language Model Capabilities." 2024.
3. Ong, I. et al. "RouteLLM: Learning to Route LLMs with Preference Data." 2024.
4. Snell, C. et al. "Scaling LLM Test-Time Compute Optimally can be More Effective than Scaling Model Parameters." 2024.
5. Kang, Z. et al. "Scalable Best-of-N Selection for Large Language Models via Self-Certainty." 2025.
6. Yang, X. et al. "RecursiveMAS: Recursive Multi-Agent Systems." 2026.
7. Liu, Y. et al. "Breaking Mental Set to Improve Reasoning through Diverse Multi-Agent Debate." ICLR 2025.
8. Wu, H. et al. "Debate or Vote: Which Yields Better Decisions in Multi-Agent Large Language Models?" NeurIPS 2025.
9. Chen, Z. et al. "Harnessing Multiple Large Language Models: A Survey on LLM Ensemble." 2025.
10. Tran, K. et al. "Multi-Agent Collaboration Mechanisms: A Survey of LLMs." 2025.