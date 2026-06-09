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
