//! Exponential-backoff reconnect loop.
//!
//! Wraps the `control::connect` function so that transient failures (network
//! drops, server restarts) result in automatic reconnection rather than
//! process exit.
//!
//! Delay schedule:
//!   initial = 1 s, multiplier = 2×, max = 60 s, jitter = ±20 %

use std::time::Duration;

use rand::Rng;
use tracing::{info, warn};

use crate::config::{ClientConfig, TunnelDef};
use crate::control;
use crate::error::{Error, Result};
use crate::output;

const INITIAL_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(60);
const MULTIPLIER: f64 = 2.0;
const JITTER: f64 = 0.20; // ±20 %

/// Run `connect` with exponential-backoff retry on failure.
///
/// Returns `Ok(())` when the connection ends cleanly (e.g. Ctrl-C) and
/// `Err(_)` on a fatal, non-retryable error (auth or tunnel-registration
/// rejection).
pub async fn run_with_reconnect(config: ClientConfig, tunnels: Vec<TunnelDef>) -> Result<()> {
    let mut delay = INITIAL_DELAY;
    let mut attempt: u32 = 0;
    let mut last_error = String::new();

    loop {
        if attempt > 0 {
            output::note_reconnecting();
            if output::json_mode() {
                output::emit(&output::Event::Reconnecting {
                    attempt,
                    reason: last_error.clone(),
                    delay_secs: delay.as_secs_f64(),
                });
            } else {
                eprintln!(
                    "  Reconnecting in {:.1}s (attempt {attempt})…",
                    delay.as_secs_f64()
                );
            }
            tokio::time::sleep(delay).await;
            delay = next_delay(delay);
        }

        info!(attempt, "connecting to tunnel server");

        match control::connect(&config, &tunnels).await {
            Ok(()) => {
                // Clean exit (e.g. Ctrl-C) — stop retrying.
                info!("connection closed cleanly");
                return Ok(());
            }
            Err(e) => {
                if is_fatal(&e) {
                    return Err(e);
                }
                warn!("connection error: {e}");
                last_error = e.to_string();
                attempt += 1;
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Deterministic server rejections where retrying cannot help:
/// - `Auth` — invalid/revoked token; the server will keep rejecting it.
/// - `Tunnel` — registration refused (subdomain taken, tunnel limit reached).
///
/// Connection/IO/protocol errors are transient and stay retryable.
fn is_fatal(e: &Error) -> bool {
    matches!(e, Error::Auth(_) | Error::Tunnel(_))
}

fn next_delay(current: Duration) -> Duration {
    let mut rng = rand::thread_rng();
    let jitter_factor = 1.0 + rng.gen_range(-JITTER..=JITTER);
    let next_secs =
        (current.as_secs_f64() * MULTIPLIER * jitter_factor).min(MAX_DELAY.as_secs_f64());
    Duration::from_secs_f64(next_secs)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_tunnel_errors_are_fatal() {
        assert!(is_fatal(&Error::Auth("bad token".into())));
        assert!(is_fatal(&Error::Tunnel("subdomain already taken".into())));
    }

    #[test]
    fn transient_errors_are_retryable() {
        assert!(!is_fatal(&Error::Connection("connection refused".into())));
        assert!(!is_fatal(&Error::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "pipe"
        ))));
        assert!(!is_fatal(&Error::Config("missing server".into())));
    }
}
