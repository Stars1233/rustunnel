//! Authentication integration tests.
//!
//! Tests the full control-plane auth handshake:
//! - Invalid token → `AuthError`
//! - Valid (admin) token → `AuthOk`
//! - `require_auth = false` → any token accepted
//! - Token "revocation" via config change (restart server)
//!
//! # Note on DB-token auth
//! The current session handler validates only against `config.auth.admin_token`.
//! Tokens stored in the `tokens` table (dashboard API) are not checked during
//! WebSocket auth.  The revocation test therefore tests the config-level
//! mechanism (require_auth + admin_token) rather than DB-level revocation.

#[path = "../common/mod.rs"]
mod common;

use common::*;

// ── 1. Invalid token is rejected ─────────────────────────────────────────────

#[tokio::test]
async fn invalid_token_returns_auth_error() {
    init_tracing();
    let server = TestServer::start().await; // require_auth = true

    let err = TestClient::connect_expect_auth_error(&server, "not-the-right-token")
        .await
        .expect("should receive AuthError frame");

    assert!(
        !err.is_empty(),
        "AuthError message should not be empty; got: {err}"
    );
}

// ── 2. Valid (admin) token succeeds ─────────────────────────────────────────

#[tokio::test]
async fn valid_token_returns_auth_ok() {
    init_tracing();
    let server = TestServer::start().await;

    let client = TestClient::connect(&server)
        .await
        .expect("auth should succeed");

    assert!(
        client.session_id.is_some(),
        "session_id must be set after AuthOk"
    );
}

// ── 3. require_auth = false accepts any token ─────────────────────────────────

#[tokio::test]
async fn require_auth_false_accepts_any_token() {
    init_tracing();
    let server = TestServer::start_with(false, "ignored-admin-token").await;

    // Should succeed with a completely random token.
    let client = TestClient::connect_with_token(&server, "totally-random-token-12345")
        .await
        .expect("auth should succeed with require_auth=false");

    assert!(client.session_id.is_some());
}

// ── 4. Token "revocation" — different admin_token rejects old credential ──────

/// When the server is (re)started with a different admin_token, old credentials
/// are rejected.  This simulates revoking access by rotating the server secret.
#[tokio::test]
async fn rotated_admin_token_rejects_old_credential() {
    init_tracing();

    // First server accepts "secret-v1".
    let server_v1 = TestServer::start_with(true, "secret-v1").await;
    let client = TestClient::connect_with_token(&server_v1, "secret-v1")
        .await
        .expect("v1 token should work");
    assert!(client.session_id.is_some());

    // Stop first server.
    drop(server_v1);
    // Small grace period for sockets to be released.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Second server uses "secret-v2" — old credential is rejected.
    let server_v2 = TestServer::start_with(true, "secret-v2").await;

    let err = TestClient::connect_expect_auth_error(&server_v2, "secret-v1")
        .await
        .expect("should receive AuthError for old token");

    assert!(!err.is_empty(), "should report an auth error; got: {err}");
}

// ── 5. Two concurrent sessions from same server ───────────────────────────────

#[tokio::test]
async fn two_concurrent_sessions_are_independent() {
    init_tracing();
    let server = TestServer::start().await;

    let client1 = TestClient::connect(&server).await.expect("client1 auth");
    let client2 = TestClient::connect(&server).await.expect("client2 auth");

    let id1 = client1.session_id.unwrap();
    let id2 = client2.session_id.unwrap();

    assert_ne!(id1, id2, "each session must have a unique ID");
    assert_eq!(server.core.sessions.len(), 2);
}

// ── 6. Dashboard token API integration ───────────────────────────────────────
//
// Demonstrates round-trip token management through the HTTP dashboard API.
// Auth via the control WS still only checks admin_token; this test validates
// the dashboard API itself (create / list / delete).

#[tokio::test]
async fn dashboard_token_crud_works() {
    init_tracing();
    let server = TestServer::start().await;
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);
    let client = insecure_http_client();
    let auth = format!("Bearer {}", server.admin_token);

    // Create a token.
    let resp = client
        .post(format!("{base}/api/tokens"))
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "label": "test-token" }))
        .send()
        .await
        .expect("create token request");

    assert_eq!(resp.status(), 201, "POST /api/tokens should return 201");
    let body: serde_json::Value = resp.json().await.expect("JSON body");
    let token_id = body["id"].as_str().expect("id field").to_string();

    // List tokens — should include the new one.
    let resp = client
        .get(format!("{base}/api/tokens"))
        .header("Authorization", &auth)
        .send()
        .await
        .expect("list tokens");

    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.expect("JSON body");
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert!(
        ids.contains(&token_id.as_str()),
        "new token must appear in list"
    );

    // Delete the token.
    let resp = client
        .delete(format!("{base}/api/tokens/{token_id}"))
        .header("Authorization", &auth)
        .send()
        .await
        .expect("delete token");

    assert!(
        resp.status() == 200 || resp.status() == 204,
        "DELETE should return 200 or 204; got {}",
        resp.status()
    );

    // Confirm it's gone.
    let resp = client
        .get(format!("{base}/api/tokens"))
        .header("Authorization", &auth)
        .send()
        .await
        .expect("list tokens after delete");

    let list: serde_json::Value = resp.json().await.expect("JSON body");
    let ids_after: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert!(
        !ids_after.contains(&token_id.as_str()),
        "deleted token must not appear in list"
    );
}

// ── 7. Failed-auth throttle — control plane ──────────────────────────────────
//
// `auth.max_failed_auth_per_minute` (10 in the harness) caps failed `Auth`
// frames per source IP. Once exhausted, the server rejects *before* comparing
// the token, so even the correct admin token is refused until the window
// slides. Every test gets its own server, so 127.0.0.1's budget is private
// to this test.

#[tokio::test]
async fn control_plane_throttles_after_repeated_failures() {
    init_tracing();
    let server = TestServer::start().await;
    let budget = server.config.auth.max_failed_auth_per_minute;

    for i in 0..budget {
        let err = TestClient::connect_expect_auth_error(&server, "wrong-token")
            .await
            .expect("should receive AuthError frame");
        assert_eq!(
            err, "invalid token",
            "attempt {i} should be a plain rejection"
        );
    }

    // Budget exhausted: the next bad attempt is refused outright …
    let err = TestClient::connect_expect_auth_error(&server, "wrong-token")
        .await
        .expect("should receive AuthError frame");
    assert_eq!(err, "too many failed auth attempts");

    // … and so is the *correct* token from the same source — the throttle
    // sits in front of the comparison, not behind it.
    let err = TestClient::connect_expect_auth_error(&server, &server.admin_token)
        .await
        .expect("throttled peer should receive AuthError even with the right token");
    assert_eq!(err, "too many failed auth attempts");
}

// ── 8. Failed-auth throttle — dashboard API ──────────────────────────────────

#[tokio::test]
async fn dashboard_throttles_failed_bearer_attempts() {
    init_tracing();
    let server = TestServer::start().await;
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);
    let client = insecure_http_client();
    let budget = server.config.auth.max_failed_auth_per_minute;

    for i in 0..budget {
        let resp = client
            .get(format!("{base}/api/tunnels"))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 401, "attempt {i} should be a plain 401");
    }

    // Over budget: 429 for a bad token …
    let resp = client
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 429);
    let body: serde_json::Value = resp.json().await.expect("JSON body");
    assert_eq!(body["error"], "too many failed auth attempts");

    // … and for the correct admin token from the same source.
    let resp = client
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {}", server.admin_token))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 429);

    // Public endpoints carry no Authorization header and are never throttled.
    let resp = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
}

/// A request with no `Authorization` header at all is not a credential guess
/// and must not eat into the budget.
#[tokio::test]
async fn dashboard_missing_token_does_not_count_toward_throttle() {
    init_tracing();
    let server = TestServer::start().await;
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);
    let client = insecure_http_client();
    let budget = server.config.auth.max_failed_auth_per_minute;

    for _ in 0..(budget + 5) {
        let resp = client
            .get(format!("{base}/api/tunnels"))
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 401);
    }

    let resp = client
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {}", server.admin_token))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "admin token must still work");
}
