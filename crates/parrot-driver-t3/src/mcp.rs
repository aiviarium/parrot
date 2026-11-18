//! A minimal MCP client over stdio: JSON-RPC 2.0, one message per line.
//!
//! T3's orchestration tools live on its `/mcp` HTTP endpoint. The driver runs a command that
//! speaks the same MCP messages on stdio, which is what `fake-t3` does; the HTTP transport is not
//! implemented yet (see the crate's README).

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;

/// The MCP protocol version the client asks for.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// How long one tool call may take before the driver gives up.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `binding/stop` waits for the server before killing it.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

/// A running MCP server.
pub struct Mcp {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Child>,
    pending: Pending,
    next: AtomicU64,
    stopping: AtomicBool,
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
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(child),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
        });
        tokio::spawn(read_loop(mcp.clone(), stdout));
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "parrot", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
        mcp.notify("notifications/initialized", json!({})).await?;
        // Listing proves the endpoint is the T3 tool server before a binding starts.
        mcp.request("tools/list", json!({})).await?;
        Ok(mcp)
    }

    async fn write(&self, msg: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
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

    /// A request and its result.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("the MCP server closed".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("the MCP server did not answer {method}"))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
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

    pub async fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(mut stdin) = self.stdin.lock().await.take() {
            let _ = stdin.shutdown().await;
        }
        let mut child = self.child.lock().await;
        if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() {
            if let Some(pid) = child.id() {
                parrot_guard::kill_tree(pid);
            }
            let _ = child.kill().await;
        }
    }
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
        let _ = tx.send(match msg.get("error") {
            Some(error) => Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the MCP server refused the call")
                .to_owned()),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        });
    }
    mcp.pending.lock().unwrap().clear();
}
