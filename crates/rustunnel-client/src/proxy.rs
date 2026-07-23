//! Local service proxy.
//!
//! `proxy_connection` bridges a yamux data stream (the tunnel side) with a
//! fresh TCP connection to the local service.
//!
//! Connections are copied byte-for-byte. When request inspection is active the
//! copy additionally feeds every byte to a [`ConnTap`], which parses HTTP
//! framing without touching the stream itself — see [`crate::inspect::tap`].

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{debug, info, warn};
use uuid::Uuid;
use yamux::Stream as YamuxStream;

use crate::inspect::tap::ConnTap;
use crate::inspect::Inspector;

/// Copy buffer size. Matches the order of magnitude `copy_bidirectional` uses.
const COPY_BUF: usize = 16 * 1024;

/// Decrements the open-connection gauge however the proxy task ends.
struct ConnGuard(Arc<Inspector>);

impl ConnGuard {
    fn new(inspector: Arc<Inspector>) -> Self {
        inspector.stats.conns_open.fetch_add(1, Ordering::Relaxed);
        inspector.stats.conns_total.fetch_add(1, Ordering::Relaxed);
        Self(inspector)
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.stats.conns_open.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Proxy bytes between `yamux_stream` (tunnel-side) and a new TCP connection
/// to `local_addr` (service-side).
///
/// `local_addr` is a `"host:port"` string; `TcpStream::connect` performs DNS
/// resolution so both IP literals and hostnames (e.g. `localhost`) are accepted.
///
/// `tap` is `Some` only for HTTP tunnels while inspection is enabled; otherwise
/// the original untapped copy runs.
///
/// Logs byte counts and duration on completion.
pub async fn proxy_connection(
    yamux_stream: YamuxStream,
    local_addr: String,
    conn_id: Uuid,
    inspector: Arc<Inspector>,
    tap: Option<ConnTap>,
) {
    debug!(%conn_id, %local_addr, "proxy: connecting to local service");

    let _guard = ConnGuard::new(Arc::clone(&inspector));

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

    let result = match &tap {
        Some(tap) => copy_tapped(&mut local, &mut remote, &inspector, tap).await,
        None => {
            // No inspection: keep the original zero-overhead copy, but still
            // account for the bytes moved so TCP/UDP tunnels get counters.
            tokio::io::copy_bidirectional(&mut local, &mut remote)
                .await
                .map(|(to_tunnel, to_local)| {
                    inspector
                        .stats
                        .bytes_to_tunnel
                        .fetch_add(to_tunnel, Ordering::Relaxed);
                    inspector
                        .stats
                        .bytes_to_local
                        .fetch_add(to_local, Ordering::Relaxed);
                    (to_local, to_tunnel)
                })
        }
    };

    if let Some(tap) = &tap {
        tap.finish();
    }

    match result {
        Ok((to_local, to_tunnel)) => {
            info!(
                %conn_id,
                bytes_to_local   = to_local,
                bytes_to_tunnel  = to_tunnel,
                duration_ms      = started.elapsed().as_millis() as u64,
                "proxy: connection done"
            );
        }
        Err(e) => {
            debug!(%conn_id, "proxy: copy error: {e}");
        }
    }
}

/// Bidirectional copy that reports every byte to `tap`.
///
/// Mirrors `tokio::io::copy_bidirectional`: each direction shuts down its
/// writer at EOF, and the first error ends the whole connection.
/// Returns `(bytes_to_local, bytes_to_tunnel)`.
async fn copy_tapped<L, R>(
    local: L,
    remote: R,
    inspector: &Arc<Inspector>,
    tap: &ConnTap,
) -> std::io::Result<(u64, u64)>
where
    L: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    let (mut local_reader, mut local_writer) = tokio::io::split(local);
    let (mut remote_reader, mut remote_writer) = tokio::io::split(remote);

    // Tunnel → local service: these bytes are HTTP requests.
    let to_local = pump(&mut remote_reader, &mut local_writer, |chunk| {
        inspector
            .stats
            .bytes_to_local
            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        tap.observe_request(chunk);
    });

    // Local service → tunnel: these bytes are HTTP responses.
    let to_tunnel = pump(&mut local_reader, &mut remote_writer, |chunk| {
        inspector
            .stats
            .bytes_to_tunnel
            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        tap.observe_response(chunk);
    });

    tokio::pin!(to_local, to_tunnel);

    let (mut bytes_to_local, mut bytes_to_tunnel) = (0u64, 0u64);
    let (mut local_done, mut tunnel_done) = (false, false);

    while !local_done || !tunnel_done {
        tokio::select! {
            result = &mut to_local, if !local_done => {
                local_done = true;
                bytes_to_local = result?;
            }
            result = &mut to_tunnel, if !tunnel_done => {
                tunnel_done = true;
                bytes_to_tunnel = result?;
            }
        }
    }

    Ok((bytes_to_local, bytes_to_tunnel))
}

/// Copy `reader` into `writer`, handing each chunk to `observe` after it has
/// been forwarded. Shuts the writer down on EOF so the peer sees the half-close.
async fn pump<R, W>(
    reader: &mut R,
    writer: &mut W,
    mut observe: impl FnMut(&[u8]),
) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; COPY_BUF];
    let mut total = 0u64;

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            let _ = writer.shutdown().await;
            return Ok(total);
        }
        writer.write_all(&buf[..n]).await?;
        writer.flush().await?;
        total += n as u64;
        observe(&buf[..n]);
    }
}

/// Bridge a yamux stream with an already-accepted local TCP connection.
/// Used by P2P subscribers — the local TCP connection was accepted on the
/// subscriber's listener before the relay was established.
pub async fn proxy_p2p_relay(
    yamux_stream: YamuxStream,
    mut local: tokio::net::TcpStream,
    conn_id: Uuid,
    inspector: Arc<Inspector>,
) {
    debug!(%conn_id, "p2p relay: bridging local TCP ↔ yamux");
    let _guard = ConnGuard::new(Arc::clone(&inspector));
    let _ = local.set_nodelay(true);
    let mut remote = yamux_stream.compat();
    let started = Instant::now();

    match tokio::io::copy_bidirectional(&mut local, &mut remote).await {
        Ok((up, down)) => {
            inspector
                .stats
                .bytes_to_tunnel
                .fetch_add(up, Ordering::Relaxed);
            inspector
                .stats
                .bytes_to_local
                .fetch_add(down, Ordering::Relaxed);
            info!(
                %conn_id,
                bytes_to_local = down,
                bytes_to_tunnel = up,
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
pub async fn proxy_udp_connection(
    yamux_stream: YamuxStream,
    local_addr: String,
    conn_id: Uuid,
    inspector: Arc<Inspector>,
) {
    debug!(%conn_id, %local_addr, "udp proxy: connecting to local service");

    let _guard = ConnGuard::new(Arc::clone(&inspector));

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
                        inspector.stats.bytes_to_local.fetch_add(data.len() as u64, Ordering::Relaxed);
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
                        inspector.stats.bytes_to_tunnel.fetch_add(n as u64, Ordering::Relaxed);
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
