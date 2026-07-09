# Smart Routing Design

Status: v0 (node registry + placement) + v1 (streaming in-flight, federation) implemented · July 2026

## The core insight

Agent clients (Claude Code, OpenCode, Hermes, custom apps) usually pin a model name.
So smart routing is two separate decisions, and the second one runs on every request:

- **Decision A — which model?** Only when the client asks for an alias (or `routing: "auto"`).
- **Decision B — where does it run?** Always. Requires live node state, not prompt analysis.

## Layers

### 1. Node registry + placement (v0 — implemented, `src/nodes.rs`)

A background poller (interval `STOKE_NODE_POLL_SECS`, default 10s) queries every
non-cloud provider whose `base_url` ends in `/v1`:

- `GET /api/tags` → which models the node has pulled
- `GET /api/ps` → which models are loaded in memory (warm)

Cloud providers are never polled — quota is scarce; their health is learned from traffic.

Per-request placement ranks candidate providers for the requested model:

1. **Score:** warm (loaded in RAM) > cold-but-present > capability unknown (fallback tail).
   A polled node that verifiably lacks the model is excluded, as is any polled-unreachable node.
2. **Tie-breaks:** tier (local > remote > cloud), fewer in-flight requests (counted by Stoke
   itself — this is what load-balances the same model across two machines, since Ollama
   never duplicates a model within one node), lower latency EWMA.

The top candidates are tried best-first with automatic failover. Non-streaming single
responses carry a `stoke_route` field: the chosen node plus every candidate's reason
string. `GET /v1/nodes` exposes the registry snapshot. Routing decisions must always
be inspectable.

**Streaming in-flight (v1, implemented).** For LLM streams the connect phase IS the
prefill — the heaviest load a node carries. The in-flight guard is therefore taken at
the connect *attempt* (released on failed attempts), and on success it is owned by the
closure wrapping the response byte stream, so it drops only when the stream ends or
the client disconnects. Dogfood-driven: an 89s Ollama prefill was invisible to
placement before this. Connect time feeds the node's latency EWMA.

### 1b. Federation — provider `type = "stoke"` (v1, implemented)

A Stoke can front another Stoke: the remote gateway's `/v1/nodes` gives richer state
than raw Ollama ever can — warmth AND in-flight load. Configured explicitly:

```toml
[[providers]]
name = "studio"
type = "stoke"
base_url = "http://192.168.1.33:8787/v1"
tier = "remote"
```

- The registry polls `{base}/nodes` and imports the remote's **direct** nodes only
  (its own federated entries are skipped — status cannot echo around a cycle).
  Unhealthy remote nodes are excluded; reported in-flight values are clamped
  (hostile/corrupt peers must not poison ranking arithmetic).
- Federated in-flight is `max(local, reported)`, never the sum: the peer's report
  already includes the requests we forwarded, so summing would double-count and
  starve federated nodes in tie-breaks. Local covers poll lag; reported covers
  the peer's other clients.
- **Hop guard (loop safety):** `call_provider` and the streaming failover stamp
  `x-stoke-hop: n+1` (saturating; inbound values clamped at parse). A gateway
  receiving `hop >= 1` excludes ALL stoke-type providers from every routing arm —
  federation depth is exactly one hop, and an A↔B cycle in config cannot loop.
  Serving from local cache under the guard is allowed (caches can't loop).
  `stream_race` does not propagate the header, so federated providers are
  **always excluded from races** — federation applies to single routing only.
- Each gateway enforces its own budgets/limits at its own door: federation extends
  enforcement across machines, it never bypasses it.
- **Status is part of the protected surface:** every endpoint except `/health`
  honors auth (model inventory and load are reconnaissance data). The federation
  poller authenticates with the provider's configured key.

**Placement semantics (post-review):** model names are tag-normalized ("llama3" ≡
"llama3:latest", matching Ollama). Only a *polled* node that lacks the model is
excluded; unpolled providers (cloud, non-Ollama) stay as unknown-capability fallback
even with a partial configured list — preserving the old first-provider fallback.
Streaming and non-streaming single paths share identical no-candidate semantics
(404 with per-candidate reasons). The failover retry loop stops on deterministic
4xx client errors — they would replay identically everywhere — and doesn't count
them against node health.

Verified by `scripts/smoke_federation.sh`: two gateways in a deliberate A↔B cycle —
inventory import, cross-gateway serving, hop-guard refusal, and fail-fast on unknown
models.

### 1c. Auto-routing v2 — the optimizer (implemented)

`routing = "auto"` no longer maps class → single role model. It scores every
eligible (model, node) candidate and picks the best under the requested mode:

- **Candidates:** `[auto_route]` roles accept ordered lists — first entry is
  the preference (quality floor), later entries are explicitly acceptable
  alternates. Unconfigured roles fall back to naming-convention heuristics
  over *discovered* models. Stoke ships zero model names.
- **Score** = w_cost · est_cost + w_speed · predicted_ms + w_pref · list-index,
  with modes selected by pseudo-model name: `auto` (balanced), `auto-cheap`,
  `auto-fast`.
- **Prediction inputs, measured not guessed:** per-(node, model) TTFT and
  tokens/sec EWMAs from live streams (SSE `data:` events ≈ tokens; recorded on
  stream drop, client disconnects included); cold-load penalty from warmth;
  `/api/show` metadata (context window, tool capability) fetched lazily and
  **propagated through federation** via `models_meta` in `/v1/nodes`.
- **Hard fit exclusions:** prompt too big for the context window; request
  carries `tools` but the model can't emit structured tool_calls.
- **Receipts:** every auto response carries `stoke_route.auto` — chosen +
  not-taken candidates with estimated cost and predicted latency, plus a
  counterfactual: the LIST PRICE of the configured `[auto_route].quality`
  model for the same token estimate. `/v1/budget` aggregates them
  (`zero_marginal_pct`, `cloud_listprice_avoided_usd_est`). Estimates, never
  quality-equivalence claims; zero claims when no quality model is configured.

**Hedged dispatch** (`hedge = true`, off by default): small prompts whose
model verifiably sits on ≥2 zero-marginal nodes are fired at both — first past
prefill wins, the loser's connection drops. Duplicate local compute buys tail
latency. Hop headers ride on both attempts, so hedging across a federated
gateway is loop-safe. Covered by `scripts/smoke.sh`.

### 2. Model aliases (next)

Policy-defined names (`coder`, `fast`, `heavy`) in TOML mapping to preference-ordered
model lists with an optional gated cloud escalation target. Pinned real model names
bypass aliasing entirely — passthrough stays honest.

### 3. Escalation ladder with gates (next)

Per alias/class, an ordered chain local → remote → cloud where moving **up** is gated
(task class allowed to spend quota ∧ cloud not in 429-cooldown ∧ per-key budget OK)
and falling **down** is automatic (429 → provider cooldown + degrade to local; timeouts
and connection failures → next rung). Cloud is a scarce resource spent deliberately.

### 4. Classification (demoted)

The keyword classifier (`auto_route.rs`) only selects which alias ladder applies when
the client sends `auto`. For agent traffic the reliable signals are structural and free:
estimated tokens, tools present, message depth. No learned routing, no LLM-judge —
v1 stays deterministic and debuggable. Outcomes (TTFT, errors, immediate client
retries) are logged so a scoring loop has data if evidence ever justifies one.

## Deliberately out of scope

- Difficulty estimation / LLM-based routing (no evidence it pays for its complexity yet)
- Duplicating a model within one node (that's Ollama's `OLLAMA_NUM_PARALLEL` domain)
- Validation-based escalation (generate → check → escalate) — revisit after the ladder ships

## Verification

Unit tests in `src/nodes.rs` (scoring, exclusions, in-flight guard). End-to-end: two mock
Ollama nodes (one warm, one cold) — placement chose warm-remote over cold-local; killing
the warm node marked it unreachable within one poll cycle and requests failed over to the
survivor, with both decisions visible in `stoke_route`.
