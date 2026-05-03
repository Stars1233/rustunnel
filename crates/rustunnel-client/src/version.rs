//! Server-version compatibility checks (TUNNEL-7 Phase 6).
//!
//! The server stamps its `Cargo.toml` version into every `AuthOk` frame.
//! When the client connects to an *older* edge that doesn't understand the
//! load-balancing additions (group fields on `RegisterTunnel`, the
//! `TunnelHealthy` / `TunnelUnhealthy` frames), it must:
//!
//!   1. Skip emitting the new wire fields (avoids old server logging
//!      `decode_frame` warnings every time we send a probe report).
//!   2. Skip spawning client-side probe loops (their only output channel
//!      is the new frames; spawning them would just consume FDs and CPU).
//!   3. Warn the user once per affected tunnel so a misconfigured edge
//!      doesn't silently degrade the feature without explanation.
//!
//! We avoid pulling `semver` into the client crate for one threshold —
//! a hand-rolled `(u32, u32, u32)` parse is enough.

/// Minimum server version required for full TUNNEL-7 / TUNNEL-8 support.
///
/// Phase 0 + 1 shipped in 0.6.0 (just the wire format additions). Phase 4
/// in 0.7.0 brought server-side handling of `TunnelHealthy` /
/// `TunnelUnhealthy`, so 0.7.0 is the floor where emitting those frames
/// is safe (= no `decode_frame` warning on the edge).
pub const MIN_SERVER_VERSION_FOR_LOAD_BALANCING: (u32, u32, u32) = (0, 7, 0);

/// Parse a `"X.Y.Z"` string into `(major, minor, patch)`. Returns `None`
/// for anything that doesn't fit that exact shape — a malformed
/// `server_version` is treated as "unknown / assume old" by callers.
///
/// Pre-release (`-rc1`) and build (`+build.5`) suffixes are stripped from
/// the patch component. We don't honour pre-release ordering — `0.7.0-rc1`
/// parses as `(0, 7, 0)` for our coarse-grained "is this server new
/// enough?" check.
pub fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    // Strip everything after a `-` or `+` once (keeps the major.minor.patch
    // numeric core only).
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next()?.parse().ok()?;
    // Reject extra dot-components in the numeric core (e.g. "0.7.0.0").
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Returns `true` if the server's reported version is `>= MIN_SERVER_VERSION_FOR_LOAD_BALANCING`.
/// Returns `false` for unparseable versions — safer to assume the worst.
pub fn server_supports_load_balancing(server_version: &str) -> bool {
    match parse_semver(server_version) {
        Some(v) => v >= MIN_SERVER_VERSION_FOR_LOAD_BALANCING,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_semver() {
        assert_eq!(parse_semver("0.7.0"), Some((0, 7, 0)));
        assert_eq!(parse_semver("1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_semver("12.34.56"), Some((12, 34, 56)));
    }

    #[test]
    fn parses_pre_release_as_patch_number() {
        // We don't honour pre-release ordering, but we shouldn't fail to
        // parse either — pre-release builds should still report a version.
        assert_eq!(parse_semver("0.7.0-rc1"), Some((0, 7, 0)));
        assert_eq!(parse_semver("0.7.0+build.5"), Some((0, 7, 0)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("0.7"), None);
        assert_eq!(parse_semver("0.7.0.0"), None);
        assert_eq!(parse_semver("not.a.version"), None);
        assert_eq!(parse_semver("v0.7.0"), None);
    }

    #[test]
    fn supports_lb_flag_for_each_threshold() {
        // Below 0.7.0 — too old.
        assert!(!server_supports_load_balancing("0.5.0"));
        assert!(!server_supports_load_balancing("0.5.1"));
        assert!(!server_supports_load_balancing("0.6.0"));
        assert!(!server_supports_load_balancing("0.6.99"));
        // Exactly the threshold and above.
        assert!(server_supports_load_balancing("0.7.0"));
        assert!(server_supports_load_balancing("0.7.5"));
        assert!(server_supports_load_balancing("0.8.0"));
        assert!(server_supports_load_balancing("1.0.0"));
        // Unparseable → assume too old.
        assert!(!server_supports_load_balancing(""));
        assert!(!server_supports_load_balancing("garbage"));
    }
}
