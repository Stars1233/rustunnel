use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::io::DuplexStream;
use tokio::sync::{mpsc, Semaphore};
use uuid::Uuid;

use rustunnel_protocol::{HealthCheckSpec, TunnelProtocol};

// ── TCP tunnel events (edge ↔ core) ───────────────────────────────────────────

/// Broadcast by `TunnelCore` whenever a TCP tunnel is added or removed.
/// The TCP edge layer subscribes to this to manage per-port listeners.
#[derive(Debug, Clone)]
pub enum TcpTunnelEvent {
    Registered { tunnel_id: Uuid, port: u16 },
    Unregistered { port: u16 },
}

/// Broadcast by `TunnelCore` whenever a UDP tunnel is added or removed.
/// The UDP edge layer subscribes to this to manage per-port listeners.
#[derive(Debug, Clone)]
pub enum UdpTunnelEvent {
    Registered { tunnel_id: Uuid, port: u16 },
    Unregistered { port: u16 },
}

// ── control-plane message ─────────────────────────────────────────────────────

/// Messages the router sends down a session's control channel.
#[derive(Debug)]
pub enum ControlMessage {
    /// A new public connection has arrived and must be proxied.
    NewConnection {
        conn_id: Uuid,
        client_addr: SocketAddr,
        protocol: TunnelProtocol,
    },
    /// Instruct the session handler to tear down cleanly.
    Shutdown,
}

// ── per-tunnel state ──────────────────────────────────────────────────────────

/// Lightweight, clone-able view of a registered tunnel.
#[derive(Debug, Clone)]
pub struct TunnelInfo {
    pub session_id: Uuid,
    pub tunnel_id: Uuid,
    pub protocol: TunnelProtocol,
    /// Present for HTTP/HTTPS tunnels; `None` for TCP.
    pub subdomain: Option<String>,
    /// Present for TCP tunnels; `None` for HTTP/HTTPS.
    pub assigned_port: Option<u16>,
    pub created_at: Instant,
    /// Monotonically-increasing counter of proxied requests/connections.
    pub request_count: Arc<AtomicU64>,
    /// Total bytes proxied through this tunnel (upstream + downstream combined).
    pub bytes_proxied: Arc<AtomicU64>,
    /// Limits concurrent proxied connections for this tunnel.
    /// Shared across all clones so every proxy task draws from the same pool.
    pub conn_semaphore: Arc<Semaphore>,
}

// ── Load-balancing groups (TUNNEL-7) ──────────────────────────────────────────
//
// A `TunnelGroup` is the routing entry for an HTTP subdomain or a TCP/UDP
// port. It holds one or more `GroupMember`s — each backed by a separate
// `RegisterTunnel` from a separate client. Phase 1 only ever has a single
// member per group (the "degenerate" case); multi-member registration arrives
// in Phase 2/3.
//
// Storing this on the routing-table value side (instead of a separate map of
// pools) makes resolution a single DashMap lookup and lets remove-on-last-leave
// happen atomically next to the existing route-removal code.

/// One backend in a `TunnelGroup`.
///
/// `healthy` defaults to `true` when no `health_spec` is set (we trust the
/// client's presence) and to `false` when a spec exists, until the first
/// `TunnelHealthy` arrives.
#[derive(Debug)]
pub struct GroupMember {
    pub info: TunnelInfo,
    pub healthy: AtomicBool,
    /// Health-check spec from `RegisterTunnel.health_check`; `None` if the
    /// client didn't ask for probes.
    pub health_spec: Option<HealthCheckSpec>,
    /// Current consecutive-failure streak — resets to 0 each time a
    /// `TunnelHealthy` arrives. Surfaced on the dashboard so operators can
    /// see "this member is currently down on its 3rd straight failure".
    pub consecutive_failures: AtomicU32,
    /// Cumulative count of `TunnelUnhealthy` frames received since this
    /// member registered. Drives the
    /// `rustunnel_group_health_failures_total{group, kind}` Prometheus
    /// counter (Phase 5). Never resets while the member is registered;
    /// goes away when the member leaves the group.
    pub total_health_failures: AtomicU64,
}

impl GroupMember {
    /// Create a member that's immediately considered healthy. Used for the
    /// Phase 1 path where no health-check spec is attached.
    pub fn healthy_with(info: TunnelInfo) -> Self {
        Self {
            info,
            healthy: AtomicBool::new(true),
            health_spec: None,
            consecutive_failures: AtomicU32::new(0),
            total_health_failures: AtomicU64::new(0),
        }
    }

    /// Create a member with an optional health-check spec.
    ///
    /// Initial `healthy` follows §4.5 of the plan: a member with no spec is
    /// trusted (presence ⇒ healthy); a member that *did* opt into probes
    /// starts unhealthy and only flips to healthy on the first
    /// `TunnelHealthy` frame from the client. This prevents routing real
    /// traffic to a backend whose upstream we haven't probed yet.
    pub fn with_health_spec(info: TunnelInfo, spec: Option<HealthCheckSpec>) -> Self {
        let initially_healthy = spec.is_none();
        Self {
            info,
            healthy: AtomicBool::new(initially_healthy),
            health_spec: spec,
            consecutive_failures: AtomicU32::new(0),
            total_health_failures: AtomicU64::new(0),
        }
    }
}

/// A pool of one or more members serving the same subdomain or port.
#[derive(Debug)]
pub struct TunnelGroup {
    /// Display name (== subdomain for HTTP, port-as-string for TCP/UDP, or
    /// the user-supplied `group` from `RegisterTunnel` once Phase 2 lands).
    pub name: String,
    /// SHA-256 of the user-supplied `group_key`. `None` for ungrouped /
    /// degenerate single-member registrations (Phase 1).
    pub key_hash: Option<String>,
    /// Members keyed by their `tunnel_id`.
    pub members: DashMap<Uuid, GroupMember>,
}

impl TunnelGroup {
    pub fn new_solo(name: String, info: TunnelInfo) -> Arc<Self> {
        let group = Arc::new(Self {
            name,
            key_hash: None,
            members: DashMap::new(),
        });
        group
            .members
            .insert(info.tunnel_id, GroupMember::healthy_with(info));
        group
    }

    /// Create a fresh group seeded with `member`. Phase 2 of TUNNEL-7 uses
    /// this for the first registration of a multi-member HTTP group; Phase
    /// 3 will reuse it for TCP. `key_hash` is `Some` for grouped pools and
    /// `None` for ungrouped/solo registrations (use `new_solo` in that
    /// case for clarity).
    pub fn new_with_member(
        name: String,
        key_hash: Option<String>,
        member: GroupMember,
    ) -> Arc<Self> {
        let tunnel_id = member.info.tunnel_id;
        let group = Arc::new(Self {
            name,
            key_hash,
            members: DashMap::new(),
        });
        group.members.insert(tunnel_id, member);
        group
    }
}

/// User-supplied parameters for joining or creating a load-balancing group.
///
/// Built by the session frame handler from `RegisterTunnel.group` /
/// `RegisterTunnel.group_key_hash` / `RegisterTunnel.health_check` once the
/// `[load_balancing] enabled = true` kill switch is on. Passed into
/// `TunnelCore::register_http_tunnel` (Phase 2) and
/// `register_tcp_tunnel` (Phase 3, future).
#[derive(Debug, Clone)]
pub struct GroupSpec {
    /// User-supplied display name (sets `TunnelGroup.name` on the first
    /// registration; subsequent joiners can supply any value, the existing
    /// name wins — same as FRP).
    pub group_name: String,
    /// SHA-256 hash of the user-supplied `group_key`. Must match across
    /// every member of the group.
    pub key_hash: String,
    /// Optional health-check spec for this member.
    pub health_check: Option<HealthCheckSpec>,
}

// ── per-session state ─────────────────────────────────────────────────────────

/// Live state for a connected client session.
pub struct SessionInfo {
    /// Remote address the client connected from.
    pub client_addr: SocketAddr,
    /// Opaque identifier of the auth token used (empty string when auth is disabled).
    pub auth_token_id: String,
    /// The `tokens.id` (UUID) of the authenticated token, if it came from the DB.
    /// `None` for the admin token or when auth is disabled.
    pub db_token_id: Option<String>,
    /// Channel for sending control messages to the session handler task.
    pub control_tx: mpsc::Sender<ControlMessage>,
    /// Tunnel IDs owned by this session.
    pub tunnels: Vec<Uuid>,
    pub connected_at: Instant,
    /// Updated on every Ping/Pong exchange.
    pub last_heartbeat: RwLock<Instant>,
    /// Loopback pipe endpoint stored here until the `/_data/<session_id>`
    /// WebSocket connection arrives and takes it for bridging.
    pub data_pipe: Option<DuplexStream>,
}

impl SessionInfo {
    pub fn new(
        client_addr: SocketAddr,
        auth_token_id: String,
        db_token_id: Option<String>,
        control_tx: mpsc::Sender<ControlMessage>,
    ) -> Self {
        let now = Instant::now();
        Self {
            client_addr,
            auth_token_id,
            db_token_id,
            control_tx,
            tunnels: Vec::new(),
            connected_at: now,
            last_heartbeat: RwLock::new(now),
            data_pipe: None,
        }
    }
}
