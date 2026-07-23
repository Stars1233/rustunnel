//! Client-side request inspection.
//!
//! The client historically forwarded bytes without understanding them, so it
//! could never show what was flowing through a tunnel. This module adds a
//! passive HTTP tap over the proxy byte streams ([`tap`]) plus the shared state
//! that both consumers read from:
//!
//! * the terminal UI ([`crate::tui`]) — live request log and counters
//! * the local web inspector ([`server`]) — browsable history, bodies, replay
//!
//! Captured exchanges mirror the server-side `CaptureEvent` / `CapturedRequest`
//! shape (`rustunnel-server/src/edge/capture.rs`) so both inspectors describe a
//! request the same way. Unlike the server, the client also keeps headers and
//! (size-capped) bodies, which is what makes replay and payload viewing
//! possible.
//!
//! Everything here is in-memory only: a bounded ring buffer per process, never
//! persisted, never reachable off-loopback.

pub mod server;
pub mod tap;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, Notify};
use uuid::Uuid;

/// Exchanges retained in memory. Matches the server's dashboard ring capacity.
pub const RING_CAPACITY: usize = 500;

/// Per-body capture cap. Larger bodies are truncated (the exchange still
/// records the true byte count).
pub const BODY_CAP: usize = 64 * 1024;

/// Log lines retained for the TUI log pane.
const LOG_CAPACITY: usize = 500;

// ── captured data ─────────────────────────────────────────────────────────────

/// A captured body prefix.
#[derive(Debug, Clone, Default)]
pub struct Body {
    /// Captured prefix, at most [`BODY_CAP`] bytes.
    pub bytes: Vec<u8>,
    /// Total body bytes observed (may exceed `bytes.len()`).
    pub total: u64,
    /// True when `total > bytes.len()`.
    pub truncated: bool,
}

impl Body {
    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let room = BODY_CAP.saturating_sub(self.bytes.len());
        if room == 0 {
            self.truncated = self.total > self.bytes.len() as u64;
            return;
        }
        let take = room.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..take]);
        self.truncated = self.total > self.bytes.len() as u64;
    }

    /// Body as text when it is valid UTF-8, else `None` (binary payload).
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }
}

/// One HTTP request/response pair observed on a tunnel.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub id: u64,
    pub conn_id: Uuid,
    /// Tunnel label (subdomain or configured name).
    pub tunnel: String,
    /// Public client address, from the `NewConnection` control frame.
    pub client_addr: String,
    pub method: String,
    pub path: String,
    pub host: Option<String>,
    pub status: u16,
    pub request_headers: Vec<(String, String)>,
    pub response_headers: Vec<(String, String)>,
    pub request_body: Body,
    pub response_body: Body,
    pub duration_ms: u64,
    pub started_at: DateTime<Utc>,
    /// True when this exchange was produced by the inspector's replay button
    /// rather than by real inbound traffic.
    pub replayed: bool,
}

impl Exchange {
    /// Compact JSON for list views — no headers or bodies.
    pub fn to_summary_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "conn_id": self.conn_id.to_string(),
            "tunnel": self.tunnel,
            "client_addr": self.client_addr,
            "method": self.method,
            "path": self.path,
            "status": self.status,
            "duration_ms": self.duration_ms,
            "request_bytes": self.request_body.total,
            "response_bytes": self.response_body.total,
            "started_at": self.started_at.to_rfc3339(),
            "replayed": self.replayed,
        })
    }

    /// Full JSON including headers and captured body prefixes.
    pub fn to_detail_json(&self) -> serde_json::Value {
        let mut v = self.to_summary_json();
        v["host"] = serde_json::json!(self.host);
        v["request_headers"] = headers_json(&self.request_headers);
        v["response_headers"] = headers_json(&self.response_headers);
        v["request_body"] = body_json(&self.request_body);
        v["response_body"] = body_json(&self.response_body);
        v
    }
}

fn headers_json(headers: &[(String, String)]) -> serde_json::Value {
    serde_json::Value::Array(
        headers
            .iter()
            .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
            .collect(),
    )
}

fn body_json(body: &Body) -> serde_json::Value {
    match body.as_text() {
        Some(text) => serde_json::json!({
            "text": text,
            "binary": false,
            "size": body.total,
            "truncated": body.truncated,
        }),
        None => serde_json::json!({
            "text": serde_json::Value::Null,
            "binary": true,
            "size": body.total,
            "truncated": body.truncated,
            "preview": hex_preview(&body.bytes),
        }),
    }
}

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(256)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ── session state ─────────────────────────────────────────────────────────────

/// Connection state of the tunnel session, as shown in the UI header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Connecting,
    Online,
    Reconnecting { attempt: u32 },
    Closed,
}

impl SessionStatus {
    pub fn label(&self) -> String {
        match self {
            SessionStatus::Connecting => "connecting".into(),
            SessionStatus::Online => "online".into(),
            SessionStatus::Reconnecting { attempt } => format!("reconnecting #{attempt}"),
            SessionStatus::Closed => "closed".into(),
        }
    }
}

/// A registered tunnel, as shown in the UI tunnel list.
#[derive(Debug, Clone, Serialize)]
pub struct TunnelInfo {
    pub name: String,
    pub proto: String,
    pub local: String,
    pub public_url: String,
    /// `None` when no health check is configured for this tunnel.
    pub healthy: Option<bool>,
}

/// Everything the UIs show that is not a captured request.
#[derive(Debug, Clone)]
pub struct Session {
    pub status: SessionStatus,
    pub server: String,
    pub region: Option<String>,
    /// Control-plane round-trip time, measured from Ping/Pong.
    pub latency_ms: Option<u64>,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub inspect_url: Option<String>,
    pub tunnels: Vec<TunnelInfo>,
}

// ── counters ──────────────────────────────────────────────────────────────────

/// Live traffic counters. Updated from the proxy tasks.
#[derive(Debug, Default)]
pub struct Stats {
    pub conns_open: AtomicU64,
    pub conns_total: AtomicU64,
    pub bytes_to_local: AtomicU64,
    pub bytes_to_tunnel: AtomicU64,
    pub requests_total: AtomicU64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            conns_open: self.conns_open.load(Ordering::Relaxed),
            conns_total: self.conns_total.load(Ordering::Relaxed),
            bytes_to_local: self.bytes_to_local.load(Ordering::Relaxed),
            bytes_to_tunnel: self.bytes_to_tunnel.load(Ordering::Relaxed),
            requests_total: self.requests_total.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct StatsSnapshot {
    pub conns_open: u64,
    pub conns_total: u64,
    pub bytes_to_local: u64,
    pub bytes_to_tunnel: u64,
    pub requests_total: u64,
}

// ── inspector ─────────────────────────────────────────────────────────────────

/// Shared runtime state for the whole client session.
///
/// Created once in `main`, shared by the proxy tasks (writers), the terminal UI
/// and the web inspector (readers). One instance always exists; `capture`
/// decides whether the HTTP tap actually runs, so `--json` / non-TTY runs keep
/// the original zero-overhead byte-copy path.
pub struct Inspector {
    /// Whether proxied HTTP connections should be tapped.
    capture: bool,
    store: Mutex<VecDeque<Arc<Exchange>>>,
    events: broadcast::Sender<Arc<Exchange>>,
    next_id: AtomicU64,
    pub stats: Stats,
    session: Mutex<Session>,
    logs: Mutex<VecDeque<String>>,
    shutdown: Notify,
    shutdown_flag: AtomicBool,
}

impl Inspector {
    pub fn new(capture: bool, server: String, region: Option<String>) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            capture,
            store: Mutex::new(VecDeque::with_capacity(64)),
            events,
            next_id: AtomicU64::new(1),
            stats: Stats::default(),
            session: Mutex::new(Session {
                status: SessionStatus::Connecting,
                server,
                region,
                latency_ms: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
                started_at: Utc::now(),
                inspect_url: None,
                tunnels: Vec::new(),
            }),
            logs: Mutex::new(VecDeque::with_capacity(64)),
            shutdown: Notify::new(),
            shutdown_flag: AtomicBool::new(false),
        })
    }

    /// True when proxied HTTP traffic should be parsed and recorded.
    pub fn capture_enabled(&self) -> bool {
        self.capture
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Exchange>> {
        self.events.subscribe()
    }

    /// Record a completed exchange: assign an id, store it, notify subscribers.
    pub fn record(&self, mut exchange: Exchange) -> Arc<Exchange> {
        exchange.id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let exchange = Arc::new(exchange);

        {
            let mut store = self.store.lock().unwrap();
            if store.len() == RING_CAPACITY {
                store.pop_front();
            }
            store.push_back(Arc::clone(&exchange));
        }
        self.stats.requests_total.fetch_add(1, Ordering::Relaxed);

        // A send error only means nobody is subscribed right now.
        let _ = self.events.send(Arc::clone(&exchange));
        exchange
    }

    /// Most recent exchanges, newest first.
    pub fn exchanges(&self) -> Vec<Arc<Exchange>> {
        let store = self.store.lock().unwrap();
        store.iter().rev().cloned().collect()
    }

    pub fn exchange(&self, id: u64) -> Option<Arc<Exchange>> {
        let store = self.store.lock().unwrap();
        store.iter().find(|e| e.id == id).cloned()
    }

    pub fn clear(&self) {
        self.store.lock().unwrap().clear();
    }

    /// p50 / p90 request duration over the retained exchanges.
    pub fn latency_percentiles(&self) -> Option<(u64, u64)> {
        let store = self.store.lock().unwrap();
        if store.is_empty() {
            return None;
        }
        let mut durations: Vec<u64> = store.iter().map(|e| e.duration_ms).collect();
        durations.sort_unstable();
        Some((percentile(&durations, 50), percentile(&durations, 90)))
    }

    // ── session accessors ────────────────────────────────────────────────

    pub fn session(&self) -> Session {
        self.session.lock().unwrap().clone()
    }

    pub fn set_status(&self, status: SessionStatus) {
        self.session.lock().unwrap().status = status;
    }

    pub fn set_latency(&self, latency_ms: u64) {
        self.session.lock().unwrap().latency_ms = Some(latency_ms);
    }

    pub fn set_inspect_url(&self, url: String) {
        self.session.lock().unwrap().inspect_url = Some(url);
    }

    pub fn set_server(&self, server: String) {
        self.session.lock().unwrap().server = server;
    }

    pub fn set_tunnels(&self, tunnels: Vec<TunnelInfo>) {
        self.session.lock().unwrap().tunnels = tunnels;
    }

    /// Update the health flag of a tunnel identified by its local address.
    pub fn set_tunnel_health(&self, local: &str, healthy: bool) {
        let mut session = self.session.lock().unwrap();
        for tunnel in session.tunnels.iter_mut() {
            if tunnel.local == local {
                tunnel.healthy = Some(healthy);
            }
        }
    }

    /// Local address of the tunnel a request arrived on, for replay.
    ///
    /// Matches by name only. Falling back to another tunnel would let a replay
    /// hit the wrong local service — with several HTTP tunnels registered, that
    /// could re-issue a mutating request against a different app. Callers
    /// surface `None` as a conflict instead.
    pub fn local_addr_for(&self, tunnel: &str) -> Option<String> {
        let session = self.session.lock().unwrap();
        session
            .tunnels
            .iter()
            .find(|t| t.name == tunnel)
            .map(|t| t.local.clone())
    }

    // ── log buffer (TUI log pane) ────────────────────────────────────────

    pub fn push_log(&self, line: String) {
        let mut logs = self.logs.lock().unwrap();
        if logs.len() == LOG_CAPACITY {
            logs.pop_front();
        }
        logs.push_back(line);
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    // ── shutdown ─────────────────────────────────────────────────────────

    /// Ask the tunnel session to shut down (TUI quit key).
    pub fn request_shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        self.shutdown.notify_waiters();
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_flag.load(Ordering::SeqCst)
    }

    /// Resolves once [`request_shutdown`](Self::request_shutdown) is called.
    pub async fn shutdown_signal(&self) {
        if self.shutdown_requested() {
            return;
        }
        self.shutdown.notified().await;
    }
}

/// Nearest-rank percentile of a pre-sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (pct * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Human-readable byte count for the UIs.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(status: u16, duration_ms: u64) -> Exchange {
        Exchange {
            id: 0,
            conn_id: Uuid::nil(),
            tunnel: "web".into(),
            client_addr: "203.0.113.7:51000".into(),
            method: "GET".into(),
            path: "/".into(),
            host: Some("example.test".into()),
            status,
            request_headers: vec![("host".into(), "example.test".into())],
            response_headers: vec![],
            request_body: Body::default(),
            response_body: Body::default(),
            duration_ms,
            started_at: Utc::now(),
            replayed: false,
        }
    }

    #[test]
    fn body_capture_truncates_at_cap_but_counts_all_bytes() {
        let mut body = Body::default();
        body.push(&vec![b'a'; BODY_CAP - 10]);
        body.push(&[b'b'; 100]);
        assert_eq!(body.bytes.len(), BODY_CAP);
        assert_eq!(body.total, (BODY_CAP - 10 + 100) as u64);
        assert!(body.truncated);
    }

    #[test]
    fn small_body_is_not_marked_truncated() {
        let mut body = Body::default();
        body.push(b"hello");
        assert_eq!(body.as_text(), Some("hello"));
        assert!(!body.truncated);
        assert_eq!(body.total, 5);
    }

    #[test]
    fn ring_buffer_evicts_oldest_beyond_capacity() {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        for i in 0..(RING_CAPACITY + 25) {
            inspector.record(exchange(200, i as u64));
        }
        let stored = inspector.exchanges();
        assert_eq!(stored.len(), RING_CAPACITY);
        // Newest first, and ids keep counting past the ring capacity.
        assert_eq!(stored[0].id, (RING_CAPACITY + 25) as u64);
        assert_eq!(
            inspector.stats.requests_total.load(Ordering::Relaxed),
            (RING_CAPACITY + 25) as u64
        );
    }

    #[test]
    fn recorded_exchanges_are_broadcast_and_retrievable_by_id() {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        let mut rx = inspector.subscribe();
        let recorded = inspector.record(exchange(201, 5));
        assert_eq!(rx.try_recv().unwrap().id, recorded.id);
        assert_eq!(inspector.exchange(recorded.id).unwrap().status, 201);
        assert!(inspector.exchange(9999).is_none());
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let sorted: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&sorted, 50), 5);
        assert_eq!(percentile(&sorted, 90), 9);
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[42], 90), 42);
    }

    #[test]
    fn latency_percentiles_none_when_empty() {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        assert!(inspector.latency_percentiles().is_none());
        for d in [10, 20, 30, 40, 500] {
            inspector.record(exchange(200, d));
        }
        let (p50, p90) = inspector.latency_percentiles().unwrap();
        assert_eq!(p50, 30);
        assert_eq!(p90, 500);
    }

    #[test]
    fn shutdown_flag_is_observable_before_awaiting() {
        let inspector = Inspector::new(false, "edge.test:4040".into(), None);
        assert!(!inspector.shutdown_requested());
        inspector.request_shutdown();
        assert!(inspector.shutdown_requested());
    }

    #[test]
    fn tunnel_health_updates_by_local_addr() {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        inspector.set_tunnels(vec![TunnelInfo {
            name: "web".into(),
            proto: "http".into(),
            local: "localhost:3000".into(),
            public_url: "https://web.edge.test".into(),
            healthy: None,
        }]);
        inspector.set_tunnel_health("localhost:3000", false);
        assert_eq!(inspector.session().tunnels[0].healthy, Some(false));
        assert_eq!(
            inspector.local_addr_for("web").as_deref(),
            Some("localhost:3000")
        );
    }

    /// Replay must resolve the tunnel it actually belongs to. With several HTTP
    /// tunnels registered, falling back to the first one would re-issue the
    /// request against a different local service.
    #[test]
    fn local_addr_matches_by_name_and_never_falls_back() {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        inspector.set_tunnels(vec![
            TunnelInfo {
                name: "web".into(),
                proto: "http".into(),
                local: "localhost:3000".into(),
                public_url: "https://web.edge.test".into(),
                healthy: None,
            },
            TunnelInfo {
                name: "api".into(),
                proto: "http".into(),
                local: "localhost:8080".into(),
                public_url: "https://api.edge.test".into(),
                healthy: None,
            },
        ]);

        assert_eq!(
            inspector.local_addr_for("api").as_deref(),
            Some("localhost:8080"),
            "must resolve the second tunnel, not the first"
        );
        assert_eq!(
            inspector.local_addr_for("gone").as_deref(),
            None,
            "an unknown tunnel must not resolve to some other service"
        );
    }

    #[test]
    fn byte_formatting_is_human_readable() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 kB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn summary_json_omits_bodies_and_headers() {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        let ex = inspector.record(exchange(404, 12));
        let summary = ex.to_summary_json();
        assert_eq!(summary["status"], 404);
        assert!(summary.get("request_headers").is_none());
        let detail = ex.to_detail_json();
        assert_eq!(detail["request_headers"][0]["name"], "host");
        assert_eq!(detail["request_body"]["binary"], false);
    }
}
