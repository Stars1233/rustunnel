# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is rustunnel

rustunnel is a self-hosted secure tunnel server written in Rust (similar to ngrok). It exposes local services through a public server via encrypted WebSocket connections with TLS termination, HTTP/TCP/UDP proxying, P2P direct mode (NAT hole punching via STUN/QUIC), a live dashboard, Prometheus metrics, and audit logging.

## Parent project

**Parent project:** [rustunnel](/Users/joaoh82/projects/rustunnel/CLAUDE.md) — read for shared dev commands, env vars, and cross-service architecture.

### How I fit in

This crate (`rustunnel/`) is the **core tunnel server + CLI client + MCP server**. Siblings in the parent meta-repo:

| Sibling | Stack | Dev port | Role |
|---------|-------|----------|------|
| `rustunnel-web/` | Rust (Axum) + Next.js 15 | 3001 / 3000 | Platform API + public website (registration, JWT, billing) |
| `rustunnel-admin-dashboard/` | Next.js 14 | 3002 | Internal admin UI |
| `rustunnel-landing/` | Next.js 15 | 3000 | Marketing landing |
| `rustunnel-private/` | Markdown / Bash / Ansible | — | Ops docs, provisioning playbooks |
| `docs/` | Mintlify (MDX) | 3000 | Public documentation |
| `homebrew-rustunnelcli/` | Ruby | — | Homebrew tap formula |

This server shares its **PostgreSQL database** with `rustunnel-web/platform-api` (the `tokens` table is the auth boundary). Edge servers (`eu`, `us`, `ap`) all run this binary; `platform-api` runs on a separate VPS at `api.rustunnel.com`.

## Common Commands

```bash
# Build
cargo build --workspace
make build
make build-full        # rebuild dashboard UI (Next.js export) + server crate (use after UI changes)

# Release build (optimized binaries: rustunnel-server, rustunnel-client, rustunnel-mcp)
make release

# Test (requires PostgreSQL — start first)
make db-start
make test
# Or manually:
TEST_DATABASE_URL=postgres://rustunnel:test@localhost:5432/rustunnel_test cargo test --workspace

# Stop test DB
make db-stop

# Lint & format
make fmt           # cargo fmt --all
make lint          # cargo clippy --workspace --all-targets -- -D warnings
make check         # fmt check + clippy (mirrors CI)

# Run a single test
TEST_DATABASE_URL=postgres://rustunnel:test@localhost:5432/rustunnel_test cargo test <test_name> -p <crate_name>

# Install pre-push hooks (run once)
make install-hooks
```

### Local Development

```bash
# One-time TLS cert in /tmp/rustunnel-dev (re-run after reboot — tmpfs is wiped)
make dev-setup

# Run server (uses deploy/local/server.toml: self-signed cert, require_auth = false)
cargo run -p rustunnel-server -- --config deploy/local/server.toml

# Run client — HTTP, TCP, UDP
cargo run -p rustunnel-client -- http 3000 --server localhost:4040 --token dev-secret-change-me --insecure
cargo run -p rustunnel-client -- tcp  5432 --server localhost:4040 --token dev-secret-change-me --insecure
cargo run -p rustunnel-client -- udp  27015 --server localhost:4040 --token dev-secret-change-me --insecure

# Dashboard UI (Next.js, dev mode — proxies API to localhost:8443)
make ui-install    # first time
make ui-dev
```

## Workspace Structure

Four crates in `crates/`, plus integration tests and a Next.js dashboard (`dashboard-ui/`) whose static export is embedded into the server binary:

| Crate | Purpose |
|-------|---------|
| `rustunnel-protocol` | Shared `ControlFrame` enum + `TunnelProtocol` (Http/Https/Tcp/Udp/P2p) and serialization |
| `rustunnel-server` | Server: control plane, HTTP/TCP/UDP edges, dashboard, TLS, ACME |
| `rustunnel-client` | CLI client with auto-reconnect, local port forwarding, P2P/STUN, region auto-select |
| `rustunnel-mcp` | MCP server exposing rustunnel as tools to AI agents |

## Architecture

### High-Level Data Flow

```
Internet → HTTP/TCP/UDP Edge → TunnelCore (router) → Control Plane → yamux stream → Local service
                                                  ↘ P2P direct (QUIC, NAT-punched) ↗
```

The server spawns concurrent tasks for each subsystem:
- **Control Plane** (`:4040`) — WebSocket server for client connections; handles auth, tunnel registration, `NewConnection` delivery, P2P signaling
- **HTTP/HTTPS Edge** (`:80` / `:443`) — TLS-terminated reverse proxy; routes by subdomain
- **TCP Edge** (`:20000+`) — Raw TCP proxy; routes by assigned port
- **UDP Edge** (configurable port range) — UDP proxy; routes by assigned port
- **Dashboard** (`:8443`) — REST API + embedded Next.js SPA + per-instance SQLite request capture
- **Prometheus Metrics** (`:9090`)
- **ACME Renewal** (background, optional — Let's Encrypt + Cloudflare DNS-01)

### Connection Handoff (Core Mechanism)

1. HTTP/TCP/UDP edge receives inbound connection → registers a `pending_conn` (oneshot channel) in `TunnelCore` with a `conn_id`
2. Control plane sends `NewConnection { conn_id, protocol }` to the relevant client over WebSocket
3. Client opens a new yamux stream for that `conn_id`
4. Server resolves the oneshot, unblocking the edge task
5. Edge task and client stream data bidirectionally through yamux

### P2P Direct Mode

For peer-to-peer tunnels (`TunnelProtocol::P2p`), the server acts as a rendezvous/signaling broker only — bytes do not flow through it once the punch succeeds:

1. Publisher registers a named tunnel with a `secret_hash`; reports its NAT type and STUN-mapped addresses (`P2pNatInfo`)
2. Subscriber sends `P2pConnect { target_tunnel_name, secret_hash }`
3. Server uses `classify_nat_pair` to pick a hole-punching strategy and exchanges peer addresses via P2P frames
4. Both peers establish a direct **QUIC** connection (via `quinn`) — server is out of the data path
5. Falls back to relayed mode when NAT classification rules out direct connectivity

Client implementation: `crates/rustunnel-client/src/p2p_direct.rs`, `stun.rs`. Server-side state: `P2pPublisher` in `core/router.rs`.

### TunnelCore (`crates/rustunnel-server/src/core/router.rs`)

Central shared state, `Arc`-wrapped and passed to all subsystems:

```rust
pub struct TunnelCore {
    http_routes: DashMap<String, Arc<TunnelGroup>>,  // subdomain → group of members
    tcp_routes:  DashMap<u16, Arc<TunnelGroup>>,     // port → group of members
    udp_routes:  DashMap<u16, Arc<TunnelGroup>>,     // port → group of members
    p2p_tunnels: DashMap<String, P2pPublisher>,      // name → P2P publisher
    sessions:    DashMap<Uuid, SessionInfo>,         // connected clients
    available_tcp_ports: Mutex<Vec<u16>>,            // free TCP port pool
    available_udp_ports: Mutex<Vec<u16>>,            // free UDP port pool
    tunnel_index: DashMap<Uuid, TunnelKey>,          // tunnel_id → key (O(1) removal)
    pending_conns: DashMap<Uuid, oneshot::Sender>,   // conn_id → yamux resolver
    tcp_events: broadcast::Sender<TcpTunnelEvent>,
    udp_events: broadcast::Sender<UdpTunnelEvent>,
    rate_limiter: Arc<RateLimiter>,                  // per-tunnel token bucket
    ip_limiter:   Arc<IpRateLimiter>,                // per-source-IP sliding window
}
```

Each route key resolves to a `TunnelGroup` of one or more `GroupMember`s
(see `core/tunnel.rs`). Today every group has exactly one member — the route
shape is in place ahead of TUNNEL-7 group-based load balancing (`see
../rustunnel-private/docs/development/load-balancing-and-health-checks.md`).
`resolve_http` / `resolve_tcp` / `resolve_udp` pick a healthy member uniformly
at random; ungrouped registrations stay healthy for life. The
`[load_balancing] enabled` flag in `server.toml` is the kill switch for
honouring multi-member groups (defaults to `false`; flipped on per region
during the EU → US → AP rollout).

### Protocol (`crates/rustunnel-protocol/src/frame.rs`)

All client↔server signaling uses JSON-serialized `ControlFrame` over WebSocket:

- `Auth` / `AuthOk` / `AuthError` — token auth, returns session_id + server version
- `RegisterTunnel` / `TunnelRegistered` / `TunnelError` / `UnregisterTunnel` — tunnel lifecycle (HTTP/HTTPS/TCP/UDP/P2P)
- `NewConnection` / `DataStreamOpen` — per-connection signaling (carries `protocol`)
- `P2pConnect` / `P2pNatInfo` and related P2P frames — rendezvous + NAT info exchange
- `Ping` / `Pong` — keepalive

### Key Design Patterns

- **DashMap** for lock-free concurrent routing tables
- **yamux** multiplexes many proxied connections over a single client WebSocket
- **tokio oneshot** decouples edge listeners from client stream delivery
- **broadcast** channels propagate TCP/UDP tunnel lifecycle events to dynamic listeners
- **Arc** shared state across tokio tasks; `parking_lot::Mutex` for synchronous locks
- **arc-swap** for hot-reloadable TLS certificates (ACME renewal swaps without dropping connections)
- **quinn** (QUIC) for P2P direct streams once NAT punch succeeds

## Databases

- **PostgreSQL** (shared with `platform-api`): `tokens` (auth), `tunnel_log` (audit), p2p tunnels (see `crates/rustunnel-server/migrations/pg/`)
- **SQLite** (per server instance): `captured_requests` — HTTP request capture for the dashboard
- `TEST_DATABASE_URL` environment variable required for integration tests (`make db-start` brings it up via `deploy/docker-compose.dev-deps.yml`)

## Configuration

- **Local dev:** `deploy/local/server.toml` — self-signed certs, `require_auth = false`, PostgreSQL at `localhost:5432`
- **Production template:** `deploy/server.toml` — Let's Encrypt + wildcard DNS, PostgreSQL, full rate limits
- **Client config:** `~/.rustunnel/config.yml` (managed via `rustunnel setup` wizard)

## Observability

- **Prometheus metrics** on `:9090`
- **Sentry** integration for panic/error reporting (gated behind config; uses `sentry`/`sentry-tracing` crates)
- **tracing** with JSON output for production log shipping

## Repository Conventions

**Public documentation** (API reference, deployment guides, client guide) lives in `docs/` in this repo (also published via the Mintlify site under `../docs/`).

**Sensitive and development documentation** (architecture plans, deployment internals, security reviews, billing plans) lives in the companion private repo at `../rustunnel-private/docs/`. That repo mirrors the `docs/deployment/` and `docs/development/` folder structure. Never commit development plans or sensitive operational docs to this repo.

The admin dashboard embedded into the server (`dashboard-ui/`) is built with `make ui-build` and statically embedded at compile time via `crates/rustunnel-server/src/dashboard/assets/`. The separate Vercel-deployed admin UI lives in the sibling `rustunnel-admin-dashboard/` repo.

The public-facing website (Next.js, registration, Stripe billing, user dashboard) lives in the sibling `rustunnel-web/` repo. See `../rustunnel-private/docs/development/public-website-platform-plan.md` for the full architecture plan.

## CI

GitHub Actions (`.github/workflows/ci.yml`) runs on every push/PR:
1. Starts PostgreSQL 16 service
2. `cargo fmt --all -- --check`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo test --workspace` with `TEST_DATABASE_URL` set

`.github/workflows/release.yml` builds release artifacts on tag push.

## Knowledge Base

This project shares its knowledge base with its parent (rustunnel). Do **not** create a separate `Projects/rustunnel-core/` folder — entries about this child go in the parent's vault.

### Project-specific — `~/Documents/josh-obsidian-synced/Projects/rustunnel/`

- **Code (this child):** `/Users/joaoh82/projects/rustunnel/rustunnel`
- **Code (parent meta-repo):** `/Users/joaoh82/projects/rustunnel`
- **Context (read first):** `~/Documents/josh-obsidian-synced/Projects/rustunnel/context.md`
- **Notes (running journal):** `~/Documents/josh-obsidian-synced/Projects/rustunnel/notes.md`
- **Project wiki:** `~/Documents/josh-obsidian-synced/Projects/rustunnel/wiki/`

**How to use each:**

- `context.md` — stable background (product goals, stakeholders, domain). Read before starting non-trivial work. Update only when underlying facts change.
- `notes.md` — append-only dated journal. Add entries under `## YYYY-MM-DD` headings for decisions, blockers, TODOs, and incidents — anything worth preserving but not stable enough for `context.md`. Notes about *this child crate* still go here, in the parent's `notes.md`.
- `wiki/` — reference sub-docs (e.g. `Architecture.md`, `Local Dev Setup.md`, `Tech Services.md`). Create new files as topics emerge. Child-specific reference material can live here too — prefix the filename with the child name (e.g. `rustunnel-core — Local Dev Setup.md`) when disambiguation helps.

**When to save:**

- New stable fact about the product/domain → update the parent's `context.md`.
- A decision, incident, or working note → append a dated entry to the parent's `notes.md`.
- Reusable reference material (setup steps, credential locations, architecture) → new/updated file in the parent's `wiki/`.

### Cross-project knowledge — `~/Documents/josh-obsidian-synced/vault/`

- **General wiki:** `~/Documents/josh-obsidian-synced/vault/wiki/` — start at `_master-index.md`, then drill into the relevant topic's `_index.md`.
- **Raw dumps:** `~/Documents/josh-obsidian-synced/vault/raw/` — drop unprocessed research here as `YYYY-MM-DD-{slug}.md`.

Read the general wiki when the question isn't specific to this project. Drop raw research or imported notes into `vault/raw/` so it's captured even before it's distilled.
