//! P2P tunnel integration tests.
//!
//! Tests the P2P publisher registration, subscriber connect, secret
//! verification, and error cases.  Data relay is tested at the protocol
//! level (registration + P2pConnect frames).

#[path = "../common/mod.rs"]
mod common;

use common::*;

// ── P2P registration ─────────────────────────────────────────────────────────

#[tokio::test]
async fn p2p_publisher_registration() {
    init_tracing();

    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await.expect("auth");

    let (tunnel_id, name) = client
        .register_p2p_tunnel("my-game", "secret-hash-abc")
        .await
        .expect("P2P registration");

    assert_eq!(name, "my-game");
    assert!(!tunnel_id.is_nil());
    assert!(
        server.core.p2p_tunnels.contains_key("my-game"),
        "P2P tunnel should be in the registry"
    );
}

#[tokio::test]
async fn p2p_duplicate_name_rejected() {
    init_tracing();

    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await.expect("auth");

    client
        .register_p2p_tunnel("clash", "hash1")
        .await
        .expect("first registration");

    let result = client.register_p2p_tunnel("clash", "hash2").await;
    assert!(
        result.is_err(),
        "duplicate P2P tunnel name should be rejected"
    );
    assert!(
        result.unwrap_err().contains("already in use"),
        "error should mention 'already in use'"
    );
}

// ── P2P connect ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn p2p_connect_unknown_tunnel() {
    init_tracing();

    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await.expect("auth");

    let result = client.p2p_connect("nonexistent", "some-hash").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[tokio::test]
async fn p2p_connect_wrong_secret() {
    init_tracing();

    let server = TestServer::start().await;

    // Publisher registers.
    let mut publisher = TestClient::connect(&server).await.expect("pub auth");
    publisher
        .register_p2p_tunnel("secret-test", "correct-hash")
        .await
        .expect("register");

    // Subscriber connects with wrong secret.
    let mut subscriber = TestClient::connect(&server).await.expect("sub auth");
    let result = subscriber.p2p_connect("secret-test", "wrong-hash").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("invalid P2P secret"));
}

#[tokio::test]
async fn p2p_connect_correct_secret() {
    init_tracing();

    let server = TestServer::start().await;

    // Publisher registers.
    let mut publisher = TestClient::connect(&server).await.expect("pub auth");
    let _pub_session = publisher.session_id.unwrap();
    publisher
        .register_p2p_tunnel("relay-test", "the-hash")
        .await
        .expect("register");

    // Subscriber connects with correct secret.
    let mut subscriber = TestClient::connect(&server).await.expect("sub auth");
    let conn_id = subscriber.p2p_connect("relay-test", "the-hash").await;

    assert!(
        conn_id.is_ok(),
        "P2P connect with correct secret should succeed"
    );
    assert!(!conn_id.unwrap().is_nil(), "conn_id should be valid");
}

// ── cleanup ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn p2p_tunnel_cleaned_up_on_disconnect() {
    init_tracing();

    let server = TestServer::start().await;

    {
        let mut client = TestClient::connect(&server).await.expect("auth");
        client
            .register_p2p_tunnel("ephemeral", "hash")
            .await
            .expect("register");
        assert!(server.core.p2p_tunnels.contains_key("ephemeral"));
        // Client drops here — session is removed.
    }

    // Give the server time to process the disconnect.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    assert!(
        !server.core.p2p_tunnels.contains_key("ephemeral"),
        "P2P tunnel should be cleaned up after publisher disconnects"
    );
}
