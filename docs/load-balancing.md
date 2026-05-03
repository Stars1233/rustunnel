# Load Balancing & Health Checks

rustunnel supports group-based load balancing for HTTP and TCP tunnels.
Multiple clients can register against the same subdomain (HTTP) or share a
TCP port pool, and inbound connections are dispatched at random across
healthy members of the group. Optional client-side health probes
automatically remove sick backends from the rotation.

The model is modeled on FRP's
[`loadBalancer.group` / `healthCheck`](https://github.com/fatedier/frp#load-balancing)
config — same shape, slightly different wire format.

---

## Concepts

- **Group** — a logical pool of tunnel members sharing the same subdomain
  (HTTP) or TCP port. Identified by a user-supplied `group` name plus a
  shared `group_key`. The server stores only the SHA-256 hash of the key
  and uses it to authorise joins; the raw key never leaves the client.
- **Member** — one tunnel inside a group. A client running
  `rustunnel start` with `group: web` registers exactly one member; running
  two clients with the same `(group, group_key)` produces a 2-member pool.
- **Health bit** — every member has a `healthy` flag. Dispatch routes
  around members whose flag is `false`. Without a health check configured,
  members are permanently healthy (the server trusts the client's presence).
- **Dispatch** — for each new public connection, the server picks one
  healthy member uniformly at random. There's no weighting and no sticky
  sessions today.

```
                  +-> client A -> backend on :3000
public ─-->  ── group "web"
                  +-> client B -> backend on :3001
```

---

## Configuration

### Server (`server.toml`)

The kill switch. When `false` (the default), the server accepts the new
fields on the wire but ignores them — every registration is a solo tunnel,
preserving v0.6.0 behaviour. When `true`, members sharing
`(subdomain, group_key_hash)` (HTTP) or `(group_name, group_key_hash)`
(TCP) form a real pool.

```toml
[load_balancing]
enabled = true
```

### Client (`~/.rustunnel/config.yml`)

Add `group`, `group_key`, and (optionally) `health_check` to a tunnel
definition:

```yaml
server: tunnel.example.com:4040
auth_token: "your-token"

tunnels:
  a:
    proto: http
    local_port: 3000
    subdomain: pool
    group: web
    group_key: shared-secret-for-this-pool
    health_check:
      type: tcp
      interval_secs: 10
      timeout_secs: 3
      max_failed: 3
```

| Field | Required | Default | Meaning |
|---|---|---|---|
| `group` | yes (for LB) | — | Display name of the pool. Doesn't have to match across clients; the first joiner sets `TunnelGroup.name`, the rest are accepted regardless of what they pass. |
| `group_key` | yes (for LB) | — | Shared secret. SHA-256-hashed before transmission. Members of one pool MUST agree on this value; the server rejects a join with a mismatched key. |
| `health_check.type` | no | — | `tcp` (open a connection) or `http` (issue a `GET`). Omit to disable probing — the member stays permanently healthy. |
| `health_check.path` | yes when `type: http` | — | Path to GET against the local service. |
| `health_check.interval_secs` | no | `10` | Probe period. |
| `health_check.timeout_secs` | no | `3` | Per-probe deadline. |
| `health_check.max_failed` | no | `3` | Consecutive failures before reporting `TunnelUnhealthy`. |
| `health_check.expect_2xx` | no | `true` | When `false`, any HTTP response counts as healthy. |

### Behaviour rules

- **HTTP groups**: members must declare the **same protocol** (`http` vs
  `https`). A mismatch is rejected with a clear error.
- **TCP groups**: the first member of a `(group, group_key)` allocates a
  port from the configured `tcp_port_range`. Subsequent members reuse that
  port; the server returns the same `assigned_port` to all joiners.
- **Solo collisions**: registering a solo (no-group) tunnel against an
  existing group's subdomain is rejected with `subdomain '...' is already
  in use`. Registering a grouped tunnel against an existing solo tunnel is
  rejected with `group key does not match`.
- **Last-leave**: the group entry is removed when its last member
  disconnects. The TCP port (if any) is returned to the pool.
- **Race safety**: the create / join / remove paths are serialised
  atomically via the routing-table entry API. Two concurrent first
  registrations produce one group, not two.

---

## Health checks

Probes run **on the client** against `local_addr`. The server never opens
a connection to the upstream itself — it just trusts the client's
`TunnelHealthy` / `TunnelUnhealthy` reports.

- **TCP probe**: opens a TCP connection. Success = connect within
  `timeout_secs`.
- **HTTP probe**: sends `GET <path> HTTP/1.0` and reads the status line.
  Success = response within `timeout_secs` and (when `expect_2xx`) status
  in `[200, 300)`.

Probe state is reported only on **edges**:

- First probe success → `TunnelHealthy` (lifts the initial `healthy=false`
  state for members that opted into probing).
- `max_failed` consecutive failures → `TunnelUnhealthy`.
- First success after a failure streak → `TunnelHealthy`.

A member with no `health_check` is permanently healthy. A member *with* a
spec starts unhealthy and only joins dispatch after the first successful
probe.

---

## Testing the feature locally

Quick end-to-end smoke test against a self-hosted edge with
`[load_balancing] enabled = true`. Spin up two clients with the same
`(group, group_key)`, point them at separate local backends, and hammer
the public URL — both backends should serve.

```bash
# Build the client locally from main (no release tag needed)
cd ~/projects/rustunnel/rustunnel
cargo build --release -p rustunnel-client

# Drop a config that opts into a group
cat > /tmp/lb-test.yml <<'EOF'
server: tunnel.example.com:4040
auth_token: "your-token"

tunnels:
  a:
    proto: http
    local_port: 3000
    subdomain: lbtest
    group: web
    group_key: shared-secret-for-lb-test
    health_check:
      type: tcp
EOF

# Terminal 1 — backend A on :3000
python3 -m http.server 3000

# Terminal 2 — client A pointing at backend A
./target/release/rustunnel start --config /tmp/lb-test.yml

# Terminal 3 — backend B on :3001
python3 -m http.server 3001

# Terminal 4 — client B with local_port 3001 (edit /tmp/lb-test.yml or use a second config file)

# Terminal 5 — hammer the public URL
for i in $(seq 1 50); do
  curl -fsS https://lbtest.tunnel.example.com/ -o /dev/null -w "%{http_code}\n"
done
```

Validate the dispatch by reading the per-group counters from the metrics
endpoint on the edge:

```bash
ssh root@tunnel.example.com 'curl -sf http://127.0.0.1:9090/metrics' \
  | grep '^rustunnel_group_'
```

You should see something like:

```
rustunnel_group_members{group="web",region="eu",healthy="true"} 2
rustunnel_group_members{group="web",region="eu",healthy="false"} 0
rustunnel_group_dispatches_total{group="web",region="eu"} 50
rustunnel_group_health_failures_total{group="web",region="eu",kind="tcp"} 0
```

Verify failover by killing one of the local backends — the probe loop on
that client will mark it unhealthy after `max_failed * interval_secs`
seconds, after which dispatch routes everything to the survivor. Restart
the backend, the probe re-registers it as healthy, and dispatch
distributes again.

---

## Observability

When `[load_balancing] enabled = true`, the Prometheus exporter on `:9090`
emits three additional series:

| Metric | Type | Labels | What it measures |
|---|---|---|---|
| `rustunnel_group_members` | gauge | `group`, `region`, `healthy` | Count of registered members partitioned by their health bit. |
| `rustunnel_group_dispatches_total` | counter | `group`, `region` | Total dispatched connections, summed across the group's members. (Per-group rather than per-member to keep cardinality bounded.) |
| `rustunnel_group_health_failures_total` | counter | `group`, `region`, `kind` | Total `TunnelUnhealthy` frames received across the group's members. `kind` is `tcp` / `http` / `none` based on the configured probe type. |

The pre-existing `rustunnel_active_tunnels_*` and `rustunnel_requests_total`
gauges/counters keep counting **members** (not groups) so historical
dashboards stay accurate.

---

## Limitations & non-goals

- **No weighted dispatch** — random uniform only. Pick weights are not
  configurable.
- **No sticky sessions** — every new connection is dispatched
  independently. Long-lived WebSocket connections that need to land on the
  same backend across reconnects need to be solved at the application
  layer.
- **No active session draining on member removal** — when a member
  disconnects (or is marked unhealthy), in-flight connections finish
  naturally; new connections route elsewhere.
- **No UDP groups** — UDP is connectionless; there's no obvious unit to
  dispatch.
- **No P2P groups** — P2P publishers are 1-to-many by design; multiple
  publishers under one name is a different problem.
- **No cross-region pools** — members must be on the same edge server.
  Layer DNS-based routing on top for global LB.
- **No `groupKey` rotation** — once a group exists, rotating its key
  requires dropping all members.

---

## See also

- [Client Guide — Multiple backends](./client-guide.md#multiple-backends-load-balancing)
- [Architecture overview](./architecture.md)
