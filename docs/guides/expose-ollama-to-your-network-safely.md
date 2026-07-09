---
title: Expose Ollama to Your Network Safely
description: Setting OLLAMA_HOST=0.0.0.0 publishes an unauthenticated model server to your whole LAN. Put an authenticated Stoke gateway in front of Ollama instead.
slug: expose-ollama-to-your-network-safely
category: Security
icon: shield
---

# Expose Ollama to Your Network Safely

You want to reach the Ollama running on your desktop from your laptop. Every answer online says the same thing: set `OLLAMA_HOST=0.0.0.0` and restart. It works — and it also opens an unauthenticated inference server to every device on the network. Anyone who can reach that IP can run generation on your GPU, enumerate every model you've pulled, and pin your machine at 100% util. There is no password, no token, no allow-list. This guide shows the safer pattern: leave Ollama bound to `127.0.0.1` and put an authenticated gateway in front of it as the only network-facing door.

## What `OLLAMA_HOST=0.0.0.0` actually does

Ollama's OpenAI-compatible server has no auth. Binding it to `0.0.0.0` means it accepts connections on every interface, from anyone who can route to the host. From a second machine on the same LAN (or the same coffee-shop wifi), this now works with no credentials:

```bash
# From any other device on the network, once you've set 0.0.0.0:
curl http://192.168.1.20:11434/api/tags        # your full model inventory
curl http://192.168.1.20:11434/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3:8b","messages":[{"role":"user","content":"hi"}]}'
```

Both succeed. The first is reconnaissance — it tells an attacker exactly which models you run. The second burns your electricity and GPU on their prompt. There is no rate limit and nothing to stop a script from looping it. That's the cost of the one-line fix.

## The safer topology

Keep Ollama private. Put Stoke — a single ~5.5 MB Rust binary that sits between agents and model APIs — on the network edge instead:

```
other machine ──HTTP+auth──▶  Stoke :8787 (0.0.0.0)  ──localhost──▶  Ollama :11434 (127.0.0.1)
                              the only open door,               never leaves the loopback
                              every request needs a key
```

Ollama stays on `127.0.0.1`, unreachable from the LAN. Stoke listens on the network, and every request except a liveness check must carry a bearer token. Stoke fails closed: if you forget to configure keys, it rejects everything rather than serving openly.

## Step 1: leave Ollama on localhost

Make sure `OLLAMA_HOST` is **not** set to `0.0.0.0`. The default is `127.0.0.1`, which is what you want. If you set it earlier, unset it and restart the daemon:

```bash
unset OLLAMA_HOST
# macOS note: quitting the Ollama menu-bar app does NOT stop `ollama serve`.
# Kill the daemon so it picks up the change:
pkill ollama
ollama serve
```

While you're here, raise the context window if you need it — Ollama's default is small, and it can only be set server-side (the OpenAI-compatible API can't change it):

```bash
OLLAMA_CONTEXT_LENGTH=8192 ollama serve
```

## Step 2: write `stoke.toml`

Generate a starter config and point Stoke's listener at the network while the provider stays on loopback:

```bash
stoke-cli init --bind-all --output stoke.toml
```

`--bind-all` sets the server host to `0.0.0.0`. `stoke-cli init` also discovers a default model from your own Ollama at generation time — Stoke ships zero model names of its own. The result looks like this (trimmed):

```toml
routing = "single"
default_model = "qwen3:8b"   # whatever `ollama list` shows on your box

[server]
host = "0.0.0.0"        # Stoke faces the LAN
port = 8787

[[providers]]
name = "ollama"
type = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"   # stays on loopback
tier = "local"
```

Note the top-level defaults (`routing`, `default_model`) sit *above* the first `[section]` header — that's a TOML requirement, and it's how `stoke init` emits them. The whole point is on two lines: `host = "0.0.0.0"` for Stoke, `base_url = "http://127.0.0.1:11434/v1"` for the provider. The outside world talks to Stoke; only Stoke talks to Ollama.

## Step 3: start Stoke with an API key

Stoke rejects all traffic unless `STOKE_API_KEYS` is set (or `STOKE_DEV=1`, which is local-dev only — do not use it for a network-facing server). Mint a key and start the server:

```bash
export STOKE_API_KEYS="stk-$(openssl rand -hex 16)"
echo "$STOKE_API_KEYS"     # copy this; the other machine needs it
stoke-cli serve            # reads stoke.toml from the current directory
```

That's the minimum for authenticated access. If you also want a hard dollar ceiling and a rate limit per key, add a `[[keys]]` block — the key must appear in both `stoke.toml` and `STOKE_API_KEYS`:

```toml
[[keys]]
key = "stk-...your-key..."
budget_usd = 5.0        # hard cap on metered (priced) traffic
rate_limit_rpm = 120    # max requests per rolling 60s
```

Enforcement runs *before* any call reaches Ollama: over-budget requests get `429 Budget exceeded`, and a loop breaker blocks a key that fires 5 near-identical requests in 60 seconds. Local inference has no per-request dollar price, so the USD cap is a no-op for it — but the rate limit and loop breaker still apply, which is what actually protects your GPU.

## Verify it worked

First, confirm Ollama is **not** reachable from the other machine. This should now fail (connection refused), proving it's back on loopback:

```bash
curl --max-time 3 http://192.168.1.20:11434/api/tags
# curl: (7) Failed to connect ... Connection refused   ← good
```

Now hit Stoke from the other machine. Health is the only open endpoint:

```bash
curl http://192.168.1.20:8787/health
# {"service":"stoke","status":"ok"}
```

Try inference **without** a key — you should be refused:

```bash
curl -s -o /dev/null -w '%{http_code}\n' \
  http://192.168.1.20:8787/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3:8b","messages":[{"role":"user","content":"hi"}]}'
# 401        ← body: "Invalid or missing API key"
```

Now **with** the key:

```bash
curl http://192.168.1.20:8787/v1/chat/completions \
  -H "Authorization: Bearer $STOKE_API_KEYS" \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3:8b","messages":[{"role":"user","content":"say hi"}]}'
# 200 with a normal chat completion; non-streaming responses also carry
# stoke_cost and stoke_route.
```

401 without the key, 200 with it, and Ollama's own port dead from the LAN — that's the whole property you wanted.

## The model list is reconnaissance too

A common mistake is to authenticate inference but leave status endpoints open. Stoke doesn't: `GET /v1/nodes` — the live registry of which models are pulled, which are warm in memory, in-flight counts, and measured latency — requires the bearer key like everything else. Model inventory is exactly the data an attacker wants first, so it lives inside the protected surface:

```bash
curl http://192.168.1.20:8787/v1/nodes            # 401
curl http://192.168.1.20:8787/v1/nodes -H "Authorization: Bearer $STOKE_API_KEYS"   # 200
```

## Multi-machine variant: two gateways (federation)

If you have several machines, you don't have to expose any Ollama. Run Stoke on each box, keep every Ollama on `127.0.0.1`, and add the peer to your config as a `stoke`-type provider pointing at its `/v1`:

```toml
[[providers]]
name = "studio"
type = "stoke"
base_url = "http://192.168.1.33:8787/v1"   # the peer's Stoke, not its Ollama
api_key_env = "STUDIO_STOKE_KEY"
tier = "remote"
```

Stoke imports the peer's models, warm state, and live load through its authenticated `/v1/nodes`, so you still get warm-aware placement across machines — but the only open port on each host is an authenticated Stoke. Forwarded requests carry an `x-stoke-hop` header and federation depth is exactly one hop, so an A↔B pair can't loop.

## What this doesn't do

- **It's not a public-internet gateway.** This is a LAN pattern. Stoke checks a bearer token; it doesn't do TLS termination or user management. Don't port-forward it to the open internet without a reverse proxy handling TLS.
- **Streaming requests aren't dollar-metered.** Spend accrues from non-streaming responses only. Auth, rate limits, and the loop breaker *do* cover streaming traffic, so runaway loops are still stopped.
- **Local inference is never "free."** It costs the electricity and the GPU you already own. Budget caps in USD apply to metered API keys, not to local models.
- **No cross-model fallback.** Failover retries the *same* model on the next healthy node; it won't silently switch you to a different model or vendor.

## Next steps

Install Stoke and try the pattern on two machines:

```bash
curl -sSf https://stokegate.com/install | sh -s -- --version nightly
```

There's no stable tag yet — a bare `| sh` resolves `latest` and compiles from source, so pin `--version nightly` for the checksum-verified static binary (macOS arm64/x64, Linux x64/arm64). Source and full config reference: https://github.com/Ozperium/stoke
