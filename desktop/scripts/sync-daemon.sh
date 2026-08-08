#!/bin/sh
# Rebuild the debug daemon so its embedded build stamp matches the desktop app.
#
# The stamp includes the git describe hash, so ANY commit made while
# `tauri dev` is running restamps the desktop binary on its next rebuild while
# the daemon binary on disk keeps the old hash. The deck then refuses the
# connection with "build mismatch: desktop is X, daemon is Y", and the GUI's
# "Replace daemon" button cannot help in dev because that path starts the
# bundled sidecar, which only exists in a packaged .app.
#
# Running this before `tauri dev` keeps the two in lockstep. It is a no-op when
# the daemon is already current, so it costs nothing on a warm tree.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)

# rustup's shims may be absent while the toolchain is installed; fall back to
# the pinned toolchain's bin directory before giving up.
if ! command -v cargo >/dev/null 2>&1; then
  for candidate in "$HOME/.cargo/bin" "$HOME/.rustup/toolchains"/*/bin; do
    if [ -x "$candidate/cargo" ]; then
      PATH="$candidate:$PATH"
      export PATH
      break
    fi
  done
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "sync-daemon: cargo not found; skipping daemon rebuild" >&2
  echo "sync-daemon: a build mismatch banner in the deck means you must build it manually" >&2
  exit 0
fi

cargo build --locked --manifest-path "$repo_root/Cargo.toml" --bin dot-agent-deck
