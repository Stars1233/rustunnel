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
    },
    TunnelRegistered {
        request_id: String,
        tunnel_id: Uuid,
        public_url: String,
        assigned_port: Option<u16>,
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
}

pub fn encode_frame(frame: &ControlFrame) -> Vec<u8> {
    serde_json::to_vec(frame).expect("ControlFrame serialization is infallible")
}

pub fn decode_frame(data: &[u8]) -> Result<ControlFrame> {
    serde_json::from_slice(data).map_err(|e| Error::Protocol(e.to_string()))
}
