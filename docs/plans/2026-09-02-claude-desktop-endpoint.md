# Next Steps: Claude Desktop via custom Stoke endpoint

> Status: researched 2026-09-02, not started. Follow-up to the native Codex/Claude Code client work (`/v1/responses`, gateway headers, `stoke run claude|codex`).

**Goal:** Route the Claude Desktop app's inference through Stoke the way Claude Code and Codex CLI already do, so Desktop traffic gets the same auth, budget caps, and audit trail.

## What is already supported (shipped)

- `POST /v1/messages` with streaming + tool use (Anthropic Messages passthrough, gateway header forwarding) — the hard requirement for Desktop's gateway mode.
- `GET /v1/models` — Desktop uses it for model auto-discovery when present.

## Path 1 — Claude Code tab inside Desktop (works today)

Desktop's Claude Code surface reads `~/.claude/settings.json`. Add:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8787",
    "ANTHROPIC_AUTH_TOKEN": "<stoke client key>"
  }
}
```

No Stoke changes needed. Verify with a `/status` check (`Anthropic base URL` row) and one prompt while watching the audit log.

## Path 2 — native chat window (needs Desktop's 3P gateway mode)

Per Anthropic's "Claude Desktop on 3P" docs, the app supports `inferenceProvider: "gateway"` against any server implementing the Messages API.

### Steps

1. **Confirm the menu exists on our build (1.40609.1):** Developer → Configure Third-Party Inference… The 3P config is aimed at enterprise/MDM rollout; the consumer build may not expose it. If absent, stop — Path 2 is not available without MDM-style managed config.
2. **Configure:** set Inference provider to Gateway, base URL to the Stoke endpoint, credential kind to static API key (`inferenceGatewayApiKey` = a Stoke client key). Export `.mobileconfig` or push managed config.
3. **Model discovery:** check Stoke's `/v1/models` response. Desktop filters out IDs that don't look like Claude models; if we serve local/aliased models, either mark them with `anthropic_family_tier` in the `/v1/models` response (small Stoke addition) or set `inferenceModels` in the managed config.
4. **Verify:** one live chat turn through Stoke; confirm the request appears in audit/workflow logs and spend is billed.

### Caveats

- Global setting for the whole app instance — replaces the claude.ai subscription path for native chat, not per-conversation.
- SSO (`inferenceGatewayOidc`) and credential helper exist but are out of scope; static key first.
- Tool-use streaming through the gateway must be re-verified (Desktop exercises tool use differently than Claude Code).

## Suggested order

Path 1 first (zero Stoke work, immediate coverage). Path 2 only if the Developer menu exists on the installed build; the only plausible Stoke-side change is `anthropic_family_tier` in `/v1/models` — gate that on step 1's outcome.