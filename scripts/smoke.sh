#!/usr/bin/env bash
# End-to-end smoke test for node-aware routing. No Ollama required.
#
# Spins two mock Ollama nodes (one warm, one cold), starts Stoke against them,
# and asserts: discovery, warm-first placement, and failover when the warm
# node dies. Cleans up after itself. Exit 0 = all green.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d)"
STOKE_PORT=18787
COLD_PORT=21001
WARM_PORT=21002
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

echo "==> building stoke"
cargo build --quiet --manifest-path "$REPO_DIR/Cargo.toml"

echo "==> starting mock nodes (cold:$COLD_PORT, warm:$WARM_PORT)"
python3 "$REPO_DIR/scripts/mock_ollama.py" "$COLD_PORT" node-cold 0 &
PIDS+=($!)
python3 "$REPO_DIR/scripts/mock_ollama.py" "$WARM_PORT" node-warm 1 &
WARM_PID=$!
PIDS+=($WARM_PID)

cat > "$WORK_DIR/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $STOKE_PORT

routing = "single"
default_model = "testmodel:latest"

[[providers]]
name = "node-cold"
base_url = "http://127.0.0.1:$COLD_PORT/v1"
tier = "local"

[[providers]]
name = "node-warm"
base_url = "http://127.0.0.1:$WARM_PORT/v1"
tier = "remote"

[auto_route]
hedge = true
EOF

echo "==> starting stoke on :$STOKE_PORT"
(cd "$WORK_DIR" && exec env STOKE_DEV=1 STOKE_NODE_POLL_SECS=1 \
  "$REPO_DIR/target/debug/stoke" > "$WORK_DIR/stoke.log" 2>&1) &
PIDS+=($!)

for _ in $(seq 1 20); do
  curl -s --max-time 1 "http://127.0.0.1:$STOKE_PORT/health" >/dev/null && break
  sleep 0.5
done
sleep 2 # let the first poll cycle complete

fail() { echo "FAIL: $1"; echo "--- stoke.log ---"; tail -20 "$WORK_DIR/stoke.log"; exit 1; }

echo "==> assert: both nodes discovered, warm state seen"
NODES=$(curl -s "http://127.0.0.1:$STOKE_PORT/v1/nodes")
echo "$NODES" | python3 -c "
import json, sys
nodes = {n['name']: n for n in json.load(sys.stdin)['nodes']}
assert nodes['node-cold']['polled'] and nodes['node-cold']['healthy'], 'cold node not polled/healthy'
assert 'testmodel:latest' in nodes['node-cold']['models'], 'cold node missing model'
assert 'testmodel:latest' in nodes['node-warm']['warm'], 'warm node not warm'
" || fail "node discovery"

echo "==> assert: placement prefers the warm node"
RESP=$(curl -s -X POST "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"testmodel:latest","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert r['stoke_route']['node'] == 'node-warm', f\"routed to {r['stoke_route']['node']}\"
assert 'node-warm' in r['choices'][0]['message']['content']
" || fail "warm placement"

echo "==> assert: /api/show metadata lands in the registry (context + tools)"
sleep 2 # extra poll cycle for lazy meta fetch
curl -s "http://127.0.0.1:$STOKE_PORT/v1/nodes" | python3 -c "
import json, sys
nodes = {n['name']: n for n in json.load(sys.stdin)['nodes']}
mm = nodes['node-warm'].get('models_meta', {})
info = mm.get('testmodel:latest', {})
assert info.get('context_length') == 32768, f'context_length missing: {mm}'
assert info.get('tools') is True, f'tools capability missing: {mm}'
" || fail "model metadata discovery"

echo "==> assert: hedged dispatch races two zero-marginal nodes"
curl -sN --max-time 30 -X POST "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"testmodel:latest","stream":true,"messages":[{"role":"user","content":"hedge me"}]}' \
  > "$WORK_DIR/hedge.out"
grep -q "tok0" "$WORK_DIR/hedge.out" || fail "hedged stream returned no content"
grep -q "hedging testmodel" "$WORK_DIR/stoke.log" || fail "hedge path not taken (log missing 'hedging')"
grep -q "hedged dispatch: .* won" "$WORK_DIR/stoke.log" || fail "hedge winner not logged"

echo "==> assert: streaming holds the in-flight count for the stream's lifetime"
curl -sN --max-time 30 -X POST "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"testmodel:latest","stream":true,"messages":[{"role":"user","content":"hi"}]}' \
  > "$WORK_DIR/stream.out" &
STREAM_PID=$!
sleep 1  # mock stream runs ~2s; sample the registry mid-stream
# hedge=true means either node may be serving — assert the TOTAL in-flight
curl -s "http://127.0.0.1:$STOKE_PORT/v1/nodes" | python3 -c "
import json, sys
total = sum(n['inflight'] for n in json.load(sys.stdin)['nodes'])
assert total >= 1, f'mid-stream total inflight={total}, want >=1'
" || fail "streaming inflight (mid-stream)"
wait "$STREAM_PID"
grep -q "tok0" "$WORK_DIR/stream.out" || fail "stream content missing"
sleep 0.5
curl -s "http://127.0.0.1:$STOKE_PORT/v1/nodes" | python3 -c "
import json, sys
total = sum(n['inflight'] for n in json.load(sys.stdin)['nodes'])
assert total == 0, f'post-stream total inflight={total}, want 0'
" || fail "streaming inflight (released after stream)"

echo "==> assert: failover to cold node when warm node dies"
kill "$WARM_PID" 2>/dev/null
sleep 0.5
RESP=$(curl -s -X POST "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"testmodel:latest","messages":[{"role":"user","content":"hi again"}]}')
echo "$RESP" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert r['stoke_route']['node'] == 'node-cold', f\"routed to {r['stoke_route']['node']}\"
" || fail "failover"

echo "==> assert: dead node marked unhealthy after next poll"
sleep 2
curl -s "http://127.0.0.1:$STOKE_PORT/v1/nodes" | python3 -c "
import json, sys
nodes = {n['name']: n for n in json.load(sys.stdin)['nodes']}
assert not nodes['node-warm']['healthy'], 'dead node still marked healthy'
" || fail "health marking"

echo "SMOKE TEST PASSED ✔ (discovery, warm placement, failover, health)"
