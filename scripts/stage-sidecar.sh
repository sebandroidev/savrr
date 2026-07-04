#!/usr/bin/env bash
# Build savr-daemon and stage it as the Tauri sidecar the app bundles.
#
# Tauri looks for `binaries/savr-daemon-<target-triple>[.exe]` next to
# tauri.conf.json's externalBin entry. Run this before `cargo tauri build` /
# `cargo tauri dev` locally. CI does the equivalent inline (see release.yml).
#
# Usage: scripts/stage-sidecar.sh [--target <triple>]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/crates/savr-app/src-tauri/binaries"

# Accept `--target <triple>`, a bare `<triple>`, or nothing (host triple).
if [ "${1:-}" = "--target" ]; then
  TARGET="${2:?--target requires a <triple>}"
elif [ -n "${1:-}" ]; then
  TARGET="$1"
else
  TARGET="$(rustc -vV | sed -n 's/^host: //p')"
fi
EXE=""
case "$TARGET" in *windows*) EXE=".exe" ;; esac

echo "Building savr-daemon for $TARGET ..."
cargo build --release -p savr-daemon --target "$TARGET"

mkdir -p "$DEST"
cp "$ROOT/target/$TARGET/release/savr-daemon$EXE" "$DEST/savr-daemon-$TARGET$EXE"
echo "Staged: $DEST/savr-daemon-$TARGET$EXE"
