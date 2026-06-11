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

# Add the ghcr image as an oci package — but only when it is anonymously
# pullable: the registry verifies oci ownership by reading the image label,
# and a private/missing package would fail the whole publish. (The package
# must be made public once, in the GitHub Packages settings.)
IMAGE_REPO="joaoh82/rustunnel-mcp"
ghcr_token=$(curl -fsSL "https://ghcr.io/token?scope=repository:$IMAGE_REPO:pull" | jq -r '.token // empty' || true)
if [[ -n "$ghcr_token" ]] && curl -fsSL -o /dev/null \
    -H "Authorization: Bearer $ghcr_token" \
    -H "Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json" \
    "https://ghcr.io/v2/$IMAGE_REPO/manifests/$VERSION" 2>/dev/null; then
  packages=$(jq --arg id "ghcr.io/$IMAGE_REPO:$VERSION" \
    '. + [{registryType: "oci", identifier: $id, transport: {type: "stdio"}}]' <<<"$packages")
else
  echo "warning: ghcr.io/$IMAGE_REPO:$VERSION not anonymously pullable — skipping oci package" >&2
fi

jq --arg v "$VERSION" --argjson pkgs "$packages" \
  '.version = $v | .packages = $pkgs' "$BASE"
