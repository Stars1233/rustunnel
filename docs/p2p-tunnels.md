# P2P Tunnels — How Peer-to-Peer Connections Work

rustunnel supports direct peer-to-peer tunnels between two clients. This allows two machines — neither with a public IP — to communicate through the rustunnel server, with an optional upgrade to a direct connection that bypasses the server entirely.

---

## Table of Contents

1. [Concepts](#concepts)
2. [Connection Modes](#connection-modes)
3. [Server-Relayed Mode](#server-relayed-mode)
4. [Direct Mode (NAT Hole Punching)](#direct-mode-nat-hole-punching)
5. [NAT Classification via STUN](#nat-classification-via-stun)
6. [Hole Punching Strategy](#hole-punching-strategy)
7. [Automatic Fallback](#automatic-fallback)
8. [Security](#security)
9. [Billing and Metering](#billing-and-metering)
10. [CLI Usage](#cli-usage)

---

## Concepts

A P2P tunnel connects two rustunnel clients:

- **Publisher** — the client exposing a local service (e.g., a game server on port 27015). Registers a named tunnel with a shared secret.
- **Subscriber** — the client connecting to the publisher's service. Listens on a local port (e.g., 8000) and forwards incoming TCP connections through the tunnel.
- **Shared secret** — a password both sides must know. The SHA-256 hash is sent to the server for verification; the plaintext never leaves the client.

```
App → Subscriber (localhost:8000) ──tunnel──▶ Publisher (localhost:27015) → Service
```

Unlike HTTP/TCP tunnels, P2P tunnels don't require a public port or subdomain on the server. The server acts as a signaling relay and (optionally) a data relay.

---

## Connection Modes

P2P tunnels support two modes:

| Mode | Data path | Latency | Server load | Metered |
|------|-----------|---------|-------------|---------|
| **Relayed** | App → Subscriber → Server → Publisher → Service | Higher (two WS hops) | Full bandwidth | Yes |
| **Direct** | App → Subscriber → Publisher → Service (via UDP hole punch + QUIC) | Lower (peer-to-peer) | Signaling only | No |

The mode is selected automatically based on NAT compatibility. If direct mode fails or isn't possible, the connection transparently falls back to relayed mode.

---

## Server-Relayed Mode

This is the default mode and always works regardless of NAT type or firewall configuration.

### Connection flow

```
Subscriber                    Server                     Publisher
    |                           |                           |
    |                           |    RegisterTunnel(P2p)    |
    |                           |◀──────────────────────────|
    |                           |    name="my-game"         |
    |                           |    secret_hash="abc..."   |
    |                           |                           |
    |  App connects to          |                           |
    |  localhost:8000            |                           |
    |                           |                           |
    |── P2pConnect ────────────▶|                           |
    |   target="my-game"        |                           |
    |   secret_hash="abc..."    |   verify secret match     |
    |                           |                           |
    |                           |── NewConnection ─────────▶|
    |                           |   (pub_conn_id)           |
    |                           |                           |
    |                           |◀── yamux stream open ─────|
    |                           |   Publisher connects to    |
    |                           |   localhost:27015          |
    |                           |                           |
    |◀── NewConnection ────────|                           |
    |   (sub_conn_id)           |                           |
    |                           |                           |
    |── yamux stream open ─────▶|                           |
    |   Subscriber bridges      |                           |
    |   accepted TCP conn       |                           |
    |                           |                           |
    |◀══════ bidirectional relay via copy_bidirectional ════▶|
```

### Key details

- **On-demand relay**: The relay is established per-connection. Each time an app connects to the subscriber's local port, a new `P2pConnect` is sent and a new relay is created.
- **Yamux multiplexing**: Both publisher and subscriber maintain persistent WebSocket connections to the server. Each relay connection uses a separate yamux stream over these existing connections.
- **No public ports needed**: Neither client needs to accept inbound connections from the internet. Both connect outbound to the server.

---

## Direct Mode (NAT Hole Punching)

Direct mode bypasses the server for the data path. After an initial signaling exchange through the server, the publisher and subscriber establish a direct UDP connection using NAT hole punching, then upgrade it to a QUIC session for reliable, encrypted transport.

### Why direct mode matters

- **Lower latency**: Data travels directly between peers instead of bouncing through the server.
- **Lower server cost**: The server only handles signaling (~1 KB), not the full data stream.
- **Better for real-time applications**: Game servers, VoIP, and live streaming benefit from reduced round-trip time.

---

## NAT Classification via STUN

Before attempting hole punching, each client determines its NAT type using the STUN protocol (Session Traversal Utilities for NAT).

### How STUN probing works

1. The client sends a STUN Binding Request to **two different STUN servers** (e.g., `stun.l.google.com:19302` and `stun1.l.google.com:19302`).
2. Each STUN server replies with the client's **mapped address** — the public IP and port the server saw the request come from.
3. By comparing the two responses, the client classifies its NAT:

```
 Client behind NAT
      |
      |── STUN request ──▶ STUN Server A
      |   Reply: 1.2.3.4:5000
      |
      |── STUN request ──▶ STUN Server B
      |   Reply: ???
      |
      ▼
Compare the two mapped addresses:

Same (1.2.3.4:5000 = 1.2.3.4:5000)  →  Cone NAT (traversable)
Different (1.2.3.4:5000 ≠ 1.2.3.4:6001)  →  Symmetric NAT (hard)
No reply  →  Unknown (use relay)
Public IP matches local IP  →  Open (no NAT)
```

### NAT types

| NAT Type | Description | Hole punch success |
|----------|-------------|-------------------|
| **Open** | No NAT — client has a public IP | ~100% (trivially reachable) |
| **Full Cone** | Same public mapping for all destinations; any external host can send to mapped port | ~100% |
| **Restricted Cone** | Same mapping, but only hosts the client has sent to can reply | ~95% |
| **Port-Restricted Cone** | Same mapping, but restricted by both IP and port | ~90% |
| **Symmetric** | Different mapping for each destination (port changes per target) | ~10-60% depending on peer |

The key distinction: **Cone NATs** reuse the same public port for all outbound connections, making the mapped address predictable. **Symmetric NATs** assign a different port per destination, making prediction unreliable.

---

## Hole Punching Strategy

The server classifies the NAT pair and selects one of three strategies:

### Strategy 1: Direct Exchange (Cone + Cone)

Both peers have predictable mapped addresses. Success rate: ~95%.

```
Publisher (Cone NAT)              Subscriber (Cone NAT)
Public: 1.2.3.4:5000             Public: 5.6.7.8:6000
    |                                 |
    |── UDP probe ───────────────────▶|  (opens NAT mapping)
    |◀── UDP probe ───────────────────|  (opens NAT mapping)
    |                                 |
    |◀═══════ UDP hole established ══▶|
    |       QUIC handshake            |
    |◀═══════ encrypted QUIC ════════▶|
```

Both sides send a UDP probe to the other's mapped address. The first probe "punches" the hole in each NAT by creating an outbound mapping. The second probe (or a reply) passes through because the NAT now recognizes the address pair.

### Strategy 2: Port Prediction (Cone + Symmetric)

The Cone peer has a predictable address. The Symmetric peer's port changes per destination, but the server observes the port increment pattern and predicts the next port.

```
Publisher (Symmetric)             Subscriber (Cone NAT)
Port changes per dest             Public: 5.6.7.8:6000
    |                                 |
    |  Server predicts port range     |
    |  e.g., ports 5010-5020          |
    |                                 |
    |◀── probes to ports 5010-5020 ──|  (Cone sends to predicted range)
    |── probe to 5.6.7.8:6000 ──────▶|  (Symmetric sends to Cone's known addr)
    |                                 |
    |◀═══════ UDP hole established ══▶|
```

Success rate: ~60-70%. Depends on how predictable the Symmetric NAT's port allocation is.

### Strategy 3: Skip (Symmetric + Symmetric)

Both peers have unpredictable port mappings. Brute-force probing (sending to hundreds of random ports) has a success rate under 10% and can trigger firewall alarms. Not worth the delay.

**Decision: fall back to relay immediately.** No hole punching attempted. The connection works via the server relay with zero additional delay.

### Decision matrix

| Publisher NAT | Subscriber NAT | Strategy | Expected success |
|---------------|----------------|----------|-----------------|
| Open/Cone | Open/Cone | Direct Exchange | ~95% |
| Cone | Symmetric | Port Prediction | ~60-70% |
| Symmetric | Cone | Port Prediction | ~60-70% |
| Symmetric | Symmetric | **Skip → Relay** | N/A |
| Unknown | Any | **Skip → Relay** | N/A |
| Any | Unknown | **Skip → Relay** | N/A |

---

## Automatic Fallback

When direct mode is attempted but fails, the connection transparently falls back to server relay:

```
1. Subscriber connects to publisher via P2pConnect
2. Server classifies NAT pair
3. If traversable:
   a. Server sends hole-punch instructions to both peers
   b. Peers send UDP probes (5-second timeout)
   c. If probes succeed → establish QUIC session → direct mode
   d. If probes fail → fall back to relay
4. If not traversable:
   a. Skip hole punching entirely
   b. Use relay immediately (no extra delay)
```

From the user's perspective:

- **Direct succeeded**: Connection established in ~1-2 seconds. Lower latency.
- **Direct failed**: Connection established in ~6-7 seconds (5s punch timeout + 1-2s relay setup). Same functionality, slightly higher latency.
- **Direct skipped**: Connection established in ~1-2 seconds via relay. No delay penalty.

The subscriber never needs to know which mode is active. The CLI command is identical in all cases.

---

## Security

### Shared secret authentication

Both publisher and subscriber must know the same shared secret. The flow:

1. Client computes `SHA-256(secret)` locally.
2. Only the **hash** is sent to the server in `RegisterTunnel` (publisher) and `P2pConnect` (subscriber).
3. The server compares hashes. If they don't match, the connection is rejected.
4. The plaintext secret never leaves the client and is never stored on the server.

### Transport encryption

- **Relayed mode**: Data is encrypted via the TLS WebSocket connections to the server. The server can see the plaintext (it's relaying bytes). This is the same trust model as HTTP/TCP tunnels.
- **Direct mode**: Data is encrypted end-to-end via QUIC (TLS 1.3). The shared secret is used to derive a pre-shared key for mutual authentication. The server cannot see the plaintext.

### Tunnel name visibility

P2P tunnel names (e.g., `my-game`) are visible to the server. Anyone who knows the tunnel name AND the shared secret can connect. Choose strong, unique secrets for production use.

---

## Billing and Metering

| Mode | Metered by server? | Billing |
|------|-------------------|---------|
| **Relayed** | Yes — all bytes flow through the server | Per-byte, same as TCP tunnels |
| **Direct** | No — data bypasses the server | Client-reported metrics (informational only) |

Direct mode is a value-add that reduces server bandwidth costs. Relay mode is always available as a fallback and is the billing-accurate path.

---

## CLI Usage

### Publisher (expose a service)

```bash
rustunnel p2p 27015 --name my-game --secret "shared-secret-123"
```

This exposes `localhost:27015` as a P2P tunnel named `my-game`.

### Subscriber (connect to a service)

```bash
rustunnel p2p 8000 --target my-game --secret "shared-secret-123"
```

This listens on `localhost:8000`. Any TCP connection to that port is forwarded through the tunnel to the publisher's `localhost:27015`.

### Config file

```yaml
tunnels:
  # Publisher
  gameserver:
    proto: p2p
    local_port: 27015
    p2p_name: my-game
    p2p_secret: shared-secret-123

  # Subscriber
  connect:
    proto: p2p
    local_port: 8000
    p2p_target: my-game
    p2p_secret: shared-secret-123
```

### Error cases

| Error | Cause |
|-------|-------|
| `P2P tunnel name 'X' is already in use` | Another publisher is using this name |
| `P2P tunnel 'X' not found` | No publisher registered with this name |
| `invalid P2P secret` | Subscriber's secret doesn't match publisher's |
| `P2P mode requires --name or --target` | Neither publisher nor subscriber mode specified |
