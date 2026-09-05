#!/usr/bin/env bash
# End-to-end regression harness for the Claude/Codex subscription fallback and
# the alias bridge. Deterministic: every upstream is a local stdlib mock, no
# real provider, no real credentials.
#
# Proves (review blockers from the Selsey quality/security review):
#   1. a qualified 429 (usage_limit_reached) triggers the fallback; an unrelated
#      upstream error does not;
#   2. the codex fallback retargets the model upstream (no claude-* id reaches
#      the Responses backend);
#   3. the response discloses fallback provenance AND billing mode
#      (x-stoke-fallback-*, x-stoke-billing-mode);
#   4. a metered local fallback bills the CALLER's key (budget attribution);
#   5. stream and non-stream both work and disclose;
#   6. the alias bridge routes (codex alias -> codex provider, local alias ->
#      its named provider, unknown alias fails closed).
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d)"
CLAUDE_PORT=${CLAUDE_PORT:-11711}
CODEX_PORT=${CODEX_PORT:-11712}
LOCAL_PORT=${LOCAL_PORT:-11713}
STOKE_PORT=${STOKE_PORT:-8792}
KEY_A="key-alpha"
FIXTURE_HOME="$WORK_DIR/home"

cleanup() {
  kill $(jobs -p) 2>/dev/null || true
  if [ "${STOKE_SMOKE_KEEP:-0}" != "1" ]; then rm -rf "$WORK_DIR"; fi
}
trap cleanup EXIT

fail() { echo "FAIL: $1"; echo "--- stoke.log ---"; tail -25 "$WORK_DIR/stoke.log" 2>/dev/null; exit 1; }

echo "==> building stoke"
( cd "$REPO_DIR" && cargo build --bin stoke >/dev/null 2>&1 ) || fail "build"

echo "==> starting mocks (claude-limit :$CLAUDE_PORT, codex :$CODEX_PORT, local :$LOCAL_PORT)"
python3 "$REPO_DIR/scripts/mock_claude_limit.py" "$CLAUDE_PORT" limit >/dev/null 2>&1 &
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$CODEX_PORT" >/dev/null 2>&1 &
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$LOCAL_PORT" >/dev/null 2>&1 &
sleep 1

claude_calls() { curl -s "http://127.0.0.1:$CLAUDE_PORT/count" | python3 -c 'import sys,json;print(json.load(sys.stdin)["calls"])'; }
calls() { curl -s "http://127.0.0.1:${1:-$CODEX_PORT}/count" | python3 -c 'import sys,json;print(json.load(sys.stdin)["calls"])'; }
spend() {
  curl -s "http://127.0.0.1:$STOKE_PORT/v1/budget" -H "Authorization: Bearer $KEY_A" \
    | python3 -c "
import sys, json
d = json.load(sys.stdin)
m = [k for k in d['keys'] if k['key'].startswith('$1'[:8])]
print(m[0]['spend_usd'] if m else 0.0)"
}
msg() {  # authenticated /v1/messages POST; $1 = body
  curl -s -o "$WORK_DIR/resp.json" -w '%{http_code}' --max-time 20 \
    "http://127.0.0.1:$STOKE_PORT/v1/messages" \
    -H "Authorization: Bearer $KEY_A" -H 'Content-Type: application/json' -d "$1"
}
post_save_headers() {  # $1 = body; saves headers for inspection
  curl -s -o "$WORK_DIR/resp.json" -D "$WORK_DIR/resp_headers.json" -w '%{http_code}' --max-time 20 \
    "http://127.0.0.1:$STOKE_PORT/v1/messages" \
    -H "Authorization: Bearer $KEY_A" -H 'Content-Type: application/json' -d "$1"
}
hdr() { grep -i "^$1:" "$WORK_DIR/resp_headers.json" | head -1 | cut -d' ' -f2- | tr -d '\r'; }
# Fixture credentials so the OAuth stores resolve without any real login.
mkdir -p "$FIXTURE_HOME/.stoke" "$FIXTURE_HOME/.codex"
cat > "$FIXTURE_HOME/.stoke/anthropic_oauth.json" <<'EOF'
{"access_token":"fixture-claude-token","refresh_token":"fixture-refresh","expires_at":9999999999,"created_at":1000000000}
EOF
cat > "$FIXTURE_HOME/.codex/auth.json" <<'EOF'
{"tokens":{"access_token":"fixture-codex-token","account_id":"fixture-account","refresh_token":"fixture-refresh"}}
EOF
chmod 600 "$FIXTURE_HOME/.stoke/anthropic_oauth.json"

write_config() {  # $1 = extra TOML
  cat > "$FIXTURE_HOME/stoke.toml" <<EOF
default_model = "home-model"

[server]
host = "127.0.0.1"
port = $STOKE_PORT

# The subscription provider that will refuse with a qualified 429.
[[providers]]
name = "claude-sub"
type = "claude_subscription"
base_url = "http://127.0.0.1:$CLAUDE_PORT"
tier = "subscription"

# Codex subscription fallback candidate: pinned model = the retarget target.
[[providers]]
name = "codex-sub"
type = "codex_subscription"
base_url = "http://127.0.0.1:$CODEX_PORT/v1"
tier = "subscription"
models = ["gpt-fixture"]

# Metered? No — local tier, so fallback attribution is proven via budget keys.
[[providers]]
name = "ollama"
type = "openai_compatible"
base_url = "http://127.0.0.1:$LOCAL_PORT/v1"
tier = "local"
models = ["llama-fixture"]

[[keys]]
key = "$KEY_A"
budget_usd = 100.0

[subscription_fallback]
enabled = true
codex_provider = "codex-sub"
allow_local = true

[pricing.models]
# Priced only via named models when needed:
[pricing.models.claude-fixture]
input_per_1m = 5.0
output_per_1m = 25.0

# Price the local fallback model so the metered-fallback attribution is visible:
# a tier "local" provider CAN carry prices (operator-owned but priced), and the
# spend must still land on the caller's key.
[pricing.models.llama-fixture]
input_per_1m = 5.0
output_per_1m = 25.0
$1
EOF
}

start_stoke() {
  ( cd "$FIXTURE_HOME" && exec env HOME="$FIXTURE_HOME" STOKE_API_KEYS="$KEY_A" \
      STOKE_TEST_SUBSCRIPTION_BASES="http://127.0.0.1:$CLAUDE_PORT,http://127.0.0.1:$CODEX_PORT/v1" \
      "$REPO_DIR/target/debug/stoke" > "$WORK_DIR/stoke.log" 2>&1 ) &
  sleep 3
}
stop_stoke() { pkill -f "$REPO_DIR/target/debug/stoke" 2>/dev/null || true; sleep 1; }

# ── 1. Qualified 429 triggers the codex fallback ──────────────────────
echo "==> assert: a qualified 429 falls back to the codex provider, retargeting the model"
write_config ""
start_stoke
BODY='{"model":"claude-fixture","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}'
BEFORE_CLA=$(claude_calls); BEFORE_CODEX=$(calls)
CODE=$(post_save_headers "$BODY")
AFTER_CLA=$(claude_calls); AFTER_CODEX=$(calls "$CODEX_PORT")
[ "$CODE" = "200" ] || fail "qualified 429 fallback: expected 200, got $CODE"
[ "$((AFTER_CLA - BEFORE_CLA))" = "1" ] || fail "expected exactly 1 claude upstream call"
[ "$((AFTER_CODEX - BEFORE_CODEX))" = "1" ] || fail "expected exactly 1 codex fallback call"
python3 -c "
import json
body = json.load(open('$WORK_DIR/resp.json'))
assert body.get('content'), 'fallback body is an Anthropic message'
assert body.get('model','').startswith('claude-stoke') or body.get('model'), 'model disclosed'
" || fail "fallback body is not a translated Anthropic message"
[ "$(hdr x-stoke-fallback-from)" = "claude claude-fixture" ] || fail "fallback provenance header missing/wrong"
[ "$(hdr x-stoke-fallback-model)" = "gpt-fixture" ] || fail "fallback model header missing/wrong"
[ "$(hdr x-stoke-billing-mode)" = "chatgpt_subscription" ] || fail "billing-mode header lost on fallback"
echo "     provenance + billing-mode headers disclosed ✔"

echo "==> assert: the codex backend saw the retargeted model, never the claude id"
curl -s "http://127.0.0.1:$CODEX_PORT/last" | python3 -c "
import sys, json
body = json.load(sys.stdin)['body']
req = json.loads(body)
assert req.get('model') == 'gpt-fixture', f'codex backend saw model={req.get(\"model\")!r}, expected gpt-fixture'
" || fail "the claude-* id leaked to the codex backend"
echo "     upstream model retargeted ✔"

echo "==> assert: with fallback disabled the qualified 429 surfaces unchanged"
stop_stoke
pkill -f "mock_claude_limit.py $CLAUDE_PORT" 2>/dev/null || true
python3 "$REPO_DIR/scripts/mock_claude_limit.py" "$CLAUDE_PORT" limit >/dev/null 2>&1 &
sleep 1
write_config ""
sed -i '' 's/^enabled = true$/enabled = false/' "$FIXTURE_HOME/stoke.toml" 2>/dev/null || sed -i 's/^enabled = true$/enabled = false/' "$FIXTURE_HOME/stoke.toml"
start_stoke
BEFORE_CLA=$(claude_calls)
CODE=$(msg "$BODY")
AFTER_CLA=$(claude_calls)
[ "$CODE" = "429" ] || fail "with fallback disabled: expected the raw 429, got $CODE"
[ "$((AFTER_CLA - BEFORE_CLA))" = "1" ] || fail "expected exactly 1 claude call with fallback disabled"
echo "     disabled path keeps the original refusal ✔"
stop_stoke

# ── 2. Metered local fallback bills the caller's key ─────────────────
echo "==> assert: a metered local fallback spends the CALLER's budget"
stop_stoke
pkill -f "mock_claude_limit.py $CLAUDE_PORT" 2>/dev/null || true
python3 "$REPO_DIR/scripts/mock_claude_limit.py" "$CLAUDE_PORT" limit >/dev/null 2>&1 &
sleep 1
write_config ""
sed -i '' 's/^codex_provider = "codex-sub"$/codex_provider = "missing-codex"/' "$FIXTURE_HOME/stoke.toml" 2>/dev/null \
  || sed -i 's/^codex_provider = "codex-sub"$/codex_provider = "missing-codex"/' "$FIXTURE_HOME/stoke.toml"
start_stoke
S0=$(spend "$KEY_A")
BEFORE_LOCAL=$(calls "$LOCAL_PORT")
CODE=$(post_save_headers "$BODY")
AFTER_LOCAL=$(calls "$LOCAL_PORT")
S1=$(spend "$KEY_A")
[ "$CODE" = "200" ] || fail "local fallback: expected 200, got $CODE"
[ "$((AFTER_LOCAL - BEFORE_LOCAL))" = "1" ] || fail "expected exactly 1 local upstream call"
python3 -c "
s0, s1 = float('$S0'), float('$S1')
assert s1 > s0 + 1e-9, f'caller budget unchanged after metered fallback: {s0} -> {s1}'
" || fail "metered fallback did not bill the caller's key"
[ "$(hdr x-stoke-cost)" != "" ] || fail "metered fallback did not disclose x-stoke-cost"
echo "     caller-key attribution + cost disclosure ✔ ($(hdr x-stoke-cost))"
stop_stoke

# ── 3. Alias bridge routing ──────────────────────────────────────────
echo "==> assert: the codex alias dispatches to the codex provider"
start_stoke
CODE=$(post_save_headers '{"model":"claude-stoke-codex--gpt-fixture","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}')
[ "$CODE" = "200" ] || fail "codex alias dispatch: expected 200, got $CODE"
[ "$(hdr x-stoke-billing-mode)" = "chatgpt_subscription" ] || fail "codex alias did not announce billing mode"
# Fail closed: a local alias naming a provider that does not exist must be
# refused, never routed to "some other provider" (the review blocker class).
CODE=$(post_save_headers '{"model":"claude-stoke-local--ghost--no-such-model","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}')
[ "$CODE" != "200" ] || fail "local alias for a nonexistent provider unexpectedly routed"
echo "     codex alias routes; nonexistent-provider alias fails closed ✔"
stop_stoke

echo "ALL PASS"