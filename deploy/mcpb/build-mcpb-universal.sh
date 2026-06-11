#!/usr/bin/env bash
# Build the "universal" MCPB bundle: one .mcpb carrying binaries for macOS
# (Apple Silicon), Linux (x86_64 musl, static), and Windows (x86_64), selected
# at install time via the manifest's mcp_config.platform_overrides. Used for
# registries that take a single bundle per listing (e.g. Smithery), unlike the
# per-target bundles build-mcpb.sh produces for the official MCP registry.
#
# Usage: build-mcpb-universal.sh <version> <out-dir>
#   Downloads the three release archives for v<version> via gh, repacks them,
#   and writes rustunnel-mcp-universal.mcpb (+ .sha256) into <out-dir>.
set -euo pipefail

VERSION=$1
OUT_DIR=$2

mkdir -p "$OUT_DIR"
OUT_DIR=$(cd "$OUT_DIR" && pwd)

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
TEMPLATE="$SCRIPT_DIR/manifest.universal.template.json"
REPO="joaoh82/rustunnel"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/dl" "$WORK/bundle/server"/{darwin,linux,win32}

gh release download "v$VERSION" --repo "$REPO" --dir "$WORK/dl" \
  --pattern "rustunnel-v${VERSION}-aarch64-apple-darwin.tar.gz" \
  --pattern "rustunnel-v${VERSION}-x86_64-unknown-linux-musl.tar.gz" \
  --pattern "rustunnel-v${VERSION}-x86_64-pc-windows-msvc.zip"

extract() { # <archive> <dest> <exe-suffix>
  local archive=$1 dest=$2 exe=$3 tmp
  tmp=$(mktemp -d)
  case "$archive" in
    *.zip) unzip -q "$archive" -d "$tmp" ;;
    *) tar -xzf "$archive" -C "$tmp" ;;
  esac
  for bin in "rustunnel-mcp$exe" "rustunnel$exe"; do
    local src
    src=$(find "$tmp" -name "$bin" -type f | head -1)
    [[ -n "$src" ]] || { echo "binary $bin not found in $archive" >&2; exit 1; }
    cp "$src" "$dest/"
    chmod +x "$dest/$bin"
  done
  rm -rf "$tmp"
}

extract "$WORK/dl/rustunnel-v${VERSION}-aarch64-apple-darwin.tar.gz"      "$WORK/bundle/server/darwin" ""
extract "$WORK/dl/rustunnel-v${VERSION}-x86_64-unknown-linux-musl.tar.gz" "$WORK/bundle/server/linux"  ""
extract "$WORK/dl/rustunnel-v${VERSION}-x86_64-pc-windows-msvc.zip"       "$WORK/bundle/server/win32"  ".exe"

sed "s/__VERSION__/$VERSION/g" "$TEMPLATE" > "$WORK/bundle/manifest.json"

# Embed the live tool definitions (name/description/inputSchema) introspected
# from the bundled binary for this host OS — Smithery scores listings on
# tool-definition quality, and name+description-only entries fail their
# deploy validation outright.
case "$(uname -s)" in
  Darwin) HOST_BIN="$WORK/bundle/server/darwin/rustunnel-mcp" ;;
  Linux)  HOST_BIN="$WORK/bundle/server/linux/rustunnel-mcp" ;;
  *)      HOST_BIN="" ;;
esac
if [[ -n "$HOST_BIN" ]]; then
  python3 "$SCRIPT_DIR/introspect-tools.py" "$HOST_BIN" > "$WORK/tools.json"
  jq --slurpfile tools "$WORK/tools.json" '.tools = $tools[0]' \
    "$WORK/bundle/manifest.json" > "$WORK/manifest.tmp"
  mv "$WORK/manifest.tmp" "$WORK/bundle/manifest.json"
  echo "embedded $(jq length "$WORK/tools.json") tool definitions"
else
  echo "warning: unsupported host OS — manifest ships without tool definitions" >&2
fi

jq empty "$WORK/bundle/manifest.json"

BUNDLE="$OUT_DIR/rustunnel-mcp-universal.mcpb"
rm -f "$BUNDLE"
(cd "$WORK/bundle" && zip -qr "$BUNDLE" .)

(cd "$OUT_DIR" && { sha256sum "$(basename "$BUNDLE")" 2>/dev/null \
  || shasum -a 256 "$(basename "$BUNDLE")"; } > "$(basename "$BUNDLE").sha256")

echo "built $BUNDLE"

# NOTE: the universal template intentionally omits the optional `tools` array
# — Smithery's deploy validator (2026-06) rejects MCPB tool entries that only
# carry name+description, and Smithery introspects tools at runtime anyway.
