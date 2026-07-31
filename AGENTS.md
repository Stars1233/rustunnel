# AGENTS.md

> **`CLAUDE.md` in this directory is the source of truth.** Read it in full
> before doing any work here — it contains the working guidelines (git/branch
> rules, knowledge base, task management, pull-request registration, reserved
> ports, engineering defaults) and points to the parent `rustunnel` `CLAUDE.md`
> for the rest. If anything in this file conflicts with `CLAUDE.md`,
> `CLAUDE.md` wins.

The rest of this file is about using rustunnel **as a tool** from an agent
session — it does not describe how to build or change this codebase. For build,
test, and lint commands and the architecture overview, see
[`CLAUDE.md`](./CLAUDE.md).

## Using rustunnel as a tool (expose local services)

rustunnel ships an **MCP server** (`rustunnel-mcp`) that lets you open public
tunnels to local services — HTTP, TCP, UDP, peer-to-peer, and load-balanced
pools — directly from an agent session.

**To make rustunnel available in your harness** (Claude Code, Claude Desktop,
Codex, Cursor, Windsurf, Cline, or a custom MCP client), follow
[`docs/agent-integration.md`](./docs/agent-integration.md), or run:

```bash
./integrations/install.sh        # prompts for your API token, writes the config
```

### One-time setup the user must do

rustunnel needs an **API token**. If one isn't already configured (via the
`RUSTUNNEL_TOKEN` env var in the MCP config, or `~/.rustunnel/config.yml`), ask
the user for it once:

> "Get a free rustunnel token at https://rustunnel.com → Dashboard → API Keys,
> then add it as `RUSTUNNEL_TOKEN` in your MCP config."

Once `RUSTUNNEL_TOKEN` is set you don't need to pass `token` on tool calls, and
you should not ask for it again.

### What you can do (MCP tools)

- `create_tunnel` — open a tunnel, get a public URL. Supports:
  - `protocol`: `http`, `tcp`, `udp`, `p2p`
  - `subdomain`, `region`, `local_host`
  - **P2P**: `secret` + `peer_name` (publish) or `peer_target` (connect)
  - **Load balancing**: `group` + `group_key` (http/tcp), optional `health_check`
- `close_tunnel`, `list_tunnels`, `get_tunnel_history`, `list_regions`
- `get_connection_info` — the CLI command/config for environments where the
  agent can't spawn subprocesses (cloud sandboxes)

Prefer the MCP tools over the raw `rustunnel` CLI — they manage the tunnel
lifecycle and clean up automatically.

See [`docs/mcp-server.md`](./docs/mcp-server.md) for the full tool reference and
the [`skills/rustunnel/SKILL.md`](./skills/rustunnel/SKILL.md) skill for workflow
examples.
