//! TUNNEL-1: Per-tenant scoping of `/api/tunnels` and `/api/groups`.
//!
//! Before this change every authenticated caller of the dashboard API
//! saw every tunnel and group across every tenant. These tests pin the
//! intended behaviour:
//!
//! - **Admin token** → sees everything (unchanged historical behaviour).
//! - **User-scoped DB token** (`tokens.user_id IS NOT NULL`) → sees only
//!   tunnels/groups whose owning sessions are authenticated against
//!   tokens belonging to the same `user_id`.
//! - **No-user-id DB token** (legacy) → sees only tunnels opened by
//!   sessions authenticated with that exact token row.
//! - **Hidden tunnels** return `404` (not `403`) so existence isn't leaked.
//! - **Aggregate group counters** never include cross-tenant data —
//!   `member_count`, `healthy_count`, etc. always reflect just the
//!   members the caller is allowed to see.

#[path = "../common/mod.rs"]
mod common;

use common::*;
use uuid::Uuid;

/// Insert a user row and return its ID.
async fn create_user(db: &rustunnel_server::db::Db, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, status, email_verified, auth_method) \
         VALUES ($1, $2, $3, 'active', true, 'password')",
    )
    .bind(id)
    .bind(email)
    .bind("not-a-real-hash")
    .execute(&db.pg)
    .await
    .expect("insert user");
    id
}

/// Insert a token tied to (optional) `user_id` and return `(token_id, raw_token)`.
async fn create_token_for_user(
    db: &rustunnel_server::db::Db,
    label: &str,
    user_id: Option<Uuid>,
) -> (String, String) {
    let raw = format!("rt_test_{}", Uuid::new_v4());
    let hash = rustunnel_server::db::hash_token(&raw);
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO tokens (id, token_hash, label, created_at, user_id, status, unlimited) \
         VALUES ($1, $2, $3, now(), $4, 'active', false)",
    )
    .bind(&id)
    .bind(&hash)
    .bind(label)
    .bind(user_id)
    .execute(&db.pg)
    .await
    .expect("insert token");
    (id, raw)
}

/// Strip a token row by ID. Used to reset state between tests so a flaky
/// run on a shared DB doesn't pollute the next run.
async fn delete_token(db: &rustunnel_server::db::Db, token_id: &str) {
    let _ = sqlx::query("DELETE FROM tokens WHERE id = $1")
        .bind(token_id)
        .execute(&db.pg)
        .await;
}

async fn delete_user(db: &rustunnel_server::db::Db, user_id: Uuid) {
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&db.pg)
        .await;
}

fn count_tunnels_with_id(body: &serde_json::Value, tunnel_id: Uuid) -> usize {
    body.as_array()
        .map(|a| {
            a.iter()
                .filter(|t| t["tunnel_id"].as_str() == Some(tunnel_id.to_string().as_str()))
                .count()
        })
        .unwrap_or(0)
}

// Tag a freshly-registered tunnel's owning session with `db_token_id` /
// `user_id` so the dashboard scope filter can route it to the right
// tenant. Production wires these up automatically from the auth handshake;
// tests bypass that path because `TestClient` connects with the admin
// token, so we patch SessionInfo directly.
fn assign_session_owner(
    server: &TestServer,
    session_id: Uuid,
    db_token_id: Option<String>,
    user_id: Option<Uuid>,
) {
    let mut entry = server
        .core
        .sessions
        .get_mut(&session_id)
        .expect("session not found");
    entry.db_token_id = db_token_id;
    entry.user_id = user_id;
}

// ── 1. Two users — each only sees their own tunnels ─────────────────────────

#[tokio::test]
async fn list_tunnels_filters_to_caller_user() {
    init_tracing();
    let server = TestServer::start().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    // Two distinct users, each with their own DB token.
    let user_a = create_user(&server.db, &format!("a-{}@example.com", Uuid::new_v4())).await;
    let user_b = create_user(&server.db, &format!("b-{}@example.com", Uuid::new_v4())).await;
    let (token_a_id, token_a_raw) = create_token_for_user(&server.db, "a", Some(user_a)).await;
    let (token_b_id, token_b_raw) = create_token_for_user(&server.db, "b", Some(user_b)).await;

    // Two clients connect (using the admin token because TestClient's
    // control-plane auth path doesn't share verify_token with the
    // dashboard). We then re-tag each session with a DB token so the
    // dashboard scope filter recognises ownership.
    let mut client_a = TestClient::connect(&server).await.expect("auth a");
    let mut client_b = TestClient::connect(&server).await.expect("auth b");
    let sess_a = client_a.session_id.unwrap();
    let sess_b = client_b.session_id.unwrap();
    assign_session_owner(&server, sess_a, Some(token_a_id.clone()), Some(user_a));
    assign_session_owner(&server, sess_b, Some(token_b_id.clone()), Some(user_b));

    let (tun_a, _, _) = client_a
        .register_http_tunnel(Some(&format!("a-{}", Uuid::new_v4())))
        .await
        .expect("register a");
    let (tun_b, _, _) = client_b
        .register_http_tunnel(Some(&format!("b-{}", Uuid::new_v4())))
        .await
        .expect("register b");

    // Admin sees both.
    let resp = http
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {}", server.admin_token))
        .send()
        .await
        .expect("admin list");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(count_tunnels_with_id(&body, tun_a), 1);
    assert_eq!(count_tunnels_with_id(&body, tun_b), 1);

    // User A's token sees only A's tunnel.
    let resp = http
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {token_a_raw}"))
        .send()
        .await
        .expect("a list");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(count_tunnels_with_id(&body, tun_a), 1);
    assert_eq!(
        count_tunnels_with_id(&body, tun_b),
        0,
        "user A must not see user B's tunnel"
    );

    // User B's token sees only B's tunnel.
    let resp = http
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {token_b_raw}"))
        .send()
        .await
        .expect("b list");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(count_tunnels_with_id(&body, tun_b), 1);
    assert_eq!(count_tunnels_with_id(&body, tun_a), 0);

    // Cleanup so other tests on the same shared DB don't see these rows.
    delete_token(&server.db, &token_a_id).await;
    delete_token(&server.db, &token_b_id).await;
    delete_user(&server.db, user_a).await;
    delete_user(&server.db, user_b).await;
}

// ── 2. Get-by-ID returns 404 for cross-tenant tunnels ───────────────────────

#[tokio::test]
async fn get_tunnel_returns_404_for_other_tenant() {
    init_tracing();
    let server = TestServer::start().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    let user_a = create_user(&server.db, &format!("a-{}@example.com", Uuid::new_v4())).await;
    let user_b = create_user(&server.db, &format!("b-{}@example.com", Uuid::new_v4())).await;
    let (token_a_id, token_a_raw) = create_token_for_user(&server.db, "a", Some(user_a)).await;
    let (token_b_id, token_b_raw) = create_token_for_user(&server.db, "b", Some(user_b)).await;

    let mut client_a = TestClient::connect(&server).await.expect("auth a");
    let client_b = TestClient::connect(&server).await.expect("auth b");
    assign_session_owner(
        &server,
        client_a.session_id.unwrap(),
        Some(token_a_id.clone()),
        Some(user_a),
    );
    assign_session_owner(
        &server,
        client_b.session_id.unwrap(),
        Some(token_b_id.clone()),
        Some(user_b),
    );

    let (tun_a, _, _) = client_a
        .register_http_tunnel(Some(&format!("a-{}", Uuid::new_v4())))
        .await
        .expect("register a");

    // B asking for A's tunnel must get 404 (not 403 — 404 hides existence).
    let resp = http
        .get(format!("{base}/api/tunnels/{tun_a}"))
        .header("Authorization", format!("Bearer {token_b_raw}"))
        .send()
        .await
        .expect("b get a");
    assert_eq!(resp.status(), 404, "cross-tenant must 404");

    // A still sees their own.
    let resp = http
        .get(format!("{base}/api/tunnels/{tun_a}"))
        .header("Authorization", format!("Bearer {token_a_raw}"))
        .send()
        .await
        .expect("a get a");
    assert_eq!(resp.status(), 200);

    // Same rule for `/health-events` and `/udp-sessions`.
    let resp = http
        .get(format!("{base}/api/tunnels/{tun_a}/health-events"))
        .header("Authorization", format!("Bearer {token_b_raw}"))
        .send()
        .await
        .expect("b get health-events");
    assert_eq!(resp.status(), 404);
    let resp = http
        .get(format!("{base}/api/tunnels/{tun_a}/udp-sessions"))
        .header("Authorization", format!("Bearer {token_b_raw}"))
        .send()
        .await
        .expect("b get udp-sessions");
    assert_eq!(resp.status(), 404);

    // And requests/replay sub-resources.
    let resp = http
        .get(format!("{base}/api/tunnels/{tun_a}/requests"))
        .header("Authorization", format!("Bearer {token_b_raw}"))
        .send()
        .await
        .expect("b get requests");
    assert_eq!(resp.status(), 404);

    delete_token(&server.db, &token_a_id).await;
    delete_token(&server.db, &token_b_id).await;
    delete_user(&server.db, user_a).await;
    delete_user(&server.db, user_b).await;
}

// ── 3. force_close cannot reach other tenants' tunnels ──────────────────────

#[tokio::test]
async fn force_close_blocked_for_other_tenant() {
    init_tracing();
    let server = TestServer::start().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    let user_a = create_user(&server.db, &format!("a-{}@example.com", Uuid::new_v4())).await;
    let user_b = create_user(&server.db, &format!("b-{}@example.com", Uuid::new_v4())).await;
    let (token_a_id, _token_a_raw) = create_token_for_user(&server.db, "a", Some(user_a)).await;
    let (token_b_id, token_b_raw) = create_token_for_user(&server.db, "b", Some(user_b)).await;

    let mut client_a = TestClient::connect(&server).await.expect("auth a");
    let client_b = TestClient::connect(&server).await.expect("auth b");
    assign_session_owner(
        &server,
        client_a.session_id.unwrap(),
        Some(token_a_id.clone()),
        Some(user_a),
    );
    assign_session_owner(
        &server,
        client_b.session_id.unwrap(),
        Some(token_b_id.clone()),
        Some(user_b),
    );

    let (tun_a, _, _) = client_a
        .register_http_tunnel(Some(&format!("a-{}", Uuid::new_v4())))
        .await
        .expect("register a");

    // B's DELETE attempt must 404 and the tunnel must remain.
    let resp = http
        .delete(format!("{base}/api/tunnels/{tun_a}"))
        .header("Authorization", format!("Bearer {token_b_raw}"))
        .send()
        .await
        .expect("b delete a");
    assert_eq!(resp.status(), 404);
    assert!(
        server
            .core
            .http_routes
            .iter()
            .any(|e| e.value().members.contains_key(&tun_a)),
        "tunnel must still be live after foreign-tenant DELETE"
    );

    delete_token(&server.db, &token_a_id).await;
    delete_token(&server.db, &token_b_id).await;
    delete_user(&server.db, user_a).await;
    delete_user(&server.db, user_b).await;
}

// ── 4. /api/groups: aggregate counters reflect only visible members ─────────

#[tokio::test]
async fn list_groups_filters_to_caller_user() {
    init_tracing();
    let server = TestServer::start_with_load_balancing().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    let user_a = create_user(&server.db, &format!("a-{}@example.com", Uuid::new_v4())).await;
    let user_b = create_user(&server.db, &format!("b-{}@example.com", Uuid::new_v4())).await;
    let (token_a_id, token_a_raw) = create_token_for_user(&server.db, "a", Some(user_a)).await;
    let (token_b_id, _token_b_raw) = create_token_for_user(&server.db, "b", Some(user_b)).await;

    let mut client_a = TestClient::connect(&server).await.expect("auth a");
    let mut client_b = TestClient::connect(&server).await.expect("auth b");
    assign_session_owner(
        &server,
        client_a.session_id.unwrap(),
        Some(token_a_id.clone()),
        Some(user_a),
    );
    assign_session_owner(
        &server,
        client_b.session_id.unwrap(),
        Some(token_b_id.clone()),
        Some(user_b),
    );

    // User A registers a load-balanced group.
    let group_a = format!("group-a-{}", Uuid::new_v4());
    let key_a = format!("hash-a-{}", Uuid::new_v4());
    let (_tun_a, sub_a, _) = client_a
        .register_http_tunnel_grouped(Some(&format!("sub-a-{}", Uuid::new_v4())), &group_a, &key_a)
        .await
        .expect("register a grouped");

    // User B registers a different load-balanced group.
    let group_b = format!("group-b-{}", Uuid::new_v4());
    let key_b = format!("hash-b-{}", Uuid::new_v4());
    let (_tun_b, sub_b, _) = client_b
        .register_http_tunnel_grouped(Some(&format!("sub-b-{}", Uuid::new_v4())), &group_b, &key_b)
        .await
        .expect("register b grouped");

    // Admin sees both groups.
    let resp = http
        .get(format!("{base}/api/groups"))
        .header("Authorization", format!("Bearer {}", server.admin_token))
        .send()
        .await
        .expect("admin list groups");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let labels: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g["label"].as_str())
        .collect();
    assert!(labels.contains(&sub_a.as_str()));
    assert!(labels.contains(&sub_b.as_str()));

    // User A's token sees only group A.
    let resp = http
        .get(format!("{base}/api/groups"))
        .header("Authorization", format!("Bearer {token_a_raw}"))
        .send()
        .await
        .expect("a list groups");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    let a_groups: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|g| g["label"].as_str() == Some(sub_a.as_str()))
        .collect();
    let b_groups: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|g| g["label"].as_str() == Some(sub_b.as_str()))
        .collect();
    assert_eq!(a_groups.len(), 1);
    assert_eq!(
        b_groups.len(),
        0,
        "user A must not see user B's group at all"
    );
    // Aggregate counters reflect only A's member.
    assert_eq!(a_groups[0]["member_count"].as_u64(), Some(1));
    assert_eq!(a_groups[0]["members"].as_array().unwrap().len(), 1);

    delete_token(&server.db, &token_a_id).await;
    delete_token(&server.db, &token_b_id).await;
    delete_user(&server.db, user_a).await;
    delete_user(&server.db, user_b).await;
}

// ── 5. No token → 401, even though endpoint is now scope-aware ──────────────

#[tokio::test]
async fn missing_or_invalid_token_returns_401() {
    init_tracing();
    let server = TestServer::start().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    // No header.
    let resp = http
        .get(format!("{base}/api/tunnels"))
        .send()
        .await
        .expect("no auth");
    assert_eq!(resp.status(), 401);

    // Bad token.
    let resp = http
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", "Bearer not-a-token")
        .send()
        .await
        .expect("bad auth");
    assert_eq!(resp.status(), 401);
}

// ── 6. Legacy DB token (no user_id) is scoped to its own session ────────────

#[tokio::test]
async fn legacy_token_sees_only_its_own_tunnels() {
    init_tracing();
    let server = TestServer::start().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    // Two legacy DB tokens (no user_id) and a real user-scoped token.
    let (legacy_id, legacy_raw) = create_token_for_user(&server.db, "legacy", None).await;
    let (other_legacy_id, _) = create_token_for_user(&server.db, "other-legacy", None).await;

    let mut client_legacy = TestClient::connect(&server).await.expect("auth legacy");
    let mut client_other = TestClient::connect(&server).await.expect("auth other");
    assign_session_owner(
        &server,
        client_legacy.session_id.unwrap(),
        Some(legacy_id.clone()),
        None,
    );
    assign_session_owner(
        &server,
        client_other.session_id.unwrap(),
        Some(other_legacy_id.clone()),
        None,
    );

    let (tun_legacy, _, _) = client_legacy
        .register_http_tunnel(Some(&format!("l-{}", Uuid::new_v4())))
        .await
        .expect("register legacy");
    let (tun_other, _, _) = client_other
        .register_http_tunnel(Some(&format!("o-{}", Uuid::new_v4())))
        .await
        .expect("register other");

    // Legacy caller sees only their own session's tunnel.
    let resp = http
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {legacy_raw}"))
        .send()
        .await
        .expect("legacy list");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(count_tunnels_with_id(&body, tun_legacy), 1);
    assert_eq!(
        count_tunnels_with_id(&body, tun_other),
        0,
        "legacy token must not see other legacy token's tunnels"
    );

    delete_token(&server.db, &legacy_id).await;
    delete_token(&server.db, &other_legacy_id).await;
}

// ── 7. SessionInfo.user_id is populated by the auth handshake ──────────────

#[tokio::test]
async fn session_info_carries_user_id_after_handshake() {
    init_tracing();
    let server = TestServer::start().await;

    let user = create_user(&server.db, &format!("u-{}@example.com", Uuid::new_v4())).await;
    let (token_id, raw) = create_token_for_user(&server.db, "u", Some(user)).await;

    // Connect with the user-scoped DB token directly so the production
    // auth path runs end-to-end. require_auth = true is the default.
    let client = TestClient::connect_with_token(&server, &raw)
        .await
        .expect("auth user");
    let session_id = client.session_id.unwrap();

    let entry = server
        .core
        .sessions
        .get(&session_id)
        .expect("session present");
    assert_eq!(
        entry.user_id,
        Some(user),
        "control-plane handshake must populate SessionInfo.user_id from tokens.user_id"
    );
    assert_eq!(
        entry.db_token_id.as_deref(),
        Some(token_id.as_str()),
        "control-plane handshake must populate SessionInfo.db_token_id"
    );

    drop(entry);
    delete_token(&server.db, &token_id).await;
    delete_user(&server.db, user).await;
}

// ── 8. End-to-end: user-scoped DB token, no SessionInfo shim ───────────────

/// Same scenario as test #1 but the user's tunnel is registered through
/// the production auth handshake (no `assign_session_owner` shim). Proves
/// the whole pipeline — handshake populates `SessionInfo.user_id`, the
/// scope filter reads it back, and the dashboard returns the right
/// rows when the same raw user-scoped token is presented over HTTP.
#[tokio::test]
async fn end_to_end_user_token_pipeline() {
    init_tracing();
    let server = TestServer::start().await;
    let http = insecure_http_client();
    let base = format!("http://127.0.0.1:{}", server.dashboard_port);

    let user = create_user(&server.db, &format!("e2e-{}@example.com", Uuid::new_v4())).await;
    let (token_id, raw) = create_token_for_user(&server.db, "e2e", Some(user)).await;

    // Admin client registers a foreign tunnel — should NOT appear in
    // the user's listing.
    let mut admin = TestClient::connect(&server).await.expect("admin auth");
    let (foreign_tun, _, _) = admin
        .register_http_tunnel(Some(&format!("foreign-{}", Uuid::new_v4())))
        .await
        .expect("register foreign");

    // User client connects with the user-scoped DB token directly.
    let mut user_client = TestClient::connect_with_token(&server, &raw)
        .await
        .expect("user auth");
    let (user_tun, _, _) = user_client
        .register_http_tunnel(Some(&format!("user-{}", Uuid::new_v4())))
        .await
        .expect("register user");

    let resp = http
        .get(format!("{base}/api/tunnels"))
        .header("Authorization", format!("Bearer {raw}"))
        .send()
        .await
        .expect("user list tunnels");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        count_tunnels_with_id(&body, user_tun),
        1,
        "user must see their own tunnel via the production pipeline"
    );
    assert_eq!(
        count_tunnels_with_id(&body, foreign_tun),
        0,
        "user must not see admin-owned tunnels"
    );

    delete_token(&server.db, &token_id).await;
    delete_user(&server.db, user).await;
}
