---
title: Use Optional Headroom for JSON Tool Results
description: Compact eligible JSON tool-result whitespace before native Responses requests, with an authenticated local Headroom worker.
slug: optional-headroom
category: Efficiency
icon: nodes
---

# Use Optional Headroom for JSON Tool Results

Headroom is an **optional, default-off** component. Stoke sends eligible tool-result strings to a local worker before forwarding a native `/v1/responses` request. It accepts only smaller JSON whitespace transformations that preserve non-whitespace lexemes; supported JSON-in-string envelopes retain their outer metadata bytes.

This is not streamed-response compression, summarization, or a general token-savings guarantee. It does not rewrite model selection, reasoning fields, tool schemas, or opaque items. Chat Completions, Messages and WebSockets are outside this integration's scope. Prose and unsupported outputs pass through unchanged.

## Installation scope

Use the launcher and binary from the same Headroom-capable checkout or release package. The current optional dependency lock is tested on **macOS arm64 with CPython 3.11**. Other worker platforms need a reviewed wheel hash and their own verification. The core Stoke binary still runs without Python or Headroom installed.

From the package or checkout root:

```sh
python3.11 -m venv .headroom-venv
.headroom-venv/bin/python -m pip install --require-hashes --no-deps -r integrations/headroom/requirements-optional.txt
```

This installs the hash-pinned official `headroom-ai` wheel without its Python dependency set or extras. The integration loads only the modules it uses and its bundled, attributed JSON-envelope adapter. It does not install `ast-grep-cli`. License and provenance records are in `integrations/headroom/licenses/` and `integrations/headroom/auditroot/`.

The wheel is downloaded from its publisher during installation, not bundled into the Stoke binary or archive. The supplied inventory covers the worker's Python runtime components; it is not an independent audit of every native dependency in the publisher's wheel.

Supported runs use this dedicated, unchanged environment. The install hash is not runtime attestation: installed files are not re-hashed on launch. Upgrading/replacing packages or setting the audit-only `HEADROOM_UPSTREAM_SOURCE` override requires a separate review.

## Run with an existing gateway configuration

Keep your existing provider setup and gateway credentials. Run from the package root, replacing the two paths below with your actual binary and config:

```sh
.headroom-venv/bin/python integrations/headroom/launch.py --stoke-bin /path/to/stoke --config /path/to/stoke.toml
```

The foreground launcher:

- starts one worker on `127.0.0.1`, with the OS selecting its port;
- generates a per-run worker token and checks authenticated readiness;
- passes only a minimal offline environment to the worker, not the gateway's provider credentials;
- creates a temporary config copy and passes its exact path through `STOKE_CONFIG`;
- starts Stoke and cleans up its owned worker and config on exit.

The original config is not overwritten. Client traffic must still authenticate to Stoke normally. Requests go through gateway auth and admission before the worker is called.

To run without Headroom:

```sh
python3.11 integrations/headroom/launch.py --off --stoke-bin /path/to/stoke --config /path/to/stoke.toml
```

`--off` does not import Headroom or start a worker. Running Stoke directly without enabling `[plugins.headroom]` also leaves the integration off.

## What to verify

Native Responses replies expose `x-stoke-headroom`:

- `disabled`: integration is off;
- `bypassed`: no eligible work was sent;
- `compressed`: a smaller transformation passed validation;
- `unavailable`: the worker or its authentication was unavailable;
- `rejected`: a transformation or boundary check was rejected.

Unavailable or rejected transformations retain the original request outputs. They do not silently switch the model or reduce reasoning. Both buffered and streaming Responses requests are supported **before dispatch**; upstream SSE bytes are not compressed.

For an offline end-to-end check from a source checkout:

```sh
python3.11 scripts/smoke_headroom_package.py --binary /path/to/stoke --worker-python .headroom-venv/bin/python
```

This exercises the actual worker and launcher with a deterministic local provider, not a live model. It checks on/off behavior, buffered/streaming requests, output preservation, unchanged surrounding fields and cleanup.

## Security and operational limits

Tool outputs can contain sensitive data. Eligible strings are sent to the local worker; other native request fields and provider credentials are not part of its payload. Treat the worker and its interpreter as trusted local code, not a sandbox or public service.

The worker requires its per-run Bearer token, bounds a request/response at 1 MiB and a batch at 128 outputs, rejects ambiguous HTTP framing and duplicate JSON-envelope keys, and uses a single-threaded server with a read timeout. Stoke also bounds its worker request and response. These limits bound the interface; they are not OS-level CPU or memory isolation.

The default gateway timeout is one second with one worker attempt. On timeout, Stoke forwards the original output; it does not cancel CPU work already running in the worker. Measure task correctness, total usage, prompt-cache effects and latency on your own workload. Smaller tool-result strings do not by themselves prove lower billing or subscription allowance consumption.
