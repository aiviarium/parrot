//! The connection to the runtime: a JSON-RPC client over one JSON message per line (spec 3.1).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parrot_proto::rpc::{Message, Notification, Request, Response, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// The code a call fails with when the runtime connection is gone.
pub const CLOSED: i64 = -32030;

/// A JSON-RPC client to the runtime. Cloning shares the connection.
#[derive(Clone)]
pub struct Upstream {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: Arc<AtomicU64>,
}

impl Upstream {
    /// Starts the reader and writer tasks. Notifications from the runtime arrive on the receiver,
    /// which closes when the connection does.
    pub fn spawn<R, W>(reader: R, writer: W) -> (Upstream, mpsc::UnboundedReceiver<Notification>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (notes_tx, notes_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let up = Upstream {
            out: out.clone(),
            pending: pending.clone(),
            next: Arc::new(AtomicU64::new(1)),
        };
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err()
                    || writer.write_all(b"\n").await.is_err()
                    || writer.flush().await.is_err()
                {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim_end_matches('\r');
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Message>(line) else {
                    continue;
                };
                match msg {
                    Message::Response(resp) => {
                        let Some(id) = resp.id.as_u64() else { continue };
                        if let Some(tx) = pending.lock().await.remove(&id) {
                            let result = match resp.error {
                                Some(err) => Err(err),
                                None => Ok(resp.result.unwrap_or(Value::Null)),
                            };
                            let _ = tx.send(result);
                        }
                    }
                    // This client provides no functions, so it answers every runtime request alike.
                    Message::Request(req) => {
                        let resp =
                            Response::err(req.id, RpcError::new(-32601, "this client provides no functions"));
                        let _ = out.send(serde_json::to_string(&resp).unwrap_or_default());
                    }
                    Message::Notification(note) => {
                        let _ = notes_tx.send(note);
                    }
                }
            }
            for (_, tx) in pending.lock().await.drain() {
                let _ = tx.send(Err(closed()));
            }
        });
        (up, notes_rx)
    }

    /// Calls a runtime method and waits for its answer.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let req = Request::new(json!(id), method, params);
        let line = serde_json::to_string(&req).map_err(|e| RpcError::new(-32603, e.to_string()))?;
        if self.out.send(line).is_err() {
            self.pending.lock().await.remove(&id);
            return Err(closed());
        }
        rx.await.unwrap_or_else(|_| Err(closed()))
    }
}

fn closed() -> RpcError {
    RpcError::new(CLOSED, "the runtime connection closed")
}
