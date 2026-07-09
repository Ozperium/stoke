# Stoke Architecture

Stoke is a single Rust binary (release build ~5.5 MB, zero runtime dependencies) that sits
between AI agents and model providers. It exposes an OpenAI-compatible
`/v1/chat/completions` endpoint (including SSE streaming and `tool_calls` passthrough) and
enforces policy *before* any provider is called: authentication, per-key budget caps in
USD, per-key rate limits, and loop detection. Requests that violate policy are refused,
not logged-and-forwarded.

Two binaries build from this crate (`Cargo.toml`):

| Binary | Entry | Role |
|---|---|---|
| `stoke` | `src/main.rs` | The gateway server (default port 8787) |
| `stoke-cli` | `src/cli.rs` | Companion CLI: `init`, `serve`, `route`, `bench`, `models`, `pricing`, `routes`, `version` |

Install options: build from a clone with `cargo build --release`, download prebuilt
`stoke`/`stoke-cli` binaries from GitHub Releases (published on tagged builds by CI,
`.github/workflows/release.yml`), or build the static Docker image from the included
`Dockerfile`.

## Request pipeline

Everything below is the actual order of operations in `chat_completions`
(`src/main.rs`), the handler behind `/v1/chat/completions` and every configured route
profile path.

### 1. Authentication (fail-closed middleware)

An axum middleware (`require_auth` in `src/main.rs`) wraps **every** endpoint except
`GET /health`, which stays open for liveness probes. Status endpoints are deliberately
inside the protected surface — model inventory and live load are reconnaissance data.

The rules (`Auth` in `src/budget.rs`):

- `STOKE_API_KEYS` set (comma-separated): requests must carry a matching
  `Authorization: Bearer <key>` header.
- No keys configured and `STOKE_DEV` is not `1`: **all requests are rejected** (401).
  There is no accidentally-open state; you must opt into dev mode explicitly.
- `STOKE_DEV=1` with no keys: requests run as the `anonymous` identity (local dev only).

The validated bearer key is the identity that budgets, rate limits, and loop detection
attach to.

### 2. Budget, rate limit, and loop checks — before any provider call

The handler hashes the prompt (SHA-256 over model + concatenated message contents +
temperature) and calls `BudgetGuard::check_with_prompt` (`src/budget.rs`). Any failure
returns `429` and the request never reaches a provider:

- **Loop block check.** If the key tripped the loop breaker and the block hasn't
  expired, reject with the remaining block time.
- **Budget cap.** If the key has a USD limit (set per key via the `[[keys]]` config
  table) and cumulative spend has reached it, reject. Spend is recorded per request
  after completion (step 9). Scope note: streaming responses are passed through
  byte-for-byte and currently bypass cost accounting — a streamed request does not add
  to a key's spend (SSE `usage` parsing is not implemented yet). Auth, rate limiting,
  and loop detection still apply to streaming traffic; only dollar-accrual from streams
  is deferred.
- **Rate limit.** Sliding 60-second window of request timestamps per key; over the
  key's configured requests-per-minute (`rate_limit_rpm` in the `[[keys]]` table),
  reject.
- **Loop detection (circuit breaker).** Two layers count "similar" requests per key
  inside a 60 s window: exact prompt-hash match, plus (when semantic detection is
  enabled via `STOKE_SEMANTIC_CACHE`) embedding cosine similarity above a threshold
  (default 0.85, tunable with `STOKE_LOOP_SEMANTIC_THRESHOLD`). Five similar requests
  in the window block the key for 120 s and clear its history. Thresholds are global
  constants in `src/budget.rs` today, not per-key settings. Embeddings come from an
  Ollama embedding endpoint (`OLLAMA_BASE_URL`, model `STOKE_EMBED_MODEL` — required
  when semantic detection is enabled; without it the semantic layer disables itself
  with a warning and exact-hash detection stays on).

### 3. Federation hop guard

The handler reads the `x-stoke-hop` header (clamped to 16 so forged values can't
overflow the hop+1 arithmetic). A value >= 1 means this request already crossed another
Stoke gateway: every routing path below then excludes providers of type `"stoke"`, so a
request can never bounce between gateways. See [Federation](#federation).

### 4. Route-profile resolution

The request path is matched against `[[routes]]` profiles from the config. Each profile
registers its own POST endpoint at startup and can pin a model, a routing mode,
`vote_models`, and a list of built-in plugins. Profile settings take precedence over
request-body fields, which take precedence over the config default.

Pre-request hooks then run, each able to block (403) or rewrite the request:

1. Webhook `pre_request` plugins (`src/plugins.rs`) — may override model/routing or block.
2. JS/TS `pre_request` plugins (`src/js_plugins.rs`, only with the `js-plugins` feature).
3. Built-in `prompt_harness` (injects a system prompt, if the profile enables it).
4. Webhook `prompt_filter` plugins — may block or redact messages.
5. Built-in `pii_redact` (if the profile enables it) — strips API keys, tokens, emails,
   private keys, and custom patterns from prompts.
6. JS/TS `prompt_filter` plugins.

### 5. Streaming path

If `stream: true` and the routing mode is `single`, the handler ranks candidate nodes
(see [Node registry](#node-registry-and-placement)) and calls `stream_with_failover`
(`src/failover.rs`):

- Candidates are tried best-first. Failover happens **at connect time only** — if a
  provider refuses the connection or returns an error status before streaming begins,
  the next candidate is tried and the failure is recorded against the node.
- An `InflightGuard` is taken when the connect attempt starts. For LLM streams the
  connect phase *is* the prefill, which is exactly the load placement needs to see.
- The winning guard is moved into the response-stream closure, so the node's in-flight
  count stays accurate for the **entire lifetime of the stream** — through prefill and
  generation, until the stream ends or the client disconnects — not just the handler's
  scope.
- The winner's connect time feeds that node's latency EWMA.

The upstream SSE bytes are passed through unmodified as `text/event-stream`.

### 6. Cache lookup (non-streaming, `single` routing)

Only deterministic requests are cacheable: temperature absent or <= 0.01
(`src/cache.rs`). The cache key is SHA-256 over model + prompt text + `max_tokens`.

Lookup is two-layer: exact hash first, then — if `STOKE_SEMANTIC_CACHE` is set — a
semantic search over cached prompt embeddings (cosine similarity threshold 0.92, TTL
3600 s; both fixed at construction in `src/main.rs`). A hit returns immediately with
`"stoke_cache": "hit"` in the response.

### 7. Placement and the provider call

For the default routing mode, `NodeRegistry::rank` orders every eligible provider for
the requested model (warm > cold > unknown; ties broken by tier, live in-flight count,
latency EWMA — details below). The handler then:

- tries up to the top **3** ranked candidates in order;
- holds an `InflightGuard` for each attempt;
- on success, records the elapsed time into the node's latency EWMA and remembers the
  placement decision;
- on a network/5xx failure, records an error against the node and moves on;
- on a 4xx from the provider, **stops immediately** — a deterministic client error
  would just replay identically elsewhere.

Every forwarded request carries `x-stoke-hop: <hop+1>` (`call_provider_hop`,
`src/router.rs`) so a downstream Stoke can enforce the hop guard. Non-Stoke providers
ignore the header.

`auto` routing runs before placement: the scorer in `src/auto_route.rs` picks the
model from the user's `[auto_route]` roles and discovered candidates (estimated cost +
predicted latency + preference order; hard context/tool fit exclusions; modes
`auto`/`auto-cheap`/`auto-fast` via the requested pseudo-model name), attaches the
`stoke_route.auto` receipt, then hands the chosen model to placement like any other
request. When `hedge = true` and a small prompt's model sits on two zero-marginal
nodes, the streaming path races both via `stream_hedged` — first past prefill wins.

Other routing modes (`cascade`, `self_consistency`, `test_vote`, `cascade_test`,
`stream_race`) are experimental multi-model request strategies that live in
`src/router.rs`, `src/stream_fusion.rs`, and `src/auto_route.rs`. They all respect the
hop guard.

### 8. Cost tracking and response annotation

`Pricer` (`src/cost.rs`) computes a per-request cost breakdown from the provider's
`usage` field against a static per-1M-token price table. Local models carry a $0.00 API
price in that table — their marginal cost per request is electricity on hardware you
already own, not API dollars. The gateway annotates the JSON response with
non-standard fields that OpenAI clients ignore:

- `stoke_cost` — model, token counts, `cost_usd`
- `stoke_elapsed_ms` — end-to-end provider latency
- `stoke_cache` — `"hit"` or `"miss"`
- `stoke_route` (single routing only) — chosen node plus the full ranked explain list:
  why every candidate ranked where it did.

These fields are added on non-streaming responses. Streaming responses are passed
through unmodified and carry no `stoke_*` annotations — a stream's placement decision is
logged, and its live load and latency are visible on `GET /v1/nodes` instead.

### 9. Post-processing and accounting

- Webhook `post_response` plugins (audit/transform; failures are logged and skipped,
  never fatal).
- Built-ins `code_formatter` and `audit_log` (if enabled by the route profile). The
  audit logger appends JSON lines (timestamp, model, cost, latency, truncated key
  prefix, optionally the body) to a configured file.
- JS/TS `post_response` plugins.
- `BudgetGuard::record_spend` adds the request's cost to the key's cumulative spend —
  this is what the budget cap in step 2 checks against. It runs on the non-streaming
  path only; streamed responses do not yet accrue spend (see the scope note in step 2).
- Cacheable responses are stored (with a prompt embedding when semantic cache is on).

## Node registry and placement

`src/nodes.rs`. The registry holds one entry per configured provider and is the basis
of every placement decision.

### Poll loop

A background task (`spawn_poller`) polls every pollable node on an interval
(`STOKE_NODE_POLL_SECS`, default 10 s; per-request poll timeout 2 s). How a provider is
polled is derived from its config (`PollKind`):

| Provider | Poll |
|---|---|
| `type = "stoke"` (federated gateway) | `GET {base_url}/nodes` with the provider's API key — the peer's status surface is fail-closed too |
| tier `cloud` | **Never polled.** Quota is scarce; health is learned from real traffic |
| `base_url` ending in `/v1` (Ollama-shaped) | `GET /api/tags` (models pulled to disk) + `GET /api/ps` (models loaded in RAM) |
| anything else | Not pollable; treated as unknown capability |

A failed poll marks the node unhealthy and clears its warm set and reported load; a
later successful poll restores it.

### Ranking

`rank(model, providers, exclude_stoke)` scores each candidate:

- **warm (3):** the model is loaded in RAM on that node right now;
- **cold (2):** the model is present on disk (discovered by polling) *or* listed in the
  provider's configured `models`;
- **unknown (1):** the provider has never been polled (cloud, non-Ollama). Kept as a
  fallback tail even if its configured model list doesn't mention the model — partial
  lists are common.

Excluded outright, each with a reason string in the explain list:

- polled and unhealthy (`excluded (unreachable)`),
- polled and verifiably lacking the model (`excluded (model not present)`),
- type `"stoke"` when the hop guard is active (`excluded (hop guard)`).

Ties break by, in order: tier (`local` < `remote` < `cloud`/unknown), live in-flight
count ascending, latency EWMA ascending (alpha 0.3).

**Tag normalization:** Ollama treats a bare name as the `:latest` tag, so matching does
too — `llama3` and `llama3:latest` are the same model, in inventory, warm sets, and
configured lists.

**In-flight accounting** is RAII: `begin(node)` increments an atomic counter and returns
an `InflightGuard` that decrements on drop. Streaming responses hold the guard until the
byte stream finishes, so long generations count as load for their whole duration.

`GET /v1/nodes` exposes the live registry: per node its type, tier, health, model
inventory, warm set, effective in-flight count, latency EWMA, and error count.

## Federation

A provider with `type = "stoke"` is another Stoke gateway. One Stoke can front another —
e.g. a MacBook's gateway forwarding to a Mac Studio's — while each machine keeps its
Ollama bound to `127.0.0.1`. Stoke is the only network-facing door, and it requires auth.

- **Discovery:** the peer is polled via its `/v1/nodes` endpoint, which is richer than
  raw Ollama can report — it carries warmth *and* live in-flight load.
- **Aggregation** (`aggregate_stoke_snapshot`): only the peer's **direct** nodes count.
  Its own federated entries are skipped so state can't echo back through a cycle, and
  unhealthy remote nodes are ignored. Warm models are implicitly present. Reported
  in-flight counts are clamped (per node and in total, cap 10,000) so a corrupted or
  malicious peer can't poison ranking arithmetic.
- **Load accounting is max, not sum:** requests we forward are counted by our local
  guards *and* show up in the peer's self-report once polled. Summing would
  double-count, so the effective in-flight is `max(local, reported)` — local covers
  poll lag, reported covers the peer's other clients.
- **Hop guard:** every forwarded request carries `x-stoke-hop` (incoming value clamped
  to 16, incremented on forward). A gateway that sees hop >= 1 excludes all `"stoke"`
  providers from every routing path. Federation depth is exactly one hop; a deliberate
  A<->B cycle cannot loop. `scripts/smoke_federation.sh` stands up two gateways in
  exactly that cycle and proves inventory import, cross-gateway routing, the hop guard,
  and cycle safety end to end.

## Module map

| File | What it is |
|---|---|
| `src/main.rs` | Axum server: fail-closed auth middleware, all `/v1` endpoints, the `chat_completions` pipeline described above |
| `src/config.rs` | TOML config loading (`./stoke.toml`, then `~/.config/stoke/stoke.toml`), provider/server/route schema, API-key resolution (inline or env var, with a warning for plaintext keys) |
| `src/budget.rs` | `BudgetGuard` (per-key USD caps, sliding-window rate limits, exact + semantic loop circuit breaker) and `Auth` (fail-closed bearer-key validation) |
| `src/nodes.rs` | Node registry: poll loop, warm/cold/unknown ranking with explain strings, RAII in-flight guards, federation polling and aggregation (incl. `models_meta`: context windows, tool capability, measured TTFT/tokens-per-sec), lazy `/api/show` metadata fetch, `record_stream_stats` EWMAs, `/v1/nodes` snapshot |
| `src/failover.rs` | Connect-phase streaming failover across ranked candidates; hands the in-flight guard to the stream so load stays counted for the stream's lifetime; `stream_hedged` races two zero-marginal nodes (hop headers on both attempts) |
| `src/router.rs` | Provider passthrough (`call_provider` / `call_provider_hop` with the hop header), the shared pooled HTTP client, and experimental multi-model request strategies |
| `src/cache.rs` | Response cache: exact hash layer always on for deterministic requests, semantic embedding layer opt-in, TTL eviction |
| `src/cost.rs` | Static per-1M-token price table and per-request `CostBreakdown` (the `stoke_cost` field) |
| `src/ttft.rs` | Time-to-first-token tracking scaffolding — not yet fed by the request path; live per-node latency EWMA is on `/v1/nodes` instead |
| `src/auto_route.rs` | Auto-routing v2: heuristic prompt classifier (no LLM call, no network) + candidate scorer — every eligible (model, node) pair scored on estimated cost, predicted latency (measured TTFT/tps, cold-load penalty), and the user's preference order; modes `auto`/`auto-cheap`/`auto-fast`; hard fit exclusions (context window, tool capability); emits the `stoke_route.auto` receipt with counterfactual |
| `src/stream_fusion.rs` | Experimental streaming strategies (racing candidate connections, first to respond wins) |
| `src/builtins.rs` | In-process plugins — prompt harness, PII redaction, code formatter, audit logger — plus the `RouteProfile` type |
| `src/plugins.rs` | HTTP webhook plugin hooks (`pre_request`, `prompt_filter`, `post_response`) with SSRF protection on webhook URLs |
| `src/js_plugins.rs` | JS/TS plugin runtime on `deno_core` (V8 on a dedicated thread); compile-time feature `js-plugins`, not built by default |
| `src/cli.rs` | The `stoke-cli` binary: config generation and inspection helpers |
| `src/lib.rs` | Library re-exports for integration tests |

## Configuration

Config is TOML, searched at `./stoke.toml` then `~/.config/stoke/stoke.toml`
(see `stoke.example.toml` for a starting point):

```toml
routing = "single"         # optional default routing mode; top-level, above [server]
default_model = "qwen3:8b" # optional; top-level keys must precede the first [section]

[server]
host = "0.0.0.0"   # default 127.0.0.1
port = 8787        # default 8787

[[providers]]
name = "ollama"
type = "openai_compatible"           # or "stoke" for a federated gateway
base_url = "http://127.0.0.1:11434/v1"
api_key = "ollama-local"             # inline key, or:
# api_key_env = "OPENROUTER_API_KEY" # preferred — resolve from environment
tier = "local"                       # "local" | "remote" | "cloud"
# models = ["qwen3:8b", "phi4-mini"] # optional; Ollama nodes are auto-discovered by polling

# A second Stoke gateway as a provider (federation)
# [[providers]]
# name = "studio"
# type = "stoke"
# base_url = "http://192.168.1.33:8787/v1"
# api_key_env = "STUDIO_STOKE_KEY"
# tier = "remote"

# Multi-endpoint route profiles: each gets its own POST path
# [[routes]]
# name = "code"
# path = "/v1/code/completions"
# routing = "single"       # default "auto"
# model = ""               # pin a model; empty keeps the request's model
# vote_models = []
# builtins = ["prompt_harness", "pii_redact", "code_formatter", "audit_log"]

# Auto-routing roles (for routing = "auto"). Roles accept one model or an
# ordered list (first = preference; alternates the optimizer may pick).
# Anything unset resolves from models discovered on your own nodes.
# [auto_route]
# fast = "your-small-model"
# coder = ["your-35b-coder", "your-8b-coder"]
# reasoner = "your-reasoning-model"
# long_context = "your-128k-model"
# quality = "your-cloud-model"   # quality-mode target + receipts counterfactual
# hedge = false                  # race small prompts across idle zero-marginal nodes

# Per-key enforcement policy. Each key here must also appear in STOKE_API_KEYS;
# the limits are applied to the budget guard at startup.
# [[keys]]
# key = "team-alpha"
# budget_usd = 50.0        # hard cumulative spend cap; over it, requests get 429
# rate_limit_rpm = 120     # max requests per rolling 60 s window (0/omitted = unlimited)

# Built-in plugin configuration
# [builtins.prompt_harness]
# mode = "prepend"
# [builtins.prompt_harness.prompts]
# code = "You are an expert code generator."
#
# [builtins.pii_redact]
# patterns = []              # extra regexes on top of the built-in secret patterns
# replacement = "[REDACTED]"
#
# [builtins.code_formatter]
# languages = ["json", "python"]
#
# [builtins.audit_log]
# path = "stoke-audit.jsonl"
# log_body = true

# Webhook plugins (any language — they're just HTTP endpoints)
# [plugins]
# pre_request  = ["http://127.0.0.1:9100/"]
# prompt_filter = ["http://127.0.0.1:9100/filter"]
# post_response = ["http://127.0.0.1:9100/post"]
# scripts = ["plugins/pii-redact.js"]   # requires `cargo build --features js-plugins`
```

Environment variables (server):

| Variable | Effect |
|---|---|
| `STOKE_API_KEYS` | Comma-separated accepted bearer keys. Unset + no `STOKE_DEV=1` = every request rejected |
| `STOKE_DEV` | `1` allows anonymous access when no keys are configured (local dev only) |
| `STOKE_SEMANTIC_CACHE` | Set (any value) to enable the semantic cache layer and semantic loop detection |
| `STOKE_LOOP_SEMANTIC_THRESHOLD` | Cosine similarity that counts as "the same prompt" for loop detection (default 0.85) |
| `STOKE_EMBED_MODEL` | Embedding model for semantic features — required when semantic cache/loop detection is enabled; no default ships |
| `OLLAMA_BASE_URL` | Where embeddings are generated (default `http://127.0.0.1:11434`) |
| `STOKE_NODE_POLL_SECS` | Node registry poll interval in seconds (default 10) |

## HTTP surface

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /health` | open | Liveness probe |
| `POST /v1/chat/completions` | required | OpenAI-compatible chat, SSE streaming, `tool_calls` passthrough |
| `POST /v1/messages` | required | Anthropic Messages API — same enforcement, then passthrough to an `anthropic`-type provider (`src/messages.rs`); cost in the `x-stoke-cost` header. Claude Code via `ANTHROPIC_BASE_URL` |
| `POST <route profile paths>` | required | Same handler with the profile's pinned defaults |
| `GET /v1/models` | required | Models known from config |
| `GET /v1/nodes` | required | Live node registry snapshot |
| `GET /v1/budget` | required | Per-key spend, limits, recent request counts, and the receipts ledger (zero-marginal share + cloud list-price counterfactual, estimates) |
| `GET /v1/pricing` | required | The price table behind `stoke_cost` |
| `GET /v1/cache` | required | Cache entry/hit counts |
| `GET /v1/routes` | required | Configured route profiles |

## Failure semantics

- **No auth configured:** fail closed. Without `STOKE_API_KEYS` and without
  `STOKE_DEV=1`, every endpoint except `/health` returns 401. There is no open default.
- **Budget exceeded / rate limited / loop tripped:** 429 with a human-readable reason,
  before any provider is contacted. These are refusals, not alerts.
- **Node dies before streaming starts:** connect-phase failover moves to the next
  ranked candidate; the failed attempt's in-flight guard drops and an error is recorded
  against the node.
- **Node dies mid-stream:** the client's SSE stream ends where the upstream died and
  the in-flight guard drops. There is no mid-stream replay — replaying after tokens
  have been delivered would duplicate output. The poll loop marks the node unhealthy on
  its next cycle, excluding it from new placements until it recovers.
- **All candidates fail:** 502 with the last provider error. The non-streaming path
  tries at most the top 3 ranked candidates; a 4xx from a provider stops failover early
  (a deterministic client error would replay identically) and is surfaced as the error.
- **No provider can serve the model:** 404 including the ranking explain list, so you
  can see exactly why each node was excluded.
- **Webhook plugin failures:** a failing `pre_request`/`prompt_filter` webhook blocks
  the request (fail closed on input control); a failing `post_response` webhook is
  logged and skipped (fail open on output transforms).
- **Unhealthy poll results:** the node's warm set and reported load are cleared and it
  is excluded from ranking until a poll succeeds again.

## Verifying this document

Two reproducible harnesses ship in the repo and exercise the behavior described here
against real processes:

- `scripts/smoke.sh` — discovery, warm-node placement, streaming in-flight accounting,
  failover, and health handling on a single gateway.
- `scripts/smoke_federation.sh` — two gateways configured in a deliberate A<->B cycle:
  inventory import over `/v1/nodes`, cross-gateway routing, the hop guard, and cycle
  safety.

## Roadmap (not built yet)

These do not exist in the code today and nothing above claims them:

- Anthropic Messages API endpoint (`/v1/messages`) for native Claude Code support.
- Homebrew formula and a published container image (a `Dockerfile` builds one today,
  but nothing is pushed to a registry yet).
- Cost accounting for streaming responses (SSE `usage` parsing so streamed requests
  accrue against a key's budget; today only non-streaming responses do).
- Per-key loop-detection thresholds via config (per-key budgets and rate limits already
  ship in the `[[keys]]` table; loop thresholds are still global constants).
- Quota-aware cloud escalation and degrade-to-local policy actions.
- Per-tenant usage entitlement enforcement.
- Published enforcement benchmarks (overhead, time-to-trip, false-positive rate).
- OpenTelemetry traces and Prometheus metrics.
