# UDP Tunnels — How UDP Forwarding Works

rustunnel supports UDP tunnels alongside HTTP and TCP. A UDP tunnel exposes a local UDP service (a game server, DNS resolver, QUIC endpoint, VoIP service, etc.) on a public port on the rustunnel server, forwarding datagrams bidirectionally between remote clients and the local service.

---

## Table of Contents

1. [Concepts](#concepts)
2. [Connection Flow](#connection-flow)
3. [Sessions](#sessions)
4. [Transport and Framing](#transport-and-framing)
5. [Server Configuration](#server-configuration)
6. [CLI Usage](#cli-usage)
7. [Config File](#config-file)
8. [Limitations and Notes](#limitations-and-notes)
9. [Troubleshooting](#troubleshooting)

---

## Concepts

A UDP tunnel works like the TCP tunnel, with one important difference: UDP is connectionless, so there is no persistent "connection" to forward. Instead, rustunnel tracks **sessions** — one per unique remote source address (`IP:port`) — and forwards datagrams in both directions for as long as the session is active.

```
Remote client → server:20100 (UDP) ──tunnel──▶ Client → localhost:27015 (UDP) → Service
```

- **Public port** — allocated from a dedicated UDP port range on the server (separate from the TCP tunnel pool).
- **Session** — identified by the remote client's source `IP:port`. Each unique source gets its own yamux stream to the client and its own locally bound UDP socket on the client side.
- **Idle timeout** — sessions are reaped after 60 seconds of inactivity.

---

## Connection Flow

```
Remote UDP client              Server                      rustunnel client
(game client, etc.)          (UDP edge :20100)             (local machine)
      |                             |                             |
      |                             |── RegisterTunnel(Udp) ─────▶|
      |                             |◀── TunnelRegistered ────────|
      |                             |   public_port: 20100        |
      |                             |                             |
      |── UDP datagram ────────────▶|                             |
      |   src=1.2.3.4:5000          |                             |
      |                             |  lookup session             |
      |                             |  (none → create)            |
      |                             |                             |
      |                             |── NewConnection ───────────▶|
      |                             |   conn_id=UUID              |
      |                             |   protocol=Udp              |
      |                             |                             |
      |                             |◀── yamux stream open ───────|
      |                             |                             |
      |                             |  forward queued datagrams   |
      |                             |── [len][payload] ──────────▶|
      |                             |                             |── UDP ─▶ localhost:27015
      |                             |                             |◀── UDP ──|
      |                             |◀── [len][payload] ──────────|
      |◀── UDP datagram ────────────|                             |
      |   from server:20100         |                             |
```

Subsequent datagrams from the same remote source reuse the existing session and yamux stream. Datagrams from a new source create a new session.

---

## Sessions

A **UDP session** is created the first time the server receives a datagram from a new `IP:port`. It holds:

- A forwarding channel used by the edge listener to push datagrams into the yamux stream.
- The last-activity timestamp, used by the reaper.
- A small buffer (up to 64 datagrams) to hold packets that arrive while the client is still opening the yamux stream.

A reaper task runs every 10 seconds and closes any session idle for more than **60 seconds**. Because UDP is connectionless, this is how rustunnel decides when a "session" has ended — there is no FIN or close frame.

On the client side, each session gets its own locally bound UDP socket that `connect()`s to the local service (e.g., `127.0.0.1:27015`). Replies from the local service travel back through the yamux stream and are delivered to the original remote source by the server's UDP edge.

---

## Transport and Framing

UDP datagrams ride over the same yamux-over-WebSocket tunnel used by HTTP and TCP. Because yamux is a reliable byte stream and UDP is message-oriented, rustunnel preserves datagram boundaries with a simple length-prefixed frame:

```
[ 4-byte u32 big-endian length ][ N-byte payload ]
```

- No encoding overhead beyond the 4-byte header.
- Maximum payload size is bounded by UDP itself (65,507 bytes after the IP/UDP header).
- Framing overhead is not counted toward `bytes_proxied` — only raw payload bytes are metered.

Because the transport is yamux, the client's existing WebSocket connection to the server is reused; UDP tunnels do not open any new sockets from client → server.

---

## Server Configuration

UDP tunnels require a dedicated port range in `server.toml`. This range must not overlap with `tcp_port_range` and the ports must be reachable through the host firewall (`ufw allow <low>:<high>/udp`).

```toml
[limits]
# Inclusive [low, high] port range reserved for UDP tunnels.
# Each active UDP tunnel consumes one port from this range.
# Set to [0, 0] to disable UDP tunnels entirely.
udp_port_range = [20100, 20199]
```

Setting `udp_port_range = [0, 0]` disables UDP tunnel registration — any client attempting to register a UDP tunnel will get an error.

The ACL and auth model is the same as for TCP tunnels: tokens that are allowed to open tunnels at all are allowed to open UDP tunnels.

---

## CLI Usage

Expose a local UDP service:

```bash
# Expose a game server (e.g., a Source-engine game on udp/27015)
rustunnel udp 27015 --token YOUR_TOKEN

# Pin to a specific region
rustunnel udp 27015 --region eu --token YOUR_TOKEN

# Local development against a self-hosted server
rustunnel udp 53 --server localhost:4040 --insecure --token dev-secret-change-me
```

On success the client prints the allocated public UDP endpoint, e.g.:

```
UDP tunnel ready: udp://eu.edge.rustunnel.com:20100 → localhost:27015
```

Any datagram sent to that public `host:port` will be forwarded to your local service, and replies are returned to the original sender.

---

## Config File

In `~/.rustunnel/config.yml`:

```yaml
tunnels:
  gameserver:
    proto: udp
    local_port: 27015

  dns:
    proto: udp
    local_port: 53
```

Start all configured tunnels with `rustunnel start`.

---

## Limitations and Notes

- **Datagram size**: Up to 65,507 bytes per datagram. Payloads that exceed the path MTU will fragment at the IP layer; rustunnel does not perform application-level segmentation.
- **Per-source sessions**: Each unique `source_ip:source_port` creates its own session and its own local UDP socket on the client. Clients behind NATs that rewrite source ports aggressively (e.g., some mobile carrier NATs) may see multiple short-lived sessions instead of one long one.
- **Idle reaping**: 60-second inactivity timeout. Services that rely on very infrequent keepalives may need to send periodic traffic to keep the session warm.
- **Rate limiting**: Per-tunnel rate limits apply; each datagram counts as one "request" for the purposes of `rate_limit_rps`.
- **Metering**: `bytes_proxied` counts raw UDP payload bytes only — the 4-byte framing header is excluded.
- **No TLS**: UDP is forwarded as-is. If you need encryption, run it at the application layer (DTLS, QUIC, WireGuard, etc.) inside the tunnel.
- **Port range is separate from TCP**: `udp_port_range` and `tcp_port_range` must not overlap. Pick non-overlapping ranges and open both in the firewall.

---

## Troubleshooting

| Symptom | Likely cause |
|---------|-------------|
| `udp_port_range is disabled` on registration | `udp_port_range = [0, 0]` in `server.toml`. Set a real range. |
| Datagrams reach the server but not the local service | Local firewall blocking the loopback UDP port, or the service isn't actually listening on UDP (check with `ss -ulnp`). |
| Replies from the local service are never seen remotely | The local service isn't replying to the source address rustunnel uses — make sure it replies to the packet's source, not a hard-coded peer. |
| New session every few packets from the same client | Source NAT is rewriting the client's port. This is the client's network, not rustunnel — often unavoidable on mobile. |
| Connection "drops" after ~60 seconds of idle | Idle reaper closed the session. Send a keepalive or reopen on demand. |
| Public UDP port not reachable from the internet | Host firewall isn't open for `udp_port_range`. Run e.g. `ufw allow 20100:20199/udp`. |
