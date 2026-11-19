#!/usr/bin/env bash
# The end-to-end check: builds the UI and a headless release binary, then drives the real UI in headless
# Chrome against that binary on a throwaway home, project and data directory. Needs Node 24 and Chrome or
# Chromium (set CM_E2E_CHROME to the executable when it is somewhere unusual). Needs no network.
# Not part of scripts/check.sh: it builds a release binary and needs a browser.
# Exit codes: 0 passed, 1 an assertion failed, 3 no Chrome, 4 unsafe sandbox, 5 the host did not start.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

step() { printf '\n== %s\n' "$*"; }

step "pnpm build"
pnpm -C ui install --frozen-lockfile
pnpm -C ui build

step "cargo build --release (headless)"
cargo build --release -p context-manager --no-default-features --target-dir target/e2e

step "e2e"
exec node tools/e2e/run.mjs "$PWD/target/e2e/release/context-manager"
