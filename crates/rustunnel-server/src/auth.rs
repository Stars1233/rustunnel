//! Authentication primitives shared by the control plane and the dashboard API.
//!
//! * [`secret_eq`] — constant-time comparison for bearer secrets (the
//!   server-wide `auth.admin_token`). Plain `==` on strings short-circuits on
//!   length and on the first mismatching byte, which lets a remote attacker
//!   with enough samples recover the secret byte by byte (CWE-208).
//! * [`AuthFailureLimiter`] — per-source-IP throttle on *failed* auth attempts,
//!   consulted before any secret is compared so a throttled peer never reaches
//!   the comparison at all.
//! * [`client_ip`] — resolves the real client address for the dashboard API,
//!   which sits behind nginx in production.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Compare two secrets in constant time.
///
/// Both sides are reduced to a fixed-size SHA-256 digest first, then compared
/// with [`subtle::ConstantTimeEq`]. Hashing removes the length short-circuit
/// of plain `==` (which would otherwise leak the secret's length) and makes
/// the comparison cost independent of how many leading bytes match.
pub fn secret_eq(a: &str, b: &str) -> bool {
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    ha.as_slice().ct_eq(hb.as_slice()).into()
}

/// Once the map holds more than this many IPs, `record_failure` sweeps idle
/// entries so an attacker cycling source addresses can't grow it unbounded.
const SWEEP_THRESHOLD: usize = 10_000;

/// Sliding-window limiter over *failed* authentication attempts per source IP.
///
/// Unlike [`crate::core::IpRateLimiter`], successful requests never count.
/// Callers peek with [`AuthFailureLimiter::is_limited`] before validating a
/// credential and call [`AuthFailureLimiter::record_failure`] after rejecting
/// one. A `max_failures` of 0 disables the limiter entirely.
pub struct AuthFailureLimiter {
    failures: DashMap<IpAddr, VecDeque<Instant>>,
    window: Duration,
    max_failures: usize,
}

impl AuthFailureLimiter {
    /// Allow `max_failures_per_minute` failed attempts per IP over a
    /// 60-second sliding window.
    pub fn new(max_failures_per_minute: u32) -> Self {
        Self::with_window(max_failures_per_minute, Duration::from_secs(60))
    }

    /// Allow `max_failures` failed attempts per IP over `window`.
    pub fn with_window(max_failures: u32, window: Duration) -> Self {
        Self {
            failures: DashMap::new(),
            window,
            max_failures: max_failures as usize,
        }
    }

    /// `true` when the limiter is active (a non-zero budget was configured).
    pub fn is_enabled(&self) -> bool {
        self.max_failures > 0
    }

    /// `true` when `ip` has exhausted its failed-attempt budget for the
    /// current window and further attempts should be rejected outright.
    pub fn is_limited(&self, ip: IpAddr) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let cutoff = Instant::now() - self.window;
        match self.failures.get_mut(&ip) {
            Some(mut entry) => {
                prune(&mut entry, cutoff);
                entry.len() >= self.max_failures
            }
            None => false,
        }
    }

    /// Record one failed attempt from `ip`.
    pub fn record_failure(&self, ip: IpAddr) {
        if !self.is_enabled() {
            return;
        }
        let now = Instant::now();
        let cutoff = now - self.window;
        {
            let mut entry = self.failures.entry(ip).or_default();
            prune(&mut entry, cutoff);
            entry.push_back(now);
        }
        // The entry guard above must be dropped before `retain`, which takes
        // every shard's write lock.
        if self.failures.len() > SWEEP_THRESHOLD {
            self.evict_idle();
        }
    }

    /// Drop IPs whose most recent failure is older than the window.
    pub fn evict_idle(&self) {
        let cutoff = Instant::now() - self.window;
        self.failures
            .retain(|_, deque| deque.back().map(|&t| t >= cutoff).unwrap_or(false));
    }
}

fn prune(deque: &mut VecDeque<Instant>, cutoff: Instant) {
    while deque.front().map(|&t| t < cutoff).unwrap_or(false) {
        deque.pop_front();
    }
}

/// Resolve the client address for a dashboard API request.
///
/// In production the dashboard listener is bound to `127.0.0.1` and fronted by
/// nginx, which sets `X-Real-IP` to the true client. That header is trusted
/// only when the TCP peer is a loopback address; for any other peer the socket
/// address wins, because a header arriving from the open internet is
/// client-controlled and would let an attacker dodge the throttle.
pub fn client_ip(peer: IpAddr, real_ip_header: Option<&str>) -> IpAddr {
    if !peer.is_loopback() {
        return peer;
    }
    real_ip_header
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
        .unwrap_or(peer)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    #[test]
    fn secret_eq_matches_equal_strings() {
        assert!(secret_eq("rt_admin_abc123", "rt_admin_abc123"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn secret_eq_rejects_same_length_mismatch() {
        assert!(!secret_eq("rt_admin_abc123", "rt_admin_abc124"));
    }

    #[test]
    fn secret_eq_rejects_length_mismatch_and_prefixes() {
        assert!(!secret_eq("rt_admin_abc123", "rt_admin_abc12"));
        assert!(!secret_eq("rt_admin_abc12", "rt_admin_abc123"));
        assert!(!secret_eq("", "x"));
        assert!(!secret_eq("x", ""));
    }

    #[test]
    fn limiter_blocks_after_budget_and_tracks_ips_independently() {
        let limiter = AuthFailureLimiter::new(3);
        assert!(!limiter.is_limited(ip(1)));
        limiter.record_failure(ip(1));
        limiter.record_failure(ip(1));
        assert!(!limiter.is_limited(ip(1)));
        limiter.record_failure(ip(1));
        assert!(limiter.is_limited(ip(1)));
        // A different source is unaffected.
        assert!(!limiter.is_limited(ip(2)));
    }

    #[test]
    fn limiter_zero_budget_disables() {
        let limiter = AuthFailureLimiter::new(0);
        for _ in 0..50 {
            limiter.record_failure(ip(1));
        }
        assert!(!limiter.is_enabled());
        assert!(!limiter.is_limited(ip(1)));
        assert!(limiter.failures.is_empty());
    }

    #[test]
    fn limiter_recovers_after_window() {
        let limiter = AuthFailureLimiter::with_window(2, Duration::from_millis(40));
        limiter.record_failure(ip(1));
        limiter.record_failure(ip(1));
        assert!(limiter.is_limited(ip(1)));
        std::thread::sleep(Duration::from_millis(60));
        assert!(!limiter.is_limited(ip(1)));
        limiter.evict_idle();
        assert!(limiter.failures.is_empty());
    }

    #[test]
    fn client_ip_trusts_real_ip_only_from_loopback() {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(
            client_ip(loopback, Some("203.0.113.7")),
            ip_v4(203, 0, 113, 7)
        );
        assert_eq!(
            client_ip(loopback, Some(" 203.0.113.7 ")),
            ip_v4(203, 0, 113, 7)
        );
        // Garbage or missing header → fall back to the peer.
        assert_eq!(client_ip(loopback, Some("not-an-ip")), loopback);
        assert_eq!(client_ip(loopback, None), loopback);
        // A non-loopback peer never has its header honoured.
        assert_eq!(client_ip(ip(9), Some("203.0.113.7")), ip(9));
    }

    fn ip_v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }
}
