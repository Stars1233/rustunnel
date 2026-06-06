# rustunnel — agent / harness integrations

Drop-in MCP configuration for connecting rustunnel to AI coding agents, plus a
one-command installer. Full guide:
[`docs/agent-integration.md`](../docs/agent-integration.md).

## Quick start

```bash
./install.sh                 # interactive: pick a harness, paste your token
./install.sh --harness codex --token rt_xxx
./install.sh --help
```

The installer prompts for your API token (get one free at
<https://rustunnel.com> → Dashboard → API Keys) and writes the right config for
the harness you choose.

## Templates

| Harness | Template | Config location |
|---------|----------|-----------------|
| Claude Code | [`claude-code/.mcp.json`](claude-code/.mcp.json) | `<project>/.mcp.json` (or use the plugin) |
| Claude Desktop | [`claude-desktop/config.json`](claude-desktop/config.json) | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Codex | [`codex/config.toml`](codex/config.toml) | `~/.codex/config.toml` |
| Cursor | [`cursor/mcp.json`](cursor/mcp.json) | `.cursor/mcp.json` or `~/.cursor/mcp.json` |
| Windsurf | [`windsurf/mcp_config.json`](windsurf/mcp_config.json) | `~/.codeium/windsurf/mcp_config.json` |
| Cline | [`cline/cline_mcp_settings.json`](cline/cline_mcp_settings.json) | via the Cline MCP settings UI |
| Generic / custom | [`generic/mcp.json`](generic/mcp.json) | your client's MCP settings |

In every template, replace `REPLACE_WITH_YOUR_TOKEN` with your API token, and
swap the `--server` / `--api` values if you self-host (add `--insecure` for
local self-signed certs).

## Prerequisites

The `rustunnel` and `rustunnel-mcp` binaries must be on your `PATH`:

```bash
brew tap joaoh82/rustunnel && brew install rustunnel
# or: make release  (from a clone)
```
