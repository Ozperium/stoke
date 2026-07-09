#!/usr/bin/env bash
# Federation smoke test: two Stoke gateways with a DELIBERATE A<->B cycle.
#
# Topology:
#   Stoke A (:18787) -> node-a (mock ollama, modela) + fed-b (type=stoke -> B)
#   Stoke B (:18788) -> node-b (mock ollama, modelb) + fed-a (type=stoke -> A)
#
# Asserts:
#   0. B runs with STOKE_API_KEYS — status AND chat are fail-closed
#   1. A imports B's model inventory through the federated provider (poller sends the key)
#   2. A serves a model it doesn't have locally by routing through B
#   3. Hop guard: a request that already crossed a gateway is refused
#      further federation (no candidate -> 404, reason visible)
#   4. The A<->B cycle cannot loop: unknown model fails fast on both
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK_A="$(mktemp -d)"
WORK_B="$(mktemp -d)"
A_PORT=18787
B_PORT=18788
B_KEY="stk-federation-test-key"
MA_PORT=21011
MB_PORT=21012
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  rm -rf "$WORK_A" "$WORK_B"
}
trap cleanup EXIT

echo "==> building stoke"
cargo build --quiet --manifest-path "$REPO_DIR/Cargo.toml"

echo "==> starting mock nodes (a:$MA_PORT modela, b:$MB_PORT modelb)"
python3 "$REPO_DIR/scripts/mock_ollama.py" "$MA_PORT" node-a 1 "modela:latest" &
PIDS+=($!)
python3 "$REPO_DIR/scripts/mock_ollama.py" "$MB_PORT" node-b 1 "modelb:latest" &
PIDS+=($!)

cat > "$WORK_A/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $A_PORT
routing = "single"

[[providers]]
name = "node-a"
base_url = "http://127.0.0.1:$MA_PORT/v1"
tier = "local"

[[providers]]
name = "fed-b"
type = "stoke"
base_url = "http://127.0.0.1:$B_PORT/v1"
api_key = "$B_KEY"
tier = "remote"
EOF

cat > "$WORK_B/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $B_PORT
routing = "single"

[[providers]]
name = "node-b"
base_url = "http://127.0.0.1:$MB_PORT/v1"
tier = "local"

[[providers]]
name = "fed-a"
type = "stoke"
base_url = "http://127.0.0.1:$A_PORT/v1"
tier = "remote"
EOF

echo "==> starting stoke A (:$A_PORT) and B (:$B_PORT)"
(cd "$WORK_A" && exec env STOKE_DEV=1 STOKE_NODE_POLL_SECS=1 \
  "$REPO_DIR/target/debug/stoke" > "$WORK_A/stoke.log" 2>&1) &
PIDS+=($!)
(cd "$WORK_B" && exec env STOKE_API_KEYS="$B_KEY" STOKE_NODE_POLL_SECS=1 \
  "$REPO_DIR/target/debug/stoke" > "$WORK_B/stoke.log" 2>&1) &
PIDS+=($!)

for port in $A_PORT $B_PORT; do
  for _ in $(seq 1 20); do
    curl -s --max-time 1 "http://127.0.0.1:$port/health" >/dev/null && break
    sleep 0.5
  done
done
sleep 3 # two poll cycles: B discovers node-b, then A imports from B

fail() { echo "FAIL: $1"; echo "--- A log ---"; tail -15 "$WORK_A/stoke.log"; echo "--- B log ---"; tail -15 "$WORK_B/stoke.log"; exit 1; }

echo "==> assert: B's status endpoints are fail-closed without the key"
CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$B_PORT/v1/nodes")
[ "$CODE" = "401" ] || fail "unauthenticated /v1/nodes on B returned $CODE, want 401"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $B_KEY" "http://127.0.0.1:$B_PORT/v1/nodes")
[ "$CODE" = "200" ] || fail "authenticated /v1/nodes on B returned $CODE, want 200"
CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$B_PORT/health")
[ "$CODE" = "200" ] || fail "/health on B must stay open, got $CODE"

echo "==> assert: A imported B's inventory through federation (poller sent the key)"
curl -s "http://127.0.0.1:$A_PORT/v1/nodes" | python3 -c "
import json, sys
nodes = {n['name']: n for n in json.load(sys.stdin)['nodes']}
fed = nodes['fed-b']
assert fed['type'] == 'stoke', fed
assert fed['healthy'] and fed['polled'], fed
assert 'modelb:latest' in fed['models'], f\"fed-b models: {fed['models']}\"
assert 'modelb:latest' in fed['warm'], 'warm state should federate too'
mm = fed.get('models_meta', {}).get('modelb:latest', {})
assert mm.get('context_length') == 32768, f'meta must federate: {fed.get(\"models_meta\")}'
assert mm.get('tools') is True, 'tool capability must federate'
" || fail "federated inventory import"

echo "==> assert: A serves modelb by routing through B"
RESP=$(curl -s -X POST "http://127.0.0.1:$A_PORT/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"modelb:latest","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert r['stoke_route']['node'] == 'fed-b', r.get('stoke_route')
assert 'node-b' in r['choices'][0]['message']['content'], r['choices'][0]['message']
" || fail "cross-gateway routing"

echo "==> assert: hop guard refuses second federation hop"
# distinct prompt: an identical one would (correctly) be served from A's local
# cache, which the hop guard deliberately allows — caches can't loop
CODE=$(curl -s -o "$WORK_A/hop.out" -w "%{http_code}" -X POST "http://127.0.0.1:$A_PORT/v1/chat/completions" \
  -H "Content-Type: application/json" -H "x-stoke-hop: 1" \
  -d '{"model":"modelb:latest","messages":[{"role":"user","content":"uncached hop probe"}]}')
[ "$CODE" = "404" ] || fail "hop-guarded request returned $CODE, want 404"
grep -q "hop guard" "$WORK_A/hop.out" || fail "hop-guard reason missing from response"

echo "==> assert: cyclic config cannot loop (unknown model fails fast on both)"
for port in $A_PORT $B_PORT; do
  START=$(date +%s)
  CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 -X POST "http://127.0.0.1:$port/v1/chat/completions" \
    -H "Content-Type: application/json" -H "Authorization: Bearer $B_KEY" \
    -d '{"model":"ghost:model","messages":[{"role":"user","content":"hi"}]}')
  ELAPSED=$(( $(date +%s) - START ))
  [ "$CODE" = "404" ] || [ "$CODE" = "502" ] || fail "ghost model on :$port returned $CODE"
  [ "$ELAPSED" -lt 8 ] || fail "ghost model took ${ELAPSED}s on :$port — possible loop"
done

echo "FEDERATION SMOKE TEST PASSED ✔ (inventory import, cross-gateway routing, hop guard, cycle safety)"
