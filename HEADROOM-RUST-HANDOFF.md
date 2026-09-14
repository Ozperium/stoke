# Headroom Rust Handoff

## Scope

Rust gateway integration is in:

- `src/plugins.rs` — opt-in native output-only filter, fixed worker auth, bounds, strict lexical/JSON-envelope validation, fail-open original body, loopback-only HTTP client.
- `src/config.rs` — validates enabled Headroom configuration and preserves `STOKE_CONFIG` override behavior.
- `src/responses.rs` — invokes Headroom only after gateway auth/admission/reservation for buffered and streaming `/v1/responses`; emits computed `x-stoke-headroom` status.
- `stoke.example.toml` — default-off example and worker contract.
- `scripts/smoke_headroom.py` — offline worker/provider HTTP-spy smoke.

Python worker/launcher owns the worker process. It must read `STOKE_HEADROOM_TOKEN`, accept `Authorization: Bearer <token>`, and implement the same 1 MiB serialized request/response and 128-output limits.

## Contract evidence

- Default is disabled; enabled configuration requires numeric-loopback `http://` URL.
- Worker receives only `{"outputs": [...]}`; request model/tools/metadata/opaque fields remain gateway-owned.
- Missing `STOKE_HEADROOM_TOKEN` returns unavailable/fail-open without sending outputs; the offline smoke also verifies provider forwarding continues with the original body while worker count stays unchanged.
- Worker requests use only `Authorization: Bearer` from `STOKE_HEADROOM_TOKEN`; no provider or gateway credential headers are forwarded.
- Oversized output batches and serialized requests bypass unchanged; oversized/malformed worker responses reject unchanged; response reading is chunk-bounded, not `bytes()`.
- Native streaming and buffered paths run after auth/admission and use computed `x-stoke-headroom` headers.

## Exact verification commands

Run from `/Users/pawloz/projects/stoke-release-audit-974fbbf9/candidate`:

```sh
cargo test --lib headroom -- --nocapture
cargo test --bin stoke responses::tests -- --nocapture
cargo build
python3 scripts/smoke_headroom.py
```

Observed evidence in this handoff:

- Headroom unit contract: **6 passed**.
- Responses focused suite: **18 passed**.
- `cargo build`: **passed**; only pre-existing repository warnings.
- Offline spy smoke: **PASS** — buffered + streaming, auth/admission deny with no worker/provider inputs, worker Bearer token, and no raw credential/payload logs.
- `cargo test --lib`: **72 passed, 1 pre-existing failure** in `subscription::tests::debug_override_accepts_only_numeric_loopback_http_without_url_tricks` (`http://[::1]:43123`); this is outside the owned Headroom files and was unchanged.

No provider live calls, installs, or commits were used.
