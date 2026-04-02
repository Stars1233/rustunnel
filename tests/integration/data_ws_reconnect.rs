//! Data WebSocket reconnect integration test (Phase C).
//!
//! Verifies that when only the data WebSocket drops (simulating a NAT timeout),
//! the server rebuilds its data plane and a new data bridge can reconnect
//! without re-authenticating or re-registering tunnels.

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

// ── local echo server ────────────────────────────────────────────────────────

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

    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, shutdown_tx)
}

/// Helper: send data through a TCP tunnel and verify echo response.
async fn verify_tunnel_echo(tunnel_port: u16, payload: &[u8]) {
    let addr: SocketAddr = format!("127.0.0.1:{tunnel_port}").parse().unwrap();
    let mut conn = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to tunnel port");
    conn.write_all(payload).await.expect("write payload");
    let mut buf = vec![0u8; payload.len()];
    conn.read_exact(&mut buf).await.expect("read response");
    assert_eq!(&buf, payload, "echo response must match");
}

// ── test: data WS reconnect preserves tunnel ─────────────────────────────────

#[tokio::test]
async fn data_ws_reconnect_preserves_tunnel() {
    init_tracing();

    let server = TestServer::start().await;
    let (local_addr, _echo_shutdown) = start_echo_server().await;

    // 1. Connect and register a TCP tunnel.
    let mut client = TestClient::connect(&server).await.expect("auth");
    let session_id = client.session_id.unwrap();
    let (_tunnel_id, assigned_port) = client.register_tcp_tunnel().await.expect("register");

    // 2. Connect the data bridge and verify the tunnel works.
    let (ready_rx, abort_handle) = connect_data_bridge_abortable(&server, session_id, local_addr);
    ready_rx.await.expect("data bridge ready");

    verify_tunnel_echo(assigned_port, b"before-drop").await;

    // 3. Forcibly kill the data WebSocket (simulates NAT timeout).
    abort_handle.abort();

    // Give the server time to detect the data WS drop and rebuild the
    // MuxSession (the driver_handle arm in main_loop fires).
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 4. Reconnect a new data bridge on the same session — the server should
    //    have a fresh pipe ready.
    let (ready_rx2, _abort_handle2) =
        connect_data_bridge_abortable(&server, session_id, local_addr);
    ready_rx2.await.expect("data bridge reconnect ready");

    // 5. Verify the tunnel still works with the same assigned port.
    verify_tunnel_echo(assigned_port, b"after-reconnect").await;
}

// ── test: multiple data WS reconnects work ───────────────────────────────────

#[tokio::test]
async fn data_ws_survives_multiple_reconnects() {
    init_tracing();

    let server = TestServer::start().await;
    let (local_addr, _echo_shutdown) = start_echo_server().await;

    let mut client = TestClient::connect(&server).await.expect("auth");
    let session_id = client.session_id.unwrap();
    let (_tunnel_id, assigned_port) = client.register_tcp_tunnel().await.expect("register");

    for round in 1..=3 {
        // Connect data bridge.
        let (ready_rx, abort_handle) =
            connect_data_bridge_abortable(&server, session_id, local_addr);
        ready_rx.await.expect("data bridge ready");

        // Verify tunnel works.
        let payload = format!("round-{round}");
        verify_tunnel_echo(assigned_port, payload.as_bytes()).await;

        // Kill the data WS.
        abort_handle.abort();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // Final reconnect — tunnel should still work.
    let (ready_rx, _abort) = connect_data_bridge_abortable(&server, session_id, local_addr);
    ready_rx.await.expect("final data bridge ready");
    verify_tunnel_echo(assigned_port, b"final").await;
}
