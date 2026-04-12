//! Minimal STUN client for NAT type detection.
//!
//! Sends STUN Binding Requests to two servers and classifies the NAT type
//! based on whether the mapped addresses match.
//!
//! STUN wire format (RFC 5389):
//!   - 20-byte header: type (2) + length (2) + magic cookie (4) + transaction ID (12)
//!   - Binding Request type: 0x0001
//!   - Binding Response type: 0x0101
//!   - XOR-MAPPED-ADDRESS attribute type: 0x0020

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use tracing::{debug, warn};

/// Default STUN servers for NAT detection.
pub const DEFAULT_STUN_SERVERS: &[&str] = &["stun.l.google.com:19302", "stun1.l.google.com:19302"];

/// STUN magic cookie (RFC 5389).
const MAGIC_COOKIE: u32 = 0x2112A442;

/// Timeout for a single STUN request.
const STUN_TIMEOUT: Duration = Duration::from_secs(3);

/// NAT type classification.
#[derive(Debug, Clone, PartialEq)]
pub enum NatType {
    /// No NAT — client has a public IP.
    Open,
    /// Same mapped address for all destinations (cone NAT — easy to traverse).
    Cone,
    /// Different mapped address per destination (symmetric NAT — hard to traverse).
    Symmetric,
    /// Could not determine (STUN servers unreachable).
    Unknown,
}

impl NatType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NatType::Open => "open",
            NatType::Cone => "cone",
            NatType::Symmetric => "symmetric",
            NatType::Unknown => "unknown",
        }
    }
}

/// Result of STUN probing: NAT type + mapped addresses.
#[derive(Debug, Clone)]
pub struct StunResult {
    pub nat_type: NatType,
    pub mapped_addrs: Vec<SocketAddr>,
    pub local_addrs: Vec<SocketAddr>,
}

/// Probe NAT type by sending STUN Binding Requests to two servers.
pub async fn probe_nat(stun_servers: &[String]) -> StunResult {
    let servers: Vec<&str> = if stun_servers.len() >= 2 {
        vec![&stun_servers[0], &stun_servers[1]]
    } else if stun_servers.len() == 1 {
        vec![&stun_servers[0], DEFAULT_STUN_SERVERS[1]]
    } else {
        vec![DEFAULT_STUN_SERVERS[0], DEFAULT_STUN_SERVERS[1]]
    };

    // Bind a single UDP socket and probe both servers from it.
    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            warn!("STUN: failed to bind socket: {e}");
            return StunResult {
                nat_type: NatType::Unknown,
                mapped_addrs: vec![],
                local_addrs: vec![],
            };
        }
    };

    let local_addr = socket
        .local_addr()
        .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());

    let mapped1 = stun_binding_request(&socket, servers[0]).await;
    let mapped2 = stun_binding_request(&socket, servers[1]).await;

    debug!(?mapped1, ?mapped2, "STUN probe results");

    let mut mapped_addrs = Vec::new();
    if let Some(a) = &mapped1 {
        mapped_addrs.push(*a);
    }
    if let Some(a) = &mapped2 {
        if !mapped_addrs.contains(a) {
            mapped_addrs.push(*a);
        }
    }

    let local_addrs = get_local_addrs().unwrap_or_else(|| vec![local_addr]);

    let nat_type = match (mapped1, mapped2) {
        (Some(a), Some(b)) => {
            // Check if public IP matches local IP (no NAT).
            if local_addrs.iter().any(|l| l.ip() == a.ip()) {
                NatType::Open
            } else if a == b {
                NatType::Cone
            } else {
                NatType::Symmetric
            }
        }
        (Some(_), None) | (None, Some(_)) => {
            // Only one server responded — can't classify, assume cone.
            NatType::Cone
        }
        (None, None) => NatType::Unknown,
    };

    debug!(nat = nat_type.as_str(), ?mapped_addrs, "NAT classification");

    StunResult {
        nat_type,
        mapped_addrs,
        local_addrs,
    }
}

/// Send a STUN Binding Request and parse the XOR-MAPPED-ADDRESS from the response.
async fn stun_binding_request(socket: &UdpSocket, server: &str) -> Option<SocketAddr> {
    // Build Binding Request (20 bytes: type + length + cookie + transaction_id).
    let mut req = [0u8; 20];
    // Type: Binding Request (0x0001)
    req[0] = 0x00;
    req[1] = 0x01;
    // Length: 0 (no attributes)
    req[2] = 0x00;
    req[3] = 0x00;
    // Magic Cookie
    req[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    // Transaction ID (12 random bytes)
    let tx_id: [u8; 12] = rand::random();
    req[8..20].copy_from_slice(&tx_id);

    // Send
    if socket.send_to(&req, server).await.is_err() {
        debug!("STUN: send to {server} failed");
        return None;
    }

    // Receive response with timeout
    let mut buf = [0u8; 512];
    let n = match tokio::time::timeout(STUN_TIMEOUT, socket.recv(&mut buf)).await {
        Ok(Ok(n)) => n,
        _ => {
            debug!("STUN: timeout from {server}");
            return None;
        }
    };

    if n < 20 {
        return None;
    }

    // Verify it's a Binding Response (0x0101)
    let msg_type = u16::from_be_bytes([buf[0], buf[1]]);
    if msg_type != 0x0101 {
        debug!("STUN: unexpected message type 0x{msg_type:04x} from {server}");
        return None;
    }

    // Verify magic cookie
    let cookie = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if cookie != MAGIC_COOKIE {
        debug!("STUN: invalid magic cookie from {server}");
        return None;
    }

    // Verify transaction ID matches
    if buf[8..20] != tx_id {
        debug!("STUN: transaction ID mismatch from {server}");
        return None;
    }

    // Parse attributes looking for XOR-MAPPED-ADDRESS (0x0020)
    // or MAPPED-ADDRESS (0x0001) as fallback.
    let msg_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let attr_end = std::cmp::min(20 + msg_len, n);
    let mut pos = 20;

    while pos + 4 <= attr_end {
        let attr_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let attr_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        let attr_start = pos + 4;

        if attr_type == 0x0020 {
            // XOR-MAPPED-ADDRESS
            return parse_xor_mapped_address(&buf[attr_start..attr_start + attr_len], &tx_id);
        }

        // Advance to next attribute (padded to 4-byte boundary)
        pos = attr_start + ((attr_len + 3) & !3);
    }

    debug!("STUN: no XOR-MAPPED-ADDRESS in response from {server}");
    None
}

/// Parse XOR-MAPPED-ADDRESS attribute value.
///
/// Format: 1 byte reserved + 1 byte family + 2 bytes port + 4/16 bytes address
/// Port and address are XORed with the magic cookie (and transaction ID for IPv6).
fn parse_xor_mapped_address(data: &[u8], _tx_id: &[u8; 12]) -> Option<SocketAddr> {
    if data.len() < 8 {
        return None;
    }

    let family = data[1];
    let xored_port = u16::from_be_bytes([data[2], data[3]]);
    let port = xored_port ^ (MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            let xored_ip = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let ip = xored_ip ^ MAGIC_COOKIE;
            let addr = std::net::Ipv4Addr::from(ip);
            Some(SocketAddr::new(addr.into(), port))
        }
        0x02 => {
            // IPv6 — XOR with cookie + transaction ID (16 bytes)
            if data.len() < 20 {
                return None;
            }
            let mut ip_bytes = [0u8; 16];
            ip_bytes.copy_from_slice(&data[4..20]);
            let cookie_bytes = MAGIC_COOKIE.to_be_bytes();
            for i in 0..4 {
                ip_bytes[i] ^= cookie_bytes[i];
            }
            for i in 0..12 {
                ip_bytes[4 + i] ^= _tx_id[i];
            }
            let addr = std::net::Ipv6Addr::from(ip_bytes);
            Some(SocketAddr::new(addr.into(), port))
        }
        _ => None,
    }
}

/// Get local network addresses for the machine.
fn get_local_addrs() -> Option<Vec<SocketAddr>> {
    // Use a UDP connect trick to find the default outbound address.
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(vec![addr])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xor_mapped_ipv4() {
        // Example: port 3478, IP 192.0.2.1
        // XORed port: 3478 ^ (0x2112 >> 0) = 3478 ^ 0x2112 = ...
        let port: u16 = 12345;
        let ip: u32 = u32::from(std::net::Ipv4Addr::new(192, 0, 2, 1));

        let xored_port = port ^ (MAGIC_COOKIE >> 16) as u16;
        let xored_ip = ip ^ MAGIC_COOKIE;

        let mut data = [0u8; 8];
        data[1] = 0x01; // IPv4
        data[2..4].copy_from_slice(&xored_port.to_be_bytes());
        data[4..8].copy_from_slice(&xored_ip.to_be_bytes());

        let tx_id = [0u8; 12];
        let result = parse_xor_mapped_address(&data, &tx_id).unwrap();
        assert_eq!(result.port(), port);
        assert_eq!(
            result.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1))
        );
    }

    #[test]
    fn nat_type_strings() {
        assert_eq!(NatType::Open.as_str(), "open");
        assert_eq!(NatType::Cone.as_str(), "cone");
        assert_eq!(NatType::Symmetric.as_str(), "symmetric");
        assert_eq!(NatType::Unknown.as_str(), "unknown");
    }
}
