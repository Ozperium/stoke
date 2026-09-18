#!/bin/sh
# Stoke installer — fetches a prebuilt binary, falls back to building from source.
# Usage: curl -sSf https://stokegate.com/install | sh
#    or: curl -sSf https://stokegate.com/install | sh -s -- --dir /usr/local/bin
#    or: curl -sSf https://stokegate.com/install | sh -s -- --version nightly
#
# Strictly POSIX sh: no `echo -e`, no `pipefail`, no process substitution —
# this is piped into /bin/sh, which is dash on most Linux distributions.
set -eu

REPO="Ozperium/stoke"
INSTALL_DIR="${HOME}/.local/bin"
VERSION="latest"
ACTION="install"

if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); GREEN=$(printf '\033[0;32m')
  RED=$(printf '\033[0;31m'); YELLOW=$(printf '\033[1;33m'); NC=$(printf '\033[0m')
else
  BOLD=''; GREEN=''; RED=''; YELLOW=''; NC=''
fi
info()  { printf '%s✓%s %s\n' "$GREEN" "$NC" "$1"; }
warn()  { printf '%s⚠%s %s\n' "$YELLOW" "$NC" "$1"; }
error() { printf '%s✗%s %s\n' "$RED" "$NC" "$1"; exit 1; }

printf '%sStoke%s — the firewall for AI agent spend.\n' "$BOLD" "$NC"
printf 'Loop kill switch, rate limits, budget caps, local-first routing. One Rust binary.\n\n'

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) INSTALL_DIR="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --uninstall) ACTION="uninstall"; shift ;;
    --rollback) ACTION="rollback"; shift ;;
    --help|-h)
      printf 'Options:\n  --dir <path>      install location (default ~/.local/bin)\n'
      printf '  --version <tag>   release tag, or "nightly" for the rolling build (default latest)\n'
      printf '  --uninstall       remove Stoke binaries\n  --rollback        restore binaries saved by the last install\n'
      exit 0 ;;
    *) error "Unknown option: $1" ;;
  esac
done

# ─── Detect platform ────────────────────────────────────────────
os="$(uname -s)"; arch="$(uname -m)"
case "$os-$arch" in
  Darwin-arm64)  asset="stoke-macos-arm64" ;;
  Darwin-x86_64) asset="stoke-macos-x64" ;;
  Linux-x86_64)  asset="stoke-linux-x64" ;;
  Linux-aarch64) asset="stoke-linux-arm64" ;;
  *) asset="" ;;  # unknown platform → source fallback
esac

# Portable SHA-256: coreutils on Linux, perl shasum on macOS.
verify_checksum() {
  # $1 = directory holding the tarball and its .sha256 file
  ( cd "$1" || return 1
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "${asset}.tar.gz.sha256" >/dev/null 2>&1
    elif command -v shasum >/dev/null 2>&1; then
      shasum -a 256 -c "${asset}.tar.gz.sha256" >/dev/null 2>&1
    else
      return 2  # no checksum tool available
    fi )
}

replace_binaries() {
  src="$1"; mkdir -p "$INSTALL_DIR" || return 1
  [ -f "$src/stoke" ] && [ -f "$src/stoke-cli" ] || return 1
  chmod +x "$src/stoke" "$src/stoke-cli" || return 1
  backed_up_stoke=0; backed_up_cli=0
  if [ -e "$INSTALL_DIR/stoke" ]; then
    rm -f "$INSTALL_DIR/stoke.stoke-backup" || return 1
    mv "$INSTALL_DIR/stoke" "$INSTALL_DIR/stoke.stoke-backup" || return 1
    backed_up_stoke=1
  fi
  if [ -e "$INSTALL_DIR/stoke-cli" ]; then
    if ! rm -f "$INSTALL_DIR/stoke-cli.stoke-backup" || ! mv "$INSTALL_DIR/stoke-cli" "$INSTALL_DIR/stoke-cli.stoke-backup"; then
      [ "$backed_up_stoke" = "1" ] && mv "$INSTALL_DIR/stoke.stoke-backup" "$INSTALL_DIR/stoke"
      return 1
    fi
    backed_up_cli=1
  fi
  if ! mv "$src/stoke" "$INSTALL_DIR/stoke" || ! mv "$src/stoke-cli" "$INSTALL_DIR/stoke-cli"; then
    rm -f "$INSTALL_DIR/stoke" "$INSTALL_DIR/stoke-cli"
    [ "$backed_up_stoke" = "1" ] && mv "$INSTALL_DIR/stoke.stoke-backup" "$INSTALL_DIR/stoke"
    [ "$backed_up_cli" = "1" ] && mv "$INSTALL_DIR/stoke-cli.stoke-backup" "$INSTALL_DIR/stoke-cli"
    return 1
  fi
}
rollback_binaries() {
  [ -f "$INSTALL_DIR/stoke.stoke-backup" ] && [ -f "$INSTALL_DIR/stoke-cli.stoke-backup" ] || error "No saved binaries to roll back in $INSTALL_DIR"
  rm -f "$INSTALL_DIR/stoke" "$INSTALL_DIR/stoke-cli"
  mv "$INSTALL_DIR/stoke.stoke-backup" "$INSTALL_DIR/stoke" && mv "$INSTALL_DIR/stoke-cli.stoke-backup" "$INSTALL_DIR/stoke-cli" || error "Rollback failed"
  info "Rolled back Stoke binaries"
}
uninstall_binaries() { rm -f "$INSTALL_DIR/stoke" "$INSTALL_DIR/stoke-cli"; info "Removed Stoke binaries (configs preserved)"; }

install_from_source() {
  warn "Building from source (requires Rust; takes a few minutes)."
  if ! command -v cargo >/dev/null 2>&1; then
    warn "Rust not found — installing via rustup."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  git clone --depth 1 "https://github.com/${REPO}.git" "$tmp/stoke"
  ( cd "$tmp/stoke" && cargo build --release )
  mkdir -p "$tmp/staged"
  cp "$tmp/stoke/target/release/stoke" "$tmp/stoke/target/release/stoke-cli" "$tmp/staged/"
  replace_binaries "$tmp/staged"
}

install_from_binary() {
  base="https://github.com/${REPO}/releases"
  if [ "$VERSION" = "latest" ]; then
    url="$base/latest/download/${asset}.tar.gz"
  else
    url="$base/download/${VERSION}/${asset}.tar.gz"
  fi
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  info "Downloading ${asset} (${VERSION})..."
  # Keep the published filename so the .sha256 manifest matches it verbatim.
  curl -fsSL "$url" -o "$tmp/${asset}.tar.gz" || return 1

  if curl -fsSL "${url}.sha256" -o "$tmp/${asset}.tar.gz.sha256" 2>/dev/null; then
    if verify_checksum "$tmp"; then
      info "Checksum verified"
    else
      rc=$?
      [ "$rc" = "2" ] && warn "No sha256 tool found — skipping verification" \
                      || error "Checksum MISMATCH — refusing to install. Please report this."
    fi
  else
    warn "No checksum published for this build — skipping verification"
  fi

  tar xzf "$tmp/${asset}.tar.gz" -C "$tmp"
  mkdir -p "$tmp/staged"
  cp "$tmp/${asset}/stoke" "$tmp/${asset}/stoke-cli" "$tmp/staged/"
  replace_binaries "$tmp/staged"
}

if [ "$ACTION" = "uninstall" ]; then uninstall_binaries; exit 0; fi
if [ "$ACTION" = "rollback" ]; then rollback_binaries; exit 0; fi

if [ -n "$asset" ] && install_from_binary; then
  info "Installed prebuilt binary to $INSTALL_DIR"
else
  if [ -n "$asset" ]; then
    warn "No prebuilt binary available — falling back to source."
  fi
  install_from_source
  info "Installed to $INSTALL_DIR"
fi

if ! printf '%s' "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
  warn "$INSTALL_DIR is not in your PATH. Add to your shell profile:"
  printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
fi

# ─── Config + first run ─────────────────────────────────────────
# `stoke-cli init` discovers a default model from your own Ollama; Stoke
# ships with no model names of its own.
CONFIG_DIR="${HOME}/.config/stoke"
mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_DIR/stoke.toml" ]; then
  if "$INSTALL_DIR/stoke-cli" init --output "$CONFIG_DIR/stoke.toml" >/dev/null 2>&1; then
    info "Config generated at $CONFIG_DIR/stoke.toml"
  else
    warn "Run 'stoke-cli init' to generate a config"
  fi
fi

printf '\n%s%sStoke installed.%s Stoke rejects unauthenticated requests by default.\n\n' "$BOLD" "$GREEN" "$NC"
printf '  export STOKE_API_KEYS=your-secret-key    # or STOKE_DEV=1 for local dev\n'
printf '  stoke-cli serve                          # start the gateway on :8787\n\n'
printf 'Point any OpenAI-compatible client or agent at it:\n'
printf '  OPENAI_BASE_URL=http://127.0.0.1:8787/v1\n'
printf '  (send the key as: Authorization: Bearer your-secret-key)\n\n'
printf 'Docs: https://github.com/%s  ·  https://stokegate.com\n' "$REPO"
