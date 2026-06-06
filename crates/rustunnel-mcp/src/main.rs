//! rustunnel-mcp — MCP server for AI agent access to rustunnel.
//!
//! Implements the [Model Context Protocol](https://spec.modelcontextprotocol.io)
//! over stdio (newline-delimited JSON-RPC 2.0).
//!
//! # Usage
//!
//! ```text
//! rustunnel-mcp \
//!   --server localhost:4040 \
//!   --api    http://localhost:4041
//! ```
//!
//! # Claude Desktop / MCP client config
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "rustunnel": {
//!       "command": "rustunnel-mcp",
//!       "args": ["--server", "tunnel.example.com:4040",
//!                "--api",    "https://tunnel.example.com:8443"],
//!       "env": { "RUSTUNNEL_TOKEN": "<your-token>" }
//!     }
//!   }
//! }
//! ```
//!
//! The `RUSTUNNEL_TOKEN` environment variable, when set, is used as the
//! default API token for every tool call — agents only need it configured
//! once here rather than passing `token` on each request.

mod api_client;
mod mcp;
mod tools;
mod tunnel_manager;

use std::sync::Arc;

use clap::Parser;
use mcp::{IncomingMessage, OutgoingMessage};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::warn;

use api_client::ApiClient;
use tunnel_manager::TunnelManager;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rustunnel-mcp",
    version,
    about = "MCP server for AI agent access to rustunnel"
)]
struct Cli {
    /// Control-plane address passed to the rustunnel CLI when spawning tunnels.
    /// Format: host:port  (e.g. tunnel.example.com:4040)
    #[arg(long, default_value = "localhost:4040")]
    server: String,

    /// Dashboard REST API base URL used to query tunnel state and history.
    /// (e.g. http://localhost:4041 or https://tunnel.example.com:8443)
    #[arg(long, default_value = "http://localhost:4041")]
    api: String,

    /// Skip TLS certificate verification (local dev / self-signed certs only).
    #[arg(long)]
    insecure: bool,
}

// ── shared state ──────────────────────────────────────────────────────────────

pub struct State {
    /// Control-plane address forwarded to the CLI subprocess.
    pub server_addr: String,
    /// Dashboard API client.
    pub api: ApiClient,
    /// Tracks CLI subprocesses spawned by create_tunnel.
    pub tunnel_manager: TunnelManager,
    /// Whether to pass --insecure to the CLI.
    pub insecure: bool,
    /// Default API token, read from the `RUSTUNNEL_TOKEN` environment variable
    /// at startup. Used as a fallback when a tool call omits the `token`
    /// argument, so agents don't have to pass the token on every call.
    pub token: Option<String>,
}

// ── entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // All diagnostic output must go to stderr — stdout is reserved for MCP.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // A token supplied via RUSTUNNEL_TOKEN becomes the default for every tool
    // call, so the agent only needs the token configured once (in the MCP
    // client config) rather than passing it on each request.
    let env_token = std::env::var("RUSTUNNEL_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());

    let state = Arc::new(State {
        server_addr: cli.server,
        api: ApiClient::new(&cli.api, cli.insecure),
        tunnel_manager: TunnelManager::new(),
        insecure: cli.insecure,
        token: env_token,
    });

    run(state).await;
}

// ── MCP stdio loop ────────────────────────────────────────────────────────────

async fn run(state: Arc<State>) {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(response) = handle_message(trimmed, &state).await {
            let mut json = serde_json::to_string(&response).unwrap_or_else(|e| {
                format!(r#"{{"jsonrpc":"2.0","error":{{"code":-32603,"message":"{e}"}}}}"#)
            });
            json.push('\n');
            if stdout.write_all(json.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    }

    // stdin closed — clean up any spawned tunnels.
    state.tunnel_manager.kill_all().await;
}

// ── message dispatch ──────────────────────────────────────────────────────────

async fn handle_message(raw: &str, state: &Arc<State>) -> Option<OutgoingMessage> {
    let msg: IncomingMessage = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            return Some(OutgoingMessage::rpc_error(
                None,
                -32700,
                format!("parse error: {e}"),
            ))
        }
    };

    // Notifications have no id and require no response.
    msg.id.as_ref()?;

    let id = msg.id.clone();

    let result = match msg.method.as_str() {
        "initialize" => handle_initialize(&msg),
        "ping" => serde_json::json!({}),
        "tools/list" => handle_tools_list(),
        "tools/call" => handle_tools_call(&msg, state).await,
        other => {
            warn!("unknown method: {other}");
            return Some(OutgoingMessage::rpc_error(id, -32601, "method not found"));
        }
    };

    Some(OutgoingMessage::ok(id, result))
}

fn handle_initialize(msg: &IncomingMessage) -> Value {
    let protocol_version = msg
        .params
        .as_ref()
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or("2024-11-05");

    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name":    "rustunnel-mcp",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn handle_tools_list() -> Value {
    serde_json::json!({
        "tools": tools::tool_definitions()
    })
}

async fn handle_tools_call(msg: &IncomingMessage, state: &Arc<State>) -> Value {
    let params = match &msg.params {
        Some(p) => p,
        None => return mcp::tool_err("missing params"),
    };

    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return mcp::tool_err("missing tool name"),
    };

    let empty = Value::Object(Default::default());
    let args = params.get("arguments").unwrap_or(&empty);

    tools::dispatch(name, args, state).await
}
