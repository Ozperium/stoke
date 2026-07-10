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
QUIET_PORT=${QUIET_PORT:-11702}
LOCAL_PORT=${LOCAL_PORT:-11703}
SLOW_PORT=${SLOW_PORT:-11704}
STOKE_PORT=${STOKE_PORT:-8791}
KEY_A="key-alpha"
KEY_B="key-beta"

cleanup() { kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK_DIR"; }
trap cleanup EXIT

fail() { echo "FAIL: $1"; echo "--- stoke.log ---"; tail -25 "$WORK_DIR/stoke.log" 2>/dev/null; exit 1; }

echo "==> building stoke"
( cd "$REPO_DIR" && cargo build --bin stoke >/dev/null 2>&1 ) || fail "build"

echo "==> starting counting mock providers (:$UP_PORT, :$QUIET_PORT no-usage, :$LOCAL_PORT free)"
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$UP_PORT" >/dev/null 2>&1 &
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$QUIET_PORT" --no-usage >/dev/null 2>&1 &
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$LOCAL_PORT" >/dev/null 2>&1 &
python3 "$REPO_DIR/scripts/mock_counting_provider.py" "$SLOW_PORT" --slow-ms 1200 >/dev/null 2>&1 &
sleep 1

calls() { curl -s "http://127.0.0.1:${1:-$UP_PORT}/count" | python3 -c 'import sys,json;print(json.load(sys.stdin)["calls"])'; }
stream_opts() { curl -s "http://127.0.0.1:${1}/count" | python3 -c 'import sys,json;print(json.load(sys.stdin)["stream_options_seen"])'; }
estimated() {
  curl -s "http://127.0.0.1:$STOKE_PORT/v1/budget" -H "Authorization: Bearer $1" \
    | python3 -c "
import sys, json
d = json.load(sys.stdin)
m = [k for k in d['keys'] if k['key'].startswith('$1'[:8])]
print(m[0]['estimated_usd'] if m else 0.0)"
}
stream() {
  curl -s -N -o /dev/null -w '%{http_code}' --max-time 20 \
    "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
    -H "Authorization: Bearer $1" -H 'Content-Type: application/json' \
    -d "{\"model\":\"$2\",\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"stream me\"}]}"
}
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

# Streams, but never reports token usage — Stoke must estimate rather than book $0.
[[providers]]
name = "hides-usage"
base_url = "http://127.0.0.1:$QUIET_PORT/v1"
tier = "cloud"
models = ["quiet-model"]

# Your own hardware: no per-token bill, and never asked to report usage.
[[providers]]
name = "own-box"
base_url = "http://127.0.0.1:$LOCAL_PORT/v1"
tier = "local"
models = ["local-model"]

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

[pricing.models.quiet-model]
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

# ── Streamed responses accrue spend ──────────────────────────────────
echo "==> assert: a streamed response is billed from the usage the provider reports"
BEFORE=$(calls); S0=$(spend "$KEY_A")
CODE=$(stream "$KEY_A" priced-model)
sleep 1
AFTER=$(calls); S1=$(spend "$KEY_A"); E1=$(estimated "$KEY_A")
[ "$CODE" = "200" ] || fail "streamed request: expected 200, got $CODE"
[ "$((AFTER - BEFORE))" = "1" ] || fail "expected exactly 1 upstream call"
[ "$(stream_opts "$UP_PORT")" != "0" ] || fail "Stoke never asked the metered provider to report usage"
python3 -c "
d = $S1 - $S0
assert abs(d - 0.030) < 1e-6, f'streamed spend not recorded: expected \$0.030, got \${d}'
assert $E1 == 0.0, f'a reported usage must not count as an estimate (got \${$E1})'" \
  || fail "streamed spend was not billed from the reported usage"

echo "==> assert: a metered provider that hides usage is billed from a flagged estimate"
S0=$(spend "$KEY_A"); E0=$(estimated "$KEY_A")
CODE=$(stream "$KEY_A" quiet-model)
sleep 1
S1=$(spend "$KEY_A"); E1=$(estimated "$KEY_A")
[ "$CODE" = "200" ] || fail "quiet stream: expected 200, got $CODE"
[ "$(stream_opts "$QUIET_PORT")" != "0" ] || fail "Stoke did not ask the quiet provider for usage"
python3 -c "
assert $S1 > $S0, 'a stream with no usage report booked \$0 — the bug this exists to prevent'
assert $E1 > $E0, 'estimated spend must be flagged as estimated'" \
  || fail "unreported streamed usage was not estimated and flagged"

echo "==> assert: a free-tier provider is never asked for usage, and is never billed"
S0=$(spend "$KEY_A")
CODE=$(stream "$KEY_A" local-model)
sleep 1
S1=$(spend "$KEY_A")
[ "$CODE" = "200" ] || fail "local stream: expected 200, got $CODE"
[ "$(stream_opts "$LOCAL_PORT")" = "0" ] || fail "a free-tier stream was modified with stream_options"
python3 -c "
assert abs($S1 - $S0) < 1e-12, 'your own hardware was billed'" || fail "free-tier stream accrued spend"

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

# ── A fan-out is held for every leg it will dispatch ────────────────
echo "==> assert: the hold covers the whole fan-out, not one leg of it"
write_config '
[limits]
max_n_samples = 5
allow_caller_routing = true'
python3 - "$WORK_DIR/stoke.toml" <<'PYEOF'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
# A cap that fits one call but not five.
s = s.replace('key = "key-alpha"\nbudget_usd = 100.0', 'key = "key-alpha"\nbudget_usd = 0.06', 1)
p.write_text(s)
PYEOF
start_stoke
BEFORE=$(calls)
CODE=$(post "$KEY_A" '{"model":"priced-model","routing":"self_consistency","n_samples":5,"max_tokens":1000,"messages":[{"role":"user","content":"fan out"}]}')
AFTER=$(calls)
[ "$CODE" = "429" ] || fail "a 5-call fan-out on a 1-call cap: expected 429, got $CODE"
[ "$((AFTER - BEFORE))" = "0" ] || fail "the refused fan-out still reached the provider $((AFTER - BEFORE)) times"
grep -q "for this request exceeds" "$WORK_DIR/resp.json" || fail "the refusal did not explain the hold"
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

# ── Streamed vote race: bill the model that served, and do not race a payer ──
echo "==> assert: a streamed vote race bills the model that served, and tries metered models in order"
write_config '
routing_override = true'
# operator pins the fan-out; the caller only supplies the candidate list
python3 - "$WORK_DIR/stoke.toml" <<'PYEOF'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
s = s.replace('routing = "single"', 'routing = "stream_race"', 1)
s = s.replace("\nrouting_override = true", "")
p.write_text(s)
PYEOF
start_stoke

BEFORE=$(calls); S0=$(spend "$KEY_A")
# The request names an UNPRICED model; the vote list names priced ones. Only the
# vote models are ever dispatched, so the bill must be priced against the winner.
CODE=$(curl -s -N -o /dev/null -w '%{http_code}' --max-time 20 \
  "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
  -H "Authorization: Bearer $KEY_A" -H 'Content-Type: application/json' \
  -d '{"model":"unpriced-model","stream":true,"vote_models":["priced-model","priced-model-2"],"messages":[{"role":"user","content":"race"}]}')
sleep 1
AFTER=$(calls); S1=$(spend "$KEY_A")
N=$((AFTER - BEFORE))
[ "$CODE" = "200" ] || fail "streamed vote race: expected 200, got $CODE"
[ "$N" = "1" ] || fail "a metered provider must not be raced: expected 1 upstream call, got $N"
python3 -c "
d = $S1 - $S0
assert abs(d - 0.030) < 1e-6, f'billed against the wrong model: expected \$0.030 for the model that served, got \${d}'" \
  || fail "the streamed vote race was priced against the requested model, not the served one"

stop_stoke

# ── Concurrent streams are held against the cap ──────────────────────
# A stream is charged when it ends. Without a hold on the money it is about to
# cost, every concurrent request on a nearly-exhausted key is admitted against a
# spend figure that ignores all the others still in flight.
echo "==> assert: concurrent streams cannot overshoot the cap while none has been charged"
cat > "$WORK_DIR/stoke.toml" <<EOF
routing = "single"
default_model = "slow-model"

[server]
host = "127.0.0.1"
port = $STOKE_PORT

[[providers]]
name = "slow-metered"
base_url = "http://127.0.0.1:$SLOW_PORT/v1"
tier = "cloud"
models = ["slow-model"]

[[keys]]
key = "$KEY_A"
budget_usd = 0.05

# max_tokens=1000 at \$25/1M output => each request holds ~\$0.025.
[pricing.models.slow-model]
input_per_1m = 5.0
output_per_1m = 25.0
EOF
start_stoke

BEFORE=$(calls "$SLOW_PORT")
PIDS=""; RESULTS="$WORK_DIR/codes"; : > "$RESULTS"
for i in 1 2 3; do
  ( curl -s -N -o /dev/null -w '%{http_code}\n' --max-time 25 \
      "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
      -H "Authorization: Bearer $KEY_A" -H 'Content-Type: application/json' \
      -d "{\"model\":\"slow-model\",\"stream\":true,\"max_tokens\":1000,\"messages\":[{\"role\":\"user\",\"content\":\"unique $i\"}]}" >> "$RESULTS" ) &
  PIDS="$PIDS $!"
  sleep 0.15
done
for pid in $PIDS; do wait "$pid"; done
sleep 1
AFTER=$(calls "$SLOW_PORT")
N=$((AFTER - BEFORE))
REFUSED=$(grep -c '^429$' "$RESULTS" || true)

[ "$REFUSED" -ge 1 ] || fail "three concurrent streams on a 2-stream cap: none was refused"
python3 -c "
import json, urllib.request
r = urllib.request.Request('http://127.0.0.1:$STOKE_PORT/v1/budget')
r.add_header('Authorization', 'Bearer $KEY_A')
k = json.load(urllib.request.urlopen(r, timeout=5))['keys'][0]
assert k['spend_usd'] <= k['limit_usd'] + 1e-9, f\"cap overshot: spent \${k['spend_usd']} against \${k['limit_usd']}\"
assert abs(k['reserved_usd']) < 1e-9, f\"a hold leaked: \${k['reserved_usd']} still reserved\"
" || fail "concurrent streams overshot the cap, or a hold leaked"
[ "$N" -le 2 ] || fail "expected at most 2 streams to reach the provider, got $N"

stop_stoke

# ── A cache hit needs no hold ────────────────────────────────────────
echo "==> assert: a free cache hit is served even when the cap could not fund a fresh call"
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
models = ["priced-model"]

[[keys]]
key = "$KEY_A"
budget_usd = 0.05

[pricing.models.priced-model]
input_per_1m = 5.0
output_per_1m = 25.0
EOF
start_stoke
BODY='{"model":"priced-model","temperature":0,"max_tokens":1000,"messages":[{"role":"user","content":"cache me"}]}'
CODE=$(post "$KEY_A" "$BODY")
[ "$CODE" = "200" ] || fail "priming the cache: expected 200, got $CODE"
# Spend is now \$0.030 of a \$0.05 cap; a fresh call would need a \$0.025 hold and
# could not fit. The identical request is a cache hit: no provider, no cost, no hold.
BEFORE=$(calls)
CODE=$(post "$KEY_A" "$BODY")
AFTER=$(calls)
[ "$CODE" = "200" ] || fail "a free cache hit was refused by the reservation gate (HTTP $CODE)"
[ "$((AFTER - BEFORE))" = "0" ] || fail "the cache hit reached the provider"
grep -q '"stoke_cache":"hit"' "$WORK_DIR/resp.json" || fail "expected a cache hit"
stop_stoke

# ── /v1/messages takes a hold too ────────────────────────────────────
echo "==> assert: the Anthropic path holds money against the cap before forwarding"
cat > "$WORK_DIR/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $STOKE_PORT

[[providers]]
name = "anthropic"
type = "anthropic"
base_url = "http://127.0.0.1:$UP_PORT"
tier = "cloud"

[[keys]]
key = "$KEY_A"
budget_usd = 0.02

# max_tokens=1000 => a \$0.025 hold, which does not fit under a \$0.02 cap.
[pricing.models.claude-fixture]
input_per_1m = 5.0
output_per_1m = 25.0
EOF
start_stoke
BEFORE=$(calls)
CODE=$(curl -s -o "$WORK_DIR/resp.json" -w '%{http_code}' --max-time 20 \
  "http://127.0.0.1:$STOKE_PORT/v1/messages" \
  -H "Authorization: Bearer $KEY_A" -H 'Content-Type: application/json' \
  -d '{"model":"claude-fixture","max_tokens":1000,"messages":[{"role":"user","content":"hi"}]}')
AFTER=$(calls)
[ "$CODE" = "429" ] || fail "/v1/messages over-cap request: expected 429, got $CODE"
[ "$((AFTER - BEFORE))" = "0" ] || fail "/v1/messages forwarded a request it could not fund"
grep -q "for this request exceeds" "$WORK_DIR/resp.json" || fail "/v1/messages refusal did not explain the hold"

echo "==> assert: ...and a request that fits is forwarded and billed"
BEFORE=$(calls)
CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 20 \
  "http://127.0.0.1:$STOKE_PORT/v1/messages" \
  -H "Authorization: Bearer $KEY_A" -H 'Content-Type: application/json' \
  -d '{"model":"claude-fixture","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}')
AFTER=$(calls)
[ "$CODE" = "200" ] || fail "/v1/messages within-cap request: expected 200, got $CODE"
[ "$((AFTER - BEFORE))" = "1" ] || fail "expected exactly one upstream call"
python3 -c "
import json, urllib.request
r = urllib.request.Request('http://127.0.0.1:$STOKE_PORT/v1/budget')
r.add_header('Authorization', 'Bearer $KEY_A')
k = json.load(urllib.request.urlopen(r, timeout=5))['keys'][0]
assert abs(k['spend_usd'] - 0.030) < 1e-6, f\"expected \$0.030 billed, got \${k['spend_usd']}\"
assert abs(k['reserved_usd']) < 1e-9, 'the hold leaked'
" || fail "/v1/messages did not bill correctly, or leaked its hold"

stop_stoke
echo
echo "SPEND FIREWALL SMOKE PASSED ✔ (pricing gate, streamed spend, in-flight holds, served-model billing, boot validation, fan-out clamp, full billing, cache isolation)"
