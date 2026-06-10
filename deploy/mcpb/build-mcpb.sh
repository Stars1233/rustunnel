#!/usr/bin/env bash
# Repack one rustunnel release archive into an MCPB bundle
# (https://github.com/anthropics/mcpb) for the official MCP registry and
# MCPB-aware clients (Claude Desktop double-click install).
#
# Usage: build-mcpb.sh <version> <target-triple> <archive> <out-dir>
#   version        e.g. 0.8.1 (no leading v)
#   target-triple  e.g. aarch64-apple-darwin
#   archive        path to rustunnel-v<version>-<target>.tar.gz (or .zip)
#   out-dir        where rustunnel-mcp-<target>.mcpb (+ .sha256) is written
#
# The bundle contains BOTH binaries: rustunnel-mcp (entry point) and the
# rustunnel CLI it spawns, wired together via the RUSTUNNEL_CLI env var in
# the manifest (requires rustunnel-mcp >= 0.8.1).
set -euo pipefail

VERSION=$1
TARGET=$2
ARCHIVE=$3
OUT_DIR=$4

# Resolve to an absolute path up front — later steps cd into temp dirs.
mkdir -p "$OUT_DIR"
OUT_DIR=$(cd "$OUT_DIR" && pwd)

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
TEMPLATE="$SCRIPT_DIR/manifest.template.json"

case "$TARGET" in
  *apple-darwin*) PLATFORM=darwin EXE="" ;;
  *windows*)      PLATFORM=win32  EXE=".exe" ;;
  *linux*)        PLATFORM=linux  EXE="" ;;
  *) echo "unknown target: $TARGET" >&2; exit 1 ;;
esac

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/extract" "$WORK/bundle/server"

case "$ARCHIVE" in
  *.zip)    unzip -q "$ARCHIVE" -d "$WORK/extract" ;;
  *.tar.gz) tar -xzf "$ARCHIVE" -C "$WORK/extract" ;;
  *) echo "unknown archive format: $ARCHIVE" >&2; exit 1 ;;
esac

for bin in "rustunnel-mcp$EXE" "rustunnel$EXE"; do
  src=$(find "$WORK/extract" -name "$bin" -type f | head -1)
  if [[ -z "$src" ]]; then
    echo "binary $bin not found in $ARCHIVE" >&2
    exit 1
  fi
  cp "$src" "$WORK/bundle/server/"
  chmod +x "$WORK/bundle/server/$bin"
done

sed -e "s/__VERSION__/$VERSION/g" \
    -e "s/__PLATFORM__/$PLATFORM/g" \
    -e "s/__EXE__/$EXE/g" \
    "$TEMPLATE" > "$WORK/bundle/manifest.json"

# Validate the generated manifest is well-formed JSON.
jq empty "$WORK/bundle/manifest.json"

BUNDLE="$OUT_DIR/rustunnel-mcp-$TARGET.mcpb"
rm -f "$BUNDLE"
(cd "$WORK/bundle" && zip -qr "$BUNDLE" .)

(cd "$OUT_DIR" && { sha256sum "$(basename "$BUNDLE")" 2>/dev/null \
  || shasum -a 256 "$(basename "$BUNDLE")"; } > "$(basename "$BUNDLE").sha256")

echo "built $BUNDLE"
