//! Client-side error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("auth failed: {0}")]
    Auth(String),

    #[error("tunnel error: {0}")]
    Tunnel(String),

    #[error("connection error: {0}")]
    Connection(String),

    #[error("protocol error: {0}")]
    Protocol(#[from] rustunnel_protocol::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Stable machine-readable error code (snake_case variant name), used as
    /// the `code` field of the `--json` error event.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Config(_) => "config",
            Error::Auth(_) => "auth",
            Error::Tunnel(_) => "tunnel",
            Error::Connection(_) => "connection",
            Error::Protocol(_) => "protocol",
            Error::Io(_) => "io",
        }
    }

    /// One-line recovery hint for agents/users, when one exists for the
    /// error category. Emitted as the `hint` field of the `--json` error event.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Error::Config(_) => Some(
                "run `rustunnel setup` to create ~/.rustunnel/config.yml, \
                 or pass --server <host:port> and --token <token> directly",
            ),
            // Auth failures from `token create` use the dashboard admin
            // token (--admin-token), not the tunnel auth token — the message
            // mentions "admin token", so key the hint off that.
            Error::Auth(msg) if msg.contains("admin token") => Some(
                "pass the server's admin token with --admin-token \
                 (set as [auth] admin_token in the server config)",
            ),
            Error::Auth(_) => Some(
                "pass a valid token with --token or the RUSTUNNEL_TOKEN env var; \
                 get one at https://rustunnel.com (Dashboard -> API Keys), or for a \
                 self-hosted server create one with `rustunnel token create`",
            ),
            Error::Connection(_) => Some(
                "check network connectivity and the server address; \
                 pass --server <host:port> or --region <eu|us|ap> to pick a different edge",
            ),
            Error::Tunnel(_) => Some(
                "the server rejected the tunnel registration; \
                 try a different --subdomain or check the server-side limits",
            ),
            Error::Protocol(_) | Error::Io(_) => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_hint_points_at_admin_token_for_token_create_failures() {
        // Message produced by `token create` on a 401/403 mentions "admin token".
        let e = Error::Auth(
            "token creation rejected by localhost:4040 (401): denied — \
             pass a valid --admin-token (the server's admin token)"
                .into(),
        );
        let hint = e.hint().unwrap();
        assert!(hint.contains("--admin-token"));
        assert!(!hint.contains("--token "));
    }

    #[test]
    fn auth_hint_points_at_token_for_tunnel_auth_failures() {
        let e = Error::Auth("server eu.edge.rustunnel.com:4040 rejected authentication".into());
        let hint = e.hint().unwrap();
        assert!(hint.contains("--token"));
        assert!(hint.contains("RUSTUNNEL_TOKEN"));
    }
}
