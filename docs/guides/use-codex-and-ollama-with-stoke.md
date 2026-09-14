---
title: Connect Codex and Ollama Through Stoke
description: Use native Codex Responses traffic and discovered Ollama models behind one authenticated Stoke gateway.
slug: use-codex-and-ollama-with-stoke
category: Routing
icon: nodes
---

# Connect Codex and Ollama Through Stoke

Use one local gateway for two different paths: native Codex Responses traffic and Ollama Chat Completions traffic. Stoke authenticates the client hop with `STOKE_API_KEYS`, keeps the two provider protocols native, and makes the route decision before dispatch.

This guide uses an existing native Codex login. It does not create, refresh, print, or copy OAuth credentials. Stoke reads the native store at `~/.codex/auth.json` only for the named Codex subscription provider. Ollama model IDs are discovered from your running Ollama; no model ID is a Stoke default.

## 1. Check the two prerequisites

Install or build Stoke using the repository's normal path. Start Ollama using your existing installation, then inspect the models already on that machine:

```bash
ollama list
```

Choose an exact ID from that output for the test request. Do not substitute a model name from an example or assume Stoke ships one.

Use your existing native Codex sign-in. The gateway process must be able to read the native store without making it part of any client configuration:

```bash
test -r "$HOME/.codex/auth.json" && echo "native Codex store is readable"
stat -f '%Sp %Su %N' "$HOME/.codex/auth.json" 2>/dev/null || stat -c '%A %U %n' "$HOME/.codex/auth.json"
```

Do not paste the file contents into a config, shell command, issue, or Hermes provider. If the file is absent, use the native Codex app/CLI's normal sign-in flow; Stoke does not invent a login or refresh command.

## 2. Configure Ollama and native Codex capacity

Create `stoke.toml` with the Ollama provider and the named native Codex provider. Replace `<ollama-model-id>` with the exact ID from `ollama list`; replace no other endpoint with a guessed model name.

```toml
routing = "single"
default_model = "<ollama-model-id>"

[server]
host = "127.0.0.1"
port = 8787

[[providers]]
name = "ollama"
type = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
tier = "local"

# Native Codex subscription path. No api_key/api_key_env: Stoke reads
# the existing native login from ~/.codex/auth.json.
[[providers]]
name = "codex-subscription"
type = "codex_subscription"
base_url = "https://chatgpt.com/backend-api/codex"
tier = "subscription"
models = ["<exact-codex-model-id>"]
```

The Codex model list is intentionally explicit: discover the ID accepted by your current native Codex account and put that exact value in `models`. Stoke ships no model catalogue or provider credits.

Start the gateway with a client key. Keep this key separate from the native Codex credential:

```bash
export STOKE_API_KEY="$(openssl rand -hex 32)"
export STOKE_API_KEYS="$STOKE_API_KEY"
stoke-cli serve
```

The server-side credential boundary is now clear: `STOKE_API_KEYS` authenticates callers to Stoke; `~/.codex/auth.json` is owned and read by Stoke for the subscription provider; Ollama receives neither credential.

## 3. Verify the gateway and Ollama route

Health is unauthenticated; the inventory endpoint is protected:

```bash
curl -s http://127.0.0.1:8787/health
curl -s http://127.0.0.1:8787/v1/nodes \
  -H "Authorization: Bearer $STOKE_API_KEY"
```

The node response should show the Ollama node and the discovered model. Values such as health, warm state, load, and latency are runtime observations, not shipped defaults or performance promises.

Send an Ollama Chat Completions request with the exact discovered model:

```bash
OLLAMA_MODEL="<ollama-model-id>"
curl -s http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $STOKE_API_KEY" \
  -H 'Content-Type: application/json' \
  -d "{\"model\":\"$OLLAMA_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with one short sentence.\"}]}"
```

This is the Ollama `chat_completions` path. It is separate from native Codex Responses and does not translate one protocol into the other.

This example assumes the selected Ollama model runs locally. Ollama can also serve cloud-backed models: a loopback endpoint alone does not prove local inference. Set provider tiers to match the actual backend; tier declarations are operator configuration, not a network firewall.

## 4. Connect native Codex or Hermes

For the supported native Codex launcher, pass only the gateway key:

```bash
stoke run codex
```

The launcher points Codex at Stoke's native `/v1/responses` endpoint. Responses JSON, reasoning fields, tool calls, and SSE events remain native. The gateway loads the subscription credential from `~/.codex/auth.json` and sends it only to the exact first-party Codex destination.

For Hermes, add a **named custom provider** with `api_mode: codex_responses`; `codex_responses` is the wire mode, not the provider name. Keep the gateway key in the client environment or its owner-only `.env`, referenced by `key_env`:

```yaml
providers:
  stoke-codex:
    name: Stoke · Codex subscription
    base_url: http://127.0.0.1:8787/v1
    api_mode: codex_responses
    key_env: STOKE_API_KEY
    discover_models: false
    models:
      "<exact-codex-model-id>": {}
  stoke-ollama:
    name: Stoke · Ollama
    base_url: http://127.0.0.1:8787/v1
    api_mode: chat_completions
    key_env: STOKE_API_KEY
    discover_models: false
    models:
      "<ollama-model-id>": {}
```

Merge these entries into your existing providers rather than replacing the whole config. Select `custom:stoke-codex` or `custom:stoke-ollama` in Hermes; adding them does not change your default. See the [Hermes configuration reference](https://hermes-agent.nousresearch.com/docs/user-guide/configuration/) for configuration locations.

Do not copy OAuth into Hermes, Ollama, shell aliases, or client config. Explicit separate client OAuth remains supported for clients that send it deliberately; it is not required for the gateway-owned native path.

## What is enforced, and what is not

- `STOKE_API_KEYS` is required for the client-to-gateway request in normal operation; the gateway remains fail-closed.
- Native Responses and Ollama Chat Completions keep their own wire formats, including Responses reasoning and SSE.
- Subscription traffic bypasses per-token USD metering because it has no per-request API price. Rate limits, loop detection, and the provider's own plan limits still apply. This is not a subscription hard-dollar cap or quota-savings claim.
- The cache policy applies only to eligible non-streaming named routes; this guide does not claim Responses caching. TTL reuse and lazy cleanup are not secure erasure.
- Exact coalescing is opt-in and bounded. Concurrent identical eligible requests can count as one provider execution, while decision-feed followers and dispatch-level receipts remain distinct accounting concepts.
- Validated retry headers are client propagation only. They do not add an internal retry or failover policy.

## Troubleshooting without exposing credentials

If Codex traffic is unavailable, check that the native store exists and is readable by the running Stoke user, that the provider URL is exactly `https://chatgpt.com/backend-api/codex`, and that the configured model ID is accepted by the current account. Do not print or upload `auth.json`.

If Ollama traffic is unavailable, run `ollama list`, use an exact discovered model ID, and check that Ollama is listening on `127.0.0.1:11434`. The two paths use different provider types and do not share credentials.
