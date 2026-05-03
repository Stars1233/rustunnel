//! Health-check integration tests (TUNNEL-7 Phase 4).
//!
//! # What these tests pin down
//!
//! 1. **`TunnelUnhealthy` excludes a member from dispatch.** Two clients
//!    register the same HTTP group; we send `TunnelUnhealthy` for one of
//!    them and assert that subsequent connections all land on the survivor.
//!    Then send `TunnelHealthy` and assert traffic resumes to both. This is
//!    the failover invariant — Phase 4's whole point.
//!
//! 2. **`TunnelUnhealthy` from a non-owning session is ignored.** A second
//!    client tries to mark the *first* client's tunnel unhealthy. The
//!    server logs a warning and drops the frame; dispatch still hits the
//!    targeted tunnel. This is the auth boundary — without it any client
//!    on a shared edge could DoS another's pool members.

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{routing::get, Router};
use common::*;
use tokio::sync::oneshot;

/// Hello-world server tagged with `who` — same shape as the http_groups suite.
async fn start_tagged_server(
    who: &'static str,
) -> (SocketAddr, Arc<AtomicU64>, oneshot::Sender<()>) {
    let counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let counter_clone = Arc::clone(&counter);

    let app = Router::new().route(
        "/",
        get(move || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                axum::http::Response::builder()
                    .header("X-Backend", who)
                    .body(format!("hello from {who}"))
                    .unwrap()
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tagged server");
    let addr = listener.local_addr().unwrap();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (addr, counter, shutdown_tx)
}

// ── 1. Health bit toggles excluded/included from dispatch ──────────────────────

#[tokio::test]
async fn unhealthy_member_is_excluded_then_restored_on_healthy() {
    init_tracing();

    let (addr_a, count_a, _hold_a) = start_tagged_server("A").await;
    let (addr_b, count_b, _hold_b) = start_tagged_server("B").await;

    let server = TestServer::start_with_load_balancing().await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");
    let session_a = client_a.session_id.unwrap();
    let session_b = client_b.session_id.unwrap();

    let group = "web";
    let key = "deadbeef".repeat(8);

    let (tid_a, _, _) = client_a
        .register_http_tunnel_grouped(Some("hpool"), group, &key)
        .await
        .expect("client A grouped");
    let (_tid_b, _, _) = client_b
        .register_http_tunnel_grouped(Some("hpool"), group, &key)
        .await
        .expect("client B grouped");

    connect_data_bridge(&server, session_a, addr_a)
        .await
        .expect("bridge A");
    connect_data_bridge(&server, session_b, addr_b)
        .await
        .expect("bridge B");

    let https_url = format!("https://127.0.0.1:{}/", server.https_port);
    let host = format!("hpool.{}", server.domain);
    let http = insecure_http_client();

    // ── Phase 1: A unhealthy → all traffic lands on B ───────────────────
    client_a
        .send_tunnel_unhealthy(tid_a, "synthetic probe failure")
        .await
        .expect("send TunnelUnhealthy");

    // Brief delay so the frame is processed before we issue requests.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    const N1: u64 = 50;
    let baseline_a = count_a.load(Ordering::Relaxed);
    let baseline_b = count_b.load(Ordering::Relaxed);
    for _ in 0..N1 {
        let resp = http
            .get(&https_url)
            .header("Host", &host)
            .send()
            .await
            .expect("HTTPS while A unhealthy");
        assert_eq!(resp.status(), 200);
    }
    let delta_a = count_a.load(Ordering::Relaxed) - baseline_a;
    let delta_b = count_b.load(Ordering::Relaxed) - baseline_b;
    assert_eq!(
        delta_a, 0,
        "A is unhealthy — dispatch must not route to it; got {delta_a} hits"
    );
    assert_eq!(delta_b, N1, "B must serve every request while A is out");

    // ── Phase 2: A healthy again → dispatch distributes ────────────────
    client_a
        .send_tunnel_healthy(tid_a)
        .await
        .expect("send TunnelHealthy");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    const N2: u64 = 200;
    let baseline_a = count_a.load(Ordering::Relaxed);
    let baseline_b = count_b.load(Ordering::Relaxed);
    for _ in 0..N2 {
        let resp = http
            .get(&https_url)
            .header("Host", &host)
            .send()
            .await
            .expect("HTTPS after A restored");
        assert_eq!(resp.status(), 200);
    }
    let delta_a = count_a.load(Ordering::Relaxed) - baseline_a;
    let delta_b = count_b.load(Ordering::Relaxed) - baseline_b;
    assert_eq!(delta_a + delta_b, N2);
    // Generous bounds: both must serve a non-trivial share.
    assert!(
        delta_a >= 30,
        "A must serve again after TunnelHealthy; got delta_a={delta_a}, delta_b={delta_b}"
    );
    assert!(
        delta_b >= 30,
        "B must keep serving; got delta_a={delta_a}, delta_b={delta_b}"
    );
}

// ── 2. Cross-session health frames are ignored ─────────────────────────────────

#[tokio::test]
async fn cross_session_tunnel_unhealthy_is_ignored() {
    init_tracing();

    let (addr_a, count_a, _hold_a) = start_tagged_server("A").await;
    let (addr_b, count_b, _hold_b) = start_tagged_server("B").await;

    let server = TestServer::start_with_load_balancing().await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");
    let session_a = client_a.session_id.unwrap();
    let session_b = client_b.session_id.unwrap();

    let group = "web";
    let key = "deadbeef".repeat(8);

    let (tid_a, _, _) = client_a
        .register_http_tunnel_grouped(Some("xpool"), group, &key)
        .await
        .expect("client A grouped");
    let (_tid_b, _, _) = client_b
        .register_http_tunnel_grouped(Some("xpool"), group, &key)
        .await
        .expect("client B grouped");

    connect_data_bridge(&server, session_a, addr_a)
        .await
        .expect("bridge A");
    connect_data_bridge(&server, session_b, addr_b)
        .await
        .expect("bridge B");

    // Client B tries to mark client A's tunnel unhealthy. The server
    // should log a warning and drop the frame — A stays in the pool.
    client_b
        .send_tunnel_unhealthy(tid_a, "i am not the owner")
        .await
        .expect("send (server will ignore)");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let https_url = format!("https://127.0.0.1:{}/", server.https_port);
    let host = format!("xpool.{}", server.domain);
    let http = insecure_http_client();

    const N: u64 = 100;
    for _ in 0..N {
        let resp = http
            .get(&https_url)
            .header("Host", &host)
            .send()
            .await
            .expect("HTTPS request");
        assert_eq!(resp.status(), 200);
    }

    let hits_a = count_a.load(Ordering::Relaxed);
    let hits_b = count_b.load(Ordering::Relaxed);
    assert_eq!(hits_a + hits_b, N);
    // Both should serve a non-trivial share — A wasn't actually marked
    // unhealthy because the frame came from a non-owning session.
    assert!(
        hits_a >= 20,
        "A must keep serving — cross-session unhealthy was ignored; got A={hits_a}, B={hits_b}"
    );
    assert!(
        hits_b >= 20,
        "B must serve normally; got A={hits_a}, B={hits_b}"
    );
}
