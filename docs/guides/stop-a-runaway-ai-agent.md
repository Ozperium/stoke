---
title: Stop a Runaway AI Agent From Burning API Credits
description: An AI agent that loops at 2am burns API credits until morning. Stop runaway agent spend at the request path with hard budget caps and a loop breaker.
slug: stop-a-runaway-ai-agent
category: Cost
icon: flatline
---

# Stop a Runaway AI Agent Before It Burns Your API Credits

An agent that starts retrying at 2am does not stop because you are asleep. It re-sends the same tool call, fans out, and re-plans — and every attempt is a metered API request. By the time the billing alert fires, the money is already gone. That alert is a receipt, not a control: it tells you what happened, hours after you could have done anything about it.

The fix is to put a gate in the request path that refuses the request *before* the provider is called. [Stoke](https://github.com/Ozperium/stoke) is a single Rust binary that sits between your agent and the model API and does exactly that. This guide sets up two independent protections — a hard per-key dollar cap and a loop breaker — and then actually triggers both with `curl` so you can see the refusals.

## The two protections you actually get

These are separate mechanisms. Either one can stop a runaway; they cover different failure modes.

- **Hard budget cap.** A per-key cumulative spend limit in USD. Once a key's spend *reaches* the cap, the next request is refused with `429 Budget exceeded` — no provider call is made. This stops slow bleed: an agent that is technically making progress but has already spent more than you authorized.
- **Loop breaker.** If one key sends 5 repeated requests within 60 seconds, that key is blocked for 120 seconds with `429 Loop detected`. By default "repeated" means an identical request (same model, same message content, same temperature — hashed). This stops the fast failure: a tight retry loop that would rack up hundreds of calls before any dollar cap noticed.

Both checks run on the request path, before Stoke ever contacts a model. Everything except `GET /health` also requires an `Authorization: Bearer <key>` header, and Stoke is fail-closed: with no keys configured and no `STOKE_DEV=1`, every request is rejected.

## Set a per-key budget and register the key

Create `stoke.toml`. A key gets enforcement only when it appears in a `[[keys]]` table **and** in the `STOKE_API_KEYS` environment variable — the table is the policy, the env var is the allowlist.

```toml
# stoke.toml
[server]
host = "127.0.0.1"
port = 8787

# Per-key enforcement policy. This key must ALSO be in STOKE_API_KEYS.
[[keys]]
key = "agent-key"
budget_usd = 5.00        # hard cumulative spend cap; over it -> 429
rate_limit_rpm = 120     # optional: max requests per rolling 60s window

# Where real traffic goes. Your local Ollama, bound to loopback.
[[providers]]
name = "local"
base_url = "http://127.0.0.1:11434/v1"
tier = "local"
```

Prefer not to hand-write it? `stoke-cli init --output stoke.toml` generates a starter config and discovers a default model from your own Ollama. You still add the `[[keys]]` table yourself.

## Start Stoke and route the agent through it

Export the allowlist and start the gateway. `stoke-cli serve` reads `stoke.toml` from the current directory and launches the server on `127.0.0.1:8787`.

```bash
export STOKE_API_KEYS="agent-key"
stoke-cli serve
```

Now point your agent at Stoke instead of the upstream API. For any OpenAI-compatible client that means two settings:

```bash
OPENAI_BASE_URL=http://127.0.0.1:8787/v1
OPENAI_API_KEY=agent-key
```

Every request the agent makes now passes through the budget cap, the rate limit, and the loop breaker before anything is spent.

## Watch the loop breaker refuse (live)

This is the demo you can run without a metered API key. Pick any model you have pulled in Ollama (`ollama list` shows them), then fire the *same* request five times in a row:

```bash
MODEL="<a-model-on-your-ollama>"

for i in 1 2 3 4 5; do
  printf '\n--- request %s ---\n' "$i"
  curl -s -w '\nHTTP %{http_code}\n' \
    http://127.0.0.1:8787/v1/chat/completions \
    -H "Authorization: Bearer agent-key" \
    -H "Content-Type: application/json" \
    -d "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"retry: fix the failing test\"}],\"temperature\":0}"
done
```

Requests 1–4 pass the loop check and go through to the model (or return an upstream error if the model isn't reachable — either way the loop counter still increments, because the check runs before the provider call). The fifth trips the breaker:

```
--- request 5 ---
Loop detected: key agent-ke sent 5 similar requests within 60s. Blocked for 120s. Check your agent's retry logic — it may be stuck.
HTTP 429
```

The whole key is now frozen for 120 seconds — not just that prompt. Send *any* request with the same key during the block and you get the second loop message:

```bash
curl -s -w '\nHTTP %{http_code}\n' \
  http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer agent-key" \
  -H "Content-Type: application/json" \
  -d "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"something completely different\"}],\"temperature\":0}"
```

```
Loop detected: key agent-ke blocked for 118s (repeated similar requests). Review your agent's retry logic.
HTTP 429
```

That freeze is the point: a looping agent is a stuck agent, so Stoke stops all of its traffic until the block expires, then lets it start fresh. (The threshold — 5 requests / 60s / 120s block — is a global constant today, not per-key. Optional semantic matching, which counts near-duplicate wording as "the same" request, is turned on with `STOKE_SEMANTIC_CACHE` plus an embedding model in `STOKE_EMBED_MODEL`.)

## Watch the budget cap refuse

The dollar cap needs real spend to accrue, so this one requires a metered provider and low limit. Set `budget_usd = 0.05` for the key, restart, and send non-streaming completions against your metered upstream until cumulative spend reaches the cap. The next request is refused:

```
Budget exceeded: $0.0512/$0.0500 for key agent-ke
HTTP 429
```

The format is `current/limit for key <first-8-chars>`. No provider call happens once you are over — the request dies at the gate, so an over-budget key cannot spend one more cent.

One honesty note that matters here: spend currently accrues from **non-streaming** responses only. Streamed-request cost accounting is not implemented yet. Auth, rate limiting, and the loop breaker all cover streaming traffic — so a streaming agent can still be loop-killed and rate-limited — but a hard USD cap only bites on non-streaming calls against a metered API key. Subscription traffic (Claude Max, ChatGPT Plus) has no per-request price, so it can be rate-limited and loop-killed but not dollar-capped.

## Read spend and the receipts ledger: GET /v1/budget

To see where a key stands before it hits the wall, read the budget endpoint (auth required):

```bash
curl -s http://127.0.0.1:8787/v1/budget \
  -H "Authorization: Bearer agent-key" | jq
```

```json
{
  "auth_enabled": true,
  "keys": [
    { "key": "agent-ke", "spend_usd": 4.83, "limit_usd": 5.0, "recent_requests": 3 }
  ],
  "receipts": {
    "requests": 128,
    "zero_marginal_requests": 119,
    "zero_marginal_pct": 93.0,
    "cloud_listprice_avoided_usd_est": 3.4012
  }
}
```

`spend_usd` versus `limit_usd` is your live headroom. The `receipts` block is the ledger: how many requests were served, how many at zero marginal cost (local models on hardware you already own — electricity and your GPU, never truly free), and a list-price counterfactual estimate for what a cloud model would have charged. It is an estimate, not a quality claim.

## Why the request path beats a dashboard

A billing dashboard and a spend alert are both after-the-fact. They read a number that has already been charged. Between the moment an agent starts looping and the moment a threshold-based alert reaches a human, an unbounded number of paid requests can complete. Nothing in that loop is *deciding* whether the next request should happen.

Stoke moves the decision into the path. The budget check, rate check, and loop check all run before any upstream call, and the gateway is fail-closed, so the failure mode is "reject," not "pass through and hope." You are not watching a graph and reacting; the runaway is refused at request N, not investigated at request 900.

## Verify it worked

Three checks confirm the gate is live:

```bash
# 1. Liveness — the only unauthenticated endpoint.
curl -s http://127.0.0.1:8787/health
# {"status":"ok","service":"stoke"}

# 2. Auth is enforced — a request with no Bearer token should be rejected.
curl -s -o /dev/null -w '%{http_code}\n' \
  http://127.0.0.1:8787/v1/models
# 401

# 3. The loop breaker fires — run the five-request loop above.
#    Request 5 returns: HTTP 429, body "Loop detected: ..."
```

Seeing `401` on an unauthenticated call and `429 Loop detected` on the fifth repeat means both the auth wall and the loop breaker are protecting your key.

## What this doesn't do

- **No dollar cap on streaming or subscription traffic.** Spend accrues from non-streaming responses only; hard USD caps need a metered API key. Loop and rate limits still cover everything.
- **Loop thresholds are global, not per-key.** The 5 / 60s / 120s values are constants today; per-key tuning is on the roadmap.
- **No cross-model fallback.** Failover retries the *same* model on the next healthy node. Automatic "fall back to a cheaper vendor" chains are not implemented.
- **`/v1/messages` forwards to Anthropic.** It is an enforced passthrough to a configured Anthropic upstream, not a way to run Claude on local GPUs.

## Install and next steps

Grab the binary (static builds for macOS and Linux, checksum-verified, with a source fallback):

```bash
curl -sSf https://stokegate.com/install | sh -s -- --version nightly
```

There is no stable tag yet — a bare `| sh` resolves `latest` and compiles from source, so pin `--version nightly` for the prebuilt binary. Then `stoke-cli init`, add a `[[keys]]` cap, export `STOKE_API_KEYS`, and route your agent through `127.0.0.1:8787`. Full config reference and source: [github.com/Ozperium/stoke](https://github.com/Ozperium/stoke).
