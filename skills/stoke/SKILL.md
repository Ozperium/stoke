---
name: stoke
description: Put a coding agent behind Stoke — a gateway that enforces hard budget caps, rate limits, and a runaway-loop kill switch before any model API is called. Use when the user wants to cap what an agent can spend, stop a runaway/looping agent, route agent traffic to local models, or asks to install or configure Stoke.
---

Install and configure Stoke so that every model call this agent makes passes through a budget cap and a loop breaker first. Requests over the cap, or caught in a retry loop, are refused with `429` before a provider is contacted.

Work through the steps in order. Verify each one before moving on — an enforcement gateway that is silently misconfigured is worse than none.

## 1. Install

```sh
curl -sSf https://stokegate.com/install | sh -s -- --version nightly
```

This downloads a checksum-verified static binary (macOS arm64/x64, Linux x64/arm64) and falls back to compiling from source on other platforms. It installs `stoke` and `stoke-cli` to `~/.local/bin`. Confirm:

```sh
stoke-cli version
```

## 2. Generate a config

```sh
stoke-cli init --output stoke.toml
```

`init` discovers a default model from the user's own Ollama — Stoke ships with no model names. If Ollama is not running, it leaves a placeholder for the user to fill in.

## 3. Set the key and the cap

Stoke is **fail-closed**: with no keys configured and no dev flag, it rejects everything. Never suggest disabling that.

Add a per-key policy to `stoke.toml`. `budget_usd` is a hard cumulative cap; over it, requests get `429`. The key must also appear in `STOKE_API_KEYS`.

```toml
[[keys]]
key = "agent-key"
budget_usd = 5.0        # hard cap in USD
rate_limit_rpm = 120    # requests per rolling 60s window
```

Ask the user for the cap amount rather than inventing one.

## 4. Start the gateway

```sh
STOKE_API_KEYS=agent-key stoke
```

It listens on `127.0.0.1:8787`. Verify it is up and can see the user's models:

```sh
curl -s localhost:8787/health
curl -s -H "Authorization: Bearer agent-key" localhost:8787/v1/nodes
```

`/v1/nodes` shows each node's health and which models are pulled and warm. If a node is unreachable it is excluded from routing until it returns.

## 5. Point the agent at it

OpenAI-compatible agents (OpenCode, aider, Codex, most SDKs):

```
OPENAI_BASE_URL=http://127.0.0.1:8787/v1
```

Claude Code speaks the Anthropic Messages API:

```
ANTHROPIC_BASE_URL=http://127.0.0.1:8787
```

This requires an `anthropic` provider in `stoke.toml`, and it forwards to Anthropic. It does **not** translate Anthropic requests onto local models — do not tell the user Claude Code will run on their GPUs.

Send the key as `Authorization: Bearer agent-key`.

## 6. Prove enforcement actually works

Do not declare success on a `200`. Show the user a refusal:

```sh
# spend accrues, then the cap refuses. Repeat until you see the 429.
curl -s -w '\n%{http_code}\n' -X POST localhost:8787/v1/chat/completions \
  -H "Authorization: Bearer agent-key" -H "Content-Type: application/json" \
  -d '{"model":"<a model from /v1/nodes>","messages":[{"role":"user","content":"hi"}]}'
```

Over the cap, the body reads `Budget exceeded: $X / $Y`. Send the same prompt five times inside 60 seconds and the loop breaker returns `Loop detected: key blocked for 120s`.

Check the ledger any time:

```sh
curl -s -H "Authorization: Bearer agent-key" localhost:8787/v1/budget
```

## Scope — be honest with the user

- Hard USD caps only work on **metered API keys**. Subscription plans (Claude Max/Pro, ChatGPT Plus) have no per-request price: Stoke can rate-limit and loop-kill that traffic, but it cannot dollar-cap it.
- Spend currently accrues from non-streaming responses. Streamed-request cost accounting is not implemented yet. Auth, rate limits, and the loop breaker **do** apply to streaming.
- Local models cost electricity and the GPU you already own — not "free". Never claim zero cost.

## Troubleshooting

- **Every request returns 401** — that is the fail-closed default. Set `STOKE_API_KEYS`, or `STOKE_DEV=1` for local experiments only.
- **`No provider for model`** — the model is not on any healthy node. Check `GET /v1/nodes`.
- **Agent hangs on tool calls** — the routed model cannot emit structured `tool_calls`. Pick a tool-capable model; `GET /v1/nodes` reports each model's `tools` capability.
- **Port already in use** — another `stoke` is running on `8787`.
