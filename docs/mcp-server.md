# rustunnel MCP Server

The `rustunnel-mcp` binary implements the
[Model Context Protocol](https://spec.modelcontextprotocol.io) (MCP) over
stdio, letting AI agents (Claude, GPT-4o, Gemini, custom agents) manage
tunnels without any manual setup.

> **Just want to wire rustunnel into your harness?** See
> [agent-integration.md](./agent-integration.md) for copy-paste config for
> Claude Code, Claude Desktop, Codex, Cursor, Windsurf, Cline, and generic MCP
> clients, plus a one-command installer (`integrations/install.sh`). This page
> is the deeper reference for the server and its tools.

---

## How it works

MCP is a standard for connecting AI agents to external tools. The agent sends
JSON-RPC calls to the MCP server; the server translates them into REST API
calls and CLI commands.

```
AI Agent ──── MCP (stdio) ────▶ rustunnel-mcp
                                    │               │
                              spawns rustunnel   calls /api/*
                              CLI subprocess     (REST API)
                                    │               │
                                    └───────────────▼
                                          rustunnel-server
```

**Key constraint:** Tunnels are established via a persistent WebSocket
connection (the control plane on port 4040), not via a REST call. The MCP
server handles this by spawning the `rustunnel` CLI as a subprocess when
`create_tunnel` is called.

---

## Installation

Build from source (included in the workspace):

```bash
make release-mcp
# Produces: target/release/rustunnel-mcp

# Install to PATH
sudo install -m755 target/release/rustunnel-mcp /usr/local/bin/rustunnel-mcp
```

Or build without the dashboard UI step:

```bash
cargo build --release -p rustunnel-mcp
```

---

## Configuration

`rustunnel-mcp` takes two flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--server` | `localhost:4040` | Control-plane address forwarded to the `rustunnel` CLI |
| `--api` | `http://localhost:4041` | Dashboard REST API base URL for tunnel queries |
| `--insecure` | false | Skip TLS certificate verification (local dev / self-signed certs) |

---

## Connecting an AI agent

### Using the hosted server (edge.rustunnel.com)

The fastest way to get started. You need an auth token — see
[Getting an auth token](https://github.com/joaoh82/rustunnel#getting-an-auth-token) in the README.

**Claude Desktop** — add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "rustunnel": {
      "command": "rustunnel-mcp",
      "args": [
        "--server", "edge.rustunnel.com:4040",
        "--api",    "https://edge.rustunnel.com:8443"
      ],
      "env": {
        "RUSTUNNEL_TOKEN": "<your-token>"
      }
    }
  }
}
```

**Claude Code (plugin — recommended)** — install the plugin for zero-config setup:

```
/plugin install rustunnel
```

Then configure your server details:

```
/plugin configure rustunnel
```

You'll be prompted for your server address, API URL, and token. These are
stored securely and persist across sessions. To reconfigure later, run
`/plugin configure rustunnel` again, then `/reload-plugins`.

See the [Claude Code Plugin guide](claude-plugin.md) for full details.

**Claude Code (manual `.mcp.json`)** — add to your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "rustunnel": {
      "command": "rustunnel-mcp",
      "args": [
        "--server", "edge.rustunnel.com:4040",
        "--api",    "https://edge.rustunnel.com:8443"
      ]
    }
  }
}
```

Once connected, you can ask the agent:

> "Expose my local server on port 3000 using my rustunnel token `<token>`."
> "Open an HTTP tunnel to port 8080 with subdomain `myapp`."
> "List all my active tunnels."

The agent will call `create_tunnel` and return a public URL like `https://abc123.edge.rustunnel.com`.

---

### Self-hosted server

Replace the server address with your own instance:

```json
{
  "mcpServers": {
    "rustunnel": {
      "command": "rustunnel-mcp",
      "args": [
        "--server", "your-server.com:4040",
        "--api",    "https://your-server.com:8443"
      ]
    }
  }
}
```

### Local development (self-signed cert)

```json
{
  "mcpServers": {
    "rustunnel": {
      "command": "rustunnel-mcp",
      "args": [
        "--server",   "localhost:4040",
        "--api",      "http://localhost:4041",
        "--insecure"
      ]
    }
  }
}
```

### Cursor / VS Code / any MCP client

Most MCP clients use the same JSON format. Consult your client's documentation
for the exact location of the config file.

### Custom / programmatic agents

Spawn `rustunnel-mcp` as a subprocess and communicate via stdin/stdout using
newline-delimited JSON-RPC 2.0:

```python
import subprocess, json

proc = subprocess.Popen(
    ["rustunnel-mcp", "--server", "localhost:4040"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
)

def call(method, params=None, id=1):
    msg = {"jsonrpc": "2.0", "id": id, "method": method}
    if params:
        msg["params"] = params
    proc.stdin.write(json.dumps(msg).encode() + b"\n")
    proc.stdin.flush()
    return json.loads(proc.stdout.readline())
```

---

## Available tools

### `create_tunnel`

Open a tunnel to a locally running service and get a public URL. Handles plain
HTTP/TCP/UDP tunnels, peer-to-peer tunnels, and load-balanced pools.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token` | string | no¹ | API token. ¹Optional when `RUSTUNNEL_TOKEN` is set in the MCP config. |
| `local_port` | integer | yes | Local port the service is listening on |
| `protocol` | `"http"` \| `"tcp"` \| `"udp"` \| `"p2p"` | yes | Tunnel type |
| `subdomain` | string | no | Custom subdomain for HTTP tunnels |
| `region` | string | no | Region ID (e.g. `"eu"`, `"us"`, `"ap"`). Omit to auto-select by latency. Use `list_regions` to see options. |
| `local_host` | string | no | Local hostname to forward to (default `localhost`) |
| `secret` | string | p2p | Shared P2P secret — publisher and subscriber must match |
| `peer_name` | string | p2p publish | Publish the local service under this name |
| `peer_target` | string | p2p connect | Connect to a published P2P tunnel by this name |
| `group` | string | LB | Load-balancing pool name (http/tcp only) |
| `group_key` | string | LB | Shared secret for the pool; members must agree |
| `health_check` | object | no | `{ type: "tcp"\|"http", path, interval_secs, timeout_secs, max_failed, expect_2xx, alert_webhook }` |

**Returns:**
```json
{
  "public_url": "https://abc123.edge.rustunnel.com",
  "tunnel_id":  "a1b2c3d4-...",
  "protocol":   "http"
}
```

The MCP server spawns `rustunnel` as a background subprocess and polls the API
until the tunnel appears (up to 15 seconds). Load-balanced tunnels are launched
via a temporary `rustunnel start` config that is cleaned up automatically. The
tunnel stays open until `close_tunnel` is called or the MCP server exits.

**Example agent prompts:**
> "Expose my local server on port 3000."
> "Open a P2P tunnel to port 3000 named `my-svc` with secret `hunter2`."
> "Load-balance ports 3000 and 3001 under subdomain `pool` with an HTTP health check on `/health`."

> **Tip:** set `RUSTUNNEL_TOKEN` once in the MCP config (see the examples above)
> so you never have to pass `token` on a tool call.

---

### `list_tunnels`

List all currently active tunnels.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token` | string | no | API token (optional if `RUSTUNNEL_TOKEN` is set) |

**Returns:** JSON array of tunnel objects from `GET /api/tunnels`.

---

### `close_tunnel`

Force-close a tunnel. The public URL stops working immediately.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token` | string | no | API token (optional if `RUSTUNNEL_TOKEN` is set) |
| `tunnel_id` | string | yes | UUID returned by `create_tunnel` or `list_tunnels` |

---

### `get_connection_info`

Returns the CLI command (and, for load-balanced tunnels, a config file) without
spawning anything. Use this when the MCP server cannot launch subprocesses
(cloud sandboxes, containers) or when you want to run the CLI yourself. Accepts
the **same arguments as `create_tunnel`** (`local_host`, `secret`/`peer_name`/
`peer_target` for P2P, `group`/`group_key`/`health_check` for load balancing).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token` | string | no¹ | API token (¹optional if `RUSTUNNEL_TOKEN` is set) |
| `local_port` | integer | yes | Local port to expose |
| `protocol` | `"http"` \| `"tcp"` \| `"udp"` \| `"p2p"` | yes | Tunnel type |
| `region` | string | no | Region ID (e.g. `"eu"`, `"us"`). Omit to auto-select. |
| … | | | plus the optional `create_tunnel` args above |

**Returns:**
```json
{
  "cli_command":  "rustunnel http 3000 --server edge.rustunnel.com:4040 --token abc123",
  "server":       "edge.rustunnel.com:4040",
  "install_url":  "https://github.com/joaoh82/rustunnel/releases/latest"
}
```

---

### `list_regions`

List available tunnel server regions with their IDs, names, and locations.
No authentication required.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| *(none)* | — | — | — |

**Returns:** JSON array of region objects:
```json
[
  { "id": "eu", "name": "Europe", "location": "Helsinki, FI", "host": "eu.edge.rustunnel.com", "control_port": 4040, "active": true },
  { "id": "us", "name": "US West", "location": "Oregon, US", "host": "us.edge.rustunnel.com", "control_port": 4040, "active": true }
]
```

---

### `get_tunnel_history`

Retrieve the history of past tunnels.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token` | string | no | API token (optional if `RUSTUNNEL_TOKEN` is set) |
| `protocol` | `"http"` \| `"tcp"` \| `"udp"` \| `"p2p"` | no | Filter by protocol |
| `limit` | integer | no | Max entries to return (default: 25) |

---

## Agent workflow examples

### Local agent exposing a dev server

```
1. Agent has a pre-existing API token (obtained from the dashboard or via
   the CLI: rustunnel token create --name agent-session)

2. Agent calls create_tunnel(token="...", local_port=3000, protocol="http")
   → MCP server spawns: rustunnel http 3000 --server ... --token ...
   → Returns: { public_url: "https://xyz.edge.rustunnel.com", tunnel_id: "..." }

3. Agent returns the public URL to the user.

4. Later: Agent calls close_tunnel(token="...", tunnel_id="...")
   → MCP server calls DELETE /api/tunnels/:id and kills the subprocess
```

### Cloud agent (no subprocess access)

```
1. Agent calls get_connection_info(token="...", local_port=8000, protocol="http")
   → Returns: { cli_command: "rustunnel http 8000 --server ... --token ..." }

2. Agent outputs the command. User runs it in their local environment.

3. Agent calls list_tunnels(token="...") to confirm the tunnel is active
   and retrieve the public URL.
```

---

## Token management

### Hosted service

Sign up at [rustunnel.com](https://rustunnel.com), then create an API key from
the dashboard under **Settings → API Keys**.

### Self-hosted server

Create tokens via the dashboard UI, the CLI, or the REST API:

```bash
# CLI
rustunnel token create --name agent-session

# REST API
curl -X POST https://edge.rustunnel.com:8443/api/tokens \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"label": "agent-session"}'
```

Store the raw token value securely — it is shown only once at creation time.

---

## OpenAPI spec

The server exposes a machine-readable API description at:

```
GET /api/openapi.json
```

No authentication required. Useful for agent discovery and client generation.

```bash
curl http://localhost:4041/api/openapi.json | jq .info
```

---

## Security notes

- The `rustunnel` binary must be installed and in `PATH` on the machine
  running `rustunnel-mcp` for `create_tunnel` to work.
- Tokens passed to tools are sent to the rustunnel server over HTTPS (or
  HTTP in local dev). Use HTTPS in production.
- Child processes spawned by `create_tunnel` are killed when the MCP server
  exits (stdin closes). They are not persisted across MCP server restarts.
- Use `--insecure` only in local development with self-signed certificates.
