//! UDP tunnel end-to-end integration test.
//!
//! Full UDP proxy chain:
//!   Public UDP client
//!     -> rustunnel UDP edge (dynamic port listener)
//!       -> yamux stream (framed datagrams via data bridge)
//!         -> local UDP echo server

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;

use common::*;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;

// ── local UDP echo server ────────────────────────────────────────────────────

async fn start_udp_echo_server() -> (SocketAddr, oneshot::Sender<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind UDP echo server");
    let addr = socket.local_addr().unwrap();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                result = socket.recv_from(&mut buf) => {
                    let (len, peer) = match result {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    let _ = socket.send_to(&buf[..len], peer).await;
                }
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (addr, shutdown_tx)
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn udp_tunnel_registration() {
    init_tracing();

    let server = TestServer::start().await;
    let mut client = TestClient::connect(&server).await.expect("auth");
    let (tunnel_id, port) = client.register_udp_tunnel().await.expect("register");

    let [low, high] = server.config.limits.udp_port_range;
    assert!(
        port >= low && port <= high,
        "assigned UDP port {port} outside range [{low}, {high}]"
    );

    // Tunnel ID should be in the core's udp_routes.
    assert!(
        server.core.udp_routes.contains_key(&port),
        "UDP route not found for port {port}"
    );

    // Verify tunnel_id is tracked.
    assert!(
        !tunnel_id.is_nil(),
        "tunnel_id should be a valid UUID"
    );
}

#[tokio::test]
#[ignore = "requires UDP-aware data bridge (tested manually via socat)"]
async fn udp_tunnel_echoes_datagram() {
    init_tracing();

    // 1. Start local UDP echo server.
    let (local_addr, _echo_shutdown) = start_udp_echo_server().await;

    // 2. Start the rustunnel server.
    let server = TestServer::start().await;

    // 3. Connect client and register a UDP tunnel.
    let mut client = TestClient::connect(&server).await.expect("auth");
    let session_id = client.session_id.unwrap();
    let (_tunnel_id, assigned_port) = client.register_udp_tunnel().await.expect("register");

    // 4. Connect the data WebSocket bridge.
    connect_data_bridge(&server, session_id, local_addr)
        .await
        .expect("data bridge ready");

    // Give the bridge a moment to fully establish.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // 5. Send a UDP datagram to the assigned port.
    let client_socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind client");
    let tunnel_addr: SocketAddr = format!("127.0.0.1:{assigned_port}").parse().unwrap();

    client_socket
        .send_to(b"hello udp", tunnel_addr)
        .await
        .expect("send datagram");

    // 6. Read back the echo.
    let mut buf = vec![0u8; 64];
    let recv = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client_socket.recv_from(&mut buf),
    )
    .await
    .expect("echo timeout")
    .expect("recv echo");

    assert_eq!(&buf[..recv.0], b"hello udp", "echo should return exact bytes");
}

#[tokio::test]
async fn two_udp_tunnels_get_distinct_ports() {
    init_tracing();
    let server = TestServer::start().await;

    let mut client = TestClient::connect(&server).await.expect("auth");
    let (_, port1) = client.register_udp_tunnel().await.expect("tunnel 1");
    let (_, port2) = client.register_udp_tunnel().await.expect("tunnel 2");

    assert_ne!(port1, port2, "each UDP tunnel must get a unique port");
}
