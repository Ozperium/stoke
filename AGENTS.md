# Working in this repo (for coding agents)

Stoke is a single Rust binary: an enforcement gateway between AI agents and model providers. This file is the contract for changing it. Follow it and CI will pass; ignore it and it won't.

## Build, test, verify

```sh
cargo build                 # debug
cargo build --release       # ~5.5 MB stripped binary
cargo test                  # unit + integration tests, all must pass
./scripts/smoke.sh          # end-to-end: discovery, placement, streaming, failover — no Ollama needed
./scripts/smoke_federation.sh   # two gateways, hop guard, cycle safety
```

CI (`.github/workflows/ci.yml`) runs all four on every push and PR. Run them locally before you push. The smoke scripts spin up stdlib mock Ollama nodes, so they need only `python3` and `curl`.

## Layout

- `src/main.rs` — the request pipeline (auth → budget/rate/loop → route profile → plugins → cache → placement → provider → post-response). This is the spine; read it first.
- `src/budget.rs` — enforcement: fail-closed auth, budget caps, rate limits, the loop breaker.
- `src/nodes.rs` — the node registry: discovery, warm/load-aware placement, federation.
- `src/auto_route.rs` — the auto-routing scorer.
- `src/messages.rs` — the Anthropic `/v1/messages` endpoint.
- `src/config.rs`, `src/cache.rs`, `src/failover.rs`, `src/router.rs`, `src/cli.rs`.
- `scripts/` — smoke harnesses and a mock Ollama.
- `landing/` — the marketing site (Cloudflare Pages) and the POSIX `install.sh`.

## Invariants — do not break these

1. **Stoke ships zero model names.** No model string may be hardcoded in `src/` outside the pricing table in `cost.rs`. Models come from user config or discovery. If you need a default, resolve it from the config or the node registry.
2. **Fail-closed stays fail-closed.** No config path may result in an open gateway. No keys + no `STOKE_DEV=1` must reject every request. Every endpoint except `/health` requires auth.
3. **Every user-facing claim must be verifiable.** No performance numbers without a runnable harness. No "zero cost" / "free" (local models cost electricity and hardware). No quality-equivalence claims. If you write it in a doc or the landing page, a reader must be able to check it.
4. **The installer is POSIX `sh`, not bash.** `landing/install.sh` is piped into `/bin/sh` (dash on Linux). No `echo -e`, no `pipefail`, no process substitution, no `[[`. Verify with `dash -n landing/install.sh`.
5. **Enforcement runs before the provider call.** Auth, budget, rate limit, and loop detection happen before any upstream request. Don't reorder them after routing.

## Conventions

- Match the surrounding code's style; no repo-wide reformatting in a feature PR.
- New behavior gets a test. New end-to-end behavior gets a smoke assertion.
- Keep the binary dependency-free at runtime (no Python/Postgres/Redis). It is the whole pitch.
- Commit messages: what changed and why, imperative mood.
