//! A small HTTP/1.1 client and server-sent-events reader for `opencode serve`.
//!
//! It speaks plain HTTP over a loopback TCP connection: one request per connection, which is all
//! the driver needs and keeps it free of an HTTP crate. Chunked responses are decoded, because an
//! event stream usually arrives chunked.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// One loopback server, by authority (`127.0.0.1:4096`).
#[derive(Debug, Clone)]
pub struct Client {
    pub authority: String,
    /// The server password, sent as HTTP basic auth. `opencode serve` refuses everything else.
    pub password: Option<String>,
}

impl Client {
    pub fn new(authority: impl Into<String>) -> Self {
        Client {
            authority: authority.into(),
            password: None,
        }
    }

    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// The `Authorization` header for the server, if it has a password.
    fn authorization(&self) -> Option<String> {
        let password = self.password.as_deref()?;
        Some(format!(
            "Basic {}",
            base64(format!("opencode:{password}").as_bytes())
        ))
    }

    async fn connect(&self) -> Result<TcpStream, String> {
        TcpStream::connect(&self.authority)
            .await
            .map_err(|e| format!("cannot reach {}: {e}", self.authority))
    }

    /// Sends a request and reads the whole answer.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let (status, body) = self.request_raw(method, path, body, &[]).await?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "{method} {path} answered {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        if body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&body).map_err(|e| format!("{method} {path}: {e}"))
    }

    /// Sends a request and returns the status and body, whatever the status.
    pub async fn request_raw(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> Result<(u16, Vec<u8>), String> {
        let mut socket = self.connect().await?;
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.authority
        );
        if body.is_some() {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        if let Some(auth) = self.authorization() {
            request.push_str(&format!("Authorization: {auth}\r\n"));
        }
        for (k, v) in extra {
            request.push_str(&format!("{k}: {v}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&payload);
        socket
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("cannot write to {}: {e}", self.authority))?;
        let mut reader = BufReader::new(socket);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .await
            .map_err(|e| format!("no answer from {}: {e}", self.authority))?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| format!("bad status line {status_line:?}"))?;
        let mut length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.map_err(|e| e.to_string())?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => length = v.trim().parse().ok(),
                    "transfer-encoding" if v.to_ascii_lowercase().contains("chunked") => chunked = true,
                    _ => {}
                }
            }
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                reader.read_line(&mut size).await.map_err(|e| e.to_string())?;
                let n = usize::from_str_radix(size.trim(), 16).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                let mut chunk = vec![0u8; n];
                reader.read_exact(&mut chunk).await.map_err(|e| e.to_string())?;
                body.extend(chunk);
                let mut crlf = [0u8; 2];
                let _ = reader.read_exact(&mut crlf).await;
            }
        } else if let Some(n) = length {
            body.resize(n, 0);
            let _ = reader.read_exact(&mut body).await;
        } else {
            let _ = reader.read_to_end(&mut body).await;
        }
        Ok((status, body))
    }

    /// Opens the event stream.
    pub async fn events(&self) -> Result<EventStream, String> {
        let socket = self.connect().await?;
        let auth = self
            .authorization()
            .map(|a| format!("Authorization: {a}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "GET /api/event HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n{auth}\r\n",
            self.authority
        );
        let (read, mut write): (OwnedReadHalf, OwnedWriteHalf) = socket.into_split();
        write
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("cannot subscribe: {e}"))?;
        let mut reader = BufReader::new(read);
        let mut status = String::new();
        reader.read_line(&mut status).await.map_err(|e| e.to_string())?;
        let code = status
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        if code != 200 {
            return Err(format!("the event stream answered {status:?}"));
        }
        let mut chunked = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.map_err(|e| e.to_string())?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
        Ok(EventStream {
            reader,
            chunked,
            buffer: Vec::new(),
            closed: false,
        })
    }
}

/// The SSE stream, decoded from chunked framing into lines.
pub struct EventStream {
    reader: BufReader<OwnedReadHalf>,
    chunked: bool,
    buffer: Vec<u8>,
    closed: bool,
}

impl EventStream {
    /// The next `data:` payload, as JSON; `None` when the stream ends.
    pub async fn next_event(&mut self) -> Option<Value> {
        loop {
            let line = self.next_line().await?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str(data) {
                return Some(value);
            }
        }
    }

    async fn next_line(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.buffer.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(String::from_utf8_lossy(&line).into_owned());
            }
            if self.closed {
                return None;
            }
            if !self.fill().await {
                self.closed = true;
                if self.buffer.is_empty() {
                    return None;
                }
            }
        }
    }

    /// Reads the next piece of the body into the buffer; false at the end.
    async fn fill(&mut self) -> bool {
        if self.chunked {
            let mut header = String::new();
            if self.reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                return false;
            }
            let Ok(n) = usize::from_str_radix(header.trim(), 16) else {
                return false;
            };
            if n == 0 {
                return false;
            }
            let mut chunk = vec![0u8; n];
            if self.reader.read_exact(&mut chunk).await.is_err() {
                return false;
            }
            self.buffer.extend(chunk);
            let mut crlf = [0u8; 2];
            let _ = self.reader.read_exact(&mut crlf).await;
            true
        } else {
            let mut chunk = vec![0u8; 4096];
            match self.reader.read(&mut chunk).await {
                Ok(0) | Err(_) => false,
                Ok(n) => {
                    chunk.truncate(n);
                    self.buffer.extend(chunk);
                    true
                }
            }
        }
    }
}

/// Standard base64 with padding, enough for one basic-auth header.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b"opencode:secret"), "b3BlbmNvZGU6c2VjcmV0");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[tokio::test]
    async fn requests_and_events_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (socket, _) = listener.accept().await.unwrap();
                let (read, mut socket) = socket.into_split();
                let mut reader = BufReader::new(read);
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    request.push_str(&line);
                }
                if request.starts_with("GET /api/event") {
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                    socket.write_all(head.as_bytes()).await.unwrap();
                    let body = "data: {\"type\":\"session.idle\",\"data\":{\"sessionID\":\"s\"}}\n\n";
                    let framed = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
                    socket.write_all(framed.as_bytes()).await.unwrap();
                } else {
                    let body = "{\"data\":{\"id\":\"s1\"}}";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    socket.write_all(head.as_bytes()).await.unwrap();
                    socket.write_all(body.as_bytes()).await.unwrap();
                }
            }
        });
        let client = Client::new(authority);
        let answer = client
            .request("POST", "/api/session", Some(&serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(answer["data"]["id"], "s1");
        let mut events = client.events().await.unwrap();
        let event = events.next_event().await.expect("one event");
        assert_eq!(event["type"], "session.idle");
    }
}
