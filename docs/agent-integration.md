# Using rustunnel from an AI agent / LLM harness

rustunnel ships an **MCP server** (`rustunnel-mcp`) that exposes tunnel
management as tools any [Model Context Protocol](https://modelcontextprotocol.io)
client can call. Once it's wired into your harness, you can just say:

> "Create an HTTP tunnel to my service on port 3000."
> "Expose port 5432 over TCP so I can reach my database."
> "Spin up a load-balanced pool across ports 3000 and 3001 with a health check."

This guide is the one-stop reference for adding rustunnel to **Claude Code,
Claude Desktop, Codex, Cursor, Windsurf, Cline, or any custom/MCP agent**.

---

## 1. Prerequisites

You need two binaries on your `PATH`:

- `rustunnel` — the client (the MCP server spawns it to open tunnels)
- `rustunnel-mcp` — the MCP server itself

```bash
# Homebrew (macOS / Linux)
brew tap joaoh82/rustunnel && brew install rustunnel

# Or build from source
git clone https://github.com/joaoh82/rustunnel.git && cd rustunnel
make release          # builds rustunnel, rustunnel-server, rustunnel-mcp
sudo install -m755 target/release/rustunnel     /usr/local/bin/rustunnel
sudo install -m755 target/release/rustunnel-mcp /usr/local/bin/rustunnel-mcp
```

## 2. Get an API token (one-time)

rustunnel requires an API token. **You only need to do this once.**

- **Hosted (rustunnel.com):** sign up free at <https://rustunnel.com> →
  **Dashboard → API Keys** → create a key. No waiting list.
- **Self-hosted:** `rustunnel token create --name agent --server <host>:4040 --admin_token <admin>`,
  or create one from your dashboard.

Set it as `RUSTUNNEL_TOKEN` in the MCP config below. When that variable is set,
the agent never has to pass the token on individual calls.

## 3. The two server settings

| Setting | Hosted example | Self-hosted example | Local dev |
|---------|----------------|---------------------|-----------|
| `--server` (control plane) | `eu.edge.rustunnel.com:4040` | `tunnel.example.com:4040` | `localhost:4040` |
| `--api` (dashboard REST API) | `https://eu.edge.rustunnel.com:8443` | `https://tunnel.example.com:8443` | `http://localhost:4041` |

Add `--insecure` only for local dev with self-signed certs.

---

## 4. Quick install (any harness)

The installer prompts for your token and writes the right config for the harness
you pick:

```bash
curl -fsSL https://raw.githubusercontent.com/joaoh82/rustunnel/main/integrations/install.sh | bash
# or, from a clone:
./integrations/install.sh
```

It supports `claude-code`, `claude-desktop`, `codex`, `cursor`, `windsurf`,
`cline`, and `generic` (prints a snippet). Run `./integrations/install.sh --help`
for non-interactive flags.

---

## 5. Per-harness configuration

Ready-to-edit templates live in [`integrations/`](../integrations/).

### Claude Code

**Plugin (recommended — zero config):**
```
/plugin install rustunnel
/plugin configure rustunnel     # prompts for server, API URL, token
```
See [claude-plugin.md](./claude-plugin.md).

**Manual `.mcp.json`** in your project root — see
[`integrations/claude-code/.mcp.json`](../integrations/claude-code/.mcp.json):
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

### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or
`%APPDATA%\Claude\claude_desktop_config.json` (Windows) — template:
[`integrations/claude-desktop/config.json`](../integrations/claude-desktop/config.json).
Same `mcpServers` shape as above.

### Codex (OpenAI)

Codex reads MCP servers from `~/.codex/config.toml`. Template:
[`integrations/codex/config.toml`](../integrations/codex/config.toml).
```toml
[mcp_servers.rustunnel]
command = "rustunnel-mcp"
args = ["--server", "eu.edge.rustunnel.com:4040", "--api", "https://eu.edge.rustunnel.com:8443"]
env = { RUSTUNNEL_TOKEN = "<your-token>" }
```
Codex also reads an [`AGENTS.md`](../AGENTS.md) at the repo root for guidance.

### Cursor

Project: `.cursor/mcp.json`. Global: `~/.cursor/mcp.json`. Template:
[`integrations/cursor/mcp.json`](../integrations/cursor/mcp.json). Same `mcpServers` shape.

### Windsurf

`~/.codeium/windsurf/mcp_config.json`. Template:
[`integrations/windsurf/mcp_config.json`](../integrations/windsurf/mcp_config.json). Same shape.

### Cline (VS Code)

`cline_mcp_settings.json` (via the Cline MCP settings UI). Template:
[`integrations/cline/cline_mcp_settings.json`](../integrations/cline/cline_mcp_settings.json). Same shape.

### Generic / custom agent

Any MCP client uses the same `command` + `args` + `env`. Template:
[`integrations/generic/mcp.json`](../integrations/generic/mcp.json).

For a programmatic agent, spawn `rustunnel-mcp` and speak newline-delimited
JSON-RPC 2.0 over stdin/stdout:

```python
import subprocess, json, os

proc = subprocess.Popen(
    ["rustunnel-mcp", "--server", "eu.edge.rustunnel.com:4040",
     "--api", "https://eu.edge.rustunnel.com:8443"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    env={**os.environ, "RUSTUNNEL_TOKEN": "<your-token>"},
)

def call(method, params=None, _id=[0]):
    _id[0] += 1
    msg = {"jsonrpc": "2.0", "id": _id[0], "method": method}
    if params: msg["params"] = params
    proc.stdin.write(json.dumps(msg).encode() + b"\n"); proc.stdin.flush()
    return json.loads(proc.stdout.readline())

call("initialize", {"protocolVersion": "2024-11-05"})
print(call("tools/call", {"name": "create_tunnel",
      "arguments": {"local_port": 3000, "protocol": "http"}}))
```

---

## 6. Available tools

Every authenticated tool accepts an optional `token` argument; omit it when
`RUSTUNNEL_TOKEN` is set.

| Tool | Purpose |
|------|---------|
| `create_tunnel` | Open a tunnel and get a public URL. HTTP/TCP/UDP, P2P, and load-balanced pools |
| `close_tunnel` | Close a tunnel by ID |
| `list_tunnels` | List active tunnels |
| `get_tunnel_history` | Past tunnel activity (audit) |
| `list_regions` | Available server regions (no auth) |
| `get_connection_info` | CLI command / config for manual runs (cloud sandboxes) |

### `create_tunnel` arguments

| Argument | When | Description |
|----------|------|-------------|
| `local_port` | always | Local port the service listens on |
| `protocol` | always | `http`, `tcp`, `udp`, or `p2p` |
| `token` | optional | Falls back to `RUSTUNNEL_TOKEN` |
| `subdomain` | http | Custom subdomain |
| `region` | optional | `eu` / `us` / `ap`; omit to auto-select |
| `local_host` | optional | Forward to another host (default `localhost`) |
| `secret` | p2p | Shared secret (publisher and subscriber match) |
| `peer_name` | p2p publish | Publish under this name |
| `peer_target` | p2p connect | Connect to this published name |
| `group` + `group_key` | load balancing | Join a pool (http/tcp); members share the key |
| `health_check` | optional | `{ type: tcp\|http, path, interval_secs, timeout_secs, max_failed, expect_2xx, alert_webhook }` |

See [mcp-server.md](./mcp-server.md) for the full tool reference,
[load-balancing.md](./load-balancing.md) for pools/health checks, and
[p2p-tunnels.md](./p2p-tunnels.md) for peer-to-peer details.

---

## 7. Security notes

- Tokens travel to the server over HTTPS. Use `--insecure` only in local dev.
- The `RUSTUNNEL_TOKEN` env var keeps the token out of tool-call payloads and
  chat history — prefer it over passing `token` inline.
- Child `rustunnel` processes spawned by `create_tunnel` are killed when the MCP
  server exits; temp config files for load-balanced tunnels are cleaned up too.
- Protect any config file you save: `chmod 600 ~/.rustunnel/config.yml`.
