---
title: Run Ollama Across Two Machines Without Exposing It
description: Run Ollama across two machines by federating two Stoke gateways. Your desktop GPU serves the laptop over LAN while Ollama stays bound to 127.0.0.1.
slug: run-ollama-across-two-machines
category: Routing
icon: nodes
---

# Run Ollama Across Two Machines Without Exposing It

You code on a laptop, but the real GPU is in the workstation or Mac Studio across the room. You want your coding agent to use the big machine when a request deserves it, and the laptop when it doesn't — without hardcoding a second base URL into every tool, and without setting `OLLAMA_HOST=0.0.0.0` on the workstation, which puts an unauthenticated model server on your LAN.

The clean way is to run a small gateway on each machine and let them federate. Stoke is a single ~5.5 MB Rust binary that sits in front of Ollama. The laptop's Stoke treats the workstation's Stoke as a peer, imports its live model inventory, and routes to it when that's the better placement. The workstation's Ollama never leaves `127.0.0.1`. This guide sets that up end to end.

## The shape of the setup

```
laptop                              workstation (the GPU)
┌────────────────────┐              ┌────────────────────┐
│ agent (OpenCode)   │              │                    │
│   ↓ localhost:8787 │              │                    │
│ stoke ─────────────┼── LAN :8787 ─┼─→ stoke            │
│   ↓ 127.0.0.1      │  (auth'd)    │     ↓ 127.0.0.1    │
│ ollama (small)     │              │   ollama (big GPU) │
└────────────────────┘              └────────────────────┘
```

Only one network port is open on the workstation: its Stoke on `:8787`, and it requires a bearer key. Ollama on both machines stays bound to loopback.

## Install Stoke on both machines

There is no tagged stable release yet, so pin the nightly build — a bare `| sh` resolves `latest` and compiles from source. Run this on the laptop and the workstation:

```bash
curl -sSf https://stokegate.com/install | sh -s -- --version nightly
```

That fetches a checksum-verified static binary for macOS (arm64/x64) or Linux (x64/arm64) and falls back to a source build on anything else. You get two binaries: `stoke` (the server) and `stoke-cli` (config + client helpers).

## Configure the workstation (the GPU machine)

Generate a starter config next to the workstation's Ollama:

```bash
stoke-cli init --output stoke.toml
```

`init` discovers a default model from the workstation's own Ollama. Edit `stoke.toml` so the server binds to the LAN while Ollama stays on loopback:

```toml
routing = "single"

[server]
host = "0.0.0.0"   # reachable from the laptop; auth is still required
port = 8787

[[providers]]
name = "studio"
base_url = "http://127.0.0.1:11434/v1"   # Ollama stays on loopback
tier = "local"
```

Coding agents send large contexts, and Ollama's default window is small. Set it server-side before starting Ollama (it cannot be set through the OpenAI-compatible API):

```bash
OLLAMA_CONTEXT_LENGTH=32768 ollama serve
```

Now start Stoke with an API key. Stoke is fail-closed: with no `STOKE_API_KEYS` and no `STOKE_DEV=1`, every request is rejected with 401. Pick a key the laptop will present:

```bash
STOKE_API_KEYS=stk-studio-9159ba0a stoke
```

Confirm it's alive from the laptop (this is the one endpoint that needs no auth):

```bash
curl http://192.168.1.20:8787/health
```

Use the workstation's real LAN address in place of `192.168.1.20`.

## Configure the laptop and federate

On the laptop, put the peer key in the environment so it never lands in the config file:

```bash
export STUDIO_STOKE_KEY=stk-studio-9159ba0a
```

Then write the laptop's `stoke.toml` with its own local Ollama plus a `type = "stoke"` provider pointing at the workstation's Stoke `/v1` URL:

```toml
routing = "single"

[server]
host = "127.0.0.1"   # the agent is local; no need to expose this one
port = 8787

[[providers]]
name = "laptop"
base_url = "http://127.0.0.1:11434/v1"
tier = "local"

# The workstation, fronted by its own Stoke (federation).
[[providers]]
name = "studio"
type = "stoke"
base_url = "http://192.168.1.20:8787/v1"
api_key_env = "STUDIO_STOKE_KEY"
tier = "remote"
```

Note the port: `8787` (the peer's Stoke), not `11434` (Ollama). `api_key_env` names the environment variable holding the bearer key; its value must match one of the workstation's `STOKE_API_KEYS`.

Start the laptop's Stoke with a key your agent will use:

```bash
STOKE_API_KEYS=stk-laptop-dev stoke
```

## Verify it worked

The registry polls the peer on a background loop. From the laptop, ask what nodes it can see:

```bash
curl -s http://localhost:8787/v1/nodes \
  -H "Authorization: Bearer stk-laptop-dev" | jq
```

You should see two nodes — the local Ollama and the imported peer. The peer's models, warm state, live in-flight count, and measured per-model performance all arrive through its authenticated `/v1/nodes`:

```json
{
  "nodes": [
    {
      "name": "laptop",
      "type": "direct",
      "tier": "local",
      "healthy": true,
      "models": ["gemma4:e4b-mlx"],
      "warm": ["gemma4:e4b-mlx"],
      "inflight": 0,
      "ewma_latency_ms": 180.0
    },
    {
      "name": "studio",
      "type": "stoke",
      "tier": "remote",
      "healthy": true,
      "models": ["qwen3.6:35b-a3b-q4_K_M", "gpt-oss:20b"],
      "warm": ["qwen3.6:35b-a3b-q4_K_M"],
      "models_meta": {
        "qwen3.6:35b-a3b-q4_K_M": {
          "context_length": 32768, "tools": true,
          "tps": 48.2, "ttft_ms": 410.0
        }
      },
      "inflight": 1,
      "ewma_latency_ms": 620.0
    }
  ]
}
```

(Values are illustrative.) If the `studio` node shows `"healthy": false` or is missing its models, check that the workstation's Stoke is reachable on `:8787` and that `STUDIO_STOKE_KEY` matches one of its `STOKE_API_KEYS`. Unreachable nodes are excluded from routing until they return.

## How placement chooses a machine

When a request names a model, Stoke ranks every node that can serve it, best first, in this order:

1. **Warm before cold** — a node with the model already loaded in RAM wins over one that only has it pulled.
2. **Local tier before remote** — ties at the same warmth prefer the laptop over the workstation.
3. **Fewest in-flight requests** — among equally-ranked nodes, it balances the same model across machines by live load.
4. **Lowest latency EWMA** — the measured tiebreaker.

So the big `qwen3.6:35b-a3b-q4_K_M` — which only exists on the workstation — routes there automatically, while a tiny chat model warm on the laptop stays local. If the chosen node errors mid-request, Stoke **fails over to the same model on the next candidate node** and continues. Non-streaming responses carry a `stoke_route` field explaining the pick and why each candidate ranked where it did; for streamed requests the same placement shows up in the logs and on `/v1/nodes`.

## Point your agent at the local Stoke

Your agent only ever talks to `localhost` — the federation is invisible to it. In OpenCode (or any OpenAI-compatible client), configure a provider:

```
base_url = http://localhost:8787/v1
api_key  = stk-laptop-dev
```

Every request now passes the laptop's Stoke first — auth, rate limits, the loop breaker — before it's placed on the laptop or forwarded to the workstation. Nothing in the agent config mentions the second machine.

## The hop guard keeps it cycle-safe

Forwarded requests carry an `x-stoke-hop` header, and federation depth is exactly one hop. When the workstation's Stoke receives a forwarded request, it excludes its own `type = "stoke"` providers from routing. That means you can point the two gateways at each other (A↔B) for symmetric use and a request can never loop between them — it's ruled out with `excluded (hop guard)` in the `stoke_route` candidate list.

## What this doesn't do

- **It doesn't expose or replace Ollama.** Both Ollama daemons stay on `127.0.0.1`; Stoke is the only network door and it always requires auth. (Remember: the Ollama app's menu-bar quit doesn't stop `ollama serve` — kill the daemon if you're locking things down.)
- **Local isn't free.** Routing to your own GPU costs electricity and the hardware you already bought; it's just not a metered per-request bill.
- **Failover is same-model only.** If a node dies, Stoke retries the same model on another node. Cross-model or cross-vendor fallback chains are on the roadmap, not shipped.
- **Streamed-request spend isn't metered.** Dollar cost accrues from non-streaming responses today. Auth, rate limits, and the loop breaker cover streaming regardless.
- **No published benchmarks.** The `tps`/`ttft_ms` numbers are per-node measurements Stoke records to inform placement, not performance claims.

## Next steps

You now have one laptop agent quietly borrowing a workstation GPU, warm-aware and auth-gated, with zero base-URL juggling. Add a third machine by dropping in another provider, or set `routing = "auto"` with an `[auto_route]` block to let Stoke classify each request and pick the model for you.

Source, config reference, and the federation smoke test: [github.com/Ozperium/stoke](https://github.com/Ozperium/stoke).
