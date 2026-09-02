# Stoke

**The kill switch for runaway AI. Hard budget caps, loop detection, local-first routing — in one Rust binary.**

Status: pre-release, built in the open, dogfooded daily by the author on a real two-Mac + Ollama Cloud setup. MIT licensed. EU-based project.

## Why

Coding agents on metered API keys fail expensively: they retry, loop, and fan out, and billing alerts fire hours after the money is gone. Observability tools tell you what happened. Stoke sits in the request path and decides what happens: requests over a budget cap or caught in a loop are **refused before any provider is called**, not reported afterwards.

Enforce, don't just observe.

## What it does today

Everything below is in the code now, covered by tests or the shipped smoke scripts. Anything not yet true lives under [Roadmap](#roadmap).

### Enforcement primitives

- **Hard budget caps** — per-API-key spend limits in USD, configured per key in a `[[keys]]` TOML table (each key must also be listed in `STOKE_API_KEYS`) and checked at the request path before any provider call. Over the limit, the request is refused with `429 Budget exceeded`, not alerted on. Streamed responses accrue spend too, read from the provider's own usage report as the stream passes.
- **Rate limiting** — per-key requests-per-minute cap over a sliding 60-second window, set in the same `[[keys]]` table. Enforced on all traffic, streaming included.
- **Loop detection (circuit breaker)** — exact prompt-hash matching plus optional semantic similarity (embedding cosine). 5 similar requests from one key within 60 seconds blocks that key for 120 seconds, with a reason in the error. It runs on all traffic, streaming included. Thresholds are global constants today; per-key tuning is on the roadmap.

### Node-aware routing

A background registry polls each Ollama node's `/api/tags` (models pulled) and `/api/ps` (models loaded in RAM). Placement prefers warm nodes, balances the same model across machines by live in-flight count, breaks ties with a latency EWMA, fails over automatically, and excludes unreachable nodes. Streaming requests count as in-flight for their entire lifetime, prefill included — and every stream *teaches* the registry: time-to-first-token and tokens/sec are measured per model per node. Context windows and tool-calling capability come from `/api/show`.

Non-streaming responses carry a `stoke_route` field explaining the decision — the chosen node and why every candidate ranked where it did. For streamed requests the same placement is logged and visible via `GET /v1/nodes`. Routing decisions are always inspectable.

### Three-node topology

Stoke keeps the client endpoint stable while its registry discovers model inventory, warm state, health, load, and measured performance from configured capacity. The same pattern scales from one local Ollama to multiple local or federated nodes:

```toml
# gateway receiving client traffic
[[providers]]
name = "ollama-local"
type = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
tier = "local"

[[providers]]
name = "gpu-west"
type = "openai_compatible"
base_url = "http://192.168.1.40:11434/v1"
tier = "remote"
models = ["coder-35b"]

[[providers]]
name = "cpu-east"
type = "openai_compatible"
base_url = "http://192.168.1.41:11434/v1"
tier = "remote"
models = ["chat-8b"]

# A federated Stoke is configured the same way, with richer /v1/nodes state.
[[providers]]
name = "studio"
type = "stoke"
base_url = "http://192.168.1.33:8787/v1"
api_key_env = "STUDIO_STOKE_KEY"
tier = "remote"
```

Clients continue to use `OPENAI_BASE_URL=http://localhost:8787/v1`. Stoke polls `GET /v1/nodes`, filters by route policy, prefers eligible warm capacity, and fails over when a node becomes unavailable. Clients do not maintain a topology map.

The authenticated node view is concrete JSON, not a health-only ping:

```json
{
  "nodes": [{
    "name": "gpu-west", "tier": "remote", "healthy": true,
    "models": ["coder-35b"], "warm_models": ["coder-35b"],
    "in_flight": 0, "latency_ewma_ms": 2100
  }]
}
```


Send `"model": "auto"` (or pick it in your agent's model list) and Stoke scores every eligible (model, node) candidate on **estimated cost, predicted latency (from measured stats), and your stated preference order**, then picks. `auto-cheap` weights spend down; `auto-fast` weights speed. Candidates come only from your config and your discovered nodes — Stoke ships with zero model names.

```toml
[auto_route]
fast = "your-small-model"                      # chat + fallback
coder = ["your-35b-coder", "your-8b-coder"]    # ordered: first = preference,
                                               # alternates the optimizer may take
reasoner = "your-reasoning-model"
long_context = "your-128k-model"
quality = "your-cloud-model"                   # quality-mode target + receipts baseline
hedge = true                                   # race small prompts across idle machines
```

Hard fit exclusions, not vibes: prompts that exceed a model's context window, or `tools` requests aimed at models that can't emit structured `tool_calls`, are ruled out with the reason on the receipt.

Every auto response carries the receipt — chosen candidate, every alternative and why it lost, and an honest counterfactual (the *list price* your configured `quality` model would have charged; an estimate, never a quality claim):

```json
"stoke_route": { "node": "studio", "auto": {
  "mode": "auto", "class": "Code",
  "chosen":    { "model": "your-35b-coder", "node": "studio", "warm": true,
                 "est_cost_usd": 0.0, "predicted_ms": 2100 },
  "not_taken": [{ "model": "your-8b-coder", "node": "macbook", "warm": false,
                  "est_cost_usd": 0.0, "predicted_ms": 9800 }],
  "counterfactual_usd_est": 0.0214
}}
```

`GET /v1/budget` aggregates the ledger: total requests, the share served at zero marginal cost, and the cumulative cloud list-price counterfactual.

### Hedged dispatch

Opt-in (`hedge = true`): a small prompt whose model sits on two zero-marginal nodes is fired at **both** — first past prefill wins, the loser's connection drops. Your idle machines race for you; duplicate local compute buys tail latency. Cloud nodes are never hedged, so it can't double a bill.

### Federation

One Stoke can front another — a provider of `type = "stoke"`:

```toml
[[providers]]
name = "studio"
type = "stoke"
base_url = "http://192.168.1.20:8787/v1"
api_key_env = "STUDIO_STOKE_KEY"
tier = "remote"
```

The peer's model inventory, warm state, **live load, and measured performance** (context windows, tool capability, per-model TTFT and tokens/sec) are imported via its `/v1/nodes` — richer signals than raw Ollama can report, so even federated models get real latency predictions. Forwarded requests carry an `x-stoke-hop` header and federation depth is exactly one hop: a deliberate A↔B cycle cannot loop (there is a shipped test script that proves it).

The practical win: Ollama on each machine stays bound to `127.0.0.1`. Stoke is the only network-facing door, and it requires auth.

### Fail-closed auth

No API keys configured and no dev flag means **all** requests are rejected — misconfiguration fails closed, not open. Every endpoint except `/health` requires a bearer key, status endpoints included: model inventory and load are reconnaissance data. Federation polling authenticates too.

Because prompts route to your own machines by default, they never leave your infrastructure — data residency by architecture, not by contract.

### Also in the box

- **Response cache** — exact-match plus semantic (semantic is opt-in via `STOKE_SEMANTIC_CACHE`).
- **Cost tracking** — non-streaming responses carry a `stoke_cost` field, and both streamed and non-streamed responses record their spend per key, including every call a fan-out pattern makes. Streamed spend is read from the provider's own usage report as the stream passes; if a metered provider reports none, Stoke bills an estimate and says so (`estimated_usd` in `/v1/budget`) rather than booking $0. Prices come from `[pricing.models]` in your config — Stoke ships none, and refuses to serve a model it cannot price on a metered provider rather than meter it at $0. Before dispatching, Stoke holds the most a request could cost against the key's cap and refuses it if the hold would not fit, so concurrent streams cannot overshoot while none of them has been charged yet. `/v1/budget` shows per-key spend, held money, and the configured prices are at `/v1/pricing`.
- **Route profiles** — multiple endpoints, each with its own model, routing pattern, plugin chain, and optional `allowed_tiers` egress allowlist. A route set to `["local", "remote"]` refuses cloud fallback before any provider is called.
- **Plugins** — webhook hooks (`pre_request`, `prompt_filter`, `post_response`) in any language; JS/TS plugins behind a compile-time feature flag (`--features js-plugins`); built-in PII redaction and JSONL audit log.
- **OpenAI-compatible** — `/v1/chat/completions` passthrough including SSE streaming and `tool_calls`. Works with any OpenAI-compatible client or agent.

Single Rust binary (~5.5 MB release build), zero runtime dependencies, TOML config, default port 8787.

## Quickstart

The product boundary and the answer to “where do the models come from?” are recorded in [`docs/control-plane-positioning.md`](docs/control-plane-positioning.md). Stoke is a control plane for capacity you attach; it does not ship a hidden model catalogue or provider credits.

Static binaries for macOS (arm64/x64) and Linux (x64/arm64) are built from every commit on `main` and published to a rolling `nightly` prerelease. The installer picks the right one, verifies its checksum, and falls back to compiling from source on any other platform.

```bash
curl -sSf https://stokegate.com/install | sh -s -- --version nightly
```

There is no tagged stable release yet, so plain `| sh` (which resolves `latest`) will compile from source until the first version is cut. To build it yourself:

```bash
git clone https://github.com/Ozperium/stoke.git
cd stoke
cargo build --release
```

```bash
# Guided local setup: discovers Ollama, writes stoke.toml and a protected .env
stoke-cli setup

# Load the generated key and start the gateway
set -a; . ./.env; set +a
stoke-cli serve
```

`stoke-cli setup` defaults to localhost and local Ollama. Use `stoke-cli setup --bind-all` only when serving a LAN, and keep the generated bearer key enabled.

For a fully manual configuration, use this minimal `stoke.toml` for a single local Ollama:

```toml
routing = "single"
default_model = "qwen3:8b"

[server]
host = "127.0.0.1"
port = 8787

[[providers]]
name = "local"
base_url = "http://127.0.0.1:11434/v1"
tier = "local"
```

(`stoke.example.toml` in the repo shows the full surface.)

Stoke is fail-closed: it serves nothing until you either set keys or explicitly opt into dev mode.

```bash
# real use: comma-separated bearer keys
STOKE_API_KEYS=stk-mykey ./target/release/stoke

# local development only: anonymous access
STOKE_DEV=1 ./target/release/stoke
```

Talk to it:

```bash
curl http://localhost:8787/v1/chat/completions \
  -H "Authorization: Bearer stk-mykey" \
  -H "Content-Type: application/json" \
  -d '{"model": "qwen3:8b", "messages": [{"role": "user", "content": "Say hello."}]}'
```

Point your agent at it — any OpenAI-compatible client works by setting the base URL. OpenCode is verified end-to-end: configure an OpenAI-compatible provider with

```
base_url = http://localhost:8787/v1
api_key  = stk-mykey
```

and every request your agent makes now passes through budget caps, rate limits, and the loop breaker first.

**Claude Code** speaks the Anthropic Messages API. Add an Anthropic upstream provider:

```toml
[[providers]]
name = "anthropic"
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"
tier = "cloud"
```

Then launch Claude Code with the gateway environment isolated from the upstream key:

```bash
STOKE_API_KEY=stk-mykey stoke run claude
# Forward Claude arguments after --:
STOKE_API_KEY=stk-mykey stoke run claude -- --print "Review this repository"
```

`stoke run claude` sets `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` for the child process and removes `ANTHROPIC_API_KEY` from that child. The separately running Stoke server keeps the real upstream credential. `POST /v1/messages` runs auth, budget, rate, and loop enforcement, then forwards the request and Claude Code's `anthropic-*` feature headers to Anthropic. (Running Claude Code against your *local* models needs Anthropic↔OpenAI translation — that's on the roadmap; today it forwards to an Anthropic upstream.)

**Codex CLI** uses OpenAI's Responses API. Configure an OpenAI upstream provider and list the Codex model it may use, then run:

```bash
STOKE_API_KEY=stk-mykey stoke run codex
# Non-interactive example:
STOKE_API_KEY=stk-mykey stoke run codex -- exec "Review this repository"
```

The launcher injects a temporary Codex custom provider with `wire_api = "responses"`, points it at Stoke, disables the WebSocket transport, and keeps the client key in `STOKE_API_KEY` rather than argv. Stoke forwards `POST /v1/responses` without translating its request, response, tool-call, or SSE event shapes. This path uses the API credential held by the Stoke server; it does not proxy ChatGPT subscription/OAuth billing.

## Multi-machine setup

Two machines, direct: point Stoke at both Ollamas. The remote Ollama must listen on the LAN for this variant.

```toml
[[providers]]
name = "macbook"
base_url = "http://127.0.0.1:11434/v1"
tier = "local"

[[providers]]
name = "studio"
base_url = "http://192.168.1.20:11434/v1"
tier = "remote"
models = ["qwen3:8b", "llama3.3", "gemma4:e4b-mlx"]
```

The registry polls both nodes and places each request on whichever machine has the model warm and the least load.

**Federation alternative** (preferred if you don't want Ollama exposed on the network): run a Stoke on the second machine too, keep its Ollama on `127.0.0.1`, and front it with a `type = "stoke"` provider as shown above. You get the same warm/load-aware placement — the peer reports its state through `/v1/nodes` — with auth at the only open port.

## Endpoints

| Endpoint | What it returns |
|---|---|
| `GET /health` | Liveness. The only endpoint that does not require auth. |
| `GET /ui` | Embedded live control room: budget state, node health, and a real-time enforcement decision feed. In authenticated mode, use the browser's Basic Auth prompt with any username and a Stoke API key as the password. |
| `POST /ui/demo` | Runs five real, repeated requests through Stoke so the live feed visibly shows allowed/cache decisions followed by loop enforcement. |
| `POST /v1/chat/completions` | OpenAI-compatible chat, streaming and `tool_calls` included; non-streaming responses carry `stoke_cost` and `stoke_route` (for streams, placement is in the logs and on `/v1/nodes`). |
| `POST /v1/messages` | Anthropic Messages API — enforced passthrough to a configured `anthropic` provider, including safe `anthropic-*` and `x-claude-code-*` feature headers. Used by `stoke run claude`; cost is in `x-stoke-cost`. |
| `POST /v1/responses` | OpenAI Responses API — native JSON/SSE passthrough to a configured OpenAI-compatible provider. Used by `stoke run codex`; same auth/budget/rate/loop enforcement, with measured stream usage charged when the stream closes. |
| `GET /v1/models` | Models declared in provider config, per provider (live per-node inventory is on `/v1/nodes`). |
| `GET /v1/nodes` | Live node registry: health, pulled/warm models, in-flight counts, latency EWMA. |
| `GET /v1/budget` | Per-key spend, limits, recent activity, and the receipts ledger (zero-marginal share, cloud list-price counterfactual). |
| `GET /v1/cache` | Cache statistics (exact + semantic layers). |
| `GET /v1/pricing` | The pricing table used for cost calculation. |
| `GET /v1/routes` | Configured route profiles. |

## Scope: what Stoke cannot do

Subscription traffic (e.g. Claude Max/Pro seats) is quota-world, not dollar-world — there is no per-request price to meter, so Stoke cannot dollar-cap it. It can still rate-limit it and kill loops in it. Budget caps in USD apply to metered API traffic, where a request has a price.

## Verification

Claims above are runnable, not asserted:

```bash
cargo test                       # unit + integration tests
./scripts/smoke_responses.sh     # Responses JSON/SSE passthrough and Codex headers
./scripts/smoke_client_run.sh    # Claude Code/Codex launcher environment and argv
./scripts/smoke.sh               # mocks two Ollama nodes: proves discovery, warm-first
                                 # placement, streaming in-flight accounting, failover
                                 # when the warm node dies, and health-based exclusion
./scripts/smoke_federation.sh    # two real gateways in a deliberate A<->B cycle: proves
                                 # fail-closed auth, inventory import, cross-gateway
                                 # routing, the hop guard, and cycle safety
```

Both smoke scripts need only `python3`, `curl`, and `cargo`; no Ollama required.

## Roadmap

Planned, not built. Nothing here is a current-feature claim.

- [x] Anthropic Messages API endpoint (`/v1/messages`) — Claude Code via `ANTHROPIC_BASE_URL`, enforced passthrough to an `anthropic` provider *(shipped)*
- [x] OpenAI Responses API endpoint (`/v1/responses`) and `stoke run claude|codex` launchers *(shipped)*
- [ ] Anthropic ↔ OpenAI translation, so Claude Code can run against your **local** models (today `/v1/messages` forwards to an Anthropic upstream)
- [ ] Homebrew formula and Docker images *(release CI and Dockerfile are in the repo; first tagged release pending)*
- [x] Cost accounting for streamed responses (SSE usage parsing) so streamed spend counts against budget caps
- [ ] Quota-aware cloud escalation (429 cooldown) and a degrade-to-local policy action
- [ ] Per-key loop-detection thresholds in TOML (budget caps and rate limits are already configured per key today)
- [ ] Per-tenant usage entitlement enforcement (per-customer caps for AI products — if you need this, open an issue and talk to us)
- [ ] Enforcement benchmark harness with published p99 overhead, time-to-trip, and false-positive rates
- [ ] OpenTelemetry traces and Prometheus metrics

## Stoke and LiteLLM

If you need a gateway that speaks to dozens of providers, lives in a Python ecosystem, and comes with mature observability integrations and a large community, use LiteLLM — it is good at that and further along. Stoke is a different shape: a single enforcing binary you put in front of your own machines, with hard refusal semantics (budget, loops, fail-closed auth) and node-aware placement across local GPUs as the core, not an add-on. Small surface, no runtime dependencies, routing decisions explained in the response (and always inspectable via `/v1/nodes`). If your problem is "stop my agents from spending money and keep prompts on my hardware," that is the problem Stoke is built for.

## For agents

Stoke's users are agents, so it's built to be agent-legible:

- [`AGENTS.md`](AGENTS.md) — how to build, test, and change this repo, and the invariants that must hold.
- [`skills/stoke/SKILL.md`](skills/stoke/SKILL.md) — a runnable procedure for an agent to put itself behind Stoke.
- [`llms.txt`](https://stokegate.com/llms.txt) — a machine-readable summary of what Stoke is and how to use it.

## Contributing

Issues and PRs welcome — the smoke scripts are the fastest way to see the whole system run, and `ARCHITECTURE.md` maps the modules. Keep claims in docs verifiable; that rule is enforced in review (see [`AGENTS.md`](AGENTS.md)).

## License

MIT. The data plane is MIT forever.

---

**[stokegate.com](https://stokegate.com)** · [github.com/Ozperium/stoke](https://github.com/Ozperium/stoke)
