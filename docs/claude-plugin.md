# rustunnel Claude Code Plugin

The rustunnel Claude Code plugin lets AI agents manage tunnels directly from
[Claude Code](https://claude.com/claude-code) with zero manual MCP configuration.

## How it works

The plugin packages the `rustunnel-mcp` MCP server with a skill definition and
user configuration prompts. When you enable the plugin, Claude Code:

1. Prompts you for your server address and API token (stored securely, entered once)
2. Starts the `rustunnel-mcp` MCP server in the background
3. Makes 6 tunnel management tools available to the agent

From that point on you can ask Claude things like *"expose port 3000"* and it
will create a tunnel and return the public URL — no manual setup needed.

---

## Installation

### From the plugin marketplace

```
/plugin install rustunnel
```

### Local development / from source

```bash
claude --plugin-dir plugins/claude-code/
```

### Setup prompts

When you enable the plugin you will be asked for three values:

| Prompt | Example (hosted) | Example (self-hosted) |
|--------|-------------------|-----------------------|
| Server address | `eu.edge.rustunnel.com:4040` | `localhost:4040` |
| API URL | `https://eu.edge.rustunnel.com:8443` | `http://localhost:4041` |
| API token | `rt_live_abc123...` | your admin token |

These are persisted by Claude Code — you won't be asked again until you
reconfigure or reinstall.

---

## Prerequisites

The `rustunnel` CLI binary must be installed and in your `PATH`. The plugin's
MCP server spawns it as a subprocess when `create_tunnel` is called.

```bash
# Homebrew
brew tap joaoh82/rustunnel
brew install rustunnel

# Or from GitHub releases
# https://github.com/joaoh82/rustunnel/releases/latest

# Or build from source
cargo install --path crates/rustunnel-client
```

---

## Available tools

| Tool | Auth | Description |
|------|------|-------------|
| `create_tunnel` | yes | Open a tunnel and get a public URL |
| `close_tunnel` | yes | Close a tunnel by UUID |
| `list_tunnels` | yes | List all active tunnels |
| `list_regions` | no | Show available server regions |
| `get_tunnel_history` | yes | View past tunnel activity |
| `get_connection_info` | yes | Get the CLI command string (cloud sandbox fallback) |

See the [MCP Server documentation](mcp-server.md) for full parameter tables and
example responses.

---

## Plugin structure

```
plugins/claude-code/
├── .claude-plugin/
│   └── plugin.json        # Manifest (name, version, userConfig)
├── skills/
│   └── rustunnel/
│       └── SKILL.md        # Agent instructions and tool reference
├── .mcp.json               # MCP server config (uses userConfig substitution)
└── README.md
```

---

## Comparison with standalone MCP setup

| Aspect | Plugin | Manual `.mcp.json` |
|--------|--------|--------------------|
| Setup | `/plugin install rustunnel` | Edit `.mcp.json` by hand |
| Token storage | Secure, entered once | Hardcoded in config or passed every call |
| Updates | `/plugin update rustunnel` | Manual git pull |
| Namespacing | `/rustunnel:expose` | N/A |
| Agent guidance | Built-in SKILL.md | Must add instructions yourself |

---

## Self-hosted servers

The plugin works with self-hosted rustunnel instances. When prompted for the
server address and API URL, enter your own server's values instead of the
hosted defaults.

See the [Self-Hosting guide](../README.md#production-deployment-ubuntu--systemd)
for server setup instructions.

---

## Security

- The API token is stored securely by Claude Code's plugin system
  (`sensitive: true` in `userConfig`)
- Tokens are transmitted over HTTPS to the rustunnel server
- Tunnel subprocesses are cleaned up when the MCP server exits
- Use `--insecure` only in local dev with self-signed certificates
