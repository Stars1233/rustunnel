//! TCP load-balancing group integration tests (TUNNEL-7 Phase 3).
//!
//! Same shape as the HTTP-groups suite, but exercising the TCP path:
//! two clients register the same `(group, group_key_hash)` and the second
//! reuses the first's allocated port. Connections to that port distribute
//! across the two members at random.
//!
//! Each backend tags its responses with `A:` or `B:` so the test can count
//! who served each connection (TCP has no obvious way to thread metadata
//! through the proxy other than the application-level bytes).

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

/// TCP backend that prefixes every response with `who:` and counts requests.
async fn start_tagged_echo_server(
    who: &'static str,
) -> (SocketAddr, Arc<AtomicU64>, oneshot::Sender<()>) {
    let counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let counter_clone = Arc::clone(&counter);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tagged echo");
    let addr = listener.local_addr().unwrap();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                result = listener.accept() => {
                    let Ok((mut stream, _)) = result else { break };
                    let counter = Arc::clone(&counter_clone);
                    tokio::spawn(async move {
                        // Read up to 32 bytes from the client, then reply
                        // with `who:<echo>` and close. One request, one
                        // tagged response — easy to count from the client.
                        let mut buf = vec![0u8; 32];
                        let n = match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                        let mut response = format!("{who}:").into_bytes();
                        response.extend_from_slice(&buf[..n]);
                        let _ = stream.write_all(&response).await;
                        let _ = stream.shutdown().await;
                    });
                }
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (addr, counter, shutdown_tx)
}

// ── 1. Random dispatch across two TCP members ──────────────────────────────────

#[tokio::test]
async fn tcp_group_random_dispatch_distributes_across_two_members() {
    init_tracing();

    // Two tagged TCP backends.
    let (addr_a, count_a, _hold_a) = start_tagged_echo_server("A").await;
    let (addr_b, count_b, _hold_b) = start_tagged_echo_server("B").await;

    let server = TestServer::start_with_load_balancing().await;

    // Two clients on the same `(group, key)`.
    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");
    let session_a = client_a.session_id.unwrap();
    let session_b = client_b.session_id.unwrap();

    let group = "ssh";
    let key = "deadbeef".repeat(8);

    let (_tid_a, port_a) = client_a
        .register_tcp_tunnel_grouped(group, &key)
        .await
        .expect("client A grouped TCP registration");
    let (_tid_b, port_b) = client_b
        .register_tcp_tunnel_grouped(group, &key)
        .await
        .expect("client B grouped TCP registration");
    assert_eq!(
        port_a, port_b,
        "second TCP group member must reuse the first's port"
    );

    // Each client bridges to its own backend.
    connect_data_bridge(&server, session_a, addr_a)
        .await
        .expect("data bridge A ready");
    connect_data_bridge(&server, session_b, addr_b)
        .await
        .expect("data bridge B ready");

    // N TCP connections; count which backend served each.
    const N: u64 = 60;
    let tunnel_addr: SocketAddr = format!("127.0.0.1:{port_a}").parse().unwrap();
    let mut hits_a = 0u64;
    let mut hits_b = 0u64;
    for i in 0..N {
        let mut conn = tokio::net::TcpStream::connect(tunnel_addr)
            .await
            .expect("connect to tunnel TCP port");
        let payload = format!("ping{i}");
        conn.write_all(payload.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        conn.read_to_end(&mut response).await.expect("read echo");
        let prefix = response
            .iter()
            .position(|&b| b == b':')
            .map(|idx| std::str::from_utf8(&response[..idx]).unwrap_or(""))
            .unwrap_or("");
        match prefix {
            "A" => hits_a += 1,
            "B" => hits_b += 1,
            other => panic!("unexpected backend tag: {other:?} (full response: {response:?})"),
        }
    }

    assert_eq!(hits_a + hits_b, N);
    // 60 trials with p=0.5 — [12, 48] is a wide envelope that catches a
    // "always picks the same member" regression with effectively zero flake.
    assert!(
        (12..=48).contains(&hits_a),
        "expected dispatch to balance; got A={hits_a}, B={hits_b}"
    );
    assert!(
        (12..=48).contains(&hits_b),
        "expected dispatch to balance; got A={hits_a}, B={hits_b}"
    );

    // Per-backend hit counters must agree.
    assert_eq!(count_a.load(Ordering::Relaxed), hits_a);
    assert_eq!(count_b.load(Ordering::Relaxed), hits_b);
}

// ── 2. Different keys produce different ports ──────────────────────────────────

#[tokio::test]
async fn tcp_group_different_keys_get_separate_ports() {
    init_tracing();
    let server = TestServer::start_with_load_balancing().await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    let (_tid_a, port_a) = client_a
        .register_tcp_tunnel_grouped("ssh", &"a".repeat(64))
        .await
        .expect("client A registers");
    let (_tid_b, port_b) = client_b
        .register_tcp_tunnel_grouped("ssh", &"b".repeat(64))
        .await
        .expect("client B registers (different key)");

    assert_ne!(
        port_a, port_b,
        "different keys → different pools → different ports"
    );
}

// ── 3. Disabled flag falls through to solo TCP allocation ──────────────────────

#[tokio::test]
async fn tcp_group_with_kill_switch_off_falls_through_to_solo() {
    init_tracing();
    // Default TestServer: load_balancing.enabled = false.
    let server = TestServer::start().await;

    let mut client_a = TestClient::connect(&server).await.expect("auth A");
    let mut client_b = TestClient::connect(&server).await.expect("auth B");

    // First grouped TCP registration is downgraded to solo (server logs warning).
    let (_tid_a, port_a) = client_a
        .register_tcp_tunnel_grouped("ssh", &"a".repeat(64))
        .await
        .expect("client A solo (kill switch off)");

    // Second grouped registration is *also* solo — and TCP doesn't have
    // the "subdomain in use" path; it simply gets a fresh port from the
    // pool. So both clients end up on different ports, each with one
    // member. This is the documented kill-switch-off behaviour: grouped
    // fields are accepted but ignored.
    let (_tid_b, port_b) = client_b
        .register_tcp_tunnel_grouped("ssh", &"a".repeat(64))
        .await
        .expect("client B also solo");

    assert_ne!(
        port_a, port_b,
        "kill switch off → grouped TCP registrations are downgraded to solo \
         and each gets its own port"
    );
}
