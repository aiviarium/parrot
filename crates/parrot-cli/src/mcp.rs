//! `parrot mcp`: the MCP server a harness starts for its binding (spec 26), on standard input
//! and output.

use std::path::PathBuf;
use std::time::Duration;

use parrot_mcp::server::{self, Auth, Config};
use parrot_mcp::upstream::Upstream;

/// The environment variable that carries the binding's credential.
const TOKEN_ENV: &str = "PARROT_MCP_TOKEN";
const BINDING_ENV: &str = "PARROT_BINDING";
const TIMEOUT_ENV: &str = "PARROT_TOOL_TIMEOUT";

#[derive(clap::Args)]
pub struct Args {
    /// The binding this server serves; default `PARROT_BINDING`.
    #[arg(long)]
    binding: Option<String>,
    /// The runtime's local socket; default `PARROT_SOCKET` or the platform path.
    #[arg(long, conflicts_with = "endpoint")]
    socket: Option<PathBuf>,
    /// A runtime WebSocket endpoint, such as `ws://127.0.0.1:7420/parrot`.
    #[arg(long)]
    endpoint: Option<String>,
    /// A file holding the binding's credential; default `PARROT_MCP_TOKEN`.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// The harness's tool-call timeout in seconds; default `PARROT_TOOL_TIMEOUT`.
    #[arg(long)]
    tool_timeout: Option<u64>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub async fn run(args: Args) -> Result<(), String> {
    let binding = args
        .binding
        .or_else(|| env(BINDING_ENV))
        .ok_or("--binding or PARROT_BINDING is required")?;
    let token = match &args.token_file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?
                .trim()
                .to_string(),
        ),
        None => env(TOKEN_ENV),
    };
    let tool_timeout = match args.tool_timeout {
        Some(s) => Some(s),
        None => env(TIMEOUT_ENV)
            .map(|v| {
                v.parse::<u64>()
                    .map_err(|_| format!("{TIMEOUT_ENV} is not a number of seconds"))
            })
            .transpose()?,
    };
    let (up, notes) = match &args.endpoint {
        Some(url) => {
            let stream = parrot_mcp::ws::connect(url, token.as_deref())
                .await
                .map_err(|e| format!("cannot connect to {url}: {e}"))?;
            let (r, w) = tokio::io::split(stream);
            Upstream::spawn(r, w)
        }
        None => {
            let path = match args.socket {
                Some(p) => p,
                None => crate::serve::socket_path().map_err(|e| e.to_string())?,
            };
            let stream = open(&path)
                .await
                .map_err(|e| format!("cannot connect to {}: {e}", path.display()))?;
            let (r, w) = tokio::io::split(stream);
            Upstream::spawn(r, w)
        }
    };
    let auth = Auth { binding, token };
    let identity = server::handshake(&up, &auth).await?;
    let config = Config {
        tool_timeout: tool_timeout.map(Duration::from_secs),
        ..Config::default()
    };
    server::serve(
        tokio::io::stdin(),
        tokio::io::stdout(),
        up,
        notes,
        identity,
        config,
    )
    .await
    .map_err(|e| e.to_string())
}

#[cfg(unix)]
async fn open(path: &std::path::Path) -> std::io::Result<tokio::net::UnixStream> {
    tokio::net::UnixStream::connect(path).await
}

#[cfg(windows)]
async fn open(path: &std::path::Path) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    tokio::net::windows::named_pipe::ClientOptions::new().open(path)
}
