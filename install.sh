#!/usr/bin/env sh
# lport installer (cargo-based)
#
# Usage:
#   curl -sfL https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/install.sh | sh
#
# This installs lport via `cargo install --git`. Requires the Rust toolchain.

set -e

REPO="https://github.com/Bae-ChangHyun/lport"
BIN="lport"

color() { printf '\033[%sm%s\033[0m' "$1" "$2"; }
info()  { printf '%s %s\n' "$(color '1;34' '==>')" "$1"; }
warn()  { printf '%s %s\n' "$(color '1;33' 'warn:')" "$1" >&2; }
err()   { printf '%s %s\n' "$(color '1;31' 'error:')" "$1" >&2; }

# 1. Linux check (lport is Linux-only)
case "$(uname -s)" in
  Linux) ;;
  *)
    err "lport currently supports Linux only (detected: $(uname -s))."
    exit 1
    ;;
esac

# 2. Required runtime tools
for tool in ss ps; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    warn "'$tool' not found in PATH — lport will fail at runtime."
    warn "  install with: sudo apt install iproute2 procps   # debian/ubuntu"
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

# 4. Install
info "Installing $BIN from $REPO ..."
cargo install --git "$REPO" --force

# 5. PATH check
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
