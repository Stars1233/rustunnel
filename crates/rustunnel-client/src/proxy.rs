//! Local service proxy.
//!
//! `proxy_connection` bridges a yamux data stream (the tunnel side) with a
//! fresh TCP connection to the local service.

use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{debug, info, warn};
use uuid::Uuid;
use yamux::Stream as YamuxStream;

/// Proxy bytes between `yamux_stream` (tunnel-side) and a new TCP connection
/// to `local_addr` (service-side).
///
/// `local_addr` is a `"host:port"` string; `TcpStream::connect` performs DNS
/// resolution so both IP literals and hostnames (e.g. `localhost`) are accepted.
///
/// Logs byte counts and duration on completion.
pub async fn proxy_connection(yamux_stream: YamuxStream, local_addr: String, conn_id: Uuid) {
    debug!(%conn_id, %local_addr, "proxy: connecting to local service");

    let mut local = match tokio::net::TcpStream::connect(&local_addr).await {
        Ok(s) => s,
        Err(e) => {
            warn!(%conn_id, %local_addr, "proxy: failed to connect to local service: {e}");
            return;
        }
    };

    // Disable Nagle's algorithm so small response headers from the local
    // service are not buffered before being forwarded through the tunnel.
    let _ = local.set_nodelay(true);

    // yamux::Stream implements futures::io::{AsyncRead, AsyncWrite}.
    // Bridge to tokio IO traits with the compat wrapper.
    let mut remote = yamux_stream.compat();

    let started = Instant::now();

    match tokio::io::copy_bidirectional(&mut local, &mut remote).await {
        Ok((up, down)) => {
            info!(
                %conn_id,
                bytes_to_local   = up,
                bytes_to_tunnel  = down,
                duration_ms      = started.elapsed().as_millis() as u64,
                "proxy: connection done"
            );
        }
        Err(e) => {
            debug!(%conn_id, "proxy: copy error: {e}");
        }
    }
}

/// Bridge a yamux stream with an already-accepted local TCP connection.
/// Used by P2P subscribers — the local TCP connection was accepted on the
/// subscriber's listener before the relay was established.
pub async fn proxy_p2p_relay(
    yamux_stream: YamuxStream,
    mut local: tokio::net::TcpStream,
    conn_id: Uuid,
) {
    debug!(%conn_id, "p2p relay: bridging local TCP ↔ yamux");
    let _ = local.set_nodelay(true);
    let mut remote = yamux_stream.compat();
    let started = Instant::now();

    match tokio::io::copy_bidirectional(&mut local, &mut remote).await {
        Ok((up, down)) => {
            info!(
                %conn_id,
                bytes_to_local = up,
                bytes_to_tunnel = down,
                duration_ms = started.elapsed().as_millis() as u64,
                "p2p relay: connection done"
            );
        }
        Err(e) => {
            debug!(%conn_id, "p2p relay: copy error: {e}");
        }
    }
}

/// Maximum UDP datagram size.
const MAX_DATAGRAM_SIZE: usize = 65535;

/// Proxy UDP datagrams between a yamux data stream (tunnel-side) and a local
/// UDP socket (service-side).  Uses 4-byte big-endian length framing over the
/// yamux byte stream to preserve datagram boundaries.
pub async fn proxy_udp_connection(yamux_stream: YamuxStream, local_addr: String, conn_id: Uuid) {
    debug!(%conn_id, %local_addr, "udp proxy: connecting to local service");

    let local = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            warn!(%conn_id, "udp proxy: failed to bind local socket: {e}");
            return;
        }
    };

    if let Err(e) = local.connect(&local_addr).await {
        warn!(%conn_id, %local_addr, "udp proxy: failed to connect to local service: {e}");
        return;
    }

    let mut remote = yamux_stream.compat();
    let started = Instant::now();
    let mut total_bytes: u64 = 0;
    let mut recv_buf = vec![0u8; MAX_DATAGRAM_SIZE];

    loop {
        tokio::select! {
            // Inbound from tunnel (yamux) → forward to local service.
            result = read_framed_datagram(&mut remote) => {
                match result {
                    Ok(data) => {
                        total_bytes += data.len() as u64;
                        if local.send(&data).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // Inbound from local service → send to tunnel (yamux).
            result = local.recv(&mut recv_buf) => {
                match result {
                    Ok(n) => {
                        total_bytes += n as u64;
                        let len = n as u32;
                        if remote.write_all(&len.to_be_bytes()).await.is_err() {
                            break;
                        }
                        if remote.write_all(&recv_buf[..n]).await.is_err() {
                            break;
                        }
                        if remote.flush().await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    info!(
        %conn_id,
        bytes = total_bytes,
        duration_ms = started.elapsed().as_millis() as u64,
        "udp proxy: session done"
    );
}

/// Read a single length-prefixed datagram from a stream.
async fn read_framed_datagram<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, std::io::Error> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_DATAGRAM_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "datagram too large",
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}
