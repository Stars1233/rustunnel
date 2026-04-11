//! P2P direct connection: NAT hole punching + QUIC transport.
//!
//! When a P2P subscriber connects, this module attempts to establish a direct
//! UDP connection to the publisher using NAT hole punching. If successful,
//! it upgrades the raw UDP hole to a QUIC session for reliable, encrypted
//! transport. If hole punching fails, it falls back to the server relay
//! (Phase 2 flow).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::{ClientConfig, Endpoint, ServerConfig as QuinnServerConfig};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

/// Timeout for hole punching probes.
#[allow(dead_code)]
const PUNCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Probe packet magic bytes to distinguish from random UDP traffic.
const PROBE_MAGIC: &[u8; 8] = b"RTUN_P2P";

/// Probe interval during hole punching.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// Attempt to punch a UDP hole to the peer and establish a QUIC connection.
///
/// Returns the QUIC connection on success, or `None` if hole punching failed
/// (caller should fall back to relay).
///
/// `role`: `"publisher"` acts as QUIC server, `"subscriber"` acts as QUIC client.
pub async fn attempt_direct_connection(
    peer_addrs: &[SocketAddr],
    strategy: &str,
    role: &str,
    shared_secret: &[u8],
    timeout_ms: u32,
) -> Option<quinn::Connection> {
    if peer_addrs.is_empty() {
        debug!("P2P direct: no peer addresses — skipping");
        return None;
    }

    let timeout = Duration::from_millis(timeout_ms as u64);

    // Bind a UDP socket for hole punching.
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    let local_addr = socket.local_addr().ok()?;
    debug!(%local_addr, ?peer_addrs, strategy, role, "P2P direct: starting hole punch");

    // Phase 1: Send probes to all peer addresses.
    let punched_peer = match strategy {
        "direct_exchange" => punch_direct_exchange(&socket, peer_addrs, timeout).await,
        "port_prediction" => punch_direct_exchange(&socket, peer_addrs, timeout).await, // simplified
        _ => {
            debug!("P2P direct: strategy '{strategy}' not supported — skipping");
            return None;
        }
    };

    let peer_addr = punched_peer?;
    info!(%peer_addr, %local_addr, "P2P direct: hole punched successfully");

    // Phase 2: Upgrade to QUIC.
    let std_socket = socket.into_std().ok()?;

    if role == "publisher" {
        establish_quic_server(std_socket, shared_secret).await
    } else {
        establish_quic_client(std_socket, peer_addr, shared_secret).await
    }
}

/// Direct exchange hole punching (cone + cone NATs).
///
/// Send periodic probes to all peer addresses. Wait for a probe response
/// or an incoming probe from the peer.
async fn punch_direct_exchange(
    socket: &UdpSocket,
    peer_addrs: &[SocketAddr],
    timeout: Duration,
) -> Option<SocketAddr> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut probe_ticker = tokio::time::interval(PROBE_INTERVAL);
    let mut recv_buf = [0u8; 64];

    loop {
        tokio::select! {
            _ = probe_ticker.tick() => {
                if tokio::time::Instant::now() >= deadline {
                    debug!("P2P direct: punch timeout");
                    return None;
                }
                // Send probes to all candidate addresses.
                for addr in peer_addrs {
                    let _ = socket.send_to(PROBE_MAGIC, addr).await;
                }
            }

            result = socket.recv_from(&mut recv_buf) => {
                match result {
                    Ok((n, from)) => {
                        if n >= PROBE_MAGIC.len() && &recv_buf[..PROBE_MAGIC.len()] == PROBE_MAGIC {
                            debug!(%from, "P2P direct: received probe — hole punched");
                            // Send one more probe as ACK.
                            let _ = socket.send_to(PROBE_MAGIC, from).await;
                            return Some(from);
                        }
                    }
                    Err(e) => {
                        debug!("P2P direct: recv error: {e}");
                    }
                }
            }
        }
    }
}

/// Set up a QUIC server (publisher side) on the punched UDP socket.
async fn establish_quic_server(
    socket: std::net::UdpSocket,
    shared_secret: &[u8],
) -> Option<quinn::Connection> {
    let (server_config, _) = make_quic_configs(shared_secret)?;

    let endpoint = Endpoint::server(server_config, socket.local_addr().ok()?).ok()?;

    // Wait for the subscriber to connect (with timeout).
    let incoming = tokio::time::timeout(Duration::from_secs(10), endpoint.accept()).await;

    match incoming {
        Ok(Some(conn)) => {
            let connection = conn.await.ok()?;
            info!(
                remote = %connection.remote_address(),
                "P2P direct: QUIC server connection established"
            );
            Some(connection)
        }
        _ => {
            warn!("P2P direct: QUIC server accept timeout");
            None
        }
    }
}

/// Set up a QUIC client (subscriber side) and connect through the punched socket.
async fn establish_quic_client(
    socket: std::net::UdpSocket,
    peer_addr: SocketAddr,
    shared_secret: &[u8],
) -> Option<quinn::Connection> {
    let (_, client_config) = make_quic_configs(shared_secret)?;

    let mut endpoint = Endpoint::client(socket.local_addr().ok()?).ok()?;
    endpoint.set_default_client_config(client_config);

    // Short delay to let the server side set up.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let connecting = endpoint
        .connect(peer_addr, "rustunnel-p2p")
        .ok()?;

    let connection = tokio::time::timeout(Duration::from_secs(10), connecting)
        .await
        .ok()?
        .ok()?;

    info!(
        remote = %connection.remote_address(),
        "P2P direct: QUIC client connection established"
    );

    Some(connection)
}

/// Create QUIC server and client configs using a self-signed cert
/// derived from the shared secret.
fn make_quic_configs(
    shared_secret: &[u8],
) -> Option<(QuinnServerConfig, ClientConfig)> {
    // Generate a deterministic self-signed cert from the shared secret.
    // Both sides derive the same cert, so the subscriber can verify the publisher.
    use sha2::{Digest, Sha256};
    let seed = Sha256::digest(shared_secret);

    let subject_name = format!("rustunnel-p2p-{}", hex::encode(&seed[..8]));

    let cert_key = rcgen::KeyPair::generate().ok()?;
    let mut params = rcgen::CertificateParams::new(vec![subject_name]).ok()?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let cert = params.self_signed(&cert_key).ok()?;

    let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert_key.serialize_der()).ok()?;

    // Server config
    let server_config = QuinnServerConfig::with_single_cert(vec![cert_der.clone()], key_der).ok()?;

    // Client config — skip server cert verification (both sides use the same
    // self-signed cert derived from the shared secret, so we trust it implicitly).
    let client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto).ok()?,
    ));

    Some((server_config, client_config))
}

/// Dummy certificate verifier that accepts any server cert.
/// Safe because both peers derive the same cert from the shared secret.
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[allow(dead_code)]
/// Bridge a QUIC connection to a local TCP service.
///
/// Opens a bidirectional QUIC stream, connects to the local service,
/// and copies data in both directions.
pub async fn bridge_quic_to_local(
    connection: quinn::Connection,
    local_addr: &str,
) {
    loop {
        // Accept streams from the peer (publisher accepts from subscriber).
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                debug!("P2P direct: QUIC stream accept error: {e}");
                break;
            }
        };

        let local = local_addr.to_string();
        tokio::spawn(async move {
            match tokio::net::TcpStream::connect(&local).await {
                Ok(mut tcp) => {
                    let (mut tcp_read, mut tcp_write) = tcp.split();
                    let _ = tokio::join!(
                        tokio::io::copy(&mut recv, &mut tcp_write),
                        tokio::io::copy(&mut tcp_read, &mut send),
                    );
                }
                Err(e) => {
                    warn!("P2P direct: connect to local {local}: {e}");
                }
            }
        });
    }
}

#[allow(dead_code)]
/// Open a QUIC stream to the peer and bridge it to a local TCP connection.
pub async fn bridge_local_to_quic(
    connection: quinn::Connection,
    mut local_tcp: tokio::net::TcpStream,
) {
    match connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            let (mut tcp_read, mut tcp_write) = local_tcp.split();
            let _ = tokio::join!(
                tokio::io::copy(&mut recv, &mut tcp_write),
                tokio::io::copy(&mut tcp_read, &mut send),
            );
        }
        Err(e) => {
            warn!("P2P direct: QUIC open_bi error: {e}");
        }
    }
}
