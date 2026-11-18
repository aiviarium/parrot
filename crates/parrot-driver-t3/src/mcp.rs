//! A minimal MCP client: JSON-RPC 2.0 over stdio, or over T3's HTTP `/mcp` endpoint.
//!
//! T3 serves its orchestration tools at `POST /mcp` with the streamable HTTP transport: a JSON
//! request, a `Mcp-Session-Id` header the client sends back, and a JSON or SSE answer. The stdio
//! form runs a command that speaks the same messages, which is what `fake-t3` does.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;

/// The MCP protocol version the client asks for.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// How long one tool call may take before the driver gives up.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `binding/stop` waits for the server before killing it.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

/// Where the client's messages go.
enum Transport {
    Stdio {
        stdin: tokio::sync::Mutex<Option<ChildStdin>>,
        child: tokio::sync::Mutex<Child>,
    },
    Http(Http),
}

/// T3's `/mcp` endpoint, by its authority (`127.0.0.1:3773`) and path (`/mcp`).
struct Http {
    authority: String,
    path: String,
    token: Option<String>,
    /// The session the server named in its first answer, sent with every later request.
    session: tokio::sync::Mutex<Option<String>>,
}

/// A running MCP server.
pub struct Mcp {
    transport: Transport,
    pending: Pending,
    next: AtomicU64,
    stopping: AtomicBool,
    /// What the server said in `initialize`.
    server: Mutex<Option<Value>>,
}

impl Mcp {
    /// Starts `argv` in `cwd`, runs the MCP handshake and lists the tools.
    pub async fn connect(
        argv: &[OsString],
        cwd: &str,
        env: &[(OsString, OsString)],
        guard: bool,
    ) -> Result<Arc<Self>, String> {
        let Some(program) = argv.first() else {
            return Err("no MCP command".into());
        };
        let mut cmd = if guard {
            parrot_guard::guarded_command(program)
        } else {
            let mut c = tokio::process::Command::new(program);
            #[cfg(unix)]
            c.process_group(0);
            c
        };
        cmd.args(&argv[1..])
            .envs(env.iter().cloned())
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", program.to_string_lossy()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let mcp = Arc::new(Mcp {
            transport: Transport::Stdio {
                stdin: tokio::sync::Mutex::new(stdin),
                child: tokio::sync::Mutex::new(child),
            },
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            server: Mutex::default(),
        });
        tokio::spawn(read_loop(mcp.clone(), stdout));
        mcp.open().await?;
        Ok(mcp)
    }

    /// Connects to a T3 server's HTTP `/mcp` endpoint and runs the handshake.
    pub async fn connect_http(url: &str, token: Option<String>) -> Result<Arc<Self>, String> {
        let (authority, path) = split_url(url)?;
        let mcp = Arc::new(Mcp {
            transport: Transport::Http(Http {
                authority,
                path,
                token,
                session: tokio::sync::Mutex::default(),
            }),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            server: Mutex::default(),
        });
        mcp.open().await?;
        Ok(mcp)
    }

    /// The handshake both transports share: initialize, initialized, tools/list.
    async fn open(&self) -> Result<(), String> {
        let answer = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "parrot", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        *self.server.lock().unwrap() = Some(answer);
        self.notify("notifications/initialized", json!({})).await?;
        // Listing proves the endpoint is the T3 tool server before a binding starts.
        self.request("tools/list", json!({})).await?;
        Ok(())
    }

    /// The server's `initialize` answer, with its name and version.
    pub fn server_info(&self) -> Option<Value> {
        self.server
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.get("serverInfo").cloned())
    }

    async fn write(&self, msg: &Value) -> Result<(), String> {
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut stdin = stdin.lock().await;
                let pipe = stdin
                    .as_mut()
                    .ok_or_else(|| "the MCP server has exited".to_string())?;
                let mut line = msg.to_string();
                line.push('\n');
                pipe.write_all(line.as_bytes())
                    .await
                    .map_err(|e| format!("cannot write to the MCP server: {e}"))?;
                pipe.flush().await.map_err(|e| e.to_string())
            }
            Transport::Http(_) => Ok(()),
        }
    }

    /// A request and its result.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        match &self.transport {
            Transport::Http(http) => http.request(msg, Some(id)).await,
            Transport::Stdio { .. } => {
                let (tx, rx) = oneshot::channel();
                self.pending.lock().unwrap().insert(id, tx);
                self.write(&msg).await?;
                match tokio::time::timeout(CALL_TIMEOUT, rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err("the MCP server closed".into()),
                    Err(_) => {
                        self.pending.lock().unwrap().remove(&id);
                        Err(format!("the MCP server did not answer {method}"))
                    }
                }
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match &self.transport {
            Transport::Http(http) => http.request(msg, None).await.map(|_| ()),
            Transport::Stdio { .. } => self.write(&msg).await,
        }
    }

    /// Calls a tool and returns its structured content, or its text as a value.
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<Value, String> {
        let result = self
            .request("tools/call", json!({ "name": tool, "arguments": arguments }))
            .await?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|c| c.first())
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("the tool failed");
            return Err(text.to_owned());
        }
        if let Some(value) = result.get("structuredContent").filter(|v| !v.is_null()) {
            return Ok(value.clone());
        }
        // Older servers answer with one text content block holding JSON.
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())))
    }

    /// Uploads bytes to the URL a T3 upload prepared, on the same server.
    pub async fn upload(&self, relative: &str, bytes: &[u8], mime: &str) -> Result<(), String> {
        let Transport::Http(http) = &self.transport else {
            return Err("uploads need the HTTP transport".into());
        };
        let (status, _, _) = raw_request(
            &http.authority,
            "POST",
            relative,
            Some(bytes),
            mime,
            http.token.as_deref(),
            None,
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(format!("the upload answered {status}"));
        }
        Ok(())
    }

    pub async fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        match &self.transport {
            Transport::Http(http) => {
                let session = http.session.lock().await.clone();
                if let Some(session) = session {
                    let _ = raw_request(
                        &http.authority,
                        "DELETE",
                        &http.path,
                        None,
                        "application/json",
                        http.token.as_deref(),
                        Some(&session),
                    )
                    .await;
                }
            }
            Transport::Stdio { stdin, child } => {
                if let Some(mut stdin) = stdin.lock().await.take() {
                    let _ = stdin.shutdown().await;
                }
                let mut child = child.lock().await;
                if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() {
                    if let Some(pid) = child.id() {
                        parrot_guard::kill_tree(pid);
                    }
                    let _ = child.kill().await;
                }
            }
        }
    }
}

impl Http {
    /// One HTTP message: a request waits for its answer, a notification only for the status.
    async fn request(&self, msg: Value, id: Option<u64>) -> Result<Value, String> {
        let session = self.session.lock().await.clone();
        let (status, headers, body) = raw_request(
            &self.authority,
            "POST",
            &self.path,
            Some(msg.to_string().as_bytes()),
            "application/json",
            self.token.as_deref(),
            session.as_deref(),
        )
        .await?;
        if let Some(session) = headers.get("mcp-session-id") {
            *self.session.lock().await = Some(session.clone());
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "POST {} answered {status}: {}",
                self.path,
                String::from_utf8_lossy(&body)
            ));
        }
        let Some(id) = id else {
            return Ok(Value::Null);
        };
        // The answer may be JSON, or one SSE event whose data carries the message.
        let content_type = headers.get("content-type").cloned().unwrap_or_default();
        if content_type.contains("text/event-stream") {
            for line in String::from_utf8_lossy(&body).lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    return answer_of(value);
                }
            }
            return Err("the event stream carried no answer".into());
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|e| format!("the MCP answer was not JSON: {e}"))?;
        answer_of(value)
    }
}

fn answer_of(value: Value) -> Result<Value, String> {
    match value.get("error") {
        Some(error) if !error.is_null() => Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the MCP server refused the call")
            .to_owned()),
        _ => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
    }
}

/// `host:port` and the path of an `http://` URL. The driver speaks plain HTTP, for a local T3.
pub fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url} is not an http:// address; the driver does not speak TLS"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/mcp".to_owned()),
    };
    if authority.is_empty() {
        return Err(format!("{url} names no host"));
    }
    Ok((authority.to_owned(), path))
}

/// A raw HTTP/1.1 request. Returned header names are lower case.
async fn raw_request(
    authority: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    content_type: &str,
    token: Option<&str>,
    session: Option<&str>,
) -> Result<(u16, HashMap<String, String>, Vec<u8>), String> {
    let mut socket = TcpStream::connect(authority)
        .await
        .map_err(|e| format!("cannot reach {authority}: {e}"))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\n"
    );
    if body.is_some() {
        request.push_str(&format!("Content-Type: {content_type}\r\n"));
        request.push_str(&format!(
            "Content-Length: {}\r\n",
            body.map(|b| b.len()).unwrap_or(0)
        ));
    }
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(session) = session {
        request.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
    }
    request.push_str("\r\n");
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("cannot write to {authority}: {e}"))?;
    if let Some(body) = body {
        socket
            .write_all(body)
            .await
            .map_err(|e| format!("cannot write to {authority}: {e}"))?;
    }
    let mut reader = BufReader::new(socket);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .map_err(|e| format!("no answer from {authority}: {e}"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line {status_line:?}"))?;
    let mut headers = HashMap::new();
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
            let k = k.to_ascii_lowercase();
            if k == "content-length" {
                length = v.trim().parse().ok();
            }
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
            headers.insert(k, v.trim().to_owned());
        }
    }
    let mut answer = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            if reader.read_line(&mut size).await.unwrap_or(0) == 0 {
                break;
            }
            let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
                break;
            };
            if n == 0 {
                break;
            }
            let mut chunk = vec![0u8; n];
            if reader.read_exact(&mut chunk).await.is_err() {
                break;
            }
            answer.extend(chunk);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf).await;
        }
    } else if let Some(n) = length {
        answer.resize(n, 0);
        let _ = reader.read_exact(&mut answer).await;
    } else {
        let _ = reader.read_to_end(&mut answer).await;
    }
    Ok((status, headers, answer))
}

async fn read_loop(mcp: Arc<Mcp>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").and_then(Value::as_u64) else {
            // A server notification; the driver has no need for them yet.
            continue;
        };
        let Some(tx) = mcp.pending.lock().unwrap().remove(&id) else {
            continue;
        };
        let _ = tx.send(answer_of(msg));
    }
    mcp.pending.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_an_http_url() {
        assert_eq!(
            split_url("http://127.0.0.1:3773/mcp").unwrap(),
            ("127.0.0.1:3773".into(), "/mcp".into())
        );
        assert_eq!(
            split_url("http://127.0.0.1:3773").unwrap(),
            ("127.0.0.1:3773".into(), "/mcp".into())
        );
        assert!(split_url("https://example.test/mcp").is_err());
    }
}
