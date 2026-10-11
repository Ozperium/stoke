# Supported capability, protocol, and configuration matrix

This is the current-state reference for the `v0.2.2` source candidate, including the
solidification fixes after the original `08d12fc` audit. See [release notes](releases/v0.2.2.md)
and the published manifest for release identity. The matrix describes source behavior,
not a promise that a provider or published binary is available.

## Protocol and provider matrix

| Surface | Activation | Provider/protocol boundary | Auth and billing | Gates and important limits | Proof |
|---|---|---|---|---|---|
| `POST /v1/chat/completions` JSON/SSE | Send `model` and `messages`; point an OpenAI-compatible client at gateway `/v1` | `openai_compatible`; native Chat Completions passthrough | Gateway key required; metered tiers use configured prices; local/remote are free-tier by configuration, not free hardware | Auth, budget hold, RPM, loop detection, route policy, provider pricing before dispatch; SSE usage is charged at close | `cargo test --locked`; `./scripts/smoke.sh` |
| `POST /v1/responses` JSON/SSE | Send Responses `model`/`input`, or `stoke run codex` | Native OpenAI Responses; `openai_compatible` or `codex_subscription`; no Anthropic translation | Gateway key required. Metered usage comes from `response.completed`; subscription traffic has no dollar spend and still has rate/loop controls. Successful streams disclose `x-stoke-node`; subscription streams disclose `x-stoke-billing-mode` and do not fabricate cost | Native reasoning, tools, and events; no claim of Responses caching | `./scripts/smoke_responses.sh`; `cargo test --locked` |
| `POST /v1/messages` native | Anthropic Messages request or `stoke run claude` | Native passthrough only to `type = "anthropic"`; requires exact validated destination/credential | Gateway key required; upstream Anthropic credential is separate; usage/cost follows provider mode | Supported Anthropic headers/tool shapes; unsupported features in translation paths are rejected | `cargo test --locked`; `./scripts/smoke_subscription_fallback.sh` |
| Messages → local Chat alias | `model=claude-stoke-local--<provider>--<model>` (legacy aliases only where catalogued) | Translates supported Messages to Chat for `openai_compatible` providers on `local` or `remote` tiers | Gateway key; local/remote pricing semantics apply | Text, function tools, tool choice and streamed deltas supported; images, thinking, server tools, and web search rejected | `cargo test --all-targets` |
| Messages → Codex alias | `claude-stoke-codex--<model>` or qualified alias when multiple subscription providers exist | Translates Messages to native Responses for `codex_subscription`; this is not Chat translation | Gateway key; native Codex OAuth is sent only to exact `https://chatgpt.com/backend-api/codex`; subscription bypasses dollar metering | Unsupported reasoning/server-tool fields and `stop_sequences` rejected; aliases are emitted only when actually routable | `cargo test --all-targets`; fallback smoke |
| Claude subscription | `stoke-cli login-claude`; provider `type=claude_subscription`, `tier=subscription` | Native Anthropic destination `https://api.anthropic.com` | OAuth stored owner-only; no static API key; plan quota is not a dollar cap | Auth/RPM/loop remain; exact destination and credential rules are fail-closed | `cargo test --locked` |
| Opt-in subscription fallback | `[subscription_fallback] enabled=true` | Typed Claude subscription refusal → named Codex subscription and/or explicitly allowed local/remote OpenAI-compatible provider | Fallback headers disclose source/model/node; local fallback is metered and budgeted on caller key | Off by default; only qualified 429/503 limit/rate/overload errors; model/provider changes are disclosed | `STOKE_PORT=8893 CLAUDE_PORT=12211 CODEX_PORT=12212 LOCAL_PORT=12213 bash ./scripts/smoke_subscription_fallback.sh` |

## Gateway controls and extensions

| Capability | Activation | Contract and caveat | Proof |
|---|---|---|---|
| Fail-closed auth | `STOKE_API_KEYS=...`; `STOKE_DEV=1` only for local anonymous dev | Every endpoint except `GET /health` requires auth. Accepted key headers include `x-stoke-key`, `x-api-key`, and matching legacy bearer auth. Gateway credential is never forwarded as provider auth | `cargo test --locked`; unauthenticated `/v1/nodes` should return 401 |
| Budget, RPM, loop, in-flight holds | `[[keys]]`; optional semantic mode needs `STOKE_SEMANTIC_CACHE` and `STOKE_EMBED_MODEL` | Refuses before provider call. Exact loop threshold is 5 in 60s with 120s block; semantic default is 0.85. Holds persist through crashes in SQLite and require reconciliation | `./scripts/smoke_spend.sh`; `cargo test --locked` |
| Durable ledger | Automatic; override `STOKE_LEDGER_PATH`; `stoke-cli ledger ...` | SQLite spend, reservations, rate/loop events and stable key IDs. Unusable storage fails closed. Raw secrets are not stored | `cargo test --locked`; `stoke-cli ledger status` |
| Node placement/failover | Providers with `tier=local|remote|cloud`; Ollama-compatible `/v1` nodes are polled | Warm > cold > unknown, then tier/load/latency; non-stream retries top three, connect-phase stream failover only. Cloud is not polled | `./scripts/smoke.sh` |
| Federation | `type=stoke`, authenticated peer `/v1/nodes`, `tier=remote` | One hop exactly; `x-stoke-hop` excludes downstream Stoke providers. Federation is excluded from stream races because that path does not propagate the hop | `./scripts/smoke_federation.sh` |
| Auto routing | `model=auto`, `auto-cheap`, `auto-fast`, or `routing=auto`; `[auto_route]` roles | Scores eligible model/node candidates and emits an estimate/counterfactual receipt. Deterministic mock smoke covers mode selection, not real-provider performance | `cargo test --locked`; `./scripts/smoke_auto_route.sh` |
| Hedge | Global `[auto_route] hedge=true` | Small prompts, two ranked non-cloud nodes with the model; races connections. Not route-profile-specific and duplicates local compute | `./scripts/smoke.sh` |
| Exact/semantic cache | Default deterministic single requests; named route `[routes.response_cache] mode="exact"`, positive `ttl_secs`; semantic env opt-in | Caller/route/model/effective-settings scoped. No streams, fan-out, unsupported shapes, or Responses claim. Semantic similarity is not answer equivalence | `scripts/smoke_cache_identity.py`; `cargo test --locked` |
| Exact coalescing | Named exact route plus `coalesce=true` | Followers wait up to five seconds and reuse cache; timeout/cancel falls back to independent dispatch. Default off | `scripts/smoke_coalescing.py`; `cargo test --locked` |
| Built-ins | Route `builtins=[...]` and `[builtins.*]` config | `prompt_harness`, `pii_redact`, `code_formatter`, `audit_log`; audit logging defaults to metadata-only and owner-only files; body logging requires explicit opt-in | `cargo test --locked` |
| HTTP webhooks | `[plugins]` URL arrays | Sequential pre/filter/post hooks; input hooks fail closed, post hooks are best effort. Non-HTTP and metadata destinations rejected; this is not an OS sandbox | `cargo test --locked`; see `docs/guides/block-prompt-injection-at-the-gateway.md` |
| JS/TS plugins | Build `cargo build --features js-plugins`; `[plugins] scripts=[...]` | In-process deno_core/V8, dedicated thread, not sandboxed. Feature-gated and optional; sequential chains have regression coverage. Default prebuilt binaries exclude V8 | `cargo test --locked --features js-plugins` |
| Headroom | `[plugins.headroom] enabled=true`, numeric loopback URL, `STOKE_HEADROOM_TOKEN` | Responses-only output filter for eligible function outputs; bounds 1 MiB/128, validates transforms, bypasses on invalid/timeout. Optional worker is trusted local code | `cargo test --locked`; `scripts/smoke_headroom.py`; `scripts/smoke_headroom_package.py`; `docs/guides/optional-headroom.md` |
| UI/status | Authenticated `/ui`, `/ui/summary`, `/ui/events`, `/ui/demo`; `/v1/*` status endpoints | Dashboard and status surfaces are protected. `/v1/ttft` exists but legacy tracker is not fed by the main request path; node stream performance is separate internal metadata | `cargo test --locked` |

## Configuration and environment quick reference

| Item | Meaning |
|---|---|
| `STOKE_CONFIG` | Explicit config path; otherwise `./stoke.toml`, then `~/.config/stoke/stoke.toml` |
| `STOKE_API_KEYS` / `STOKE_DEV` | Protected gateway keys / explicit anonymous local development |
| `STOKE_LEDGER_PATH` | SQLite ledger path; default `~/.stoke/ledger.db` |
| `STOKE_SEMANTIC_CACHE` + `STOKE_EMBED_MODEL` | Enable semantic cache/loop mode; embedding model is required and has no shipped default |
| `STOKE_LOOP_SEMANTIC_THRESHOLD` | Semantic loop cosine threshold, default 0.85 |
| `OLLAMA_BASE_URL` | Embedding Ollama base, default `http://127.0.0.1:11434` |
| `STOKE_NODE_POLL_SECS` | Node poll interval, default 10 seconds |
| `STOKE_HEADROOM_TOKEN` | Bearer token for optional local Headroom worker |
| `[pricing.models]` / `[pricing] unpriced="free"` | Metered model prices; the explicit escape hatch accepts unpriced spend as unmetered |
| `[limits]` | `max_n_samples`, `max_vote_models`, `allow_caller_routing`, `assumed_max_output_tokens` |
| `[[routes]]` | Named POST paths, pinned model/routing/plugins, `allowed_tiers`, exact cache/coalescing policy |

## Reachability boundary

| Name | Source code | Gateway selectable | Status |
|---|---:|---:|---|
| `single` | yes | yes/default | supported |
| `cascade` | yes | yes when pinned/configured | supported, non-streaming |
| `self_consistency` | yes | yes when pinned/configured | supported, non-streaming and bounded |
| `stream_race` | yes | yes on streaming path | supported narrowly; SSE only |
| `auto`, `auto-cheap`, `auto-fast` | yes | yes via pseudo-model/routing | supported with unit evidence; live mode proof limited |
| `parallel_vote` | yes | no | source helper only; not a shipped gateway route |
| `deliberation` | yes | no | source helper only; not a shipped gateway route |
| `test_vote`, `cascade_test` | removed/refused | no | explicitly refused; never restore gateway-side code execution |

There are no invented models, prices, latency metrics, savings percentages, or
quality-equivalence claims in this matrix. Re-run the proof command against the exact
source or binary under review; a successful source test does not prove a stable artifact
or customer outcome.
