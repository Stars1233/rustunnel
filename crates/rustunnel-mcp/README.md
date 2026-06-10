# rustunnel-mcp

Expose any localhost service so an AI agent can reach it — public HTTPS/TCP/UDP
URLs via six MCP tools. Open source, self-hostable.

`rustunnel-mcp` is the [Model Context Protocol](https://modelcontextprotocol.io)
server for [rustunnel](https://rustunnel.com), an open-source tunnel server
written in Rust. Any MCP-compatible agent (Claude Code, Claude Desktop, Cursor,
Windsurf, Cline, Codex) can give a local service a public URL in one tool call:
test webhooks against a local server, share a dev build, or let another agent
or device reach localhost.

## Tools

| Tool | Purpose |
|------|---------|
| `create_tunnel` | Open a tunnel and get a public URL (HTTP/TCP/UDP, P2P, load-balanced pools) |
| `list_tunnels` | List active tunnels / recover a public URL |
| `close_tunnel` | Close a tunnel by ID |
| `get_connection_info` | Get the CLI command instead of spawning (sandboxes) |
| `list_regions` | List edge regions (eu / us / ap) |
| `get_tunnel_history` | Audit past tunnel activity |

## Install

```bash
# Homebrew (installs rustunnel CLI + rustunnel-mcp)
brew tap joaoh82/rustunnel && brew install rustunnel

# Cargo
cargo install rustunnel-mcp
```

> The MCP server spawns the `rustunnel` CLI to open tunnels — both binaries
> must be on `PATH`. With a cargo-only install, grab the CLI from
> [GitHub releases](https://github.com/joaoh82/rustunnel/releases/latest) or
> the Homebrew tap.

## Configure your agent

Get a free API token at [rustunnel.com](https://rustunnel.com) (Dashboard →
API Keys), then add to your MCP client config (`.mcp.json`, `.cursor/mcp.json`,
etc.):

```json
{
  "mcpServers": {
    "rustunnel": {
      "command": "rustunnel-mcp",
      "args": ["--server", "eu.edge.rustunnel.com:4040", "--api", "https://eu.edge.rustunnel.com:8443"],
      "env": { "RUSTUNNEL_TOKEN": "<your-token>" }
    }
  }
}
```

Self-hosting? Point `--server` / `--api` at your own instance — the same
server binary is free under AGPL.

## Links

- Website: <https://rustunnel.com>
- Agent manual (copy-paste recipes): <https://rustunnel.com/agents.md>
- Per-harness setup: <https://rustunnel.com/docs/guides/agent-integration>
- Source: <https://github.com/joaoh82/rustunnel>
- MCP Registry name: `mcp-name: io.github.joaoh82/rustunnel`
