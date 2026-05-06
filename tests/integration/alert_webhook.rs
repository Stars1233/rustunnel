//! Alert-webhook integration test (TUNNEL-7 Phase 5).
//!
//! Stand up a mock HTTP receiver, configure the test server with its
//! URL as `[load_balancing] alert_webhook_url`, register two grouped
//! tunnels, mark them both unhealthy, and assert the receiver got the
//! `group_zero_healthy` POST exactly once (debounce: re-firing is a
//! regression).

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{routing::post, Json, Router};
use common::*;
use tokio::sync::{oneshot, Mutex};

/// Mock webhook receiver. Captures every request body it gets so the test
/// can assert "exactly one POST".
async fn start_mock_webhook() -> (
    SocketAddr,
    Arc<Mutex<Vec<serde_json::Value>>>,
    oneshot::Sender<()>,
) {
    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    let app = Router::new().route(
        "/alerts",
        post(move |Json(payload): Json<serde_json::Value>| {
            let captured = Arc::clone(&captured_clone);
            async move {
                captured.lock().await.push(payload);
                axum::http::StatusCode::OK
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock webhook");
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
    tokio::time::sleep(Duration::from_millis(20)).await;

    (addr, captured, shutdown_tx)
}

#[tokio::test]
async fn alert_webhook_fires_once_when_group_goes_zero_healthy() {
    init_tracing();

    let (webhook_addr, captured, _hold_webhook) = start_mock_webhook().await;
    let webhook_url = format!("http://{webhook_addr}/alerts");

    // Server with the kill switch on AND the alert webhook configured.
    let control_port = free_port();
    let http_port = free_port();
    let https_port = free_port();
    let dashboard_port = free_port();
    let tcp_low = alloc_tcp_port_range(10);
    let udp_low = alloc_tcp_port_range(10);
    let server = TestServer::start_with_opts(TestServerOpts {
        control_port,
        http_port,
        https_port,
        dashboard_port,
        tcp_port_range: [tcp_low, tcp_low + 9],
        udp_port_range: [udp_low, udp_low + 9],
        require_auth: true,
        admin_token: "integration-test-token".into(),
        load_balancing_enabled: true,
        alert_webhook_url: Some(webhook_url.clone()),
        server_version_override: None,
    })
    .await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    let group = "web";
    let key = "deadbeef".repeat(8);

    let (tid_a, _, _) = client_a
        .register_http_tunnel_grouped(Some("alertpool"), group, &key)
        .await
        .expect("client A grouped");
    let (tid_b, _, _) = client_b
        .register_http_tunnel_grouped(Some("alertpool"), group, &key)
        .await
        .expect("client B grouped");

    // Mark A unhealthy → group still has B → no alert.
    client_a
        .send_tunnel_unhealthy(tid_a, "synthetic A failure")
        .await
        .expect("send unhealthy A");
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        captured.lock().await.len(),
        0,
        "alert must not fire while at least one member is healthy"
    );

    // Mark B unhealthy → group transitions to 0/2 healthy → fire.
    client_b
        .send_tunnel_unhealthy(tid_b, "synthetic B failure")
        .await
        .expect("send unhealthy B");

    // Webhook is delivered in a spawned task; allow generous time.
    let mut waited_ms = 0u64;
    while captured.lock().await.is_empty() && waited_ms < 2_000 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited_ms += 50;
    }

    let bodies = captured.lock().await.clone();
    assert_eq!(
        bodies.len(),
        1,
        "alert webhook must fire exactly once on the 0-healthy transition; got {bodies:?}"
    );

    let body = &bodies[0];
    assert_eq!(body["event"], "group_zero_healthy");
    assert_eq!(body["region_id"], "test");
    assert_eq!(body["protocol"], "http");
    assert_eq!(body["label"], "alertpool");
    assert_eq!(body["group_name"], "web");
    assert_eq!(body["member_count"], 2);
    assert!(body["key_hash_short"].is_string());
    assert!(body["at"].is_string());

    // Mark A unhealthy *again* — debounce still armed → no re-fire.
    client_a
        .send_tunnel_unhealthy(tid_a, "still down")
        .await
        .expect("send unhealthy A again");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        captured.lock().await.len(),
        1,
        "alert must not re-fire while group is still 0/N healthy"
    );

    // Recover one member → debounce resets → next 0-healthy transition
    // would fire again. We don't drive that transition here (the test is
    // about the upward-edge reset), just confirm that recovery doesn't
    // generate an extra POST on its own.
    client_a
        .send_tunnel_healthy(tid_a)
        .await
        .expect("send healthy A");
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        captured.lock().await.len(),
        1,
        "recovery alone must not fire the alert"
    );

    // And now another fall to 0 should re-fire.
    client_a
        .send_tunnel_unhealthy(tid_a, "second flap")
        .await
        .expect("send unhealthy A 2");

    let mut waited_ms = 0u64;
    while captured.lock().await.len() < 2 && waited_ms < 2_000 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited_ms += 50;
    }
    assert_eq!(
        captured.lock().await.len(),
        2,
        "alert must re-fire after recovery + another fall"
    );
}

// ── Per-tenant webhook fan-out (Phase 5 webhook redesign) ────────────────────

/// A multi-tenant group: two members with two distinct
/// `health_check.alert_webhook` URLs. When the group goes 0/N, the server
/// must fire BOTH tenant URLs (deduped by URL — but here they're distinct)
/// alongside the operator URL.
#[tokio::test]
async fn per_tenant_webhooks_fan_out_alongside_operator_url() {
    init_tracing();

    let (op_addr, op_captured, _hold_op) = start_mock_webhook().await;
    let (alice_addr, alice_captured, _hold_alice) = start_mock_webhook().await;
    let (bob_addr, bob_captured, _hold_bob) = start_mock_webhook().await;
    let op_url = format!("http://{op_addr}/alerts");
    let alice_url = format!("http://{alice_addr}/alerts");
    let bob_url = format!("http://{bob_addr}/alerts");

    let control_port = free_port();
    let http_port = free_port();
    let https_port = free_port();
    let dashboard_port = free_port();
    let tcp_low = alloc_tcp_port_range(10);
    let udp_low = alloc_tcp_port_range(10);
    let server = TestServer::start_with_opts(TestServerOpts {
        control_port,
        http_port,
        https_port,
        dashboard_port,
        tcp_port_range: [tcp_low, tcp_low + 9],
        udp_port_range: [udp_low, udp_low + 9],
        require_auth: true,
        admin_token: "integration-test-token".into(),
        load_balancing_enabled: true,
        alert_webhook_url: Some(op_url.clone()),
        server_version_override: None,
    })
    .await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    let group = "fanout";
    let key = "deadbeef".repeat(8);
    let (tid_a, _, _) = client_a
        .register_http_tunnel_grouped_with_alert(Some("fanout"), group, &key, Some(&alice_url))
        .await
        .expect("register A");
    let (tid_b, _, _) = client_b
        .register_http_tunnel_grouped_with_alert(Some("fanout"), group, &key, Some(&bob_url))
        .await
        .expect("register B");

    // A registered with `health_check`, so the server starts it unhealthy
    // (per plan §4.5: members opt-in to probing must wait for the first
    // TunnelHealthy). Bring both members up explicitly so the group has
    // ≥1 healthy member before we start the test scenario.
    client_a
        .send_tunnel_healthy(tid_a)
        .await
        .expect("healthy A");
    client_b
        .send_tunnel_healthy(tid_b)
        .await
        .expect("healthy B");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Take both down. Group → 0/2 → fire on operator + both tenants.
    client_a
        .send_tunnel_unhealthy(tid_a, "A down")
        .await
        .expect("unhealthy A");
    client_b
        .send_tunnel_unhealthy(tid_b, "B down")
        .await
        .expect("unhealthy B");

    // Wait for delivery on all three receivers.
    let mut waited_ms = 0u64;
    while (op_captured.lock().await.is_empty()
        || alice_captured.lock().await.is_empty()
        || bob_captured.lock().await.is_empty())
        && waited_ms < 2_000
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited_ms += 50;
    }

    let op_bodies = op_captured.lock().await.clone();
    let alice_bodies = alice_captured.lock().await.clone();
    let bob_bodies = bob_captured.lock().await.clone();
    assert_eq!(op_bodies.len(), 1, "operator URL fires once");
    assert_eq!(alice_bodies.len(), 1, "Alice's tenant URL fires once");
    assert_eq!(bob_bodies.len(), 1, "Bob's tenant URL fires once");

    // All three receive the same payload shape — only the destination differs.
    for body in [&op_bodies[0], &alice_bodies[0], &bob_bodies[0]] {
        assert_eq!(body["event"], "group_zero_healthy");
        assert_eq!(body["protocol"], "http");
        assert_eq!(body["label"], "fanout");
        assert_eq!(body["group_name"], group);
        assert_eq!(body["member_count"], 2);
    }
}

/// Two members of a group sharing the *same* tenant webhook URL must
/// deliver only one POST per transition, not two.
#[tokio::test]
async fn shared_tenant_webhook_is_deduped() {
    init_tracing();

    let (op_addr, op_captured, _hold_op) = start_mock_webhook().await;
    let (tenant_addr, tenant_captured, _hold_t) = start_mock_webhook().await;
    let op_url = format!("http://{op_addr}/alerts");
    let tenant_url = format!("http://{tenant_addr}/alerts");

    let control_port = free_port();
    let http_port = free_port();
    let https_port = free_port();
    let dashboard_port = free_port();
    let tcp_low = alloc_tcp_port_range(10);
    let udp_low = alloc_tcp_port_range(10);
    let server = TestServer::start_with_opts(TestServerOpts {
        control_port,
        http_port,
        https_port,
        dashboard_port,
        tcp_port_range: [tcp_low, tcp_low + 9],
        udp_port_range: [udp_low, udp_low + 9],
        require_auth: true,
        admin_token: "integration-test-token".into(),
        load_balancing_enabled: true,
        alert_webhook_url: Some(op_url.clone()),
        server_version_override: None,
    })
    .await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");
    let group = "dedup";
    let key = "deadbeef".repeat(8);
    let (tid_a, _, _) = client_a
        .register_http_tunnel_grouped_with_alert(Some("dedup"), group, &key, Some(&tenant_url))
        .await
        .expect("register A");
    let (tid_b, _, _) = client_b
        .register_http_tunnel_grouped_with_alert(Some("dedup"), group, &key, Some(&tenant_url))
        .await
        .expect("register B");

    client_a
        .send_tunnel_healthy(tid_a)
        .await
        .expect("healthy A");
    client_b
        .send_tunnel_healthy(tid_b)
        .await
        .expect("healthy B");
    tokio::time::sleep(Duration::from_millis(50)).await;

    client_a
        .send_tunnel_unhealthy(tid_a, "A down")
        .await
        .expect("unhealthy A");
    client_b
        .send_tunnel_unhealthy(tid_b, "B down")
        .await
        .expect("unhealthy B");

    let mut waited_ms = 0u64;
    while (op_captured.lock().await.is_empty() || tenant_captured.lock().await.is_empty())
        && waited_ms < 2_000
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited_ms += 50;
    }

    assert_eq!(op_captured.lock().await.len(), 1, "operator fires once");
    assert_eq!(
        tenant_captured.lock().await.len(),
        1,
        "shared tenant URL is deduped — fires once even though two members carry it"
    );
}
