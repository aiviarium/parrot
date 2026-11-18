//! The MCP server over loopback HTTP (spec 26), in the streamable HTTP shape harnesses speak:
//! a `POST` carries one JSON-RPC message and a request's answer comes back as the response body;
//! a `GET` opens an event stream for the server's notifications, such as
//! `notifications/tools/list_changed`.
//!
//! Every request carries the binding's credential as `Authorization: Bearer <token>`, and a
//! request from a browser page (an `Origin` that is not loopback) is refused, so a web page
//! cannot reach the server through DNS rebinding. The front bridges to [`crate::server::serve`]
//! over a line stream, so both transports run the same server.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast, oneshot};

/// The largest request body the server reads.
const BODY_MAX: usize = 4 << 20;
/// The largest request head the server reads.
const HEAD_MAX: usize = 64 << 10;

type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Front {
    token: String,
    path: String,
    to_server: Mutex<tokio::io::WriteHalf<DuplexStream>>,
    waiting: Waiting,
    notes: broadcast::Sender<String>,
    next: AtomicU64,
}

/// Serves MCP over HTTP on `listener` at `path` until the server side ends. `server` is the
/// other end of a duplex stream that [`crate::server::serve`] runs on.
pub async fn serve(
    listener: TcpListener,
    path: &str,
    token: String,
    server: DuplexStream,
) -> std::io::Result<()> {
    let (from_server, to_server) = tokio::io::split(server);
    let (notes, _) = broadcast::channel(64);
    let front = Arc::new(Front {
        token,
        path: path.to_string(),
        to_server: Mutex::new(to_server),
        waiting: Waiting::default(),
        notes,
        next: AtomicU64::new(1),
    });
    // Answers go to the POST that waits for them; everything else is a notification.
    let mut reader = {
        let front = front.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(from_server).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(mut msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let ours = msg.get("method").is_none().then(|| msg["id"].as_u64()).flatten();
                match ours {
                    Some(n) => {
                        if let Some(tx) = front.waiting.lock().await.remove(&n) {
                            msg.as_object_mut().map(|o| o.remove("id"));
                            let _ = tx.send(msg);
                        }
                    }
                    None => {
                        let _ = front.notes.send(line);
                    }
                }
            }
        })
    };
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let front = front.clone();
                tokio::spawn(async move {
                    let _ = connection(&front, stream).await;
                });
            }
            _ = &mut reader => return Ok(()),
        }
    }
}

struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Reads one request head; `None` when the peer closed first.
async fn read_head(stream: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<Option<(Head, usize)>> {
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);
        match req.parse(buf) {
            Ok(httparse::Status::Complete(n)) => {
                let head = Head {
                    method: req.method.unwrap_or_default().to_string(),
                    path: req.path.unwrap_or_default().to_string(),
                    headers: req
                        .headers
                        .iter()
                        .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).into_owned()))
                        .collect(),
                };
                return Ok(Some((head, n)));
            }
            Ok(httparse::Status::Partial) if buf.len() < HEAD_MAX => {}
            _ => return Err(std::io::Error::other("bad request head")),
        }
        let mut chunk = [0u8; 8192];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn respond(stream: &mut TcpStream, status: &str, body: Option<&Value>) -> std::io::Result<()> {
    let text = body.map(Value::to_string).unwrap_or_default();
    let kind = if body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    let head = format!(
        "HTTP/1.1 {status}\r\n{kind}Content-Length: {}\r\n\r\n",
        text.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(text.as_bytes()).await?;
    stream.flush().await
}

/// Whether `origin` is a loopback page, or absent as from a harness.
fn loopback_origin(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let host = origin.split("://").nth(1).unwrap_or(origin).trim_end_matches('/');
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Compares in time independent of where the strings differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn connection(front: &Front, mut stream: TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    loop {
        let Some((head, used)) = read_head(&mut stream, &mut buf).await? else {
            return Ok(());
        };
        buf.drain(..used);
        let close = head
            .header("connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        let len: usize = head
            .header("content-length")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        if len > BODY_MAX {
            return respond(&mut stream, "413 Payload Too Large", None).await;
        }
        while buf.len() < len {
            let mut chunk = [0u8; 8192];
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body: Vec<u8> = buf.drain(..len).collect();

        if !loopback_origin(head.header("origin")) {
            respond(&mut stream, "403 Forbidden", None).await?;
            if close {
                return Ok(());
            }
            continue;
        }
        let bearer = head
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default();
        if !same(bearer.trim(), &front.token) {
            respond(&mut stream, "401 Unauthorized", None).await?;
            if close {
                return Ok(());
            }
            continue;
        }
        if head.path.split('?').next() != Some(front.path.as_str()) {
            respond(&mut stream, "404 Not Found", None).await?;
            if close {
                return Ok(());
            }
            continue;
        }
        match head.method.as_str() {
            "POST" => post(front, &mut stream, &body).await?,
            "GET" => return events(front, stream).await,
            _ => respond(&mut stream, "405 Method Not Allowed", None).await?,
        }
        if close {
            return Ok(());
        }
    }
}

async fn post(front: &Front, stream: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    let Ok(mut msg) = serde_json::from_slice::<Value>(body) else {
        let err =
            json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } });
        return respond(stream, "400 Bad Request", Some(&err)).await;
    };
    let is_request = msg.get("method").is_some() && msg.get("id").is_some_and(|i| !i.is_null());
    if !is_request {
        // Notifications and answers are passed on; nothing comes back for them.
        send(front, &msg).await?;
        return respond(stream, "202 Accepted", None).await;
    }
    // Ids are the server's own, so two clients' ids never meet; the caller's comes back.
    let theirs = msg["id"].take();
    let n = front.next.fetch_add(1, Ordering::Relaxed);
    msg["id"] = json!(n);
    let (tx, rx) = oneshot::channel();
    front.waiting.lock().await.insert(n, tx);
    send(front, &msg).await?;
    let Ok(mut answer) = rx.await else {
        return respond(stream, "503 Service Unavailable", None).await;
    };
    answer["id"] = theirs;
    respond(stream, "200 OK", Some(&answer)).await
}

async fn send(front: &Front, msg: &Value) -> std::io::Result<()> {
    let mut w = front.to_server.lock().await;
    w.write_all(format!("{msg}\n").as_bytes()).await?;
    w.flush().await
}

/// The event stream: each server notification as one `message` event.
async fn events(front: &Front, mut stream: TcpStream) -> std::io::Result<()> {
    let mut notes = front.notes.subscribe();
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n")
        .await?;
    stream.flush().await?;
    loop {
        match notes.recv().await {
            Ok(line) => {
                stream
                    .write_all(format!("event: message\ndata: {line}\n\n").as_bytes())
                    .await?;
                stream.flush().await?;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_pages_pass() {
        assert!(loopback_origin(None));
        assert!(loopback_origin(Some("http://localhost:5173")));
        assert!(loopback_origin(Some("http://127.0.0.1")));
        assert!(loopback_origin(Some("http://[::1]:80")));
        assert!(!loopback_origin(Some("https://evil.example.test")));
        assert!(!loopback_origin(Some("http://127.0.0.1.example.test")));
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same("tok-a", "tok-a"));
        assert!(!same("tok-a", "tok-b"));
        assert!(!same("tok-a", "tok-ab"));
    }
}
