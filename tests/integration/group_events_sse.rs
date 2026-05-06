//! SSE event-stream integration test (TUNNEL-7 Phase 5).
//!
//! Open `GET /api/groups/:protocol/:label/events` over the dashboard
//! HTTPS, drive a member health-bit flip via control-WS frames, and
//! read the SSE stream to confirm the `group_event` lands with the
//! expected JSON shape.
//!
//! Pitfall avoided: reqwest's response body is read chunk-by-chunk
//! (no `stream` feature needed). One event per probe-edge transition
//! is enough to assert correctness — we don't try to test buffering
//! or back-pressure here.

#[path = "../common/mod.rs"]
mod common;

use std::time::Duration;

use common::*;

#[tokio::test]
async fn sse_emits_group_event_on_health_flip() {
    init_tracing();

    let server = TestServer::start_with_load_balancing().await;

    // Two clients on the same group so the dispatch path actually has
    // members to flip.
    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    let group = "web";
    let key = "deadbeef".repeat(8);
    let (tid_a, _, _) = client_a
        .register_http_tunnel_grouped(Some("ssepool"), group, &key)
        .await
        .expect("client A grouped");
    let (_tid_b, _, _) = client_b
        .register_http_tunnel_grouped(Some("ssepool"), group, &key)
        .await
        .expect("client B grouped");

    // Open the SSE stream. Subscribe BEFORE driving the flip so we
    // don't miss the event (the broadcast channel doesn't replay).
    // The dashboard binds plain HTTP — production gets TLS via nginx.
    let url = format!(
        "http://127.0.0.1:{}/api/groups/http/ssepool/events",
        server.dashboard_port
    );
    let http = reqwest::Client::new();
    let mut response = http
        .get(&url)
        .header("Authorization", format!("Bearer {}", server.admin_token))
        .send()
        .await
        .expect("open SSE stream");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    // Tiny delay so the subscription is registered before the next frame.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Drive the flip.
    client_a
        .send_tunnel_unhealthy(tid_a, "synthetic SSE test")
        .await
        .expect("send unhealthy");

    // Read chunks until we accumulate a `data:` line. Cap at 3s so a
    // failure surfaces quickly instead of hanging the suite.
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let chunk = match tokio::time::timeout(Duration::from_millis(500), response.chunk()).await {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => break, // stream closed
            Ok(Err(e)) => panic!("chunk read error: {e}"),
            Err(_) => continue, // 500 ms timeout, try again
        };
        buf.push_str(std::str::from_utf8(&chunk).expect("utf8"));
        if buf.contains("event: group_event") && buf.contains("data: ") {
            break;
        }
    }

    assert!(
        buf.contains("event: group_event"),
        "no group_event seen on SSE stream within 3s; got: {buf:?}"
    );

    // Parse out the `data:` line and assert the payload shape.
    let data_line = buf
        .lines()
        .find(|l| l.starts_with("data: "))
        .expect("data: line present");
    let json_str = data_line.trim_start_matches("data: ");
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).expect("event data is valid JSON");
    assert_eq!(parsed["protocol"], "http");
    assert_eq!(parsed["label"], "ssepool");
    assert_eq!(parsed["healthy"], false);
    assert_eq!(parsed["reason"], "synthetic SSE test");
    assert_eq!(parsed["member_count"], 2);
    assert_eq!(parsed["healthy_count"], 1);
    assert_eq!(parsed["tunnel_id"], tid_a.to_string());
}
