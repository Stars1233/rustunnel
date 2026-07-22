# rustunnel Client — User Guide

`rustunnel` exposes local services to the internet through a secure, self-hosted tunnel server.

---

## Table of Contents

1. [Installation](#installation)
2. [Quick Start](#quick-start)
3. [Configuration File](#configuration-file)
4. [Commands](#commands)
   - [setup — Interactive config wizard](#setup--interactive-config-wizard)
   - [http — HTTP tunnel](#http--http-tunnel)
   - [tcp — TCP tunnel](#tcp--tcp-tunnel)
   - [udp — UDP tunnel](#udp--udp-tunnel)
   - [p2p — Peer-to-peer tunnel](#p2p--peer-to-peer-tunnel)
   - [start — Multi-tunnel mode](#start--multi-tunnel-mode)
   - [token create — API token management](#token-create--api-token-management)
5. [Flags Reference](#flags-reference)
6. [Region Selection](#region-selection)
7. [Reconnection Behavior](#reconnection-behavior)
8. [Terminal Output](#terminal-output)
9. [Request Inspector](#request-inspector)
10. [Environment Variables](#environment-variables)
11. [Error Reference](#error-reference)
12. [Troubleshooting](#troubleshooting)

---

## Installation

### From source (recommended)

Requires [Rust](https://rustup.rs/) 1.75 or later.

```bash
git clone https://github.com/your-org/rustunnel
cd rustunnel
cargo build --release -p rustunnel-client
sudo install -Dm755 target/release/rustunnel /usr/local/bin/rustunnel
```

Or use the Makefile shortcut:

```bash
make deploy-client
```

### Verify

```bash
rustunnel --version
```

---

## Quick Start

```bash
# 1. Get an auth token
#    → Sign up at https://rustunnel.com, then go to Dashboard → API Keys → Create token

# 2. Create a config file interactively
rustunnel setup
# → prompts for region and auth token, writes ~/.rustunnel/config.yml

# 3. Expose a local web server running on port 3000
rustunnel http 3000

# 4. Expose a raw TCP service (e.g. SSH on port 22)
rustunnel tcp 22

# 5. Expose a UDP service (e.g. game server)
rustunnel udp 27015

# 6. P2P tunnel — expose a service to another client
rustunnel p2p 27015 --name my-game --secret "shared-secret"
```

After connecting, the terminal displays the public URL:

```
╭────────────────────────────────────────────────────────────╮
│                         rustunnel                          │
├────────────────────────────────────────────────────────────┤
│  HTTP [myapp] → localhost:3000                             │
│   https://myapp.tunnel.example.com                        │
╰────────────────────────────────────────────────────────────╯

  ✓ Tunnels active. Press Ctrl-C to quit.
```

---

## Configuration File

The client reads `~/.rustunnel/config.yml` automatically. CLI flags always override file values.

### Full example

```yaml
# Tunnel server address (required)
server: edge.rustunnel.com:4040

# Authentication token (required)
auth_token: rt_live_abc123...

# Skip TLS certificate verification — local dev ONLY, never use in production
insecure: false

# Region preference: auto (probe & pick nearest), or eu / us / ap.
# Omit for self-hosted / single-server setups.
region: auto

# Named tunnel definitions used by `rustunnel start`
tunnels:
  web:
    proto: http
    local_port: 3000
    local_host: localhost       # optional, defaults to localhost
    subdomain: myapp            # optional, requests a specific subdomain

  api:
    proto: http
    local_port: 8080
    subdomain: myapi

  database:
    proto: tcp
    local_port: 5432

  gameserver:
    proto: udp
    local_port: 27015

  p2p-publish:
    proto: p2p
    local_port: 27015
    p2p_name: my-game
    p2p_secret: shared-secret-123
```

### Field reference

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `server` | string | — | Tunnel server host:port (e.g. `edge.rustunnel.com:4040`) |
| `auth_token` | string | — | Authentication token issued by the server |
| `insecure` | bool | `false` | Skip TLS certificate verification (dev only) |
| `region` | string | omit | `auto` (probe nearest), `eu`, `us`, `ap`. Omit for self-hosted setups. |
| `tunnels` | map | `{}` | Named tunnel definitions (used by `rustunnel start`) |
| `tunnels.<name>.proto` | string | — | `http`, `tcp`, `udp`, or `p2p` |
| `tunnels.<name>.local_port` | integer | — | Local port to forward |
| `tunnels.<name>.local_host` | string | `localhost` | Local hostname to connect to |
| `tunnels.<name>.subdomain` | string | auto-assigned | Requested HTTP subdomain |
| `tunnels.<name>.p2p_name` | string | — | P2P publisher tunnel name (P2P only) |
| `tunnels.<name>.p2p_target` | string | — | P2P subscriber target name (P2P only) |
| `tunnels.<name>.p2p_secret` | string | — | Shared secret for P2P authentication |

---

## Commands

### `setup` — Interactive config wizard

Create (or overwrite) `~/.rustunnel/config.yml` through a guided prompt sequence.

```
rustunnel setup
```

**Prompts:**

| Prompt | Default | Description |
|--------|---------|-------------|
| Region | `auto` | `auto` (probe nearest), `eu`, `us`, `ap`, or `self-hosted` |
| Auth token | _(blank)_ | Token issued by the server; leave empty to fill in later |
| Server address | _(auto-resolved)_ | Only prompted when `self-hosted` is selected |

**Region behavior:**

| Choice | Server resolution |
|--------|-------------------|
| `eu` / `us` / `ap` | Auto-set to `<region>.edge.rustunnel.com:4040` |
| `auto` | Probes all regions, picks the nearest by latency |
| `self-hosted` | Prompts for your server address |

**Behaviour:**

- Creates `~/.rustunnel/` if the directory doesn't exist.
- If a config file already exists it is overwritten — a backup is not kept, so copy the old file first if you want to preserve it.
- Writes a commented `tunnels:` block with HTTP and TCP examples so you can see the structure right away.
- Prints `Created:` or `Updated:` with the full path when done.

**Example session (auto region):**

```
rustunnel setup — create ~/.rustunnel/config.yml

Region [auto / eu / us / ap / self-hosted] (default: auto):
  Selecting nearest region… eu 12ms · us 143ms · ap 311ms · → eu (Helsinki, FI) 12ms
  Server set to: eu.edge.rustunnel.com:4040

Auth token (leave blank to skip): rt_live_abc123xyz

Created: /Users/alice/.rustunnel/config.yml
Run `rustunnel start` to connect using this config.
```

**Example session (self-hosted):**

```
rustunnel setup — create ~/.rustunnel/config.yml

Region [auto / eu / us / ap / self-hosted] (default: auto): self-hosted

Tunnel server address: tunnel.internal.corp:4040

Auth token (leave blank to skip): my-token

Created: /Users/alice/.rustunnel/config.yml
Run `rustunnel start` to connect using this config.
```

**Generated file (managed):**

```yaml
# rustunnel configuration
# Documentation: https://github.com/joaoh82/rustunnel

server: eu.edge.rustunnel.com:4040
auth_token: rt_live_abc123xyz
region: auto

# tunnels:
#   web:
#     proto: http
#     local_port: 3000
#   api:
#     proto: http
#     local_port: 8080
#     subdomain: myapi
#   database:
#     proto: tcp
#     local_port: 5432
```

**Generated file (self-hosted):**

```yaml
# rustunnel configuration
# Documentation: https://github.com/joaoh82/rustunnel

server: tunnel.internal.corp:4040
auth_token: my-token
# region: not applicable (self-hosted)

# tunnels:
#   ...
```

After running `setup`, uncomment and fill in the `tunnels:` section then run `rustunnel start`, or use `rustunnel http <port>` / `rustunnel tcp <port>` directly.

---

### `http` — HTTP tunnel

Expose a local HTTP/HTTPS service through the tunnel server.

```
rustunnel http <port> [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<port>` | Local TCP port to forward (e.g. `3000`) |

**Options:**

| Flag | Default | Description |
|------|---------|-------------|
| `--subdomain <name>` | auto-assigned | Request a specific subdomain (e.g. `myapp` → `myapp.eu.edge.rustunnel.com`) |
| `--server <host:port>` | from config | Override the server address (bypasses region selection) |
| `--token <token>` | from config | Override the auth token |
| `--local-host <host>` | `localhost` | Local hostname to forward to |
| `--region <id>` | from config | Region to connect to: `eu`, `us`, `ap`, or `auto`. Ignored if `--server` is set. |
| `--no-reconnect` | off | Exit instead of reconnecting on failure |
| `--insecure` | off | Skip TLS verification (dev only) |

**Examples:**

```bash
# Expose port 3000 — auto-selects the nearest region
rustunnel http 3000

# Connect to a specific region
rustunnel http 3000 --region eu

# Request a specific subdomain
rustunnel http 3000 --subdomain myapp

# Forward to a non-localhost service
rustunnel http 8080 --local-host 192.168.1.10

# One-shot connection (exit on disconnect instead of reconnecting)
rustunnel http 3000 --no-reconnect

# Use an explicit server address (bypasses region selection)
rustunnel http 3000 --server tunnel.example.com:9000 --token rt_live_abc123
```

**Receiving webhooks (Twilio, Stripe, GitHub, …):**

HTTP tunnels are safe for HMAC-signed webhooks:

- The request body is forwarded **byte-for-byte** — the proxy never parses or
  re-serializes it, so signatures computed over the raw payload stay valid.
- Every proxied request carries `X-Forwarded-For` (caller IP, appended to any
  existing chain), `X-Forwarded-Proto` (`http` or `https`) and
  `X-Forwarded-Host` (the public tunnel host). Frameworks use these to
  reconstruct the public URL, which signed-webhook validation depends on —
  no manual base-URL override needed. `X-Forwarded-Proto` and
  `X-Forwarded-Host` are set authoritatively by the edge; for
  `X-Forwarded-For`, only the **rightmost** entry is edge-verified — earlier
  entries are supplied by the caller and must not be trusted for IP
  allowlists or logging.
- Prefer configuring providers with the **`https://` tunnel URL**. `http://`
  URLs also work when the server runs with `plain_http_mode = "proxy"`
  (the default on rustunnel.com edges); on `redirect` servers an `http://`
  webhook URL gets a 308 redirect, which many providers either refuse to
  follow or re-sign against the target URL — breaking signature validation.

---

### `tcp` — TCP tunnel

Expose any raw TCP service (database, SSH, game server, etc.).

```
rustunnel tcp <port> [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<port>` | Local TCP port to forward |

**Options:** Same as `http` except `--subdomain` has no effect for TCP tunnels. `--region` still works.

**Examples:**

```bash
# Expose a local PostgreSQL instance
rustunnel tcp 5432

# Expose SSH on a non-standard port
rustunnel tcp 2222 --local-host 10.0.0.5
```

The server assigns a random public port from its configured TCP port range. The public address is displayed in the startup box.

---

### `udp` — UDP tunnel

Expose a local UDP service (game server, DNS, VoIP, etc.).

```
rustunnel udp <port> [options]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<port>` | Local UDP port to forward |

**Options:** Same as `http` except `--subdomain` has no effect. `--region` still works.

**Examples:**

```bash
# Expose a game server
rustunnel udp 27015

# Expose a DNS service
rustunnel udp 53 --local-host 10.0.0.1
```

The server assigns a random public UDP port from its configured UDP port range. Incoming datagrams are forwarded to the local service; responses are sent back to the original sender. Sessions are tracked by remote address and expire after 60 seconds of inactivity.

---

### `p2p` — Peer-to-peer tunnel

Connect two rustunnel clients directly. One client acts as a **publisher** (exposes a service), the other as a **subscriber** (connects to it). Data is relayed through the server.

```
rustunnel p2p <port> --name <name> --secret <secret>     # publisher
rustunnel p2p <port> --target <name> --secret <secret>    # subscriber
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<port>` | Local port — the service port (publisher) or the listener port (subscriber) |

**Options:**

| Flag | Description |
|------|-------------|
| `--name <name>` | Publish a service under this name (publisher mode) |
| `--target <name>` | Connect to a published tunnel by name (subscriber mode) |
| `--secret <secret>` | Shared secret for authentication (both sides must match) |
| `--server`, `--token`, `--region`, `--insecure`, `--no-reconnect` | Same as other tunnel types |

`--name` and `--target` are mutually exclusive. One of them is required.

**Examples:**

```bash
# Publisher: expose a game server under the name "my-game"
rustunnel p2p 27015 --name my-game --secret "shared-secret-123"

# Subscriber: connect to "my-game" and listen on local port 8000
rustunnel p2p 8000 --target my-game --secret "shared-secret-123"

# Any app connecting to localhost:8000 on the subscriber's machine
# will be forwarded to localhost:27015 on the publisher's machine.
```

See the [P2P Tunnels reference](p2p-tunnels.md) for details on relay mode, direct mode, NAT classification, and hole punching.

---

### `start` — Multi-tunnel mode

Start all tunnels defined in a config file simultaneously.

```
rustunnel start [--config <path>]
```

**Options:**

| Flag | Default | Description |
|------|---------|-------------|
| `-c, --config <path>` | `~/.rustunnel/config.yml` | Path to a config file |

**Example:**

```bash
# Use default config file
rustunnel start

# Use a custom config file
rustunnel start --config /etc/rustunnel/production.yml
```

`start` always reconnects automatically (equivalent to running each tunnel without `--no-reconnect`). At least one tunnel must be defined in the config file or the command exits with an error.

---

### `token create` — API token management

Create a new API token via the server's dashboard REST API. Requires admin credentials.

```
rustunnel token create --name <label> [options]
```

**Options:**

| Flag | Default | Description |
|------|---------|-------------|
| `--name <label>` | — | Human-readable label for the token (required) |
| `--server <host:port>` | `localhost:4040` | Dashboard server address |
| `--admin-token <token>` | — | Admin token for authentication |

**Example:**

```bash
rustunnel token create \
  --name "production-server" \
  --server tunnel.example.com:4040 \
  --admin-token admin_secret_here
```

**Output:**

```
Token created:
  id:    f47ac10b-58cc-4372-a567-0e02b2c3d479
  token: rt_live_abc123xyz...
  label: production-server
```

Copy the `token` value — it is shown only once. Add it to your config file as `auth_token`.

---

## Flags Reference

This table summarises all flags across all commands:

| Flag | Commands | Description |
|------|----------|-------------|
| `--server <host:port>` | http, tcp | Tunnel server address (bypasses region selection) |
| `--token <token>` | http, tcp | Auth token (overrides config; also read from `RUSTUNNEL_TOKEN`) |
| `--subdomain <name>` | http | Requested HTTP subdomain |
| `--local-host <host>` | http, tcp | Local hostname (default: `localhost`) |
| `--region <id>` | http, tcp | Region: `eu`, `us`, `ap`, or `auto`. Ignored if `--server` is set. |
| `--no-reconnect` | http, tcp | Exit on failure instead of reconnecting |
| `--insecure` | http, tcp | Skip TLS certificate verification |
| `--json` | http, tcp, udp, p2p, start, token create | Emit machine-readable NDJSON events on stdout instead of human output |
| `--no-tui` | http, tcp, udp, p2p, start | Disable the full-screen terminal UI; print one line per request instead |
| `--inspect-port <port>` | http, tcp, udp, p2p, start | Port for the local web inspector (default `4040`; `0` picks any free port) |
| `--no-inspect` | http, tcp, udp, p2p, start | Disable the local web inspector |
| `-c, --config <path>` | start | Config file path |
| `--name <label>` | token create | Token label (required) |
| `--admin-token <token>` | token create | Admin token for dashboard API |
| `--version` | all | Print version and exit |
| `--help` | all | Print help and exit |

`setup` takes no flags — all input is collected interactively.

---

## Region Selection

rustunnel can connect to multiple edge servers in different geographic regions. The region selection logic follows this priority order:

1. **`--server <host:port>`** — explicit server address always wins; region logic is skipped entirely.
2. **`--region <id>`** — connect directly to the named region without probing.
3. **`region: auto`** (config file or `--region auto`) — probe all regions in parallel and pick the nearest.
4. **No region preference** — use `server:` from config as-is (backward compatible with self-hosted setups).

### Available regions (hosted service)

| Region ID | Location | Server |
|-----------|----------|--------|
| `eu` | Helsinki, FI | `eu.edge.rustunnel.com:4040` |
| `us` | Hillsboro, OR | `us.edge.rustunnel.com:4040` |
| `ap` | Singapore | `ap.edge.rustunnel.com:4040` |

### Auto-select output

When `region: auto` is active, the client probes all regions by TCP connect time and prints:

```
  Selecting nearest region… eu 12ms · us 143ms · ap 311ms → eu (Helsinki, FI) 12ms
```

Unreachable regions time out after 3 seconds and are assigned a 10-second penalty so they never win the selection.

### Region list refresh

The region list is cached at `~/.rustunnel/regions.json` for 24 hours. On expiry the client fetches a fresh list from `GET https://<host>:8443/api/regions`; if that fails it falls back to the hardcoded list compiled into the binary.

---

## Reconnection Behavior

By default, `rustunnel` reconnects automatically when the connection drops. The retry delay follows an **exponential backoff** schedule:

| Attempt | Delay |
|---------|-------|
| 1 | ~1 s |
| 2 | ~2 s |
| 3 | ~4 s |
| 4 | ~8 s |
| … | … |
| n≥6 | ~60 s (max) |

Each delay has ±20% random jitter to prevent thundering-herd reconnects when a server restarts.

```
  Reconnecting in 2.3s (attempt 2)…
  Reconnecting in 5.1s (attempt 3)…
```

### Fatal errors (no reconnect)

The following errors cause an immediate exit — retrying would not help:

- **Auth failed** — invalid or revoked token. Fix: create a new token at [rustunnel.com](https://rustunnel.com) (Dashboard → API Keys) and update your config.
- **Tunnel error** — the server rejected the registration (subdomain already taken, tunnel limit reached). Fix: pick a different `--subdomain` or close an existing tunnel.
- **Config error** — missing required fields. Fix: check your `~/.rustunnel/config.yml`.

### Disabling reconnect

Use `--no-reconnect` for scripting, CI, or when you want manual control:

```bash
rustunnel http 3000 --no-reconnect || echo "Tunnel exited"
```

---

## Terminal Output

When stdout is a terminal, the client takes over the screen with a live UI.
Otherwise — piped output, CI, `--no-tui`, or `--json` — it falls back to the
line-based output described further down.

### Terminal UI

```
┌ rustunnel ───────────────────────────────────────────────────────────────────┐
│Session  ● online                    Server   eu.edge.rustunnel.com:4040      │
│Uptime   00:12:43                    Region   eu · 57ms                       │
│Version  0.8.2                       Inspect  http://127.0.0.1:4040           │
└──────────────────────────────────────────────────────────────────────────────┘
┌ Tunnels ─────────────────────────────────────────────────────────────────────┐
│HTTP   web    http://bb176fb6.eu.edge.rustunnel.com  → localhost:3000  ● healthy│
└──────────────────────────────────────────────────────────────────────────────┘
┌ Traffic ─────────────────────────────────────────────────────────────────────┐
│Conns    2 open · 27 total    Requests 1204                        req/s       │
│Traffic  ↑ 1.2 MB  ↓ 830.0 kB    Latency  p50 6ms · p90 41ms      ▁▂▃▅▂▁▃▂    │
└──────────────────────────────────────────────────────────────────────────────┘
┌ Requests ────────────────────────────────────────────────────────────────────┐
│23:21:54 GET     /style.css                    200   3 ms    95.99.57.76:51224│
│23:21:53 POST    /api/items                    201  18 ms    95.99.57.76:51223│
│23:21:53 GET     /missing                      404   2 ms    95.99.57.76:51222│
└──────────────────────────────────────────────────────────────────────────────┘
q quit  ↑↓ scroll  f pause  c clear  l logs
```

- **Session** — connection state, uptime, client version, edge server, region and
  live control-plane latency (measured from the keepalive round-trip), and the
  local inspector URL.
- **Tunnels** — one row per tunnel with its public URL, local target, and health
  when a `health_check` is configured.
- **Traffic** — open/total connections, bytes each way, p50/p90 request duration,
  and a requests-per-second sparkline.
- **Requests** — live log of every HTTP request: time, method, path, status,
  duration, and the public client address. Replayed requests are marked `↻`.

Keys:

| Key | Action |
|-----|--------|
| `q`, `Esc`, `Ctrl-C` | Quit (closes the tunnel) |
| `↑` `↓` / `k` `j` | Scroll the request log |
| `PgUp` / `PgDn` | Scroll by page |
| `g` / `G` | Jump to newest / oldest |
| `f` | Pause or resume following new requests |
| `c` | Clear the captured requests |
| `l` | Toggle the log pane (client diagnostics) |

Only HTTP tunnels produce request rows. TCP and UDP tunnels are opaque byte
streams, so they show connection and traffic counters only.

### Line mode

With `--no-tui`, a non-terminal stdout, or `--json`, output stays line-based.

While establishing the connection a spinner is shown:

```
⠙ Connecting to tunnel server…
⠹ Authenticating…
⠸ Registering tunnels…
```

Once all tunnels are registered, a bordered box appears:

```
╭────────────────────────────────────────────────────────────╮
│                         rustunnel                          │
├────────────────────────────────────────────────────────────┤
│   HTTP [myapp] → localhost:3000                            │
│   https://myapp.tunnel.example.com                        │
│   TCP  [ssh]   → localhost:22                             │
│   tcp://tunnel.example.com:34521                          │
╰────────────────────────────────────────────────────────────╯

  ✓ Tunnels active. Press Ctrl-C to quit.

  Inspect http://127.0.0.1:4040
```

Color coding:
- Protocol label — **bold yellow**
- Tunnel name — dim
- Public URL — **bold green**
- Border — cyan

Requests then stream one per line as they arrive:

```
23:21:54    GET /style.css                     200 3ms
23:21:53   POST /api/items                     201 18ms
23:21:53    GET /missing                       404 2ms
```

### JSON output (`--json`)

With `--json`, the spinner and startup box are suppressed and stdout instead carries NDJSON — one JSON event object per line — for scripts and AI agents:

```json
{"event":"tunnel_ready","protocol":"http","public_url":"https://myapp.tunnel.example.com","local_port":3000,"local_host":"localhost","tunnel_id":"6f9a…","name":"myapp"}
```

Events: `tunnel_ready` (includes `public_addr` host:port for tcp/udp), `reconnecting` (`attempt`, `reason`, `delay_secs`), `reconnected`, `error` (`code`, `message`, `hint`; exit code 1 follows), and `token_created` (for `token create --json`). Diagnostics still go to stderr.

### Graceful shutdown

Press `Ctrl-C` to cleanly close the tunnel and exit. The control WebSocket is closed before the process exits. In the terminal UI, `q` and `Esc` do the same.

---

## Request Inspector

Every tunnel session also starts a small web inspector on loopback, printed at
startup and shown in the terminal UI:

```
Inspect http://127.0.0.1:4040
```

Open it to browse everything that flowed through the tunnel:

- **Request list** — live-updating, with method, path, status, and duration.
  Filter by method, path, or status.
- **Detail view** — Summary (bodies), Headers (full request and response
  headers), and Raw (the reconstructed HTTP messages).
- **Replay** — re-issue any captured request against your local service without
  the original caller doing anything. Handy for webhooks: trigger once, then
  iterate against the same payload. Replayed requests appear in the list marked
  `replay`.

### Scope and limits

- Captures **HTTP tunnels only**. TCP and UDP tunnels are raw byte streams; they
  contribute connection and traffic counters but no request entries.
- Keeps the **last 500 requests in memory**, per process. Nothing is written to
  disk and nothing survives a restart.
- Bodies are captured up to **64 KB each**; larger ones are marked truncated
  (the reported size is still exact). A truncated request body cannot be
  replayed byte-for-byte.
- WebSocket and other upgraded connections are recorded as their handshake
  (`101`); the frames afterwards are not parsed.

### Configuration

| Flag | Effect |
|------|--------|
| `--inspect-port <port>` | Bind a specific port (default `4040`). If it is taken, the next free port is used and the real URL is displayed. |
| `--no-inspect` | Disable the inspector entirely. |

The inspector binds `127.0.0.1` only and has no authentication, so treat it as
local-only — captured payloads may contain credentials, tokens, and personal
data. Use `--no-inspect` on shared or multi-user machines.

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Log level filter (e.g. `debug`, `info`, `warn`, `rustunnel=debug`). Default: `warn`. |
| `RUSTUNNEL_TOKEN` | Auth token for the `http`, `tcp`, `udp`, and `p2p` commands, used when `--token` is not passed (takes precedence over the config file; empty values are ignored). `start` reads tokens from the config file only, and `token create` authenticates with `--admin-token`. |

**Examples:**

```bash
# Enable debug logging for all crates
RUST_LOG=debug rustunnel http 3000

# Enable debug only for rustunnel internals
RUST_LOG=rustunnel=debug rustunnel http 3000

# Quiet mode (errors only)
RUST_LOG=error rustunnel http 3000
```

Log output and human-readable reconnect notices go to **stderr**. Normal tunnel output (startup box, or NDJSON events with `--json`) goes to **stdout**.

---

## Error Reference

| Error | Cause | Fix |
|-------|-------|-----|
| `config error: server address is required` | No `--server` flag and no config file | Add `server:` to `~/.rustunnel/config.yml` or pass `--server` |
| `auth failed: <message>` | Token invalid or revoked | Create a new token at [rustunnel.com](https://rustunnel.com) (Dashboard → API Keys) or with `rustunnel token create` for self-hosted setups |
| `tunnel error: <message>` | Subdomain already in use or server limit reached | Use a different `--subdomain` or wait |
| `connection error: cannot reach <server> (…)` | Can't reach the server | Check network, firewall, and server address; pass `--server <host:port>` or `--region <id>` |
| `connection error: heartbeat timeout` | Server stopped responding to pings | Transient — reconnect loop will retry |
| `connection error: timeout waiting for server response` | Auth/registration timed out (10 s) | Check server health; may be overloaded |
| `no tunnels defined in config file` | `rustunnel start` with an empty `tunnels:` map | Add at least one tunnel to the config |

---

## Troubleshooting

### Tunnel connects but requests don't arrive

- Verify your local service is running and listening: `curl http://localhost:<port>`
- Check `--local-host` if forwarding to a non-localhost address

### Certificate verification failed

If your server uses a self-signed certificate (common for local/staging environments), use `--insecure`:

```bash
rustunnel http 3000 --insecure
```

**Never use `--insecure` in production** — it disables all TLS certificate checks.

### Subdomain already taken

The server returns `tunnel error: subdomain already in use`. Either:
- Omit `--subdomain` to get an auto-assigned subdomain, or
- Choose a different name: `--subdomain myapp-dev`

### Debugging connection issues

Enable verbose logging to see full protocol traces:

```bash
RUST_LOG=debug rustunnel http 3000 2>&1 | tee rustunnel.log
```

Key log messages to look for:

| Message | Meaning |
|---------|---------|
| `authenticated session_id=...` | Auth succeeded |
| `tunnel registered public_url=...` | Tunnel is active |
| `data WebSocket connected` | Data plane is ready |
| `new connection from server conn_id=...` | Incoming proxied request |
| `yamux data conn error` | Data-plane transport error |
| `heartbeat timeout` | Server stopped responding — will reconnect |

### Multiple tunnels on the same server

Use `rustunnel start` with a config file to open all tunnels over a single control connection:

```yaml
server: tunnel.example.com:9000
auth_token: rt_live_abc123

tunnels:
  frontend:
    proto: http
    local_port: 3000
    subdomain: app
  backend:
    proto: http
    local_port: 8080
    subdomain: api
  metrics:
    proto: tcp
    local_port: 9090
```

```bash
rustunnel start
```
