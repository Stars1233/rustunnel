//! Tracks spawned `rustunnel` CLI subprocesses so they can be killed when a
//! tunnel is closed or when the MCP server exits.
//!
//! Load-balanced tunnels are started via `rustunnel start --config <tmp>`,
//! so each tracked process may also own a temporary config file that must be
//! removed when the process is killed.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::process::Child;
use tokio::sync::Mutex;

/// A spawned CLI process plus any temporary file it owns.
struct Tracked {
    child: Child,
    /// Temp config file written for load-balanced tunnels (`rustunnel start`).
    /// Removed when the process is killed.
    temp_config: Option<PathBuf>,
}

pub struct TunnelManager {
    processes: Mutex<HashMap<String, Tracked>>,
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
        }
    }

    /// Register a child process for the given tunnel ID, optionally tracking a
    /// temporary config file to delete when the process is killed.
    pub async fn insert(&self, tunnel_id: String, child: Child, temp_config: Option<PathBuf>) {
        self.processes
            .lock()
            .await
            .insert(tunnel_id, Tracked { child, temp_config });
    }

    /// Kill the process associated with `tunnel_id`, if one exists.
    /// Returns `true` if a process was found and signalled.
    pub async fn kill(&self, tunnel_id: &str) -> bool {
        let mut guard = self.processes.lock().await;
        if let Some(mut tracked) = guard.remove(tunnel_id) {
            let _ = tracked.child.start_kill();
            if let Some(path) = tracked.temp_config.take() {
                let _ = std::fs::remove_file(path);
            }
            true
        } else {
            false
        }
    }

    /// Kill all tracked processes. Called on server shutdown.
    pub async fn kill_all(&self) {
        let mut guard = self.processes.lock().await;
        for (_, mut tracked) in guard.drain() {
            let _ = tracked.child.start_kill();
            if let Some(path) = tracked.temp_config.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
