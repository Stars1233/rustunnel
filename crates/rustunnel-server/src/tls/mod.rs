//! TLS certificate management.
//!
//! [`CertManager`] handles initial certificate loading, optional ACME
//! issuance via Let's Encrypt, daily renewal checks, and hot-reloading the
//! live `rustls::ServerConfig` without a process restart.
//!
//! # Hot-reload integration
//!
//! Pass the handle returned by [`CertManager::tls_handle`] to the control-
//! plane server.  On each accepted TCP connection, read the current config
//! with `handle.load()` to pick up renewed certificates automatically:
//!
//! ```text
//! let tls_handle = cert_manager.tls_handle();
//! // … inside the accept loop:
//! let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&tls_handle.load()));
//! ```

pub mod acme;

pub use acme::CertManager;
