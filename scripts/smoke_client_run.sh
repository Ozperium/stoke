#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BIN=${STOKE_BIN:-$ROOT/target/debug/stoke}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/bin"

cat > "$TMP/bin/claude" <<'EOF'
#!/bin/sh
{
    printf 'program=claude\n'
    printf 'base=%s\n' "${ANTHROPIC_BASE_URL-}"
    printf 'token=%s\n' "${ANTHROPIC_AUTH_TOKEN-}"
    printf 'api_key=%s\n' "${ANTHROPIC_API_KEY-unset}"
    for arg in "$@"; do printf 'arg=%s\n' "$arg"; done
} > "$CAPTURE_FILE"
EOF

cat > "$TMP/bin/codex" <<'EOF'
#!/bin/sh
{
    printf 'program=codex\n'
    printf 'stoke_key=%s\n' "${STOKE_API_KEY-}"
    for arg in "$@"; do printf 'arg=%s\n' "$arg"; done
} > "$CAPTURE_FILE"
EOF
chmod +x "$TMP/bin/claude" "$TMP/bin/codex"

PATH="$TMP/bin:$PATH" \
CAPTURE_FILE="$TMP/claude.txt" \
STOKE_URL='http://127.0.0.1:9999/v1/' \
STOKE_API_KEY='client-key' \
ANTHROPIC_API_KEY='must-not-leak' \
"$BIN" run claude -- --print hello

python3 - "$TMP/claude.txt" <<'PY'
import sys
lines = open(sys.argv[1], encoding="utf-8").read().splitlines()
assert "program=claude" in lines, lines
assert "base=http://127.0.0.1:9999" in lines, lines
assert "token=client-key" in lines, lines
assert "api_key=unset" in lines, lines
assert lines[-2:] == ["arg=--print", "arg=hello"], lines
PY

PATH="$TMP/bin:$PATH" \
CAPTURE_FILE="$TMP/codex.txt" \
STOKE_URL='http://127.0.0.1:9999' \
STOKE_API_KEY='client-key' \
"$BIN" run codex -- exec hello

python3 - "$TMP/codex.txt" <<'PY'
import sys
lines = open(sys.argv[1], encoding="utf-8").read().splitlines()
assert "program=codex" in lines, lines
assert "stoke_key=client-key" in lines, lines
assert 'arg=model_provider="stoke"' in lines, lines
assert 'arg=model_providers.stoke.base_url="http://127.0.0.1:9999/v1"' in lines, lines
assert 'arg=model_providers.stoke.env_key="STOKE_API_KEY"' in lines, lines
assert 'arg=model_providers.stoke.wire_api="responses"' in lines, lines
assert "arg=model_providers.stoke.supports_websockets=false" in lines, lines
assert lines[-2:] == ["arg=exec", "arg=hello"], lines
assert not any("client-key" in line for line in lines if line.startswith("arg=")), lines
PY

printf 'client launcher smoke: ok\n'
