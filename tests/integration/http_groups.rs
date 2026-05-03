//! HTTP load-balancing group integration tests (TUNNEL-7 Phase 2).
//!
//! # What these tests pin down
//!
//! 1. **Random dispatch across two members.** Two clients register the same
//!    subdomain with the same `(group, group_key_hash)`. Each terminates at
//!    its own local backend that tags responses with its identity. We hit
//!    the tunnel with N requests and assert that *both* backends served a
//!    non-trivial share — the load-balancing promise of Phase 2.
//!
//! 2. **`group_key_hash` mismatch is rejected.** A second client trying to
//!    join the same subdomain with a different key gets `TunnelError`. This
//!    is the auth boundary that prevents one tenant from inserting itself
//!    into another's pool on a shared edge.
//!
//! 3. **Kill switch (`[load_balancing] enabled = false`).** When the flag
//!    is off, the server logs a warning and falls through to solo
//!    registration — which then rejects the duplicate subdomain. Confirms
//!    the rollout's safe-default story (plan §6).

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{routing::get, Router};
use common::*;
use tokio::sync::oneshot;

/// Hello-world server tagged with `who` — every response includes
/// `X-Backend: who` so the test can count which member served it.
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

// ── 1. Random dispatch distributes across two members ──────────────────────────

#[tokio::test]
async fn http_group_random_dispatch_distributes_across_two_members() {
    init_tracing();

    // Two backends, each tagging its responses.
    let (addr_a, count_a, _hold_a) = start_tagged_server("A").await;
    let (addr_b, count_b, _hold_b) = start_tagged_server("B").await;

    // Server with the kill switch on.
    let server = TestServer::start_with_load_balancing().await;

    // Two independent client sessions on the same subdomain + key.
    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");
    let session_a = client_a.session_id.unwrap();
    let session_b = client_b.session_id.unwrap();

    let group = "web";
    let key = "deadbeef".repeat(8); // 64-char hex placeholder for SHA-256

    let (_tid_a, sub_a, _) = client_a
        .register_http_tunnel_grouped(Some("pool"), group, &key)
        .await
        .expect("client A grouped registration");
    let (_tid_b, sub_b, _) = client_b
        .register_http_tunnel_grouped(Some("pool"), group, &key)
        .await
        .expect("client B grouped registration");
    assert_eq!(sub_a, "pool");
    assert_eq!(sub_b, "pool");

    // Each client bridges to its own backend.
    connect_data_bridge(&server, session_a, addr_a)
        .await
        .expect("data bridge A ready");
    connect_data_bridge(&server, session_b, addr_b)
        .await
        .expect("data bridge B ready");

    // Hammer the tunnel with N requests; sum-by-backend via X-Backend header.
    const N: u64 = 200;
    let https_url = format!("https://127.0.0.1:{}/", server.https_port);
    let host = format!("pool.{}", server.domain);
    let client = insecure_http_client();

    let mut hits_a = 0u64;
    let mut hits_b = 0u64;
    for _ in 0..N {
        let resp = client
            .get(&https_url)
            .header("Host", &host)
            .send()
            .await
            .expect("HTTPS request");
        assert_eq!(resp.status(), 200);
        let backend = resp
            .headers()
            .get("X-Backend")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        match backend.as_str() {
            "A" => hits_a += 1,
            "B" => hits_b += 1,
            other => panic!("unexpected backend tag: {other:?}"),
        }
    }

    assert_eq!(hits_a + hits_b, N);

    // Generous bounds — 200 trials, p=0.5, the [40, 160] envelope catches
    // "always picks the same one" while keeping flake rate effectively zero.
    assert!(
        (40..=160).contains(&hits_a),
        "expected dispatch to balance across both members; got A={hits_a}, B={hits_b}"
    );
    assert!(
        (40..=160).contains(&hits_b),
        "expected dispatch to balance across both members; got A={hits_a}, B={hits_b}"
    );

    // The per-backend hit counters must agree with what we observed at the edge.
    assert_eq!(count_a.load(Ordering::Relaxed), hits_a);
    assert_eq!(count_b.load(Ordering::Relaxed), hits_b);
}

// ── 2. Mismatched group_key_hash is rejected ───────────────────────────────────

#[tokio::test]
async fn http_group_mismatched_key_is_rejected() {
    init_tracing();
    let server = TestServer::start_with_load_balancing().await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    client_a
        .register_http_tunnel_grouped(Some("pool"), "web", &"a".repeat(64))
        .await
        .expect("client A registers first with key A");

    let err = client_b
        .register_http_tunnel_grouped(Some("pool"), "web", &"b".repeat(64))
        .await
        .expect_err("client B with different key must be rejected");

    assert!(
        err.contains("TunnelError") && err.contains("group key does not match"),
        "expected key-mismatch TunnelError; got: {err}"
    );
}

// ── 3. Disabled flag falls through to solo registration ────────────────────────

#[tokio::test]
async fn http_group_with_kill_switch_off_falls_through_to_solo() {
    init_tracing();
    // Default TestServer: load_balancing.enabled = false.
    let server = TestServer::start().await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    // First grouped registration is downgraded to solo (server logs a warning).
    client_a
        .register_http_tunnel_grouped(Some("pool"), "web", &"a".repeat(64))
        .await
        .expect("client A registers solo (kill switch off)");

    // Second grouped registration on the same subdomain is also solo →
    // collides on the existing solo entry → rejected with the legacy
    // "subdomain already in use" error.
    let err = client_b
        .register_http_tunnel_grouped(Some("pool"), "web", &"a".repeat(64))
        .await
        .expect_err("second registration must fail when load balancing is off");

    assert!(
        err.contains("TunnelError") && err.contains("already in use"),
        "expected solo-collision TunnelError; got: {err}"
    );
}
