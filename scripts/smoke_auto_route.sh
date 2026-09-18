#!/usr/bin/env bash
# Deterministic end-to-end proof for auto, auto-cheap, and auto-fast.
# All providers are stdlib mocks; no model download, network, or credentials.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d)"
STOKE_PORT=${STOKE_PORT:-18789}
PIDS=()

fail() { echo "FAIL: $1"; tail -30 "$WORK_DIR/stoke.log" 2>/dev/null || true; tail -20 "$WORK_DIR/mock.log" 2>/dev/null || true; exit 1; }
cleanup() {
  trap - EXIT
  for pid in "${PIDS[@]:-}"; do kill -TERM "$pid" 2>/dev/null || true; done
  for pid in "${PIDS[@]:-}"; do wait "$pid" 2>/dev/null || true; done
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT
wait_ready() {
  local url=$1
  for _ in $(seq 1 40); do
    if curl -fsS --max-time 1 "$url" >/dev/null 2>&1; then return 0; fi
    sleep 0.25
  done
  return 1
}

cat > "$WORK_DIR/mock.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn
port, name, model, warm, context = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4] == '1', int(sys.argv[5])
class H(BaseHTTPRequestHandler):
    def send_json(self, obj):
        b = json.dumps(obj).encode(); self.send_response(200)
        self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b)))
        self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path == '/api/tags': self.send_json({'models': [{'name': model}]})
        elif self.path == '/api/ps': self.send_json({'models': [{'name': model}] if warm else []})
        elif self.path == '/v1/models': self.send_json({'data': [{'id': model}]})
        elif self.path == '/health': self.send_json({'ok': True})
        else: self.send_response(404); self.end_headers()
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0)); raw = self.rfile.read(n)
        if self.path == '/api/show':
            self.send_json({'model_info': {'testarch.context_length': context}, 'capabilities': ['completion', 'tools']}); return
        if self.path == '/v1/chat/completions':
            req = json.loads(raw)
            self.send_json({'id': 'auto-smoke', 'object': 'chat.completion', 'created': 0, 'model': model,
              'choices': [{'index': 0, 'finish_reason': 'stop', 'message': {'role': 'assistant', 'content': 'served by ' + name}}],
              'usage': {'prompt_tokens': 4, 'completion_tokens': 3, 'total_tokens': 7}}); return
        self.send_response(404); self.end_headers()
    def log_message(self, *args): pass
class S(ThreadingMixIn, HTTPServer): daemon_threads = True
S(('127.0.0.1', port), H).serve_forever()
PY

# Ports are independently overridable so repeated runs cannot collide.
LOCAL_PORT=${LOCAL_PORT:-21021}; PAID_PORT=${PAID_PORT:-21022}; SLOW_PORT=${SLOW_PORT:-21023}
FAST_PORT=${FAST_PORT:-21024}; TINY_PORT=${TINY_PORT:-21025}; FIT_PORT=${FIT_PORT:-21026}
start_mock() { python3 "$WORK_DIR/mock.py" "$1" "$2" "$3" "$4" "$5" >>"$WORK_DIR/mock.log" 2>&1 & PIDS+=("$!"); }
start_mock "$LOCAL_PORT" local cheap-local 0 32768
start_mock "$PAID_PORT" paid paid-cloud 1 32768
start_mock "$SLOW_PORT" slow slow-model 0 32768
start_mock "$FAST_PORT" fast fast-model 1 32768
start_mock "$TINY_PORT" tiny tiny-model 1 100
start_mock "$FIT_PORT" fit fitting-model 0 32768
for port in "$LOCAL_PORT" "$PAID_PORT" "$SLOW_PORT" "$FAST_PORT" "$TINY_PORT" "$FIT_PORT"; do
  wait_ready "http://127.0.0.1:$port/health" || fail "mock readiness on $port"
done

cat > "$WORK_DIR/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $STOKE_PORT
routing = "single"

[[providers]]
name = "local"
base_url = "http://127.0.0.1:$LOCAL_PORT/v1"
tier = "local"
[[providers]]
name = "paid"
base_url = "http://127.0.0.1:$PAID_PORT/v1"
tier = "remote"
[[providers]]
name = "slow"
base_url = "http://127.0.0.1:$SLOW_PORT/v1"
tier = "local"
[[providers]]
name = "fast"
base_url = "http://127.0.0.1:$FAST_PORT/v1"
tier = "local"
[[providers]]
name = "tiny"
base_url = "http://127.0.0.1:$TINY_PORT/v1"
tier = "local"
[[providers]]
name = "fit"
base_url = "http://127.0.0.1:$FIT_PORT/v1"
tier = "local"

[auto_route]
fast = ["paid-cloud", "cheap-local"]
coder = ["tiny-model", "fitting-model"]
quality = "paid-cloud"

[pricing.models.paid-cloud]
input_per_1m = 10.0
output_per_1m = 30.0
EOF

(cd "$WORK_DIR" && exec env STOKE_DEV=1 STOKE_NODE_POLL_SECS=1 STOKE_LEDGER_PATH="$WORK_DIR/ledger.db" \
  "$REPO_DIR/target/debug/stoke" > "$WORK_DIR/stoke.log" 2>&1) & PIDS+=("$!")
wait_ready "http://127.0.0.1:$STOKE_PORT/health" || fail "stoke readiness"
sleep 3

post() {
  curl -fsS -X POST "http://127.0.0.1:$STOKE_PORT/v1/chat/completions" \
    -H 'Content-Type: application/json' -d "$1"
}
check_auto() {
  local mode=$1 expected=$2 body
  body=$(post "{\"model\":\"$mode\",\"routing\":\"auto\",\"max_tokens\":16,\"messages\":[{\"role\":\"user\",\"content\":\"hello for $mode\"}]}") || fail "$mode request"
  MODE="$mode" EXPECTED="$expected" BODY="$body" python3 - <<'PY' || fail "$mode receipt/dispatch"
import json, os
r=json.loads(os.environ['BODY']); a=r['stoke_route']['auto']
assert a['mode'] == os.environ['MODE']
assert a['chosen']['model'] == os.environ['EXPECTED'], a
assert a['chosen']['node'] in r['choices'][0]['message']['content'], r
assert 'not_taken' in a and isinstance(a['not_taken'], list), a
assert 'counterfactual_usd_est' in a and a['counterfactual_usd_est'] > 0, a
PY
}

echo "==> assert: auto balanced dispatches eligible cheap candidate with receipt"
check_auto auto paid-cloud
echo "==> assert: auto-cheap dispatches eligible cheap candidate"
check_auto auto-cheap cheap-local
echo "==> assert: auto-fast dispatches fastest warm candidate"
check_auto auto-fast paid-cloud

echo "==> assert: fit exclusion is retained and fallback dispatches"
LONG_PROMPT="implement a python function $(printf 'x%.0s' {1..600})"
BODY=$(post "{\"model\":\"auto\",\"routing\":\"auto\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"$LONG_PROMPT\"}],\"tools\":[{\"type\":\"function\",\"function\":{\"name\":\"fixture\"}}]}") || fail "fit request"
BODY="$BODY" python3 - <<'PY' || fail "fit exclusion receipt/dispatch"
import json, os
r=json.loads(os.environ['BODY']); a=r['stoke_route']['auto']
assert a['chosen']['model'] == 'fitting-model', a
excluded=[x for x in a['not_taken'] if x['model']=='tiny-model']
assert excluded and 'context window' in excluded[0]['excluded'], a
assert 'fit' in r['choices'][0]['message']['content'], r
PY

echo "AUTO-ROUTE SMOKE TEST PASSED (auto, auto-cheap, auto-fast, receipts, counterfactual, fit exclusion, dispatch)"
