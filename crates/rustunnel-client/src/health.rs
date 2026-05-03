//! Client-side health probes (TUNNEL-7 Phase 4).
//!
//! For each tunnel registered with a `HealthCheckConfig`, the client spawns
//! a background task that periodically probes the local service and reports
//! state changes back to the server via the control channel:
//!
//!   - First probe success after a streak of failures (or the first probe
//!     of a freshly-registered tunnel) → `TunnelHealthy`
//!   - `max_failed` consecutive failures → `TunnelUnhealthy`
//!
//! TCP probe: open a connection, success = connect within `timeout_secs`.
//! HTTP probe: `GET <path>`, success = response within `timeout_secs` and
//! 2xx (when `expect_2xx`).
//!
//! The probe loop is deliberately small and self-contained: it owns no
//! shared state, mutates nothing on the client side, and writes to the
//! server via an `mpsc::Sender<ControlFrame>` that the main control loop
//! drains. Probe tasks die naturally when the channel closes (i.e. when
//! the session ends).

use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{self, timeout};
use tracing::{debug, warn};
use uuid::Uuid;

use rustunnel_protocol::ControlFrame;

use crate::config::HealthCheckConfig;
use crate::error::{Error, Result};

/// What kind of probe to run.
#[derive(Debug, Clone)]
pub enum ProbeKind {
    Tcp,
    Http { path: String, expect_2xx: bool },
}

/// Resolved per-tunnel health-probe spec.
///
/// `HealthCheckConfig` is the user-facing TOML/YAML shape; this is the
/// validated runtime shape. Conversion happens in `from_config`.
#[derive(Debug, Clone)]
pub struct ProbeSpec {
    pub kind: ProbeKind,
    pub interval: Duration,
    pub timeout: Duration,
    pub max_failed: u32,
}

impl ProbeSpec {
    /// Build a runtime `ProbeSpec` from the user's `HealthCheckConfig`.
    /// Returns an error for unknown kinds or for HTTP without a `path`.
    pub fn from_config(cfg: &HealthCheckConfig) -> Result<Self> {
        let kind = match cfg.kind.as_str() {
            "tcp" => ProbeKind::Tcp,
            "http" => {
                let path = cfg.path.clone().ok_or_else(|| {
                    Error::Config("health_check kind='http' requires a path".into())
                })?;
                ProbeKind::Http {
                    path,
                    expect_2xx: cfg.expect_2xx,
                }
            }
            other => {
                return Err(Error::Config(format!(
                    "unknown health_check kind '{other}' (expected 'tcp' or 'http')"
                )));
            }
        };
        Ok(Self {
            kind,
            interval: Duration::from_secs(cfg.interval_secs.max(1) as u64),
            timeout: Duration::from_secs(cfg.timeout_secs.max(1) as u64),
            max_failed: cfg.max_failed.max(1),
        })
    }
}

/// Run one TCP probe against `addr`.
pub async fn probe_tcp(addr: &str, probe_timeout: Duration) -> bool {
    matches!(
        timeout(probe_timeout, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Run one HTTP probe: `GET <path>` over plain HTTP/1.0 against `addr`.
///
/// We hand-roll a minimal request rather than pulling reqwest into the
/// probe path — this makes the probe latency-insensitive to TLS, redirects,
/// or connection pooling. `expect_2xx` controls whether 3xx/4xx/5xx counts
/// as a failure.
pub async fn probe_http(addr: &str, path: &str, probe_timeout: Duration, expect_2xx: bool) -> bool {
    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\nUser-Agent: rustunnel-healthcheck\r\n\r\n",
        path, addr
    );
    let result = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        stream.write_all(request.as_bytes()).await.ok()?;
        // We only need the first ~16 bytes — `HTTP/1.X NNN ...`.
        let mut buf = [0u8; 32];
        let mut total = 0;
        while total < buf.len() {
            let n = match tokio::io::AsyncReadExt::read(&mut stream, &mut buf[total..]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            total += n;
            if buf[..total].contains(&b'\n') {
                break;
            }
        }
        let line = std::str::from_utf8(&buf[..total]).ok()?;
        // "HTTP/1.x SSS ..." — pluck the status code.
        let mut parts = line.split_ascii_whitespace();
        let _version = parts.next()?;
        let status: u16 = parts.next()?.parse().ok()?;
        Some(if expect_2xx {
            (200..300).contains(&status)
        } else {
            status >= 100
        })
    };
    matches!(timeout(probe_timeout, result).await, Ok(Some(true)))
}

/// Run the per-tunnel probe loop forever (until the channel closes).
///
/// `local_addr` is what the client is forwarding to (e.g. `127.0.0.1:3000`).
/// `frame_tx` is an `mpsc::Sender<ControlFrame>` that the main control loop
/// drains and writes to the WebSocket; we send `TunnelHealthy` /
/// `TunnelUnhealthy` through it.
///
/// The very first probe success emits a `TunnelHealthy` so the server can
/// flip the bit on a freshly-registered member with `health_check` set
/// (which starts unhealthy by design, plan §4.5).
pub async fn run_probe_loop(
    tunnel_id: Uuid,
    local_addr: String,
    spec: ProbeSpec,
    frame_tx: mpsc::Sender<ControlFrame>,
) {
    let mut consecutive_failures: u32 = 0;
    // `was_healthy` starts None so the first probe — pass or fail — emits a
    // frame. After that, we only emit on edges (healthy → unhealthy or
    // unhealthy → healthy).
    let mut was_healthy: Option<bool> = None;
    let mut ticker = time::interval(spec.interval);
    // First tick fires immediately so we don't wait an interval before
    // sending the first health report.
    loop {
        ticker.tick().await;
        let healthy = match &spec.kind {
            ProbeKind::Tcp => probe_tcp(&local_addr, spec.timeout).await,
            ProbeKind::Http { path, expect_2xx } => {
                probe_http(&local_addr, path, spec.timeout, *expect_2xx).await
            }
        };

        let edge = match (was_healthy, healthy) {
            (None, true) => Some(true),
            (None, false) => {
                // First probe is a failure — track consecutive count, only
                // emit Unhealthy once we cross max_failed. (Don't emit
                // Healthy: the server has the member as unhealthy already,
                // we just don't disagree yet.)
                consecutive_failures = consecutive_failures.saturating_add(1);
                if consecutive_failures >= spec.max_failed {
                    Some(false)
                } else {
                    None
                }
            }
            (Some(true), true) => {
                consecutive_failures = 0;
                None
            }
            (Some(true), false) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                if consecutive_failures >= spec.max_failed {
                    Some(false)
                } else {
                    None
                }
            }
            (Some(false), true) => {
                consecutive_failures = 0;
                Some(true)
            }
            (Some(false), false) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                None
            }
        };

        if let Some(now_healthy) = edge {
            let frame = if now_healthy {
                ControlFrame::TunnelHealthy { tunnel_id }
            } else {
                ControlFrame::TunnelUnhealthy {
                    tunnel_id,
                    reason: format!("{} consecutive probe failures", consecutive_failures),
                }
            };
            if frame_tx.send(frame).await.is_err() {
                debug!(%tunnel_id, "control channel closed — probe loop exiting");
                return;
            }
            was_healthy = Some(now_healthy);
            if !now_healthy {
                warn!(
                    %tunnel_id, %local_addr,
                    "health probe reported unhealthy after {consecutive_failures} consecutive failures"
                );
            }
        } else if was_healthy.is_none() {
            // We probed but didn't cross any edge — first probe was a
            // failure under max_failed. Don't update was_healthy yet so the
            // next probe still treats this as the "first run" path.
        } else {
            // Steady state — same as before. Nothing to send.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// `probe_tcp` succeeds against a listening socket and fails against
    /// a closed port.
    #[tokio::test]
    async fn tcp_probe_basics() {
        // Spawn a dummy listener; the probe should connect.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            // Accept exactly one connection then drop the listener.
            let _ = listener.accept().await;
        });
        assert!(probe_tcp(&addr, Duration::from_millis(500)).await);

        // Closed port — should fail. Bind a fresh socket then drop it
        // immediately so the OS frees the port; the probe will see
        // refused/timeout depending on platform.
        let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = l2.local_addr().unwrap().to_string();
        drop(l2);
        // Tiny delay so the kernel releases the listener cleanly.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!probe_tcp(&dead_addr, Duration::from_millis(500)).await);
    }

    /// `probe_http` recognises 2xx as healthy and 5xx as unhealthy
    /// (when `expect_2xx = true`). We hand-roll a minimal HTTP responder
    /// rather than pull axum into the client crate just for tests — the
    /// probe is HTTP/1.0 close-on-finish, so a one-shot TCP responder is
    /// the matching shape.
    #[tokio::test]
    async fn http_probe_distinguishes_2xx_and_5xx() {
        async fn responder_with_status(status_line: &'static str) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move {
                loop {
                    let Ok((mut s, _)) = listener.accept().await else {
                        break;
                    };
                    let body = format!(
                        "HTTP/1.0 {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        status_line
                    );
                    let _ = s.write_all(body.as_bytes()).await;
                    let _ = s.shutdown().await;
                }
            });
            tokio::time::sleep(Duration::from_millis(20)).await;
            addr
        }

        let timeout_dur = Duration::from_millis(500);
        let addr_200 = responder_with_status("200 OK").await;
        let addr_500 = responder_with_status("500 Internal Server Error").await;

        // 200 → healthy regardless of expect_2xx.
        assert!(probe_http(&addr_200, "/", timeout_dur, true).await);
        assert!(probe_http(&addr_200, "/", timeout_dur, false).await);
        // 500 → unhealthy when expect_2xx; healthy when we don't require 2xx.
        assert!(!probe_http(&addr_500, "/", timeout_dur, true).await);
        assert!(probe_http(&addr_500, "/", timeout_dur, false).await);
    }

    /// The probe loop emits `TunnelHealthy` on the first success and
    /// `TunnelUnhealthy` after `max_failed` consecutive failures.
    #[tokio::test]
    async fn probe_loop_emits_on_state_edges() {
        // Start with a listener that accepts probes — first probe healthy.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let listener_arc = std::sync::Arc::new(tokio::sync::Mutex::new(Some(listener)));
        let listener_clone = std::sync::Arc::clone(&listener_arc);
        tokio::spawn(async move {
            loop {
                let g = listener_clone.lock().await;
                if let Some(l) = g.as_ref() {
                    let _ = l.accept().await;
                } else {
                    break;
                }
            }
        });

        let spec = ProbeSpec {
            kind: ProbeKind::Tcp,
            interval: Duration::from_millis(50),
            timeout: Duration::from_millis(100),
            max_failed: 2,
        };
        let (tx, mut rx) = mpsc::channel::<ControlFrame>(8);
        let tunnel_id = Uuid::new_v4();
        let probe_handle = tokio::spawn(run_probe_loop(tunnel_id, addr.clone(), spec, tx));

        // First emitted frame should be TunnelHealthy.
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("first frame within 2s")
            .expect("first frame present");
        assert!(matches!(first, ControlFrame::TunnelHealthy { tunnel_id: t } if t == tunnel_id));

        // Now break the listener so the probe starts failing.
        {
            let mut g = listener_arc.lock().await;
            *g = None;
        }

        // After max_failed=2 consecutive failures we should see Unhealthy.
        let unhealthy = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("unhealthy frame within 3s")
            .expect("frame present");
        assert!(matches!(
            unhealthy,
            ControlFrame::TunnelUnhealthy { tunnel_id: t, .. } if t == tunnel_id
        ));

        probe_handle.abort();
    }
}
