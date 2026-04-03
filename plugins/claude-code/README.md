# rustunnel — Claude Code Plugin

Expose local services via secure tunnels directly from Claude Code. Create public HTTPS and TCP URLs for webhooks, demos, database access, and AI agent workflows.

## Install

```
/plugin install rustunnel
```

You'll be prompted for:
- **Server address** — e.g. `eu.edge.rustunnel.com:4040` (hosted) or `localhost:4040` (self-hosted)
- **API URL** — e.g. `https://eu.edge.rustunnel.com:8443` or `http://localhost:4041`
- **API token** — from [rustunnel.com](https://rustunnel.com) dashboard or your self-hosted admin token

## Prerequisites

The `rustunnel` CLI must be installed on your machine for `create_tunnel` to work (it spawns the CLI as a subprocess).

```bash
# Homebrew (macOS/Linux)
brew tap joaoh82/rustunnel
brew install rustunnel

# Or download from GitHub releases
# https://github.com/joaoh82/rustunnel/releases/latest
```

## Configuration

After installing, configure the plugin with your server details:

```
/plugin configure rustunnel
```

You will be prompted for three values:

| Setting | Hosted example | Self-hosted example |
|---------|---------------|---------------------|
| Server address | `eu.edge.rustunnel.com:4040` | `localhost:4040` |
| Dashboard API URL | `https://eu.edge.rustunnel.com:8443` | `http://localhost:4041` |
| API token | `rt_live_abc123...` | your admin token |

These values are stored securely by Claude Code and persist across sessions.

**To reconfigure later**, run `/plugin configure rustunnel` again. Run `/reload-plugins` after reconfiguring for changes to take effect.

**Important:** The MCP server will not start until all three values are configured. If you see a "Missing required user configuration value" error, run `/plugin configure rustunnel`.

## Usage

Once installed, just ask Claude:

> "Expose my local server on port 3000."

> "Open an HTTP tunnel to port 8080 with subdomain myapp."

> "List my active tunnels."

> "Close tunnel a1b2c3d4-..."

## Available Tools

| Tool | Description |
|------|-------------|
| `create_tunnel` | Open a tunnel and get a public URL |
| `close_tunnel` | Close a tunnel by ID |
| `list_tunnels` | List all active tunnels |
| `list_regions` | Show available server regions |
| `get_tunnel_history` | View past tunnel activity |
| `get_connection_info` | Get the CLI command (for cloud sandboxes) |

## Links

- [GitHub](https://github.com/joaoh82/rustunnel)
- [Documentation](https://docs.rustunnel.com)
- [MCP Server Guide](https://docs.rustunnel.com/guides/mcp-server)
