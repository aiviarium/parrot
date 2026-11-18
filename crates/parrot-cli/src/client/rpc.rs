//! A small JSON-RPC client over the local socket (spec 3.3): one message per line.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// A connection that has sent `initialize`.
pub struct Rpc {
    write: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    incoming: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    /// Notifications that arrived while waiting for a response.
    pending: std::collections::VecDeque<Value>,
}

/// The socket: the given path, `PARROT_SOCKET`, or the platform default.
pub fn socket(path: Option<&Path>) -> Result<PathBuf, String> {
    match path {
        Some(p) => Ok(p.to_path_buf()),
        None => crate::serve::socket_path().map_err(|e| e.to_string()),
    }
}

impl Rpc {
    pub async fn connect(path: &Path) -> Result<Self, String> {
        let (read, write) = open(path).await.map_err(|e| {
            format!(
                "cannot connect to {}: {e} (is `parrot serve` running?)",
                path.display()
            )
        })?;
        let (tx, incoming) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(v).is_err()
                {
                    break;
                }
            }
        });
        let mut rpc = Rpc {
            write,
            incoming,
            next_id: 1,
            pending: Default::default(),
        };
        rpc.call(
            "initialize",
            json!({
                "protocol": [1],
                "peer": { "kind": "client", "name": "parrot-cli", "version": env!("CARGO_PKG_VERSION") },
                "features": {},
            }),
        )
        .await?;
        Ok(rpc)
    }

    /// Sends a request and returns its result, or the error as text.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        self.write
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("cannot send {method}: {e}"))?;
        loop {
            let msg = self
                .incoming
                .recv()
                .await
                .ok_or_else(|| format!("the runtime closed the connection during {method}"))?;
            if msg.get("method").is_some() {
                self.pending.push_back(msg);
                continue;
            }
            if msg["id"] != id {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let code = &err["code"];
                let text = err["message"].as_str().unwrap_or("error");
                return Err(format!("{method}: {text} ({code})"));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// The next notification, or `None` when the connection closes.
    pub async fn notification(&mut self) -> Option<Value> {
        if let Some(n) = self.pending.pop_front() {
            return Some(n);
        }
        loop {
            let msg = self.incoming.recv().await?;
            if msg.get("method").is_some() {
                return Some(msg);
            }
        }
    }
}

type Halves = (
    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
);

#[cfg(unix)]
async fn open(path: &Path) -> std::io::Result<Halves> {
    let (r, w) = tokio::net::UnixStream::connect(path).await?.into_split();
    Ok((Box::new(r), Box::new(w)))
}

#[cfg(windows)]
async fn open(path: &Path) -> std::io::Result<Halves> {
    let pipe = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    let (r, w) = tokio::io::split(pipe);
    Ok((Box::new(r), Box::new(w)))
}
