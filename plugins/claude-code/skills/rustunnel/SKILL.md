---
name: rustunnel
description: "Expose local services via secure tunnels using rustunnel MCP server. Create public URLs for local HTTP/TCP services for testing, webhooks, and deployment."
version: 1.0.0
author: rustunnel
tags: [tunnel, ngrok, expose, devops, deployment, testing, webhooks]
---

# Rustunnel - Secure Tunnel Management

Expose local services (HTTP/TCP) through public URLs using rustunnel. Perfect for testing webhooks, sharing local development, and deployment workflows.

## When to Use

- **Webhook testing** - Expose local server to receive webhooks from external services
- **Demo sharing** - Share local development with stakeholders
- **CI/CD integration** - Expose preview environments
- **Database access** - Expose local TCP services (PostgreSQL, Redis, etc.)
- **Mobile testing** - Test mobile apps against local backend

## IMPORTANT: Use MCP Tools, Not CLI

**Always use MCP tools for tunnel management.** They handle lifecycle automatically.

| Method | Lifecycle | Recommended |
|--------|-----------|-------------|
| **MCP tools** (create_tunnel, close_tunnel) | Automatic cleanup | Yes |
| **CLI** (rustunnel http 3000) | Manual process management | Only for cloud sandboxes |

**Why MCP tools?**
- Automatic cleanup when closed
- No orphaned processes
- Proper tunnel lifecycle management
- Returns tunnel_id for tracking

---

## Token

The API token is configured when the plugin is enabled and is available via the
`RUSTUNNEL_TOKEN` environment variable. Read it from there when making tool calls —
**do not ask the user for their token**.

```
token = env("RUSTUNNEL_TOKEN")
```

---

## MCP Tools

### create_tunnel

Expose a local port and get a public URL.

**Parameters:**
| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | yes | API token (read from `RUSTUNNEL_TOKEN` env var) |
| `local_port` | integer | yes | Local port to expose |
| `protocol` | "http" \| "tcp" | yes | Tunnel type |
| `subdomain` | string | no | Custom subdomain (HTTP only) |
| `region` | string | no | Region ID (e.g. `"eu"`, `"us"`, `"ap"`). Omit to auto-select. Use `list_regions` to see options. |

**Returns:**
```json
{
  "public_url": "https://abc123.edge.rustunnel.com",
  "tunnel_id": "a1b2c3d4-...",
  "protocol": "http"
}
```

**Lifecycle:** Tunnel stays open until `close_tunnel` is called or MCP server exits.

### close_tunnel

Close a tunnel by ID. Public URL stops working immediately.

**Parameters:**
| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | yes | API token |
| `tunnel_id` | string | yes | UUID from create_tunnel |

**This is the proper way to close tunnels.** No orphaned processes.

### list_tunnels

List all currently active tunnels.

**Parameters:**
| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | yes | API token |

**Returns:** JSON array of tunnel objects.

### get_tunnel_history

Retrieve history of past tunnels.

**Parameters:**
| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | yes | API token |
| `protocol` | "http" \| "tcp" | no | Filter by protocol |
| `limit` | integer | no | Max entries (default: 25) |

### list_regions

List available tunnel server regions. No authentication required.

**Parameters:** None

**Returns:** JSON array of region objects:
```json
[
  { "id": "eu", "name": "Europe", "location": "Helsinki, FI", "host": "eu.edge.rustunnel.com", "control_port": 4040, "active": true }
]
```

### get_connection_info

Returns the CLI command string without spawning anything. Use when MCP can't
spawn subprocesses (cloud sandboxes, containers) or you prefer running the CLI yourself.

**Parameters:**
| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | yes | API token |
| `local_port` | integer | yes | Local port to expose |
| `protocol` | "http" \| "tcp" | yes | Tunnel type |
| `region` | string | no | Region ID (e.g. `"eu"`). Omit to auto-select. |

**Returns:**
```json
{
  "cli_command": "rustunnel http 3000 --server edge.rustunnel.com:4040 --token abc123",
  "server": "edge.rustunnel.com:4040",
  "install_url": "https://github.com/joaoh82/rustunnel/releases/latest"
}
```

---

## Common Workflows

### 1. Expose Local API

```
1. Read token from RUSTUNNEL_TOKEN env var
2. Create tunnel: create_tunnel(token, local_port=3000, protocol="http")
3. Store tunnel_id for later cleanup
4. Return public_url to user
5. When done: close_tunnel(token, tunnel_id)
```

### 2. Custom Subdomain

```
1. Read token from RUSTUNNEL_TOKEN env var
2. create_tunnel(token, local_port=5173, protocol="http", subdomain="myapp-preview")
3. Return URL: https://myapp-preview.edge.rustunnel.com
4. close_tunnel(token, tunnel_id) when done
```

### 3. TCP Tunnel (Database)

```
1. Read token from RUSTUNNEL_TOKEN env var
2. create_tunnel(token, local_port=5432, protocol="tcp")
3. Return tcp://host:port for connection
4. close_tunnel(token, tunnel_id) when done
```

### 4. Cloud Sandbox (CLI Fallback)

```
1. Read token from RUSTUNNEL_TOKEN env var
2. get_connection_info(token, local_port=3000, protocol="http")
3. Output CLI command for user to run locally
4. User runs command
5. list_tunnels(token) to verify and get public_url
6. When done, user Ctrl+C the CLI process
```

---

## Architecture

```
Internet ──── :443 ────▶ rustunnel-server ────▶ WebSocket ────▶ rustunnel-client ────▶ localhost:PORT
                              │
                        Dashboard (:8443)
                        REST API
```

## Security Notes

- Tokens are sent over HTTPS (use `--insecure` only in local dev)
- MCP tools handle process cleanup automatically
- Tunnels are closed when MCP server exits
- Token is stored securely by Claude Code plugin system

---

## Resources

- [GitHub Repository](https://github.com/joaoh82/rustunnel)
- [MCP Server Documentation](https://docs.rustunnel.com/guides/mcp-server)
- [Plugin Documentation](https://docs.rustunnel.com/guides/claude-plugin)
