//! Per-client WebSocket session handler.
//!
//! Lifecycle
//! ---------
//! 1. Auth handshake (5 s timeout).
//! 2. Main select loop: frames from the WebSocket OR control messages from
//!    the router (NewConnection, Shutdown).
//! 3. Heartbeat: Ping every 30 s; drop session if Pong not received in 10 s.
//! 4. Cleanup: `core.remove_session` on any exit path.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::poll_fn;
use futures_util::io::AsyncWriteExt;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use uuid::Uuid;

use rustunnel_protocol::{decode_frame, encode_frame, ControlFrame, TunnelProtocol};

use chrono::Utc;

use crate::audit::{AuditEvent, AuditTx};
use crate::config::ServerConfig;
use crate::control::mux::MuxSession;
use crate::core::{ControlMessage, TunnelCore};
use crate::db::{self, models::Token, Db};
use crate::error::{Error, Result};

// ── constants ─────────────────────────────────────────────────────────────────

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_DEADLINE: Duration = Duration::from_secs(10);
const DATA_PING_INTERVAL: Duration = Duration::from_secs(20);
const DATA_PONG_DEADLINE: Duration = Duration::from_secs(10);
const CTRL_CHANNEL_SIZE: usize = 64;

// ── session context ───────────────────────────────────────────────────────────

/// Bundles the per-session immutable references that are needed throughout
/// `main_loop` and `handle_client_message` to keep function argument counts
/// within the lint limit.
struct SessionCtx<'a> {
    session_id: Uuid,
    core: &'a Arc<TunnelCore>,
    config: &'a Arc<ServerConfig>,
    audit_tx: &'a AuditTx,
    db: &'a Db,
    /// `tokens.id` string — used for audit log attribution.
    db_token_id: Option<String>,
    /// Full token record from the DB — used for limit enforcement at RegisterTunnel.
    /// `None` for admin-token sessions and when auth is disabled.
    db_token: Option<Token>,
}

// ── public entry point ────────────────────────────────────────────────────────

pub async fn handle_session<S>(
    ws: WebSocketStream<S>,
    peer_addr: SocketAddr,
    core: Arc<TunnelCore>,
    config: Arc<ServerConfig>,
    audit_tx: AuditTx,
    db: Db,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match run_session(ws, peer_addr, &core, &config, &audit_tx, &db).await {
        Ok(()) => tracing::info!(%peer_addr, "session ended cleanly"),
        Err(e) => tracing::warn!(%peer_addr, "session error: {e}"),
    }
}

// ── session driver ────────────────────────────────────────────────────────────

async fn run_session<S>(
    ws: WebSocketStream<S>,
    peer_addr: SocketAddr,
    core: &Arc<TunnelCore>,
    config: &Arc<ServerConfig>,
    audit_tx: &AuditTx,
    db: &Db,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // Create the control channel up-front so we only register_session once.
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<ControlMessage>(CTRL_CHANNEL_SIZE);

    // Auth.
    let (mut ws, session_id, db_token) =
        auth_handshake(ws, peer_addr, core, config, ctrl_tx, audit_tx, db).await?;
    let db_token_id = db_token.as_ref().map(|t| t.id.clone());

    tracing::info!(%peer_addr, %session_id, "session authenticated");

    // Heartbeat channels.
    let (ping_out_tx, mut ping_out_rx) = mpsc::channel::<u64>(4);
    let (pong_in_tx, pong_in_rx) = mpsc::channel::<u64>(4);
    let (hb_stop_tx, hb_stop_rx) = oneshot::channel::<()>();

    tokio::spawn(heartbeat_task(
        ping_out_tx,
        pong_in_rx,
        hb_stop_rx,
        session_id,
    ));

    let (open_tx, driver_handle) = spawn_yamux_driver(core, session_id);

    let ctx = SessionCtx {
        session_id,
        core,
        config,
        audit_tx,
        db,
        db_token_id,
        db_token,
    };
    let result = main_loop(
        &mut ws,
        &mut ctrl_rx,
        &mut ping_out_rx,
        pong_in_tx,
        &ctx,
        open_tx,
        driver_handle,
    )
    .await;

    let _ = hb_stop_tx.send(());

    // Mark any tunnels still open at disconnect time as unregistered.
    // Collect request counts and bytes from the routing table BEFORE remove_session clears them.
    let remaining: Vec<(String, u64, u64)> = core
        .sessions
        .get(&session_id)
        .map(|s| {
            s.tunnels
                .iter()
                .map(|id| {
                    (
                        id.to_string(),
                        core.get_tunnel_request_count(id),
                        core.get_tunnel_bytes_proxied(id),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for (tid, request_count, bytes_proxied) in &remaining {
        let _ = db::log_tunnel_unregistered(&db.pg, tid, *request_count, *bytes_proxied).await;
    }

    core.remove_session(&session_id);
    tracing::debug!(%session_id, "session removed");

    result
}

// ── auth handshake ────────────────────────────────────────────────────────────

async fn auth_handshake<S>(
    mut ws: WebSocketStream<S>,
    peer_addr: SocketAddr,
    core: &Arc<TunnelCore>,
    config: &Arc<ServerConfig>,
    ctrl_tx: mpsc::Sender<ControlMessage>,
    audit_tx: &AuditTx,
    db: &Db,
) -> Result<(WebSocketStream<S>, Uuid, Option<Token>)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let raw = timeout(AUTH_TIMEOUT, ws.next())
        .await
        .map_err(|_| Error::Auth("auth timeout".into()))?
        .ok_or_else(|| Error::Auth("connection closed before auth".into()))?
        .map_err(|e| Error::Auth(e.to_string()))?;

    let frame = parse_binary(raw)?;

    let (token, _client_version) = match frame {
        ControlFrame::Auth {
            token,
            client_version,
        } => (token, client_version),
        other => {
            let _ = send_frame(
                &mut ws,
                &ControlFrame::AuthError {
                    message: "expected Auth frame".into(),
                },
            )
            .await;
            let _ = audit_tx.try_send(AuditEvent::AuthAttempt {
                peer: peer_addr.to_string(),
                success: false,
                token_id: None,
            });
            return Err(Error::Auth(format!("unexpected frame: {other:?}")));
        }
    };

    // Resolve auth and capture the full DB token record for limit enforcement.
    // Admin token → None; DB token → Some(Token).
    let db_token: Option<Token>;
    let authed: bool;

    if !config.auth.require_auth {
        // Auth disabled — still try to resolve the DB token for tracking.
        db_token = db::verify_token(&db.pg, &token).await.ok().flatten();
        authed = true;
    } else if token == config.auth.admin_token {
        db_token = None;
        authed = true;
    } else {
        match db::verify_token(&db.pg, &token).await {
            Ok(Some(t)) => {
                db_token = Some(t);
                authed = true;
            }
            _ => {
                db_token = None;
                authed = false;
            }
        }
    }

    if !authed {
        let _ = send_frame(
            &mut ws,
            &ControlFrame::AuthError {
                message: "invalid token".into(),
            },
        )
        .await;
        let _ = audit_tx.try_send(AuditEvent::AuthAttempt {
            peer: peer_addr.to_string(),
            success: false,
            token_id: None,
        });
        return Err(Error::Auth("invalid token".into()));
    }

    let token_id = token.clone();
    let db_token_id = db_token.as_ref().map(|t| t.id.clone());
    let session_id = core.register_session(peer_addr, token, db_token_id, ctrl_tx);

    let _ = audit_tx.try_send(AuditEvent::AuthAttempt {
        peer: peer_addr.to_string(),
        success: true,
        token_id: Some(token_id),
    });

    send_frame(
        &mut ws,
        &ControlFrame::AuthOk {
            session_id,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    )
    .await?;

    Ok((ws, session_id, db_token))
}

// ── main loop ─────────────────────────────────────────────────────────────────

async fn main_loop<S>(
    ws: &mut WebSocketStream<S>,
    ctrl_rx: &mut mpsc::Receiver<ControlMessage>,
    ping_out_rx: &mut mpsc::Receiver<u64>,
    pong_in_tx: mpsc::Sender<u64>,
    ctx: &SessionCtx<'_>,
    mut open_tx: mpsc::Sender<uuid::Uuid>,
    mut driver_handle: JoinHandle<()>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let session_id = ctx.session_id;
    loop {
        tokio::select! {
            // Outbound Ping queued by the heartbeat task.
            ts = ping_out_rx.recv() => {
                match ts {
                    None => return Ok(()),
                    Some(timestamp) => {
                        send_frame(ws, &ControlFrame::Ping { timestamp }).await?;
                        tracing::trace!(%session_id, timestamp, "ping sent");
                    }
                }
            }

            // Inbound WebSocket frame.
            msg = ws.next() => {
                match msg {
                    None => {
                        tracing::debug!(%session_id, "peer closed WebSocket");
                        return Ok(());
                    }
                    Some(Err(e)) => {
                        tracing::warn!(%session_id, "ws error: {e}");
                        return Err(Error::Io(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe, e.to_string())));
                    }
                    Some(Ok(msg)) => {
                        handle_client_message(msg, ws, ctx, &pong_in_tx).await?;
                    }
                }
            }

            // Control message from the router.
            ctrl = ctrl_rx.recv() => {
                match ctrl {
                    None | Some(ControlMessage::Shutdown) => {
                        tracing::info!(%session_id, "shutdown");
                        let _ = ws.close(None).await;
                        return Ok(());
                    }
                    Some(ControlMessage::NewConnection { conn_id, client_addr, protocol }) => {
                        tracing::debug!(%session_id, %conn_id, %client_addr, "NewConnection: opening yamux stream");
                        // Ask the yamux driver task to open an outbound stream,
                        // write the conn_id bytes (forcing SYN), and hand the
                        // stream to the waiting edge task.
                        if open_tx.send(conn_id).await.is_err() {
                            // The driver exited (data WebSocket dropped). Cancel
                            // the pending conn so the edge task gets a fast 502.
                            // The driver_handle arm will rebuild the data plane.
                            tracing::warn!(%session_id, %conn_id, "yamux driver dead — cancelling connection");
                            ctx.core.cancel_pending_conn(&conn_id);
                        } else {
                            // Notify the client so it can correlate the arriving
                            // yamux stream with the local service to proxy.
                            send_frame(ws, &ControlFrame::NewConnection {
                                conn_id,
                                client_addr: client_addr.to_string(),
                                protocol,
                            }).await?;
                        }
                    }
                }
            }

            // Yamux driver task exited — data WebSocket dropped or yamux error.
            // Rebuild the data plane so the client can reconnect /_data/<session_id>
            // without tearing down the control WS or re-registering tunnels.
            _ = &mut driver_handle => {
                tracing::warn!(%session_id, "yamux driver exited — rebuilding data plane for reconnect");
                let (new_open_tx, new_handle) = spawn_yamux_driver(ctx.core, session_id);
                open_tx = new_open_tx;
                driver_handle = new_handle;
            }
        }
    }
}

// ── yamux driver ─────────────────────────────────────────────────────────

/// Create a new `MuxSession`, store its loopback pipe in the session, and
/// spawn the yamux driver task that opens outbound streams for each
/// `NewConnection`.
///
/// Returns the channel sender for open-stream requests and the driver's
/// `JoinHandle` (so the main loop can detect driver exit and rebuild).
fn spawn_yamux_driver(
    core: &Arc<TunnelCore>,
    session_id: Uuid,
) -> (mpsc::Sender<Uuid>, JoinHandle<()>) {
    let mut mux = MuxSession::start_detached();
    if let Some(pipe) = mux.take_pipe_client() {
        core.set_data_pipe(&session_id, pipe);
    }
    let conn = mux.into_conn();
    let (open_tx, mut open_rx) = mpsc::channel::<Uuid>(16);
    let core_clone = Arc::clone(core);

    // yamux 0.13 uses lazy SYNs and requires the Connection to be polled
    // continuously to flush outbound frames to the underlying IO (the duplex
    // pipe that is bridged to the real data WebSocket).  The driver task:
    //   • Accepts open-stream requests from the main loop via `open_rx`.
    //   • For each request: opens an outbound stream (Mode::Client), writes
    //     the 16-byte conn_id to force the SYN+DATA to be flushed, then hands
    //     the stream to the edge task via `core.resolve_pending_conn`.
    //   • Continuously polls `poll_next_inbound` (which also drives all
    //     outbound IO) between requests.
    let handle = tokio::spawn(async move {
        let mut conn = conn;
        loop {
            tokio::select! {
                req = open_rx.recv() => {
                    let conn_id = match req { None => break, Some(id) => id };
                    match tokio::time::timeout(
                        Duration::from_secs(5),
                        poll_fn(|cx| conn.poll_new_outbound(cx)),
                    )
                    .await
                    {
                        Ok(Ok(mut stream)) => {
                            let core = Arc::clone(&core_clone);
                            tokio::spawn(async move {
                                if stream.write_all(conn_id.as_bytes()).await.is_err()
                                    || stream.flush().await.is_err()
                                {
                                    tracing::warn!(%conn_id, "failed to write/flush yamux stream");
                                    return;
                                }
                                if !core.resolve_pending_conn(&conn_id, stream) {
                                    tracing::warn!(%conn_id, "no edge task waiting for this conn_id");
                                }
                            });
                        }
                        Ok(Err(e)) => tracing::warn!(%conn_id, "yamux open_stream: {e}"),
                        Err(_) => {
                            tracing::warn!(%conn_id, "yamux open_stream timed out — breaking potential deadlock");
                            core_clone.cancel_pending_conn(&conn_id);
                        }
                    }
                }

                result = poll_fn(|cx| conn.poll_next_inbound(cx)) => {
                    match result {
                        Some(Ok(_)) => tracing::debug!("unexpected inbound yamux stream — ignored"),
                        Some(Err(e)) => { tracing::debug!("yamux driver error: {e}"); break; }
                        None => { tracing::debug!("yamux connection closed"); break; }
                    }
                }
            }
        }
        // Drain any connection requests that arrived while the driver was
        // exiting.  Each waiting edge task holds a oneshot receiver; removing
        // the sender here causes RecvError immediately, so edge tasks get a
        // fast 502 instead of waiting out the full STREAM_TIMEOUT.
        while let Ok(conn_id) = open_rx.try_recv() {
            core_clone.cancel_pending_conn(&conn_id);
        }
    });

    (open_tx, handle)
}

// ── frame dispatch ────────────────────────────────────────────────────────────

async fn handle_client_message<S>(
    msg: Message,
    ws: &mut WebSocketStream<S>,
    ctx: &SessionCtx<'_>,
    pong_in_tx: &mpsc::Sender<u64>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let SessionCtx {
        session_id,
        core,
        config,
        audit_tx,
        db,
        db_token_id,
        db_token,
    } = ctx;
    let session_id = *session_id;
    let frame = match parse_binary(msg) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };

    match frame {
        ControlFrame::RegisterTunnel {
            request_id,
            protocol,
            subdomain,
            local_addr: _,
            p2p_secret_hash,
            p2p_name,
            // Phase 0: accept the new load-balancing fields on the wire so old
            // clients keep working unchanged. Server-side handling lands in
            // later phases of TUNNEL-7.
            group: _,
            group_key_hash: _,
            health_check: _,
        } => {
            tracing::debug!(%session_id, %request_id, ?protocol, "register tunnel");

            // ── Token limit enforcement ────────────────────────────────────
            // status is always checked; limit/expiry checks are skipped for
            // unlimited tokens and for tokens with no user_id (direct/admin/legacy tokens).
            // Tunnel limit is enforced at the user level from the plan, not per-token,
            // so creating multiple tokens cannot be used to bypass the plan limit.
            if let Some(token) = db_token {
                if token.status != "active" {
                    send_frame(
                        ws,
                        &ControlFrame::TunnelError {
                            request_id,
                            message: "token is suspended or revoked".into(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
                if !token.unlimited {
                    if let Some(exp) = token.expires_at {
                        if Utc::now() > exp {
                            send_frame(
                                ws,
                                &ControlFrame::TunnelError {
                                    request_id,
                                    message: "token has expired".into(),
                                },
                            )
                            .await?;
                            return Ok(());
                        }
                    }
                    // Enforce the user's plan tunnel limit globally across all their tokens.
                    // Bypassed for tokens with no user_id (direct/admin/legacy tokens).
                    if let Some(user_id) = token.user_id {
                        let plan = sqlx::query_as::<_, (Option<i32>, bool)>(
                            "SELECT p.max_tunnels, p.allow_custom_subdomains \
                             FROM subscriptions s \
                             JOIN plans p ON p.id = s.plan_id \
                             WHERE s.user_id = $1 AND s.status = 'active' \
                             ORDER BY s.created_at DESC LIMIT 1",
                        )
                        .bind(user_id)
                        .fetch_optional(&db.pg)
                        .await?;

                        if let Some((max_tunnels, allow_custom_subdomains)) = plan {
                            // Tunnel count limit.
                            if let Some(limit) = max_tunnels {
                                let row: (i64,) = sqlx::query_as(
                                    "SELECT COUNT(*) FROM tunnel_log \
                                     WHERE user_id = $1 AND unregistered_at IS NULL",
                                )
                                .bind(user_id)
                                .fetch_one(&db.pg)
                                .await?;
                                if row.0 >= limit as i64 {
                                    send_frame(
                                        ws,
                                        &ControlFrame::TunnelError {
                                            request_id,
                                            message: format!("tunnel limit of {limit} reached"),
                                        },
                                    )
                                    .await?;
                                    return Ok(());
                                }
                            }

                            // Custom subdomain gate — only HTTP/HTTPS tunnels use subdomains.
                            if !allow_custom_subdomains {
                                if let Some(ref requested) = subdomain {
                                    if !requested.is_empty() {
                                        send_frame(
                                            ws,
                                            &ControlFrame::TunnelError {
                                                request_id,
                                                message: "custom subdomains require a paid plan"
                                                    .into(),
                                            },
                                        )
                                        .await?;
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Registration ───────────────────────────────────────────────
            let token_user_id = db_token.as_ref().and_then(|t| t.user_id);
            match &protocol {
                TunnelProtocol::Http | TunnelProtocol::Https => {
                    match core.register_http_tunnel(&session_id, subdomain, protocol.clone()) {
                        Ok((tunnel_id, sub)) => {
                            let scheme = if protocol == TunnelProtocol::Https {
                                "https"
                            } else {
                                "http"
                            };
                            let public_url = format!("{scheme}://{}.{}", sub, config.server.domain);
                            let proto_str = format!("{protocol:?}").to_lowercase();
                            let _ = audit_tx.try_send(AuditEvent::TunnelRegistered {
                                session_id: session_id.to_string(),
                                tunnel_id: tunnel_id.to_string(),
                                protocol: proto_str.clone(),
                                label: sub.clone(),
                            });
                            let _ = db::log_tunnel_registered(
                                &db.pg,
                                db::TunnelRegistration {
                                    tunnel_id: &tunnel_id.to_string(),
                                    protocol: &proto_str,
                                    label: &sub,
                                    session_id: &session_id.to_string(),
                                    token_id: db_token_id.as_deref(),
                                    region_id: &config.region.id,
                                    user_id: token_user_id,
                                },
                            )
                            .await;
                            send_frame(
                                ws,
                                &ControlFrame::TunnelRegistered {
                                    request_id,
                                    tunnel_id,
                                    public_url,
                                    assigned_port: None,
                                    p2p_tunnel_name: None,
                                },
                            )
                            .await?;
                        }
                        Err(e) => {
                            send_frame(
                                ws,
                                &ControlFrame::TunnelError {
                                    request_id,
                                    message: e.to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                }
                TunnelProtocol::Udp => match core.register_udp_tunnel(&session_id) {
                    Ok((tunnel_id, port)) => {
                        let public_url = format!("udp://{}:{port}", config.server.domain);
                        let port_str = port.to_string();
                        let _ = audit_tx.try_send(AuditEvent::TunnelRegistered {
                            session_id: session_id.to_string(),
                            tunnel_id: tunnel_id.to_string(),
                            protocol: "udp".into(),
                            label: port_str.clone(),
                        });
                        let _ = db::log_tunnel_registered(
                            &db.pg,
                            db::TunnelRegistration {
                                tunnel_id: &tunnel_id.to_string(),
                                protocol: "udp",
                                label: &port_str,
                                session_id: &session_id.to_string(),
                                token_id: db_token_id.as_deref(),
                                region_id: &config.region.id,
                                user_id: token_user_id,
                            },
                        )
                        .await;
                        send_frame(
                            ws,
                            &ControlFrame::TunnelRegistered {
                                request_id,
                                tunnel_id,
                                public_url,
                                assigned_port: Some(port),
                                p2p_tunnel_name: None,
                            },
                        )
                        .await?;
                    }
                    Err(e) => {
                        send_frame(
                            ws,
                            &ControlFrame::TunnelError {
                                request_id,
                                message: e.to_string(),
                            },
                        )
                        .await?;
                    }
                },
                TunnelProtocol::Tcp => match core.register_tcp_tunnel(&session_id) {
                    Ok((tunnel_id, port)) => {
                        let public_url = format!("tcp://{}:{port}", config.server.domain);
                        let port_str = port.to_string();
                        let _ = audit_tx.try_send(AuditEvent::TunnelRegistered {
                            session_id: session_id.to_string(),
                            tunnel_id: tunnel_id.to_string(),
                            protocol: "tcp".into(),
                            label: port_str.clone(),
                        });
                        let _ = db::log_tunnel_registered(
                            &db.pg,
                            db::TunnelRegistration {
                                tunnel_id: &tunnel_id.to_string(),
                                protocol: "tcp",
                                label: &port_str,
                                session_id: &session_id.to_string(),
                                token_id: db_token_id.as_deref(),
                                region_id: &config.region.id,
                                user_id: token_user_id,
                            },
                        )
                        .await;
                        send_frame(
                            ws,
                            &ControlFrame::TunnelRegistered {
                                request_id,
                                tunnel_id,
                                public_url,
                                assigned_port: Some(port),
                                p2p_tunnel_name: None,
                            },
                        )
                        .await?;
                    }
                    Err(e) => {
                        send_frame(
                            ws,
                            &ControlFrame::TunnelError {
                                request_id,
                                message: e.to_string(),
                            },
                        )
                        .await?;
                    }
                },
                TunnelProtocol::P2p => {
                    if !config.p2p.enabled {
                        send_frame(
                            ws,
                            &ControlFrame::TunnelError {
                                request_id,
                                message: "P2P tunnels are not enabled on this server".into(),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                    let p2p_name = match p2p_name {
                        Some(name) => name,
                        None => {
                            send_frame(
                                ws,
                                &ControlFrame::TunnelError {
                                    request_id,
                                    message: "P2P tunnel requires a name (p2p_name)".into(),
                                },
                            )
                            .await?;
                            return Ok(());
                        }
                    };
                    let secret_hash = match p2p_secret_hash {
                        Some(h) => h,
                        None => {
                            send_frame(
                                ws,
                                &ControlFrame::TunnelError {
                                    request_id,
                                    message: "P2P tunnel requires a secret hash (p2p_secret_hash)"
                                        .into(),
                                },
                            )
                            .await?;
                            return Ok(());
                        }
                    };
                    match core.register_p2p_tunnel(&session_id, p2p_name.clone(), secret_hash) {
                        Ok((tunnel_id, name)) => {
                            let public_url = format!("p2p://{name}");
                            let _ = audit_tx.try_send(AuditEvent::TunnelRegistered {
                                session_id: session_id.to_string(),
                                tunnel_id: tunnel_id.to_string(),
                                protocol: "p2p".into(),
                                label: name.clone(),
                            });
                            let _ = db::log_tunnel_registered(
                                &db.pg,
                                db::TunnelRegistration {
                                    tunnel_id: &tunnel_id.to_string(),
                                    protocol: "p2p",
                                    label: &name,
                                    session_id: &session_id.to_string(),
                                    token_id: db_token_id.as_deref(),
                                    region_id: &config.region.id,
                                    user_id: token_user_id,
                                },
                            )
                            .await;
                            send_frame(
                                ws,
                                &ControlFrame::TunnelRegistered {
                                    request_id,
                                    tunnel_id,
                                    public_url,
                                    assigned_port: None,
                                    p2p_tunnel_name: Some(name),
                                },
                            )
                            .await?;
                        }
                        Err(e) => {
                            send_frame(
                                ws,
                                &ControlFrame::TunnelError {
                                    request_id,
                                    message: e.to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                }
            }
        }

        ControlFrame::UnregisterTunnel { tunnel_id } => {
            tracing::debug!(%session_id, %tunnel_id, "unregister tunnel");
            let _ = audit_tx.try_send(AuditEvent::TunnelRemoved {
                tunnel_id: tunnel_id.to_string(),
                label: String::new(),
            });
            // Read counters before remove_tunnel clears the routing entry.
            let request_count = core.get_tunnel_request_count(&tunnel_id);
            let bytes_proxied = core.get_tunnel_bytes_proxied(&tunnel_id);
            let _ = db::log_tunnel_unregistered(
                &db.pg,
                &tunnel_id.to_string(),
                request_count,
                bytes_proxied,
            )
            .await;
            core.remove_tunnel(&tunnel_id);
        }

        ControlFrame::P2pConnect {
            request_id,
            target_tunnel_name,
            secret_hash,
        } => {
            tracing::debug!(%session_id, %target_tunnel_name, "P2P connect request");

            if !config.p2p.enabled {
                send_frame(
                    ws,
                    &ControlFrame::P2pError {
                        request_id,
                        message: "P2P tunnels are not enabled on this server".into(),
                    },
                )
                .await?;
                return Ok(());
            }

            // Look up the publisher.
            let resolved = core.resolve_p2p(&target_tunnel_name);
            match resolved {
                None => {
                    send_frame(
                        ws,
                        &ControlFrame::P2pError {
                            request_id,
                            message: format!("P2P tunnel '{target_tunnel_name}' not found"),
                        },
                    )
                    .await?;
                }
                Some((publisher, publisher_tx)) => {
                    // Verify the shared secret.
                    if publisher.secret_hash != secret_hash {
                        send_frame(
                            ws,
                            &ControlFrame::P2pError {
                                request_id,
                                message: "invalid P2P secret".into(),
                            },
                        )
                        .await?;
                        return Ok(());
                    }

                    // Classify NAT pair and decide on direct vs relay.
                    let pub_nat = publisher.nat_type.as_deref();
                    // Subscriber doesn't probe STUN before P2pConnect, so we
                    // assume "cone" for the subscriber side. If the publisher
                    // is cone/open, we attempt direct. The relay always runs
                    // in parallel as fallback, so a wrong guess only costs
                    // the punch timeout (5s) before falling back.
                    let sub_nat_assumed = if pub_nat.is_some() {
                        Some("cone")
                    } else {
                        None
                    };
                    let (strategy, attempt_direct) =
                        crate::core::classify_nat_pair(pub_nat, sub_nat_assumed);

                    // Generate a conn_id for this P2P connection.
                    let conn_id = Uuid::new_v4();
                    tracing::info!(
                        %conn_id, %target_tunnel_name,
                        publisher_session = %publisher.tunnel_info.session_id,
                        subscriber_session = %session_id,
                        strategy,
                        attempt_direct,
                        "P2P connection"
                    );

                    // If direct mode is possible, send punch instructions to
                    // both peers. They attempt hole punching in parallel with
                    // the relay setup. If punching succeeds, the clients
                    // upgrade to QUIC and stop using the relay.
                    if attempt_direct && !publisher.mapped_addrs.is_empty() {
                        // Send publisher's mapped addrs to subscriber.
                        send_frame(
                            ws,
                            &ControlFrame::P2pPunchInstructions {
                                conn_id,
                                peer_addrs: publisher.mapped_addrs.clone(),
                                strategy: strategy.to_string(),
                                punch_timeout_ms: 5000,
                            },
                        )
                        .await?;

                        // Send subscriber info to publisher (via publisher's
                        // control channel — we don't have subscriber's mapped
                        // addrs here yet, so this is best-effort).
                    }

                    // Tell the publisher to open a yamux stream for this conn_id.
                    // The publisher's session handler will send NewConnection to
                    // the publisher client, which will open a stream and proxy
                    // to its local service.
                    let pub_stream_rx = core.register_pending_conn(conn_id);

                    if publisher_tx
                        .send(ControlMessage::NewConnection {
                            conn_id,
                            client_addr: std::net::SocketAddr::from((
                                std::net::Ipv4Addr::UNSPECIFIED,
                                0,
                            )),
                            protocol: TunnelProtocol::P2p,
                        })
                        .await
                        .is_err()
                    {
                        core.cancel_pending_conn(&conn_id);
                        send_frame(
                            ws,
                            &ControlFrame::P2pError {
                                request_id,
                                message: "publisher session is disconnected".into(),
                            },
                        )
                        .await?;
                        return Ok(());
                    }

                    // Use two distinct conn_ids: one for the publisher yamux
                    // stream, one for the subscriber. The server bridges them.
                    let pub_conn_id = conn_id;
                    let sub_conn_id = Uuid::new_v4();

                    // Register subscriber pending conn.
                    let sub_stream_rx = core.register_pending_conn(sub_conn_id);

                    // Tell the subscriber (this session) about the connection.
                    send_frame(
                        ws,
                        &ControlFrame::P2pConnected {
                            request_id,
                            conn_id: sub_conn_id,
                        },
                    )
                    .await?;

                    // Trigger a NewConnection on the subscriber's own session
                    // so that main_loop opens a yamux stream for the subscriber
                    // side of the relay. The subscriber client will receive
                    // NewConnection and proxy to its local port.
                    if let Some(sub_session) = core.sessions.get(&session_id) {
                        let _ = sub_session
                            .control_tx
                            .try_send(ControlMessage::NewConnection {
                                conn_id: sub_conn_id,
                                client_addr: std::net::SocketAddr::from((
                                    std::net::Ipv4Addr::UNSPECIFIED,
                                    0,
                                )),
                                protocol: TunnelProtocol::P2p,
                            });
                    }

                    // Spawn a task that waits for both streams and bridges them.
                    let core_relay = Arc::clone(core);
                    let pub_bytes = Arc::clone(&publisher.tunnel_info.bytes_proxied);
                    tokio::spawn(async move {
                        let pub_stream = match tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            pub_stream_rx,
                        )
                        .await
                        {
                            Ok(Ok(s)) => s,
                            _ => {
                                tracing::warn!(%pub_conn_id, "P2P relay: publisher stream timeout");
                                core_relay.cancel_pending_conn(&sub_conn_id);
                                return;
                            }
                        };
                        let sub_stream = match tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            sub_stream_rx,
                        )
                        .await
                        {
                            Ok(Ok(s)) => s,
                            _ => {
                                tracing::warn!(%sub_conn_id, "P2P relay: subscriber stream timeout");
                                return;
                            }
                        };

                        // Bridge the two yamux streams bidirectionally.
                        let mut pub_compat =
                            tokio_util::compat::FuturesAsyncReadCompatExt::compat(pub_stream);
                        let mut sub_compat =
                            tokio_util::compat::FuturesAsyncReadCompatExt::compat(sub_stream);

                        match tokio::io::copy_bidirectional(&mut pub_compat, &mut sub_compat).await
                        {
                            Ok((up, down)) => {
                                tracing::info!(
                                    %pub_conn_id, %sub_conn_id,
                                    bytes_up = up, bytes_down = down,
                                    "P2P relay done"
                                );
                                pub_bytes
                                    .fetch_add(up + down, std::sync::atomic::Ordering::Relaxed);
                            }
                            Err(e) => {
                                tracing::debug!(%pub_conn_id, "P2P relay error: {e}");
                            }
                        }
                    });
                }
            }
        }

        ControlFrame::Ping { timestamp } => {
            send_frame(ws, &ControlFrame::Pong { timestamp }).await?;
        }

        ControlFrame::Pong { timestamp } => {
            tracing::trace!(%session_id, timestamp, "pong");
            let _ = pong_in_tx.try_send(timestamp);
        }

        ControlFrame::P2pNatInfo {
            tunnel_id,
            nat_type,
            mapped_addrs,
            local_addrs: _,
        } => {
            tracing::debug!(%session_id, %tunnel_id, %nat_type, "P2P NAT info received");
            core.update_p2p_nat_info(&tunnel_id, nat_type, mapped_addrs);
        }

        ControlFrame::P2pPunchResult {
            conn_id,
            success,
            direct_addr,
        } => {
            tracing::info!(%session_id, %conn_id, success, ?direct_addr, "P2P punch result");
            // Punch results are logged for analytics. Direct connections are
            // already established client-side; no server action needed.
        }

        ControlFrame::P2pMetrics {
            tunnel_id,
            bytes_sent,
            bytes_received,
        } => {
            tracing::debug!(
                %session_id, %tunnel_id,
                bytes_sent, bytes_received,
                "P2P client-reported metrics"
            );
            // Store for dashboard visibility. These are informational only —
            // not used for billing (direct P2P traffic can't be verified).
        }

        // Phase 0 of TUNNEL-7: accept the new health-report frames so newer
        // clients connecting to this server don't trip a `decode_frame`
        // warning. Behaviour wires up in Phase 4 (group dispatch + healthy
        // bit). Until then the only effect is a debug-level log line.
        ControlFrame::TunnelHealthy { tunnel_id } => {
            tracing::debug!(%session_id, %tunnel_id, "tunnel reported healthy (no-op pre-Phase-4)");
        }
        ControlFrame::TunnelUnhealthy { tunnel_id, reason } => {
            tracing::debug!(
                %session_id, %tunnel_id, %reason,
                "tunnel reported unhealthy (no-op pre-Phase-4)"
            );
        }

        other => {
            tracing::warn!(%session_id, ?other, "unexpected frame — ignored");
        }
    }
    Ok(())
}

// ── heartbeat task ────────────────────────────────────────────────────────────

async fn heartbeat_task(
    ping_out_tx: mpsc::Sender<u64>,
    mut pong_in_rx: mpsc::Receiver<u64>,
    mut stop: oneshot::Receiver<()>,
    session_id: Uuid,
) {
    let mut ticker = interval(PING_INTERVAL);
    ticker.tick().await; // skip immediate first tick

    let mut pending: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = &mut stop => break,

            _ = ticker.tick() => {
                if let Some(sent_at) = pending {
                    if sent_at.elapsed() > PONG_DEADLINE {
                        tracing::warn!(%session_id, "heartbeat timeout");
                        break;
                    }
                }
                let ts = now_ms();
                if ping_out_tx.send(ts).await.is_err() {
                    break;
                }
                pending = Some(Instant::now());
            }

            pong = pong_in_rx.recv() => {
                match pong {
                    None => break,
                    Some(_) => {
                        pending = None;
                        tracing::trace!(%session_id, "heartbeat ok");
                    }
                }
            }
        }
    }
}

// ── data-plane bridge ─────────────────────────────────────────────────────────

/// Bridge the client's data WebSocket to the session's loopback pipe.
///
/// The yamux `Connection` inside `MuxSession` is backed by one end of an
/// in-process `tokio::io::duplex` pair.  This function takes the other
/// (client) end of that pair and bidirectionally copies bytes between it and
/// the real data WebSocket, making yamux frames from the remote client flow
/// transparently into the server-side yamux `Connection`.
pub async fn handle_data_connection<S>(
    ws: WebSocketStream<S>,
    session_id: uuid::Uuid,
    core: Arc<TunnelCore>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // Retrieve the loopback pipe end that `run_session` stored after creating
    // the `MuxSession`.  A brief retry loop handles the unlikely race where
    // the data WebSocket arrives before `run_session` has called `set_data_pipe`.
    let mut pipe = None;
    for _ in 0..40 {
        pipe = core.take_data_pipe(&session_id);
        if pipe.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let Some(pipe) = pipe else {
        tracing::warn!(%session_id, "data connection arrived but no pipe found (session unknown?)");
        return;
    };

    tracing::info!(%session_id, "data WebSocket connected, bridging to yamux session");

    // Bridge the WebSocket to the yamux pipe with WS-level keepalive pings.
    // We drive the copy manually (instead of copy_bidirectional) so that we
    // can inject periodic WebSocket Ping frames to keep NAT / load-balancer
    // connections alive and detect silent drops within DATA_PONG_DEADLINE.
    let (mut ws_sink, mut ws_stream) = ws.split();
    let (mut pipe_read, mut pipe_write) = tokio::io::split(pipe);

    let mut ping_interval = tokio::time::interval(DATA_PING_INTERVAL);
    ping_interval.tick().await; // skip the immediate first tick

    let mut last_pong = tokio::time::Instant::now();
    let mut awaiting_pong = false;
    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            // ── data WebSocket → yamux pipe ──────────────────────────────
            msg = ws_stream.next() => {
                match msg {
                    None => {
                        tracing::debug!(%session_id, "data WebSocket closed by peer");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::debug!(%session_id, "data WebSocket error: {e}");
                        break;
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if pipe_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        awaiting_pong = false;
                        last_pong = tokio::time::Instant::now();
                        tracing::trace!(%session_id, "data WS pong received");
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::debug!(%session_id, "data WebSocket close frame received");
                        break;
                    }
                    Some(Ok(_)) => {} // ignore other frame types (text, ping)
                }
            }

            // ── yamux pipe → data WebSocket ──────────────────────────────
            n = pipe_read.read(&mut buf) => {
                match n {
                    Ok(0) | Err(_) => {
                        tracing::debug!(%session_id, "yamux pipe closed");
                        break;
                    }
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        if ws_sink.send(Message::Binary(data)).await.is_err() {
                            break;
                        }
                    }
                }
            }

            // ── keepalive ping ───────────────────────────────────────────
            _ = ping_interval.tick() => {
                if awaiting_pong && last_pong.elapsed() > DATA_PONG_DEADLINE {
                    tracing::warn!(%session_id, "data WebSocket keepalive timeout — closing bridge");
                    break;
                }
                if ws_sink.send(Message::Ping(vec![])).await.is_err() {
                    break;
                }
                awaiting_pong = true;
            }
        }
    }

    tracing::debug!(%session_id, "data WebSocket bridge ended");
}

// ── helpers ───────────────────────────────────────────────────────────────────

async fn send_frame<S>(ws: &mut WebSocketStream<S>, frame: &ControlFrame) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let bytes = encode_frame(frame);
    ws.send(Message::Binary(bytes)).await.map_err(|e| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            e.to_string(),
        ))
    })
}

fn parse_binary(msg: Message) -> Result<ControlFrame> {
    match msg {
        Message::Binary(data) => decode_frame(&data).map_err(Error::Protocol),
        other => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected binary frame, got {other:?}"),
        ))),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
