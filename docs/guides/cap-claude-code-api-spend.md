---
title: Cap Claude Code API Spend with a Local Budget Gateway
description: Cap Claude Code API spend with Stoke, a local gateway enforcing a hard budget, rate limits, and a loop breaker before any request reaches Anthropic.
slug: cap-claude-code-api-spend
category: Cost
icon: gauge
---

# Cap Claude Code API Spend with a Local Budget Gateway

Claude Code on a metered Anthropic API key can retry a failed tool call, re-send the same prompt in a loop, or grind through a task far longer than you expected. Anthropic's billing alerts fire hours late, so the first time you notice is on the invoice. This guide puts a small local gateway — [Stoke](https://github.com/Ozperium/stoke) — between Claude Code and the Anthropic API so requests over your cap are refused *before* Anthropic is contacted, and repeated identical prompts are stopped by a loop breaker.

## How this works

Stoke is a single Rust binary (~5.5 MB) that listens on `127.0.0.1:8787` and speaks the Anthropic Messages API on `POST /v1/messages`. You point `ANTHROPIC_BASE_URL` at it. On every request Stoke runs its enforcement — auth, budget cap, rate limit, loop breaker — and only then forwards the request to Anthropic. Nothing that trips a check ever reaches Anthropic, so it never costs a token.

Be clear about the scope: **this forwards to Anthropic. It does not run Claude Code on local models.** Stoke is a policy-enforcing passthrough, not a translator — your prompts still go to Claude, you still pay Anthropic's per-token price, and the gateway's job is to refuse the calls you don't want to pay for.

## Install Stoke

```bash
curl -sSf https://stokegate.com/install | sh -s -- --version nightly
```

This pulls a checksum-verified static binary for macOS (arm64/x64) or Linux (x64/arm64). There is no stable tag yet — a bare `| sh` resolves `latest` and compiles from source, so keep the `--version nightly` flag. It installs two binaries: `stoke` (the server) and `stoke-cli`.

## Configure the gateway

Create `stoke.toml`. You need one `[[providers]]` block of `type = "anthropic"` and one `[[keys]]` block that sets the dollar cap:

```toml
[server]
host = "127.0.0.1"
port = 8787

[[providers]]
name = "anthropic"
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"
tier = "cloud"

[[keys]]
key = "cc-cap-key"
budget_usd = 20.0
rate_limit_rpm = 60
```

Two separate keys are in play, and keeping them straight is the whole trick:

- **The gateway key** (`cc-cap-key` above) is a string you invent. Claude Code sends it to Stoke, and Stoke meters *it* — the budget and rate limit apply to this key.
- **The real Anthropic key** (`sk-ant-...`) is read server-side from the `ANTHROPIC_API_KEY` environment variable via `api_key_env`. Stoke attaches it as `x-api-key` on the way out to Anthropic. It never lives in Claude Code's config.

When the metered spend for `cc-cap-key` reaches `budget_usd`, the next request is refused with HTTP 429 and a body like `Budget exceeded: $20.0130/$20.0000 for key cc-cap-k` (Stoke identifies the key by its first eight characters). `rate_limit_rpm` caps requests per rolling 60-second window.

One caveat that decides whether the dollar cap does anything: Stoke only meters models it can price. It ships a built-in price table that today knows `claude-haiku-4` and `claude-sonnet-4`; a model ID it doesn't recognize is priced at $0, so its spend never moves the meter. Point Claude Code at a model Stoke prices (this guide uses `claude-haiku-4`), and check the table with `stoke-cli pricing`.

## Start Stoke

Stoke is fail-closed: with no `STOKE_API_KEYS` set (and no `STOKE_DEV=1`), every request is rejected with 401. The gateway key from `stoke.toml` must also appear in `STOKE_API_KEYS`. From the directory containing `stoke.toml`:

```bash
export ANTHROPIC_API_KEY="sk-ant-...your real key..."
export STOKE_API_KEYS="cc-cap-key"
stoke-cli serve
```

The real key lives only in this process's environment. Anyone hitting the gateway needs the gateway key, not the Anthropic key.

## Point Claude Code at Stoke

Claude Code authenticates to a base URL with a Bearer token when you set `ANTHROPIC_AUTH_TOKEN` (it sends `Authorization: Bearer <token>`). Stoke authenticates callers by exactly that header — so use `ANTHROPIC_AUTH_TOKEN`, not `ANTHROPIC_API_KEY` (which Claude Code would send as `x-api-key`, a header Stoke's auth ignores, giving you a 401). In the shell where you run Claude Code:

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8787"
export ANTHROPIC_AUTH_TOKEN="cc-cap-key"
unset ANTHROPIC_API_KEY
claude
```

`unset ANTHROPIC_API_KEY` keeps the client shell free of the real key — Stoke holds it. Claude Code now sends every request to `http://127.0.0.1:8787/v1/messages` with `Authorization: Bearer cc-cap-key`, and Stoke enforces before forwarding to Anthropic.

## Verify it worked

Health check needs no auth:

```bash
curl -sS http://127.0.0.1:8787/health
# {"status":"ok","service":"stoke"}
```

Confirm fail-closed — a request with no Bearer token is rejected before it touches Anthropic:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/v1/messages \
  -H "content-type: application/json" \
  -d '{"model":"claude-haiku-4","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
# 401
```

Now a real forwarded request, proving passthrough:

```bash
curl -sS http://127.0.0.1:8787/v1/messages \
  -H "Authorization: Bearer cc-cap-key" \
  -H "content-type: application/json" \
  -d '{
    "model": "claude-haiku-4",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "Say hi in three words."}]
  }'
```

You get back a normal Anthropic Messages response. Trip the loop breaker by sending the *same* prompt five times inside 60 seconds — the threshold is a global constant (5 similar requests in 60s → blocked for 120s):

```bash
for i in 1 2 3 4 5; do
  curl -sS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/v1/messages \
    -H "Authorization: Bearer cc-cap-key" \
    -H "content-type: application/json" \
    -d '{"model":"claude-haiku-4","max_tokens":16,"messages":[{"role":"user","content":"retry me"}]}'
done
# 200
# 200
# 200
# 200
# 429
```

The fifth call's body reads: `Loop detected: key cc-cap-k sent 5 similar requests within 60s. Blocked for 120s. Check your agent's retry logic — it may be stuck.` That is a stuck-retry storm — the most common way agents burn credits — stopped at the gateway.

Inspect the per-key ledger any time. `spend_usd` accrues only from the priced, non-streaming calls above, so expect a small figure; your exact numbers will differ:

```bash
curl -sS http://127.0.0.1:8787/v1/budget -H "Authorization: Bearer cc-cap-key"
# {"auth_enabled":true,"keys":[{"key":"cc-cap-k","spend_usd":0.0007,
#   "limit_usd":20.0,"recent_requests":6}], ... }
```

## What this doesn't do

Read this section before you trust the dollar figure.

- **It forwards to Anthropic; it does not run Claude Code on local models.** Your traffic still goes to Claude and still bills Anthropic. Stoke controls *whether* a call goes out, not *where* it runs.
- **Subscriptions can't be dollar-capped.** If you use Claude Code on a Claude Max or Pro subscription rather than a metered API key, there is no per-request dollar price to cap. Stoke can still rate-limit and loop-kill that traffic — it just can't put a USD ceiling on it. Hard `budget_usd` caps apply only to metered API keys.
- **Streamed responses don't yet accrue spend.** Spend records from non-streaming responses only; streamed-request cost accounting isn't implemented. Claude Code streams by default, so treat the loop breaker and `rate_limit_rpm` — which enforce on *all* traffic, streaming included, alongside auth — as your primary guardrails, and `budget_usd` as a hard backstop for the non-streaming, priced portion.
- **Pricing comes from a built-in table.** Stoke meters a response by looking its model up in a built-in price table; a model ID it doesn't recognize is priced at $0, so the dollar meter won't move for it. Check which models Stoke prices before you rely on the cap:

```bash
stoke-cli pricing
```

- **Loop thresholds are global**, not per-key, and this is pre-release software (MIT).

## Next steps

Get the binary, drop a `[[keys]]` cap in front of your metered key, and let the loop breaker do the rest — source, config reference, and issues are at [github.com/Ozperium/stoke](https://github.com/Ozperium/stoke).
