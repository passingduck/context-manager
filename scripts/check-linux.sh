#!/usr/bin/env bash
# Clippy for the headless build on Linux (x86_64), with warnings denied. The host's own clippy
# run cannot see code that is compiled out there, such as a `mut` that only macOS uses. This
# compiles for that target. It does not run anything on Linux.
#
#   scripts/check-linux.sh               installs the target when it is missing
#   scripts/check-linux.sh --if-installed  prints one line and exits 0 when it is missing
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
fi

target="x86_64-unknown-linux-gnu"
if ! rustup target list --installed | grep -qx "$target"; then
  if [ "${1:-}" = "--if-installed" ]; then
    echo "check-linux.sh: SKIPPED, the $target target is not installed (rustup target add $target)"
    exit 0
  fi
  echo "== rustup target add $target"
  rustup target add "$target"
fi

echo "== cargo clippy --target $target --no-default-features (warnings denied)"
cargo clippy --target "$target" -p context-manager --no-default-features --all-targets -- -D warnings
echo "check-linux.sh: ok"
