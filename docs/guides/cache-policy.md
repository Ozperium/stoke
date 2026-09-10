# Named-route response cache policy

A route may opt into a narrow response-cache policy with `response_cache`:

```toml
[[routes]]
name = "repeatable-work"
path = "/v1/repeatable/completions"
model = "your-configured-model"
routing = "single"
stream = false

[routes.response_cache]
mode = "exact"
ttl_secs = 60
```

`mode = "exact"` requires a positive `ttl_secs` and uses the full A1 request
identity (scope, model, and the complete effective request). It does not perform
semantic lookup or embedding work. Only successful, complete text completions
(`finish_reason = "stop"`, string message content, and no `tool_calls`) are
stored. `mode = "off"` disables response-cache lookup and storage for that
route. An omitted policy preserves the legacy global exact-plus-semantic cache.

The route policy is eligible only for the existing `routing = "single"` and
`stream = false` cache gate. Streaming, fan-out, off-policy, and
`Cache-Control: no-store`/`no-cache` requests do not serialize or hash the whole
request for response caching. All duplicate Cache-Control values and directive
case variants are honored. These request directives do not alter provider-native
prompt caching.

The route TTL can shorten the existing global cache retention, never extend it:
`effective_ttl = min(route.ttl_secs, global_ttl)`. The current global retention
is 3600 seconds. A TTL larger than that is therefore capped rather than silently
claimed as an extension.

Invalid policy modes, missing/zero exact TTLs, off policies carrying a TTL, and
wrong TOML TTL types are rejected during config validation.

## Runnable verification

Build and run the provider-fixture smoke test from the repository root:

```sh
cargo build --offline --locked
STOKE_BIN=target/debug/stoke python3 scripts/smoke_cache_identity.py
```

The smoke starts a fresh gateway and mock provider, then verifies legacy exact
behavior, exact-route hits, off-route misses, Cache-Control bypass without
population, and non-cacheable truncated responses using provider call counts.
The test is a wire-behavior check, not an inference or savings benchmark.

## Limitations

The cache remains in-process and global retention remains configured by the
existing gateway construction. This package does not add persistence,
coalescing, retries, or subscription/prompt-cache accounting changes. A
zero-temperature request is only a repeatable-workload opt-in under the existing
cache threshold; it is not a mathematical determinism guarantee.
