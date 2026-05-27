#!/usr/bin/env sh
# lport installer (cargo-based)
#
# Usage:
#   curl -sfL https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/install.sh | sh
#
#   # reinstall even if the installed version is already up to date:
#   curl -sfL .../install.sh | sh -s -- --force
#
# This installs lport via `cargo install --git`. Requires the Rust toolchain.

set -e

REPO="https://github.com/Bae-ChangHyun/lport"
RAW_CARGO_TOML="https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/Cargo.toml"
BIN="lport"

FORCE=0
for arg in "$@"; do
  case "$arg" in
    --force|-f) FORCE=1 ;;
  esac
done

color() { printf '\033[%sm%s\033[0m' "$1" "$2"; }
info()  { printf '%s %s\n' "$(color '1;34' '==>')" "$1"; }
warn()  { printf '%s %s\n' "$(color '1;33' 'warn:')" "$1" >&2; }
err()   { printf '%s %s\n' "$(color '1;31' 'error:')" "$1" >&2; }

# 1. Platform check (Linux + macOS)
OS="$(uname -s)"
case "$OS" in
  Linux)  REQUIRED_TOOLS="ss ps" ;;
  Darwin) REQUIRED_TOOLS="lsof ps" ;;
  *)
    err "lport supports Linux and macOS only (detected: $OS)."
    exit 1
    ;;
esac

# 2. Required runtime tools
for tool in $REQUIRED_TOOLS; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    warn "'$tool' not found in PATH — lport will fail at runtime."
    case "$OS" in
      Linux)  warn "  install with: sudo apt install iproute2 procps   # debian/ubuntu" ;;
      Darwin) warn "  '$tool' ships with macOS; check your PATH." ;;
    esac
  fi
done

# 3. Check for cargo
if ! command -v cargo >/dev/null 2>&1; then
  err "cargo (Rust toolchain) not found."
  echo
  echo "Install Rust in one line:"
  echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  echo
  echo "Then re-run this installer."
  exit 1
fi

# 4. Detect existing install + compare with latest upstream version
INSTALLED_VERSION=""
if command -v "$BIN" >/dev/null 2>&1; then
  INSTALLED_VERSION="$("$BIN" -V 2>/dev/null | awk '{print $2}')"
fi

LATEST_VERSION=""
if command -v curl >/dev/null 2>&1; then
  LATEST_VERSION="$(curl -fsSL --max-time 5 "$RAW_CARGO_TOML" 2>/dev/null \
    | awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}')"
fi

if [ "$FORCE" = 0 ] && [ -n "$INSTALLED_VERSION" ] && [ -n "$LATEST_VERSION" ] \
    && [ "$INSTALLED_VERSION" = "$LATEST_VERSION" ]; then
  info "$BIN $INSTALLED_VERSION is already up to date. Pass --force to reinstall."
  exit 0
fi

# 5. Install / update
if [ -n "$INSTALLED_VERSION" ] && [ -n "$LATEST_VERSION" ] \
    && [ "$INSTALLED_VERSION" != "$LATEST_VERSION" ]; then
  info "Updating $BIN: $INSTALLED_VERSION -> $LATEST_VERSION ..."
elif [ -n "$INSTALLED_VERSION" ]; then
  info "Reinstalling $BIN $INSTALLED_VERSION from $REPO ..."
else
  info "Installing $BIN from $REPO ..."
fi
cargo install --git "$REPO" --force

# 6. PATH check
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
case ":$PATH:" in
  *":$CARGO_BIN:"*) ;;
  *)
    warn "$CARGO_BIN is not in your PATH."
    warn "  add this to your shell rc:"
    warn "    export PATH=\"\$HOME/.cargo/bin:\$PATH\""
    ;;
esac

info "Done. Run '$BIN --help' to get started."
