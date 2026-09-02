#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BIN=${STOKE_BIN:-$ROOT/target/debug/stoke}
TMP=$(mktemp -d)
PROVIDER_PORT=$((21000 + $$ % 10000))
STOKE_PORT=$((PROVIDER_PORT + 1))
PROVIDER_PID=
STOKE_PID=

cleanup() {
    [ -z "$STOKE_PID" ] || kill "$STOKE_PID" 2>/dev/null || true
    [ -z "$PROVIDER_PID" ] || kill "$PROVIDER_PID" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

cat > "$TMP/stoke.toml" <<EOF
[server]
host = "127.0.0.1"
port = $STOKE_PORT

routing = "single"
default_model = "gpt-test"

[[providers]]
name = "openai-test"
type = "openai"
base_url = "http://127.0.0.1:$PROVIDER_PORT"
api_key_env = "OPENAI_API_KEY"
models = ["gpt-test"]
tier = "cloud"

[pricing.models."gpt-test"]
input_per_1m = 1.0
output_per_1m = 2.0

[[keys]]
key = "test-key"
budget_usd = 1.0
EOF

CAPTURE_FILE="$TMP/capture.json" python3 "$ROOT/scripts/mock_responses_provider.py" "$PROVIDER_PORT" &
PROVIDER_PID=$!
attempt=0
until curl -fsS "http://127.0.0.1:$PROVIDER_PORT/health" >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 50 ] || exit 1
    sleep 0.1
done
(
    cd "$TMP"
    STOKE_API_KEYS=test-key OPENAI_API_KEY=upstream-key "$BIN" >"$TMP/stoke.log" 2>&1
) &
STOKE_PID=$!

attempt=0
until curl -fsS "http://127.0.0.1:$STOKE_PORT/health" >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
        cat "$TMP/stoke.log" >&2
        exit 1
    fi
    sleep 0.1
done

curl -fsS \
    -H 'authorization: Bearer test-key' \
    -H 'content-type: application/json' \
    -H 'openai-beta: responses=experimental' \
    -H 'originator: codex_cli_rs' \
    -d '{"model":"gpt-test","input":"hello","stream":false}' \
    "http://127.0.0.1:$STOKE_PORT/v1/responses" > "$TMP/response.json"

python3 - "$TMP/response.json" "$TMP/capture.json" <<'PY'
import json
import sys

response = json.load(open(sys.argv[1], encoding="utf-8"))
capture = json.load(open(sys.argv[2], encoding="utf-8"))
assert response["object"] == "response", response
assert response["usage"] == {"input_tokens": 13, "output_tokens": 8, "total_tokens": 21}, response
assert capture["path"] == "/v1/responses", capture
assert capture["authorization"] == "Bearer upstream-key", capture
assert capture["openai_beta"] == "responses=experimental", capture
assert capture["originator"] == "codex_cli_rs", capture
assert capture["body"]["input"] == "hello", capture
PY

curl -fsSN \
    -H 'authorization: Bearer test-key' \
    -H 'content-type: application/json' \
    -H 'openai-beta: responses=experimental' \
    -H 'originator: codex_cli_rs' \
    -d '{"model":"gpt-test","input":"stream me","stream":true}' \
    "http://127.0.0.1:$STOKE_PORT/v1/responses" > "$TMP/stream.txt"

python3 - "$TMP/stream.txt" "$TMP/capture.json" <<'PY'
import json
import sys

stream = open(sys.argv[1], encoding="utf-8").read()
capture = json.load(open(sys.argv[2], encoding="utf-8"))
assert "event: response.output_text.delta" in stream, stream
assert '"delta": "mock stream"' in stream, stream
assert "event: response.completed" in stream, stream
assert '"input_tokens": 13' in stream, stream
assert capture["body"]["stream"] is True, capture
assert capture["authorization"] == "Bearer upstream-key", capture
PY

curl -fsS \
    -H 'authorization: Bearer test-key' \
    "http://127.0.0.1:$STOKE_PORT/v1/budget" > "$TMP/budget.json"
python3 - "$TMP/budget.json" <<'PY'
import json
import sys

budget = json.load(open(sys.argv[1], encoding="utf-8"))
assert budget["keys"], budget
key = budget["keys"][0]
expected = 2 * (13 * 1.0 + 8 * 2.0) / 1_000_000
assert abs(key["spend_usd"] - expected) < 1e-9, (key, expected)
assert abs(key["reserved_usd"]) < 1e-9, key
PY

printf 'responses non-stream + stream smoke: ok\n'
