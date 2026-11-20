#!/usr/bin/env bash
# The gate: formatter, UI type check and build, clippy, tests, binding drift. Stops at the first failure.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

step() { printf '\n== %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --all -- --check

# The UI builds before the Rust steps because the serve shell and the desktop shell embed ui/dist.
step "pnpm install and type check"
pnpm -C ui install --frozen-lockfile
pnpm -C ui typecheck

step "pnpm build"
pnpm -C ui build

step "cargo clippy (warnings denied)"
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p context-manager --no-default-features --all-targets -- -D warnings

# Clippy for the host platform cannot see code that is compiled out there.
step "cargo clippy for Linux (warnings denied)"
scripts/check-linux.sh --if-installed

step "cargo test"
cargo test --workspace
cargo test -p context-manager --no-default-features

# The tests above regenerate ui/src/gen. Any change, or any new file, means the committed
# bindings were stale.
step "binding drift"
git diff --exit-code -- ui/src/gen
if [ -n "$(git ls-files --others --exclude-standard -- ui/src/gen)" ]; then
  echo "untracked files in ui/src/gen: commit the regenerated bindings" >&2
  git ls-files --others --exclude-standard -- ui/src/gen >&2
  exit 1
fi

step "pnpm type check against the regenerated bindings"
pnpm -C ui typecheck

# A notice, not a gate: the code the write pipeline model covers changed after the commit
# the model was last confirmed against.
step "write pipeline model drift"
formal/write-pipeline/drift.sh

printf '\ncheck.sh: ok\n'
