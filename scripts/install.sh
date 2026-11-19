#!/usr/bin/env bash
# Builds the UI and a release binary, then installs `context-manager`.
#
#   scripts/install.sh              desktop build (Tauri window, and --serve)
#   scripts/install.sh --headless   build without the window; only --serve works
#
# The binary goes to ~/.local/bin, or to the directory in PREFIX. The script never uses sudo.
set -euo pipefail

cd "$(dirname "$0")/.."

features=()
mode="desktop"
for arg in "$@"; do
  case "$arg" in
    --headless)
      features=(--no-default-features)
      mode="headless"
      ;;
    -h | --help)
      sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "install.sh: unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
fi
for tool in cargo pnpm; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "install.sh: $tool is not on PATH" >&2
    exit 1
  }
done

dest="${PREFIX:-$HOME/.local/bin}"

echo "== building the UI"
pnpm -C ui install --frozen-lockfile
pnpm -C ui build

echo "== building the $mode release binary"
cargo build --release -p context-manager ${features[@]+"${features[@]}"}

echo "== installing"
mkdir -p "$dest"
install -m 0755 target/release/context-manager "$dest/context-manager"

size="$(wc -c <"$dest/context-manager" | tr -d ' ')"
echo
echo "Installed the $mode build of context-manager ($size bytes)."
echo "Binary: $dest/context-manager"
case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "Note: $dest is not on your PATH." ;;
esac
