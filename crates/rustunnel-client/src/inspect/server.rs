//! Local web inspector.
//!
//! A small axum server bound to loopback only, serving the request history
//! captured by [`crate::inspect::tap`]: list, detail (headers + bodies), live
//! updates over SSE, and replay against the local service.
//!
//! Captured payloads can contain credentials and personal data, so this server
//! never binds anything but `127.0.0.1` and is disabled entirely with
//! `--no-inspect`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, Json};
use axum::routing::{delete, get, post};
use axum::Router;
use chrono::Utc;
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

use super::{Body, Exchange, Inspector, BODY_CAP};

/// Default inspector port — the same one ngrok uses, for muscle memory.
pub const DEFAULT_PORT: u16 = 4040;

/// How many ports past the preferred one to try before giving up.
const PORT_SCAN_RANGE: u16 = 20;

/// Never bind this port: it is reserved for other local services and is the
/// single most likely port for the app being tunnelled.
const RESERVED_PORT: u16 = 3000;

/// How long a replayed request may take before it is abandoned.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait when probing whether a port is already serving.
const PROBE_TIMEOUT: Duration = Duration::from_millis(150);

/// Bind the inspector to loopback, starting at `preferred` and scanning
/// forward if it is taken. Returns the listener and the URL it is reachable on.
pub async fn bind(preferred: u16) -> Option<(TcpListener, String)> {
    // Port 0 means "any free port" — bind once and report what we got.
    if preferred == 0 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.ok()?;
        let port = listener.local_addr().ok()?.port();
        return Some((listener, format!("http://127.0.0.1:{port}")));
    }

    for port in preferred..preferred.saturating_add(PORT_SCAN_RANGE) {
        if port == RESERVED_PORT {
            continue;
        }
        // A successful bind is not proof the port is free: on macOS/BSD,
        // binding 127.0.0.1:P succeeds even when another process holds
        // 0.0.0.0:P, and loopback traffic then reaches us instead of them.
        // That would silently hijack the local tunnel server (which listens on
        // :4040 too), so probe for a live listener before claiming the port.
        if port_is_serving(port).await {
            debug!(port, "inspector: port already serving — trying next");
            continue;
        }
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Some((listener, format!("http://127.0.0.1:{port}"))),
            Err(e) => debug!(port, "inspector: port unavailable ({e}) — trying next"),
        }
    }
    None
}

/// True when something already accepts connections on loopback at `port`.
async fn port_is_serving(port: u16) -> bool {
    tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

/// Serve the inspector until the process exits.
pub async fn serve(listener: TcpListener, inspector: Arc<Inspector>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/requests", get(list_requests))
        .route("/api/requests", delete(clear_requests))
        .route("/api/requests/:id", get(request_detail))
        .route("/api/requests/:id/replay", post(replay_request))
        .route("/api/stream", get(stream))
        .with_state(inspector);

    if let Err(e) = axum::serve(listener, app).await {
        warn!("inspector: server stopped: {e}");
    }
}

// ── handlers ──────────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn status(State(inspector): State<Arc<Inspector>>) -> Json<serde_json::Value> {
    let session = inspector.session();
    let stats = inspector.stats.snapshot();
    let (p50, p90) = inspector.latency_percentiles().unwrap_or((0, 0));

    Json(serde_json::json!({
        "status": session.status.label(),
        "server": session.server,
        "region": session.region,
        "latency_ms": session.latency_ms,
        "version": session.version,
        "started_at": session.started_at.to_rfc3339(),
        "tunnels": session.tunnels,
        "stats": stats,
        "p50_ms": p50,
        "p90_ms": p90,
    }))
}

async fn list_requests(
    State(inspector): State<Arc<Inspector>>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    let method = params.get("method").map(|m| m.to_ascii_uppercase());
    let status_filter = params.get("status").and_then(|s| s.parse::<u16>().ok());
    let query = params.get("q").map(|q| q.to_lowercase());

    let items: Vec<serde_json::Value> = inspector
        .exchanges()
        .into_iter()
        .filter(|ex| method.as_ref().is_none_or(|m| &ex.method == m))
        .filter(|ex| status_filter.is_none_or(|s| ex.status == s))
        .filter(|ex| {
            query.as_ref().is_none_or(|q| {
                ex.path.to_lowercase().contains(q)
                    || ex.method.to_lowercase().contains(q)
                    || ex.status.to_string().contains(q)
            })
        })
        .take(limit)
        .map(|ex| ex.to_summary_json())
        .collect();

    Json(serde_json::json!({ "requests": items }))
}

async fn request_detail(
    State(inspector): State<Arc<Inspector>>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    inspector
        .exchange(id)
        .map(|ex| Json(ex.to_detail_json()))
        .ok_or(StatusCode::NOT_FOUND)
}

async fn clear_requests(State(inspector): State<Arc<Inspector>>) -> StatusCode {
    inspector.clear();
    StatusCode::NO_CONTENT
}

/// Server-sent events: one JSON summary per captured exchange.
async fn stream(
    State(inspector): State<Arc<Inspector>>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let receiver = inspector.subscribe();

    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(exchange) => {
                    let event = SseEvent::default().data(exchange.to_summary_json().to_string());
                    return Some((Ok(event), receiver));
                }
                // A slow browser tab just misses events; keep the stream alive.
                Err(RecvError::Lagged(skipped)) => {
                    debug!(skipped, "inspector: SSE subscriber lagged");
                    continue;
                }
                Err(RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Re-issue a captured request against the local service and record the result.
async fn replay_request(
    State(inspector): State<Arc<Inspector>>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let original = inspector
        .exchange(id)
        .ok_or((StatusCode::NOT_FOUND, "no such request".to_string()))?;

    let local_addr = inspector.local_addr_for(&original.tunnel).ok_or((
        StatusCode::CONFLICT,
        "no local address known for this tunnel".to_string(),
    ))?;

    let method = reqwest::Method::from_bytes(original.method.as_bytes()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            format!("cannot replay method {}", original.method),
        )
    })?;

    let url = format!("http://{}{}", local_addr, original.path);
    let client = reqwest::Client::builder()
        .timeout(REPLAY_TIMEOUT)
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut request = client.request(method, &url);
    for (name, value) in &original.request_headers {
        if is_hop_by_hop(name) {
            continue;
        }
        request = request.header(name, value);
    }
    if !original.request_body.bytes.is_empty() {
        request = request.body(original.request_body.bytes.clone());
    }

    info!(%url, original_id = id, "inspector: replaying request");
    let started = Instant::now();
    let started_at = Utc::now();

    let response = request
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("replay failed: {e}")))?;

    let status = response.status().as_u16();
    let response_headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("replay failed: {e}")))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let mut response_body = Body::default();
    response_body.push(&body_bytes[..body_bytes.len().min(BODY_CAP)]);
    response_body.total = body_bytes.len() as u64;
    response_body.truncated = body_bytes.len() > response_body.bytes.len();

    let recorded = inspector.record(Exchange {
        id: 0,
        conn_id: original.conn_id,
        tunnel: original.tunnel.clone(),
        client_addr: "replay".to_string(),
        method: original.method.clone(),
        path: original.path.clone(),
        host: original.host.clone(),
        status,
        request_headers: original.request_headers.clone(),
        response_headers,
        request_body: original.request_body.clone(),
        response_body,
        duration_ms,
        started_at,
        replayed: true,
    });

    Ok(Json(serde_json::json!({
        "replayed": recorded.to_summary_json(),
        // The capture is capped, so a huge original body cannot be replayed byte-exact.
        "body_truncated": original.request_body.truncated,
    })))
}

/// Headers that describe a single hop and must not be copied into a replay.
fn is_hop_by_hop(name: &str) -> bool {
    const HOP_BY_HOP: [&str; 8] = [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];
    let lower = name.to_ascii_lowercase();
    // Content-Length is recomputed by the HTTP client from the body we set.
    HOP_BY_HOP.contains(&lower.as_str()) || lower == "content-length"
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_headers_are_stripped_case_insensitively() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(is_hop_by_hop("Content-Length"));
        assert!(!is_hop_by_hop("Authorization"));
        assert!(!is_hop_by_hop("Host"));
        assert!(!is_hop_by_hop("content-type"));
    }

    #[tokio::test]
    async fn bind_skips_the_reserved_port() {
        // Starting the scan at the reserved port must move past it.
        let (listener, url) = bind(RESERVED_PORT).await.expect("a port should be free");
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, RESERVED_PORT);
        assert!(url.starts_with("http://127.0.0.1:"));
    }

    #[tokio::test]
    async fn bind_falls_forward_when_the_preferred_port_is_taken() {
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();

        let (listener, _) = bind(taken).await.expect("a later port should be free");
        assert_ne!(listener.local_addr().unwrap().port(), taken);
    }

    /// Regression: binding 127.0.0.1:P while another process holds 0.0.0.0:P
    /// succeeds on macOS/BSD, and loopback traffic would then reach the
    /// inspector instead of that process — which hijacked the local tunnel
    /// server's control port (:4040) and broke the client's own connection.
    #[tokio::test]
    async fn bind_skips_a_port_served_on_all_interfaces() {
        let wildcard = TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
        let taken = wildcard.local_addr().unwrap().port();
        // Keep accepting so the probe sees a live listener.
        tokio::spawn(async move {
            loop {
                if wildcard.accept().await.is_err() {
                    break;
                }
            }
        });

        let (listener, _) = bind(taken).await.expect("a later port should be free");
        assert_ne!(
            listener.local_addr().unwrap().port(),
            taken,
            "must not shadow a wildcard listener"
        );
    }

    #[tokio::test]
    async fn port_probe_detects_live_listeners_only() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let live = listener.local_addr().unwrap().port();
        assert!(port_is_serving(live).await);

        drop(listener);
        assert!(!port_is_serving(live).await);
    }

    #[tokio::test]
    async fn bind_is_loopback_only() {
        let (listener, _) = bind(0).await.unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }
}
