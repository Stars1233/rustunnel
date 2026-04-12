use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelProtocol {
    Http,
    Https,
    Tcp,
    Udp,
    P2p,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlFrame {
    Auth {
        token: String,
        client_version: String,
    },
    AuthOk {
        session_id: Uuid,
        server_version: String,
    },
    AuthError {
        message: String,
    },
    RegisterTunnel {
        request_id: String,
        protocol: TunnelProtocol,
        subdomain: Option<String>,
        local_addr: String,
        /// SHA-256 hash of the shared secret (P2P publisher only).
        #[serde(skip_serializing_if = "Option::is_none")]
        p2p_secret_hash: Option<String>,
        /// Human-readable tunnel name for P2P discovery (P2P publisher only).
        #[serde(skip_serializing_if = "Option::is_none")]
        p2p_name: Option<String>,
    },
    TunnelRegistered {
        request_id: String,
        tunnel_id: Uuid,
        public_url: String,
        assigned_port: Option<u16>,
        /// The assigned P2P tunnel name (P2P publisher only).
        #[serde(skip_serializing_if = "Option::is_none")]
        p2p_tunnel_name: Option<String>,
    },
    TunnelError {
        request_id: String,
        message: String,
    },
    UnregisterTunnel {
        tunnel_id: Uuid,
    },
    NewConnection {
        conn_id: Uuid,
        client_addr: String,
        protocol: TunnelProtocol,
    },
    DataStreamOpen {
        conn_id: Uuid,
    },
    Ping {
        timestamp: u64,
    },
    Pong {
        timestamp: u64,
    },

    // ── P2P frames ───────────────────────────────────────────────────────
    /// Subscriber requests a P2P connection to a named publisher tunnel.
    P2pConnect {
        request_id: String,
        target_tunnel_name: String,
        secret_hash: String,
    },

    /// Server confirms a P2P connection has been established (sent to subscriber).
    P2pConnected {
        request_id: String,
        conn_id: Uuid,
    },

    /// Server rejects a P2P connection request (sent to subscriber).
    P2pError {
        request_id: String,
        message: String,
    },

    // ── P2P direct (Phase 3) ─────────────────────────────────────────────
    /// Client reports its NAT type and mapped addresses after STUN probing.
    /// Sent by both publisher and subscriber during P2P setup.
    P2pNatInfo {
        tunnel_id: Uuid,
        nat_type: String,
        mapped_addrs: Vec<String>,
        local_addrs: Vec<String>,
    },

    /// Server sends each peer the other's NAT info and hole-punch instructions.
    P2pPunchInstructions {
        conn_id: Uuid,
        peer_addrs: Vec<String>,
        strategy: String,
        punch_timeout_ms: u32,
    },

    /// Client reports hole-punch result back to the server.
    P2pPunchResult {
        conn_id: Uuid,
        success: bool,
        direct_addr: Option<String>,
    },

    /// Client periodically reports bandwidth for direct P2P connections.
    P2pMetrics {
        tunnel_id: Uuid,
        bytes_sent: u64,
        bytes_received: u64,
    },
}

pub fn encode_frame(frame: &ControlFrame) -> Vec<u8> {
    serde_json::to_vec(frame).expect("ControlFrame serialization is infallible")
}

pub fn decode_frame(data: &[u8]) -> Result<ControlFrame> {
    serde_json::from_slice(data).map_err(|e| Error::Protocol(e.to_string()))
}
