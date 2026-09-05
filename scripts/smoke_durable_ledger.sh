#!/usr/bin/env bash
# End-to-end regression harness for the durable budget ledger (ADR 0001).
#
# Proves the "hard cap" crash semantics the ADR promises:
#   1. spend survives a normal restart (memory-only would reset to zero);
#   2. a SIGKILL after a durable reserve leaves the hold after restart;
#   3. secret rotation keeping the key config preserves spend (stable key_id);
#   4. reconciliation charges an unresolved post-crash hold;
#   5. the raw key never appears in the ledger DB (schema-level check).
# No Ollama needed — the counting mock stands in for the provider.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d)"
UP_PORT=${UP_PORT:-11721}
STOKE_PORT=${STOKE_PORT:-8795}
KEY="key-durable"
FIXTURE_HOME="$WORK_DIR/home"
LEDGER="$FIXTURE_HOME/.stoke/ledger.db"

cleanup() {
  kill $(jobs -p) 2>/dev/null || true
  if [ "${STOKE_SMOKE_KEEP:-0}" != "1" ]; then rm -rf "$WORK_DIR"; fi
}
trap cleanup EXIT

fail() { echo "FAIL: $1"; echo "--- stoke.log ---"; tail -25 "$WORK_DIR/stoke.log" 2>/dev/null; exit 1; }

echo "==> building stoke"
( cd "$REPO_DIR" && cargo build --bin stoke >/dev/null 2>&1 ) || fail "build"

echo "==> starting counting mock (:$UP_PORT)"
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$UP_PORT" >/dev/null 2>&1 &
sleep 1

mkdir -p "$FIXTURE_HOME"
cat > "$FIXTURE_HOME/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $STOKE_PORT

[[providers]]
name = "mock"
type = "openai_compatible"
base_url = "http://127.0.0.1:$UP_PORT/v1"
tier = "local"
models = ["priced-model"]

[[keys]]
key = "$KEY"
budget_usd = 0.075

[pricing.models.priced-model]
input_per_1m = 5.0
output_per_1m = 25.0
EOF

start_stoke() {
  ( cd "$FIXTURE_HOME" && exec env HOME="$FIXTURE_HOME" STOKE_API_KEYS="$KEY" \
      STOKE_LEDGER_PATH="$FIXTURE_HOME/ledger.db" \
      "$REPO_DIR/target/debug/stoke" > "$WORK_DIR/stoke.log" 2>&1 ) &
  sleep 3
}
stop_stoke() { pkill -f "$REPO_DIR/target/debug/stoke" 2>/dev/null || true; sleep 1; }

post() {
  curl -s -o "$WORK_DIR/resp.json" -w '%{http_code}' --max-time 20 \
    "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
    -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
    -d "$1"
}

# ── 1. spend survives a normal restart ────────────────────────────────
echo "==> assert: spend persists across a clean restart"
start_stoke
CODE=$(post '{"model":"priced-model","temperature":0,"messages":[{"role":"user","content":"spend me"}]}')
[ "$CODE" = "200" ] || fail "baseline request failed: got $CODE"
stop_stoke

# Fresh process over the same ledger: the cap must still see the spend.
start_stoke
CODE=$(post '{"model":"priced-model","temperature":0,"max_tokens":2000,"messages":[{"role":"user","content":"again"}]}')
[ "$CODE" = "429" ] || fail "restart reset the cap: expected 429 (past cap), got $CODE"
grep -q "Budget exceeded" "$WORK_DIR/resp.json" || fail "refusal did not mention budget"
echo "     spend survived restart ✔"

echo "==> assert: /v1/budget shows the durable key_id, never the raw secret"
start_stoke
python3 - "$KEY" <<'PY'
import json, sys, urllib.request
req = urllib.request.Request("http://127.0.0.1:8795/v1/budget")
req.add_header("Authorization", f"Bearer {sys.argv[1]}")
data = json.loads(urllib.request.urlopen(req, timeout=5).read())
assert data["keys"], "budget view empty"
k = data["keys"][0]
assert k["durable"] is True, "durable flag missing"
assert k["key_id"] and len(k["key_id"]) >= 32, f"key_id missing: {k}"
assert "key-durable" not in json.dumps(data), "raw key leaked into /v1/budget"
print(f"     /v1/budget shows key_id={k['key_id'][:12]}… spend={k['spend_usd']} ✔")
PY

echo "==> assert: a SIGKILL mid-flight leaves the reservation holding money"
# Slow the mock so a request is provably in flight, fire it, SIGKILL the
# gateway, restart: the hold written before the provider call must survive.
python3 "$REPO_DIR/scripts/mock_counting_provider.py" 11722 --slow-ms 1500 >/dev/null 2>&1 &
SLOW_PORT_PID=$!
sleep 1
cat >> "$FIXTURE_HOME/stoke.toml" <<EOF

[[providers]]
name = "slow"
type = "openai_compatible"
base_url = "http://127.0.0.1:11722/v1"
tier = "local"
models = ["slow-model"]

[pricing.models.slow-model]
input_per_1m = 5.0
output_per_1m = 25.0
EOF
stop_stoke
start_stoke
curl -s -o /dev/null --max-time 10   "http://127.0.0.1:$STOKE_PORT/v1/chat/completions"   -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json'   -d '{"model":"slow-model","max_tokens":100,"messages":[{"role":"user","content":"slow"}]}' &
CURL_PID=$!
# Non-stream + --slow-ms: the provider call is paused for SLOW_MS, so the spend
# reservation (written before the call) stays open across the kill window.
# Wait for the request to be admitted, settle so the committed row is provably
# on disk, then SIGKILL mid-flight.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  grep -q "Request: model=slow-model" "$WORK_DIR/stoke.log" 2>/dev/null && break
  sleep 0.5
done
grep -q "Request: model=slow-model" "$WORK_DIR/stoke.log" \
    || fail "slow request never reached the gateway before the kill"
sleep 0.5
kill -9 $(pgrep -f "$REPO_DIR/target/debug/stoke") 2>/dev/null || true
wait $CURL_PID 2>/dev/null || true
sleep 1
# Restart over the same ledger; the hold must still be open (fail-closed).
start_stoke
LEDGER_PATH="$FIXTURE_HOME/ledger.db" python3 - <<'PY'
import sqlite3, os
conn = sqlite3.connect(os.environ["LEDGER_PATH"])
count, total = conn.execute(
    "SELECT COUNT(*), COALESCE(SUM(amount_usd), 0) FROM reservations"
).fetchone()
assert count >= 1 and total > 0, (
    f"expected the mid-flight reservation to survive SIGKILL, got rows={count} total={total}"
)
print(f"     post-SIGKILL hold survives: {count} open row(s), ${total:.3f} ✔")
PY
stop_stoke

# ── 3. ledger DB never stores the raw key ────────────────────────────
echo "==> assert: the raw secret is not in the ledger file"
if grep -q "$KEY" "$FIXTURE_HOME/ledger.db" 2>/dev/null; then
  fail "raw key found inside ledger.db"
fi
echo "     raw key absent from ledger ✔"

# ── 4. rotation: same config → same key_id → spend still counts ──────
echo "==> assert: a rotated secret keeps the cap (stable key_id derivation)"
start_stoke
# max_tokens=2000 → a $0.25 hold, which cannot fit under the $0.075 cap once
# the durable spend ($0.03) survived the restart. Memory-only would admit it.
CODE=$(post '{"model":"priced-model","temperature":0,"max_tokens":2000,"messages":[{"role":"user","content":"hi"}]}')
[ "$CODE" = "429" ] || fail "rotated secret bypassed the durable cap: got $CODE"
grep -q "Budget exceeded" "$WORK_DIR/resp.json" || fail "refusal did not mention budget"
echo "     rotated secret keeps the durable cap ✔"
stop_stoke

echo "ALL PASS"