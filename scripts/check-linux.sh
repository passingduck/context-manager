#!/usr/bin/env bash
# Compile-checks the headless build for Linux (x86_64). This proves the code compiles for that
# target. It does not run anything on Linux.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
fi

target="x86_64-unknown-linux-gnu"
if ! rustup target list --installed | grep -qx "$target"; then
  echo "== rustup target add $target"
  rustup target add "$target"
fi

echo "== cargo check --target $target --no-default-features -p context-manager"
cargo check --target "$target" --no-default-features -p context-manager
echo
echo "check-linux.sh: ok"
