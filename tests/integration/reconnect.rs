//! Reconnect integration test.
//!
//! # What this tests
//!
//! Simulates a server restart and client reconnection:
//!   1. Start server v1, connect client, register HTTP tunnel, verify it works.
//!   2. Drop server v1 (aborts all tasks — simulates server death).
//!   3. Wait for OS to release the ports.
//!   4. Start server v2 on the *same* ports with the same admin token.
//!   5. Connect a new client, register a new tunnel — verify it works.
//!
//! The test validates that there is no state leak between server instances and
//! that a reconnecting client can register tunnels and proxy traffic normally.

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;

use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

// ── local echo server (same as tcp_tunnel.rs) ─────────────────────────────────

async fn start_echo_server() -> (SocketAddr, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind echo server");
    let addr = listener.local_addr().unwrap();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                result = listener.accept() => {
                    let Ok((mut stream, _)) = result else { break };
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 4096];
                        loop {
                            let n = match stream.read(&mut buf).await {
                                Ok(0)  => break,
                                Ok(n)  => n,
                                Err(_) => break,
                            };
                            if stream.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (addr, shutdown_tx)
}

// ── reconnect: TCP tunnel works after server restart ─────────────────────────

#[tokio::test]
async fn tcp_tunnel_works_after_server_restart() {
    init_tracing();

    let admin_token = "reconnect-test-token";

    // ── Phase 1: initial server ───────────────────────────────────────────────

    let server_v1 = TestServer::start_with(true, admin_token).await;
    let control_port = server_v1.control_port;
    let http_port = server_v1.http_port;
    let https_port = server_v1.https_port;
    let dashboard_port = server_v1.dashboard_port;
    let tcp_port_range = server_v1.config.limits.tcp_port_range;

    // Start echo server.
    let (local_addr, _echo_shutdown) = start_echo_server().await;

    // Connect and register a TCP tunnel on server v1.
    let mut client_v1 = TestClient::connect(&server_v1).await.expect("v1 auth");
    let session_id_v1 = client_v1.session_id.unwrap();
    let (_tunnel_id, assigned_port) = client_v1
        .register_tcp_tunnel()
        .await
        .expect("v1 tunnel registration");

    // Connect the data WebSocket bridge and wait for it to be ready.
    connect_data_bridge(&server_v1, session_id_v1, local_addr)
        .await
        .expect("v1 data bridge ready");

    // Verify the tunnel works on server v1.
    let tunnel_addr: SocketAddr = format!("127.0.0.1:{assigned_port}").parse().unwrap();
    let mut conn = tokio::net::TcpStream::connect(tunnel_addr)
        .await
        .expect("connect to v1 tunnel port");
    conn.write_all(b"ping-v1").await.expect("write v1");
    let mut buf = [0u8; 7];
    conn.read_exact(&mut buf).await.expect("read v1");
    assert_eq!(&buf, b"ping-v1", "echo should work on server v1");
    drop(conn);

    // ── Phase 2: stop server v1 ───────────────────────────────────────────────

    drop(server_v1); // aborts all task handles via Drop impl

    // Give the OS time to release the ports.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // ── Phase 3: restart on the same ports ───────────────────────────────────

    // Build server v2 with the exact same ports and token.
    let server_v2 = TestServer::start_on_ports(
        control_port,
        http_port,
        https_port,
        dashboard_port,
        tcp_port_range,
        [0, 0],
        true,
        admin_token,
    )
    .await;

    // Connect a fresh client and register a new tunnel.
    let mut client_v2 = TestClient::connect(&server_v2).await.expect("v2 auth");
    let session_id_v2 = client_v2.session_id.unwrap();
    let (_tunnel_id2, assigned_port2) = client_v2
        .register_tcp_tunnel()
        .await
        .expect("v2 tunnel registration");

    connect_data_bridge(&server_v2, session_id_v2, local_addr)
        .await
        .expect("v2 data bridge ready");

    // Verify the tunnel works on server v2.
    let tunnel_addr2: SocketAddr = format!("127.0.0.1:{assigned_port2}").parse().unwrap();
    let mut conn2 = tokio::net::TcpStream::connect(tunnel_addr2)
        .await
        .expect("connect to v2 tunnel port");
    conn2.write_all(b"ping-v2").await.expect("write v2");
    let mut buf2 = [0u8; 7];
    conn2.read_exact(&mut buf2).await.expect("read v2");
    assert_eq!(
        &buf2, b"ping-v2",
        "echo should work on server v2 after restart"
    );
}

// ── reconnect: old tunnel port no longer responds after restart ───────────────

#[tokio::test]
async fn old_tunnel_port_closed_after_restart() {
    init_tracing();

    let admin_token = "reconnect-close-test-token";

    // Start server v1, register a TCP tunnel, record its port.
    let server_v1 = TestServer::start_with(true, admin_token).await;
    let mut client_v1 = TestClient::connect(&server_v1).await.expect("v1 auth");
    let (_, old_port) = client_v1.register_tcp_tunnel().await.expect("v1 tunnel");

    // Drop the server — all listeners should close.
    drop(server_v1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Attempt to connect to the old port — must fail (connection refused).
    let result = tokio::net::TcpStream::connect(
        format!("127.0.0.1:{old_port}")
            .parse::<SocketAddr>()
            .unwrap(),
    )
    .await;

    assert!(
        result.is_err(),
        "old tunnel port {old_port} should be closed after server shutdown"
    );
}

// ── reconnect: new client authenticates successfully after restart ─────────────

#[tokio::test]
async fn new_client_auth_succeeds_after_restart() {
    init_tracing();

    let admin_token = "reconnect-auth-test-token";

    // Start and stop server v1.
    let server_v1 = TestServer::start_with(true, admin_token).await;
    let control_port = server_v1.control_port;
    let http_port = server_v1.http_port;
    let https_port = server_v1.https_port;
    let dashboard_port = server_v1.dashboard_port;
    let tcp_port_range = server_v1.config.limits.tcp_port_range;
    drop(server_v1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Start server v2 on the same ports.
    let server_v2 = TestServer::start_on_ports(
        control_port,
        http_port,
        https_port,
        dashboard_port,
        tcp_port_range,
        [0, 0],
        true,
        admin_token,
    )
    .await;

    // A fresh client must authenticate successfully.
    let client = TestClient::connect(&server_v2)
        .await
        .expect("auth must succeed on restarted server");

    assert!(
        client.session_id.is_some(),
        "session_id must be assigned after AuthOk on restarted server"
    );
}
