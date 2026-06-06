#!/usr/bin/env bash
#
# rustunnel agent installer — wires the rustunnel MCP server into an AI harness.
#
# Usage:
#   ./install.sh                         # interactive
#   ./install.sh --harness codex --token rt_xxx
#   ./install.sh --harness claude-code --server localhost:4040 \
#                --api http://localhost:4041 --insecure
#
# Supported harnesses: claude-code, claude-desktop, codex, cursor, windsurf,
#                      cline, generic
#
# Docs: https://github.com/joaoh82/rustunnel/blob/main/docs/agent-integration.md

set -euo pipefail

HARNESS=""
TOKEN="${RUSTUNNEL_TOKEN:-}"
SERVER="eu.edge.rustunnel.com:4040"
API="https://eu.edge.rustunnel.com:8443"
INSECURE=0
ASSUME_YES=0

usage() {
  sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --harness)  HARNESS="$2"; shift 2 ;;
    --token)    TOKEN="$2"; shift 2 ;;
    --server)   SERVER="$2"; shift 2 ;;
    --api)      API="$2"; shift 2 ;;
    --insecure) INSECURE=1; shift ;;
    --yes|-y)   ASSUME_YES=1; shift ;;
    -h|--help)  usage 0 ;;
    *) echo "unknown argument: $1" >&2; usage 1 ;;
  esac
done

info()  { printf '\033[36m%s\033[0m\n' "$*"; }
warn()  { printf '\033[33m%s\033[0m\n' "$*" >&2; }
err()   { printf '\033[31m%s\033[0m\n' "$*" >&2; }

# ── prerequisites ─────────────────────────────────────────────────────────────

if ! command -v rustunnel-mcp >/dev/null 2>&1; then
  warn "rustunnel-mcp is not on your PATH."
  warn "Install it first:  brew tap joaoh82/rustunnel && brew install rustunnel"
  warn "(or build from source: make release). Continuing — the config will still be written."
fi

# ── gather inputs ─────────────────────────────────────────────────────────────

if [ -z "$HARNESS" ]; then
  echo "Which harness do you want to configure?"
  echo "  1) claude-code     2) claude-desktop   3) codex"
  echo "  4) cursor          5) windsurf         6) cline       7) generic"
  printf "Choice [1-7]: "
  read -r choice
  case "$choice" in
    1) HARNESS="claude-code" ;;
    2) HARNESS="claude-desktop" ;;
    3) HARNESS="codex" ;;
    4) HARNESS="cursor" ;;
    5) HARNESS="windsurf" ;;
    6) HARNESS="cline" ;;
    7|"") HARNESS="generic" ;;
    *) err "invalid choice"; exit 1 ;;
  esac
fi

if [ -z "$TOKEN" ]; then
  echo "Enter your rustunnel API token."
  echo "  Get one free at https://rustunnel.com (Dashboard -> API Keys),"
  echo "  or self-hosted: rustunnel token create --name agent"
  printf "Token: "
  read -r TOKEN
fi
if [ -z "$TOKEN" ]; then
  err "An API token is required. Aborting."
  exit 1
fi

ARGS_JSON="[\"--server\", \"$SERVER\", \"--api\", \"$API\""
ARGS_TOML="[\"--server\", \"$SERVER\", \"--api\", \"$API\""
if [ "$INSECURE" -eq 1 ]; then
  ARGS_JSON="$ARGS_JSON, \"--insecure\""
  ARGS_TOML="$ARGS_TOML, \"--insecure\""
fi
ARGS_JSON="$ARGS_JSON]"
ARGS_TOML="$ARGS_TOML]"

json_block() {
  cat <<EOF
{
  "mcpServers": {
    "rustunnel": {
      "command": "rustunnel-mcp",
      "args": $ARGS_JSON,
      "env": { "RUSTUNNEL_TOKEN": "$TOKEN" }
    }
  }
}
EOF
}

# Merge the rustunnel server into an existing JSON config (or create it).
write_json() {
  target="$1"
  mkdir -p "$(dirname "$target")"
  if command -v jq >/dev/null 2>&1 && [ -s "$target" ]; then
    tmp="$(mktemp)"
    jq --arg srv "$SERVER" --arg api "$API" --arg tok "$TOKEN" \
       --argjson args "$ARGS_JSON" \
       '.mcpServers.rustunnel = {command:"rustunnel-mcp", args:$args, env:{RUSTUNNEL_TOKEN:$tok}}' \
       "$target" > "$tmp" && mv "$tmp" "$target"
    info "Updated $target"
  elif [ -s "$target" ]; then
    warn "$target already exists and jq is not installed."
    warn "Add this block manually under \"mcpServers\":"
    json_block
  else
    json_block > "$target"
    info "Wrote $target"
  fi
}

write_codex() {
  target="$HOME/.codex/config.toml"
  mkdir -p "$(dirname "$target")"
  if [ -f "$target" ] && grep -q '\[mcp_servers.rustunnel\]' "$target"; then
    warn "$target already has an [mcp_servers.rustunnel] block — leaving it untouched."
    return
  fi
  {
    echo ""
    echo "[mcp_servers.rustunnel]"
    echo "command = \"rustunnel-mcp\""
    echo "args = $ARGS_TOML"
    echo "env = { RUSTUNNEL_TOKEN = \"$TOKEN\" }"
  } >> "$target"
  info "Appended [mcp_servers.rustunnel] to $target"
}

# ── apply ─────────────────────────────────────────────────────────────────────

case "$HARNESS" in
  claude-code)
    write_json "$(pwd)/.mcp.json" ;;
  claude-desktop)
    case "$(uname -s)" in
      Darwin) write_json "$HOME/Library/Application Support/Claude/claude_desktop_config.json" ;;
      *)      write_json "$HOME/.config/Claude/claude_desktop_config.json" ;;
    esac ;;
  codex)
    write_codex ;;
  cursor)
    write_json "$HOME/.cursor/mcp.json" ;;
  windsurf)
    write_json "$HOME/.codeium/windsurf/mcp_config.json" ;;
  cline)
    warn "Cline stores MCP settings inside VS Code. Open Cline -> MCP Servers -> Configure,"
    warn "and add this block:"
    json_block ;;
  generic)
    info "Generic MCP config — add this to your client's MCP settings:"
    json_block ;;
  *)
    err "unknown harness: $HARNESS"; exit 1 ;;
esac

info ""
info "Done. Restart your harness (or reload its MCP servers) to pick up rustunnel."
info "Then try: \"Create an HTTP tunnel to my service on port 3000.\""
