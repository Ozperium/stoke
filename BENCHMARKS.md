# Benchmarks

Stoke publishes no performance numbers yet — deliberately. Every number on this
page will be one you can reproduce yourself, or it won't be here.

## What will be measured

Stoke is an enforcement gateway, so the numbers that matter are enforcement
numbers, not capability scores:

- **p99 added latency** — what the gateway costs per request on the hot path
- **Time-to-trip** — how long a runaway agent loop runs before the circuit breaker blocks the key
- **Loop-detector false-positive rate** — how often legitimate traffic gets blocked, measured on real agent traces
- **Throughput and memory** — sustained load on a single binary

These will ship together with a public, reproducible enforcement harness.
Numbers appear here only when you can run the harness and check them yourself.

## What you can verify today

The repo ships runnable smoke harnesses:

- [`scripts/smoke.sh`](scripts/smoke.sh) — node discovery, warm-model placement, streaming in-flight accounting, failover, health
- [`scripts/smoke_federation.sh`](scripts/smoke_federation.sh) — two gateways in a deliberate A↔B cycle: inventory import, cross-gateway routing, hop guard, cycle safety

Both run against real local processes on your machine. No published numbers,
no trust required.

## About the old capability benchmarks

Earlier versions of this page carried HumanEval and GPQA results from
fusion/ensemble experiments (June 2026, on models that are now dated). Those
results were moved out of the product story.

The research conclusion stands, and is documented in
[`docs/PATTERNS.md`](docs/PATTERNS.md): ensemble patterns can preserve quality
at lower cost, but they don't add intelligence. That's why Stoke doesn't market
them — and why the project's focus is enforcement.
