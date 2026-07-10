#!/usr/bin/env bash
# End-to-end regression harness for the spend firewall.
#
# Each assertion below corresponds to a bug that shipped once. The upstream call
# count is load-bearing: it separates "refused before spending" from "spent, then
# errored", which look identical from the client's side.
#
# No Ollama needed — a counting mock stands in for the provider.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d)"
UP_PORT=${UP_PORT:-11701}
STOKE_PORT=${STOKE_PORT:-8791}
KEY_A="key-alpha"
KEY_B="key-beta"

cleanup() { kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK_DIR"; }
trap cleanup EXIT

fail() { echo "FAIL: $1"; echo "--- stoke.log ---"; tail -25 "$WORK_DIR/stoke.log" 2>/dev/null; exit 1; }

echo "==> building stoke"
( cd "$REPO_DIR" && cargo build --bin stoke >/dev/null 2>&1 ) || fail "build"

echo "==> starting counting mock provider on :$UP_PORT"
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$UP_PORT" >/dev/null 2>&1 &
sleep 1

calls() { curl -s "http://127.0.0.1:$UP_PORT/count" | python3 -c 'import sys,json;print(json.load(sys.stdin)["calls"])'; }
spend() {
  curl -s "http://127.0.0.1:$STOKE_PORT/v1/budget" -H "Authorization: Bearer $1" \
    | python3 -c "
import sys, json
d = json.load(sys.stdin)
m = [k for k in d['keys'] if k['key'].startswith('$1'[:8])]
print(m[0]['spend_usd'] if m else 0.0)"
}
post() {
  curl -s -o "$WORK_DIR/resp.json" -w '%{http_code}' --max-time 20 \
    "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
    -H "Authorization: Bearer $1" -H 'Content-Type: application/json' -d "$2"
}
start_stoke() {
  ( cd "$WORK_DIR" && exec env STOKE_API_KEYS="$KEY_A,$KEY_B" \
      "$REPO_DIR/target/debug/stoke" > "$WORK_DIR/stoke.log" 2>&1 ) &
  sleep 3
}
stop_stoke() { pkill -f "$REPO_DIR/target/debug/stoke" 2>/dev/null || true; sleep 1; }

write_config() {  # $1 = extra TOML appended
  cat > "$WORK_DIR/stoke.toml" <<EOF
routing = "single"
default_model = "priced-model"

[server]
host = "127.0.0.1"
port = $STOKE_PORT

[[providers]]
name = "metered"
base_url = "http://127.0.0.1:$UP_PORT/v1"
tier = "cloud"
models = ["priced-model", "priced-model-2", "unpriced-model"]

[[keys]]
key = "$KEY_A"
budget_usd = 100.0

[[keys]]
key = "$KEY_B"
budget_usd = 100.0

# 1000 in @ \$5/1M + 1000 out @ \$25/1M = \$0.030 per call
[pricing.models.priced-model]
input_per_1m = 5.0
output_per_1m = 25.0

[pricing.models.priced-model-2]
input_per_1m = 5.0
output_per_1m = 25.0

# Two vote models + a code prompt + test_code is what makes auto_route::decide
# answer "cascade_test" — a fan-out the caller never names.
[auto_route]
coder = "priced-model"
fast = "priced-model"
vote_models = ["priced-model", "priced-model-2"]
$1
EOF
}

# ── A provider with no tier must not boot ────────────────────────────
echo "==> assert: a provider with no declared tier is refused at boot"
cat > "$WORK_DIR/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $STOKE_PORT
[[providers]]
name = "mystery"
base_url = "http://127.0.0.1:$UP_PORT/v1"
EOF
if ( cd "$WORK_DIR" && env STOKE_API_KEYS="$KEY_A" timeout 5 "$REPO_DIR/target/debug/stoke" >"$WORK_DIR/boot.log" 2>&1 ); then
  fail "started with an untiered provider"
fi
grep -q "tier" "$WORK_DIR/boot.log" || fail "boot error did not mention the tier"

# ── The pricing gate ─────────────────────────────────────────────────
write_config ""
start_stoke

echo "==> assert: an unpriced model on a metered provider is refused, before any call"
BEFORE=$(calls)
CODE=$(post "$KEY_A" '{"model":"unpriced-model","messages":[{"role":"user","content":"x"}]}')
AFTER=$(calls)
[ "$CODE" = "403" ] || fail "unpriced non-stream: expected 403, got $CODE"
[ "$((AFTER - BEFORE))" = "0" ] || fail "unpriced non-stream reached the provider"

echo "==> assert: ...and stream=true is not a way around it"
BEFORE=$(calls)
CODE=$(post "$KEY_A" '{"model":"unpriced-model","stream":true,"messages":[{"role":"user","content":"x"}]}')
AFTER=$(calls)
[ "$CODE" = "403" ] || fail "unpriced stream: expected 403, got $CODE"
[ "$((AFTER - BEFORE))" = "0" ] || fail "unpriced stream reached the provider"

echo "==> assert: a priced model is served and metered"
BEFORE=$(calls); S0=$(spend "$KEY_A")
CODE=$(post "$KEY_A" '{"model":"priced-model","messages":[{"role":"user","content":"x"}]}')
AFTER=$(calls); S1=$(spend "$KEY_A")
[ "$CODE" = "200" ] || fail "priced model: expected 200, got $CODE"
[ "$((AFTER - BEFORE))" = "1" ] || fail "expected exactly 1 upstream call"
python3 -c "
d = $S1 - $S0
assert abs(d - 0.030) < 1e-6, f'expected \$0.030 recorded, got \${d}'" || fail "spend not recorded correctly"

# ── Caller-driven fan-out ────────────────────────────────────────────
echo "==> assert: a caller cannot select a fan-out routing pattern"
BEFORE=$(calls)
CODE=$(post "$KEY_A" '{"model":"priced-model","routing":"self_consistency","n_samples":20,"messages":[{"role":"user","content":"y"}]}')
AFTER=$(calls)
[ "$CODE" = "403" ] || fail "caller fan-out: expected 403, got $CODE"
[ "$((AFTER - BEFORE))" = "0" ] || fail "refused fan-out still called the provider"

echo "==> assert: routing=\"auto\" cannot launder a caller into a fan-out pattern"
# `auto` is not itself a fan-out, so a check on the *requested* routing waves it
# through — but decide() resolves it to cascade_test when the caller supplies a
# code prompt plus test_code/entry_point. Enforcement must run post-resolution.
BEFORE=$(calls)
CODE=$(post "$KEY_A" '{"model":"auto","routing":"auto","test_code":"assert f(1) == 1","entry_point":"f","messages":[{"role":"user","content":"implement a python function that adds one"}]}')
AFTER=$(calls)
[ "$CODE" = "403" ] || fail "auto resolved into a fan-out for a caller: expected 403, got $CODE"
[ "$((AFTER - BEFORE))" = "0" ] || fail "auto fan-out reached the provider $((AFTER - BEFORE)) times"

# ── Cross-key cache isolation ────────────────────────────────────────
echo "==> assert: one key's cached response is never served to another key"
post "$KEY_A" '{"model":"priced-model","temperature":0,"messages":[{"role":"user","content":"CONFIDENTIAL"}]}' >/dev/null
BEFORE=$(calls)
post "$KEY_B" '{"model":"priced-model","temperature":0,"messages":[{"role":"user","content":"CONFIDENTIAL"}]}' >/dev/null
AFTER=$(calls)
[ "$((AFTER - BEFORE))" = "1" ] || fail "key B was served key A's cached response"

echo "==> assert: a key still hits its own cache"
BEFORE=$(calls)
post "$KEY_A" '{"model":"priced-model","temperature":0,"messages":[{"role":"user","content":"CONFIDENTIAL"}]}' >/dev/null
AFTER=$(calls)
[ "$((AFTER - BEFORE))" = "0" ] || fail "scoping broke the cache for its own scope"
grep -q '"stoke_cache":"hit"' "$WORK_DIR/resp.json" || fail "expected a cache hit marker"

stop_stoke

# ── Clamped fan-out, billed in full ──────────────────────────────────
echo "==> assert: an allowed fan-out is clamped, and every call it makes is billed"
write_config '
[limits]
max_n_samples = 3
allow_caller_routing = true'
start_stoke

BEFORE=$(calls); S0=$(spend "$KEY_A")
CODE=$(post "$KEY_A" '{"model":"priced-model","routing":"self_consistency","n_samples":20,"messages":[{"role":"user","content":"z"}]}')
AFTER=$(calls); S1=$(spend "$KEY_A")
N=$((AFTER - BEFORE))
[ "$CODE" = "200" ] || fail "allowed fan-out: expected 200, got $CODE"
[ "$N" = "3" ] || fail "n_samples=20 should clamp to 3, made $N calls"
python3 -c "
d = $S1 - $S0
want = 3 * 0.030
assert abs(d - want) < 1e-6, f'fan-out under-billed: recorded \${d}, spent \${want}'" \
  || fail "fan-out spend was not billed in full"

stop_stoke
echo
echo "SPEND FIREWALL SMOKE PASSED ✔ (pricing gate incl. streams, boot validation, fan-out clamp, full billing, cache isolation)"
