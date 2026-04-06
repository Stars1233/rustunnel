# rustunnel Roadmap

This document tracks the features that have already shipped and ideas planned for future releases. It is a living reference — items may be re-prioritised or added as the project evolves.

---

## Implemented

### Core tunnel engine
- [x] HTTP tunnel proxying with automatic subdomain routing (`<id>.yourdomain.com`)
- [x] Custom subdomain support (`--subdomain myapp`)
- [x] TCP tunnel proxying with dynamic port allocation from a configurable range
- [x] yamux stream multiplexing over a single WebSocket connection
- [x] Automatic client reconnection with configurable retry logic
- [x] Independent data WebSocket reconnect — data plane failures no longer tear down the control session; tunnels keep their subdomain/port (v0.4.10+)
- [x] Graceful shutdown — drains active sessions with a 30-second timeout on SIGINT/SIGTERM

### TLS & security
- [x] TLS termination on the HTTPS edge using rustls
- [x] Static PEM certificate support (BYO cert from Let's Encrypt, Certbot, etc.)
- [x] Built-in ACME client for automatic certificate provisioning and renewal (Cloudflare DNS-01 challenge)
- [x] Per-tunnel request rate limiting (requests/second)
- [x] Per-source-IP rate limiting
- [x] Request body size cap
- [x] Maximum tunnels per session limit
- [x] Maximum concurrent connections per tunnel limit (semaphore)

### Authentication & tokens
- [x] Admin token authentication (static secret in server config)
- [x] Database-backed API tokens (create, list, delete)
- [x] Token scope field for future RBAC use
- [x] Token last-used timestamp tracking
- [x] Per-token tunnel count tracking
- [x] Tunnel history page in the dashboard (paginated table with protocol filter, duration, token attribution)
- [x] Token management via CLI (`rustunnel token create / list / delete`)
- [x] Token management via Dashboard UI

### Dashboard UI
- [x] Live dashboard built with Next.js (static export embedded in server binary)
- [x] Active sessions panel with real-time polling
- [x] Active tunnels panel (HTTP and TCP)
- [x] Live request inspector (captures HTTP requests proxied through tunnels)
- [x] API token management panel (create / view / delete tokens with one-time raw token display)
- [x] Per-token tunnel usage counter

### Observability
- [x] Structured JSON logging (via `tracing` + `tracing-subscriber`)
- [x] Append-only audit log (JSON-lines) for auth, tunnel, and token events
- [x] Prometheus metrics endpoint (`/metrics` on `:9090`)
  - `rustunnel_active_sessions`
  - `rustunnel_active_tunnels_http`
  - `rustunnel_active_tunnels_tcp`
- [x] SQLite-backed tunnel activity log (`tunnel_log` table with token attribution)

### Deployment
- [x] Multi-stage Dockerfile for minimal production images
- [x] Docker Compose stack (server + optional Prometheus + Grafana)
- [x] systemd service unit with dedicated system user
- [x] `make deploy` / `make update-server` helpers for bare-metal deployments
- [x] Pre-built Grafana dashboard for tunnel metrics

### Developer experience
- [x] Cargo workspace with separate `rustunnel-server`, `rustunnel-client`, and `rustunnel-protocol` crates
- [x] Integration test suite (spins up a real server on random ports, tests auth, HTTP/TCP tunnels, reconnection)
- [x] GitHub Actions CI (format check + Clippy + full test suite)
- [x] Pre-push git hook mirroring CI checks (`make install-hooks`)
- [x] Local development config (`deploy/local/server.toml`) and self-signed cert setup instructions
- [x] Pre-built release binaries for Linux (x86_64, aarch64) and macOS via GitHub Releases
- [x] `rustunnel setup` — interactive wizard that creates `~/.rustunnel/config.yml` with prompted server, auth token, and region values

### Managed service & self-service accounts
- [x] Public website at [rustunnel.com](https://rustunnel.com) with marketing page, pricing, and documentation
- [x] Self-service user registration and email verification — no manual token issuance
- [x] User dashboard — API key management (create, label, revoke), usage stats, tunnel history
- [x] Free tier — up to 3 tunnels, TLS/HTTPS termination included
- [x] Pay-as-you-go plan — unlimited tunnels, custom subdomains, TLS/HTTPS termination
- [x] Stripe billing integration — $3/month minimum + $0.10/GB overage above 30 GB
- [x] Spend cap setting — users can cap their monthly PAYG spend from the dashboard
- [x] Payment method management via Stripe Customer Portal
- [x] Invoice history in the user dashboard
- [x] Custom subdomains gated by plan (PAYG and self-hosted only)

### Multi-region infrastructure
- [x] PostgreSQL-backed `regions` table with region metadata (id, name, location, host, control_port, active)
- [x] `region_id` column on `tunnel_log` for per-region tunnel attribution
- [x] `[region]` section in `server.toml` — each instance declares its own region identity
- [x] `GET /api/regions` endpoint — returns active region list for client discovery
- [x] `--region <id>` CLI flag for `rustunnel http` / `rustunnel tcp` (`eu`, `us`, `ap`, `auto`)
- [x] `region:` field in `~/.rustunnel/config.yml`
- [x] Parallel TCP latency probing across all regions — auto-selects nearest on `region: auto`
- [x] Three-tier region list resolution: local cache → API fetch → hardcoded fallback compiled into binary
- [x] 24-hour region list cache at `~/.rustunnel/regions.json`
- [x] Global edge fleet: EU (Helsinki), US (Hillsboro, OR), AP (Singapore)

### Observability (continued)
- [x] Sentry integration for error tracking and distributed tracing
- [x] Accurate bytes-proxied tracking per tunnel session
- [x] Per-request body size capture via RAII `CaptureGuard`
- [x] `GET /api/admin/metrics/users-over-time` — user growth metrics for admin dashboard

### Authentication (continued)
- [x] Google OAuth sign-in for the managed service

### AI agent integration (Phase 1)
- [x] `rustunnel-mcp` binary — MCP server with stdio transport
- [x] `create_tunnel` tool — spawns `rustunnel` CLI subprocess and polls API for the public URL
- [x] `list_tunnels` tool — REST wrapper for `GET /api/tunnels`
- [x] `close_tunnel` tool — REST wrapper for `DELETE /api/tunnels/:id` + kills spawned process
- [x] `get_connection_info` tool — returns CLI command for cloud/sandbox agents
- [x] `get_tunnel_history` tool — REST wrapper for `GET /api/history`
- [x] `GET /api/openapi.json` — machine-readable API spec for agent discovery
- [x] Claude Code plugin — `/plugin install rustunnel` with secure token storage, skill definition, and zero-config MCP setup
- [x] `list_regions` MCP tool — calls `GET /api/regions`, returns region list to the agent
- [x] `region` parameter on `create_tunnel` and `get_connection_info` MCP tools

---

## Planned / Ideas

Items below are not committed to any release timeline. They represent directions the project may grow in.

### Short-term
- [ ] Shell completions for the CLI (bash, zsh, fish)
- [ ] `rustunnel status` command to inspect the active connection and registered tunnels
- [ ] Extended Prometheus metrics (bytes proxied, request latency histograms, error rates)
- [ ] `rustunnel setup --update` flag to edit an existing config file non-destructively
- [ ] Token-scoped tunnel isolation — `list_tunnels` and `close_tunnel` restricted to tunnels owned by the calling token

### AI agent integration (Phase 2 — x402 payments)
- [ ] x402 middleware on `POST /api/tokens` — gate token creation behind USDC micropayment
- [ ] Token TTL + tier metadata (`expires_at`, `tier`, `tunnel_limit` columns)
- [ ] Token expiry enforcement at tunnel registration time
- [ ] `purchase_tunnel_pass` MCP tool — drives x402 payment flow using agent's wallet
- [ ] Coinbase facilitator integration for on-chain payment verification

### AI agent integration (Phase 3 — remote MCP + metering)
- [ ] Streamable HTTP transport — deploy MCP server as `mcp.tunnel.example.com`
- [ ] OAuth 2.1 on the remote MCP endpoint
- [ ] `GET /api/usage` — tunnel-hours, bytes, request counts per token

### Medium-term
- [ ] Token RBAC — enforce scope restrictions (e.g. `http-only`, `tcp-only`, read-only dashboard)
- [ ] Bandwidth limiting per tunnel
- [ ] Webhook notifications on tunnel connect / disconnect events
- [ ] Dashboard dark mode
- [ ] Windows support for the client binary
- [ ] Config file hot-reload (SIGHUP) without restarting the server
- [ ] Health check / heartbeat endpoint for load balancer probing

### Multi-region (Phase 5 — unified dashboard) ✅ Complete
- [x] Dashboard fan-out queries — active tunnels aggregated across all regions via parallel API calls
- [x] Per-region health indicators in the dashboard header (one dot per region)
- [x] Region column in active tunnels table and tunnel history table
- [x] Region-aware request inspector — routes to the correct regional server via `region_id`
- [ ] Cross-region token validation (tokens issued on one region accepted by all — already works via shared PostgreSQL)

### Multi-region (Phase 6 — MCP region support) ✅ Complete
- [x] `list_regions` MCP tool — calls `GET /api/regions`, returns region list to the agent
- [x] `region` parameter on `create_tunnel` MCP tool — passes `--region <id>` to CLI subprocess
- [x] `region` parameter on `get_connection_info` — included in the CLI command string and JSON response

### Long-term / Exploratory
- [ ] SSH tunnel support (`rustunnel ssh`)
- [ ] Custom domain per tunnel (BYOD — bring your own domain with DNS verification)
- [ ] Multi-user / team management with role-based access control
- [ ] Traffic inspector with request replay in the dashboard
- [ ] Tunnel persistence across server restarts (reconnect to the same subdomain/port)
- [ ] mTLS client authentication
- [ ] Plugin / middleware system for request transformation and filtering
- [ ] Distributed server mode (multiple instances sharing state via a database)

---

## Changelog highlights

| Version | Highlights |
|---------|-----------|
| 0.1.0 | Initial release — HTTP/TCP tunnels, TLS, admin token auth, dashboard, Prometheus metrics |
| 0.2.0 | API token management (create/list/delete), tunnel activity log, per-token tunnel counts |
| 0.3.0 | Tunnel history dashboard page, stale tunnel cleanup on restart, MCP server (Phase 1), OpenAPI spec |
| 0.3.1 | Multi-region server infrastructure — `regions` table, `region_id` on tunnel log, `GET /api/regions`, `[region]` server config |
| 0.3.2 | Multi-region client — `--region` flag, `region:` config field, parallel latency probing, auto-select, 3-tier region discovery |
| 0.3.6 | Unified dashboard — per-region health dots, region column in tunnels + history, region-aware request inspector; MCP `list_regions` tool + `region` param on `create_tunnel` |
| 0.4.0 | Public platform launch — rustunnel.com with self-service registration, user dashboard, API key management, free tier |
| 0.4.2 | Stripe billing — PAYG plan with metered bandwidth ($0.10/GB), spend cap, Stripe Customer Portal integration |
| 0.4.6 | PAYG minimum fee — $3/month floor covering first 30 GB; overage charged via invoice webhook; TLS/HTTPS termination listed on all plans; custom subdomains gated by plan |
| 0.4.10 | Zero-downtime data WebSocket reconnect — when the data plane drops (NAT timeout, network blip), the client reconnects only the data WebSocket without re-authenticating or re-registering tunnels; same subdomain/port preserved. Server-side change is backwards compatible with older clients. |
| 0.4.12 | Sentry integration for error tracking and distributed tracing |
| 0.4.13 | Fix bytes-proxied tracking — tunnels now report actual transfer instead of 0 |
| 0.4.14 | Accurate per-request body size capture via RAII `CaptureGuard` |
| 0.4.16 | Admin metrics — `GET /api/admin/metrics/users-over-time` for user growth charts |
| 0.4.18 | Claude Code plugin (`/plugin install rustunnel`), Google OAuth sign-in, plugin configuration docs |
