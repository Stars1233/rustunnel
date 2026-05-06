//! Cross-version compatibility tests (TUNNEL-7 Phase 6).
//!
//! The `[testing] override_server_version` knob in `ServerConfig` lets us
//! spin up a server that *advertises* an old version in `AuthOk` while
//! still running the current binary. This proves the server-side half
//! of the cross-version contract: the wire frame carries whatever
//! `server_version` an operator configures, with `CARGO_PKG_VERSION` as
//! the fallback. The client-side gate (which uses that string to decide
//! whether to emit group fields and spawn probe loops) is unit-tested
//! in `rustunnel-client::version::tests` — those tests cover the
//! `parse_semver` + `server_supports_load_balancing` decision matrix
//! deterministically without needing a server at all.
//!
//! What we cannot meaningfully integration-test from the harness:
//! the test `TestClient` builds raw `RegisterTunnel` frames manually,
//! so it doesn't go through `rustunnel-client::control::connect` where
//! the version gate lives. Trying to assert "the gate suppressed group
//! fields" from this layer would require running the actual client
//! binary, which the integration suite doesn't do today. Phase 4 and
//! Phase 6 unit tests cover that path.

#[path = "../common/mod.rs"]
mod common;

use common::*;

/// Spin up a server that *pretends* to be `pretend_version` in `AuthOk`.
/// Everything else about the server is normal (current binary, normal
/// load-balancing behaviour) — only the advertised version changes.
async fn start_with_pretend_version(pretend_version: &str) -> TestServer {
    let control_port = free_port();
    let http_port = free_port();
    let https_port = free_port();
    let dashboard_port = free_port();
    let tcp_low = alloc_tcp_port_range(10);
    let udp_low = alloc_tcp_port_range(10);
    TestServer::start_with_opts(TestServerOpts {
        control_port,
        http_port,
        https_port,
        dashboard_port,
        tcp_port_range: [tcp_low, tcp_low + 9],
        udp_port_range: [udp_low, udp_low + 9],
        require_auth: true,
        admin_token: "integration-test-token".into(),
        load_balancing_enabled: true,
        alert_webhook_url: None,
        server_version_override: Some(pretend_version.to_string()),
    })
    .await
}

// ── `[testing] override_server_version` actually overrides AuthOk ──────────

#[tokio::test]
async fn override_makes_server_advertise_old_version() {
    init_tracing();
    let server = start_with_pretend_version("0.5.0").await;

    let client = TestClient::connect(&server).await.expect("auth");

    assert_eq!(
        client.server_version.as_deref(),
        Some("0.5.0"),
        "TestClient should capture the overridden server_version verbatim"
    );
}

// ── Without the override, the server reports CARGO_PKG_VERSION ──────────────

#[tokio::test]
async fn no_override_uses_cargo_pkg_version() {
    init_tracing();
    // Default TestServer — no version override. We can't hardcode the
    // expected value because Cargo.toml drifts; instead, assert that the
    // string parses as semver and is at least 0.7.0 (the floor where
    // load-balancing support exists, which is what the client gate cares
    // about).
    let server = TestServer::start_with_load_balancing().await;
    let client = TestClient::connect(&server).await.expect("auth");

    let v = client
        .server_version
        .as_deref()
        .expect("server_version captured");

    // Naive parse so we don't pull a semver dep into the integration crate.
    let mut parts = v.split('.');
    let major: u32 = parts.next().unwrap().parse().expect("major");
    let minor: u32 = parts.next().unwrap().parse().expect("minor");
    assert!(
        (major, minor) >= (0, 7),
        "server_version {v} must be >= 0.7.0 (post-Phase-4 floor)"
    );
}
