#!/usr/bin/env bash
# Generate the server.json payload published to the official MCP registry.
#
# Usage: make-server-json.sh <version> <sha-dir> [base-server-json] > server.json
#   version  e.g. 0.8.1 (no leading v)
#   sha-dir  directory containing rustunnel-mcp-<target>.mcpb.sha256 files
#
# Static fields (name, description, repository, ...) come from the repo's
# server.json; this script overrides the version and replaces `packages` with
# one mcpb entry per bundle found in sha-dir. The checked-in cargo package is
# intentionally dropped: the production registry does not accept
# registryType "cargo" yet (live on their staging since 2026-06-03; re-add
# here once a registry release > v1.7.9 reaches production).
set -euo pipefail

VERSION=$1
SHA_DIR=$2
BASE=${3:-server.json}
REPO_URL="https://github.com/joaoh82/rustunnel"

packages=$(
  for f in "$SHA_DIR"/rustunnel-mcp-*.mcpb.sha256; do
    [[ -e "$f" ]] || { echo "no .mcpb.sha256 files in $SHA_DIR" >&2; exit 1; }
    name=$(basename "$f" .sha256)
    sha=$(awk '{print $1}' "$f")
    jq -n --arg url "$REPO_URL/releases/download/v$VERSION/$name" --arg sha "$sha" \
      '{registryType: "mcpb", identifier: $url, fileSha256: $sha, transport: {type: "stdio"}}'
  done | jq -s .
)

jq --arg v "$VERSION" --argjson pkgs "$packages" \
  '.version = $v | .packages = $pkgs' "$BASE"
