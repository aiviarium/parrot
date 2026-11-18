//! A fake T3 MCP server: the orchestration tools Parrot calls, over stdio or T3's HTTP `/mcp`.
//!
//! It answers tool calls with made-up threads and messages, accepts attachment uploads on the
//! HTTP transport, and never starts an agent.

use std::io::{BufRead, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::mcp::MCP_PROTOCOL_VERSION;

/// What the fake remembers between calls.
#[derive(Default)]
pub struct Fake {
    /// The messages sent to each thread, newest last.
    sent: Mutex<Vec<String>>,
    /// The last attachment metadata a send named.
    attachments: Mutex<usize>,
}

fn tool(name: &str, description: &str, properties: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties },
    })
}

/// Every tool the fake advertises.
pub fn tools() -> Value {
    json!({ "tools": [
        tool("orchestrator_capabilities", "Providers and models", json!({})),
        tool("t3_thread_launch", "Create a top-level thread", json!({ "title": {"type": "string"}, "message": {"type": "string"} })),
        tool("t3_thread_send", "Send to a thread", json!({ "threadId": {"type": "string"}, "message": {"type": "string"}, "mode": {"type": "string"} })),
        tool("t3_thread_send_attachments", "Send attachments to a thread", json!({ "threadId": {"type": "string"}, "message": {"type": "string"}, "attachments": {"type": "array"} })),
        tool("t3_thread_read", "Read a thread", json!({ "threadId": {"type": "string"} })),
        tool("t3_thread_wait", "Wait for a thread's run", json!({ "threadId": {"type": "string"}, "timeoutMs": {"type": "number"} })),
        tool("t3_thread_interrupt", "Interrupt a thread", json!({ "threadId": {"type": "string"} })),
        tool("t3_pending_request", "List, read or answer a pending request", json!({ "action": {"type": "string"} })),
        tool("t3_attachment_prepare_upload", "Get an upload URL", json!({ "upload": {"type": "object"} })),
    ]})
}

/// One `tools/call` answer.
pub fn call(fake: &Fake, name: &str, arguments: &Value) -> Value {
    let thread = arguments
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("thread-1");
    if name == "t3_thread_launch" {
        // T3's launch names the provider inside `modelSelection`, never at the top level.
        if arguments.get("providerInstanceId").is_some() {
            return failure("t3_thread_launch has no providerInstanceId; use modelSelection");
        }
        if let Some(selection) = arguments.get("modelSelection")
            && selection.get("instanceId").and_then(Value::as_str).is_none()
        {
            return failure("modelSelection needs instanceId and model");
        }
    }
    if name == "t3_thread_send"
        && arguments.get("mode").and_then(Value::as_str).is_none()
    {
        return failure("t3_thread_send names its mode");
    }
    let structured = match name {
        "orchestrator_capabilities" => json!({
            "providers": [{
                "providerInstanceId": "fake",
                "driverKind": "fake",
                "displayName": "Fake",
                "models": [{"id": "fake-model"}],
                "canRunChildTask": false,
                "canRunCrossProviderChildTask": false,
                "constraints": [],
            }],
        }),
        "t3_thread_launch" => json!({
            "threadId": "thread-1",
            "projectId": "project-1",
            "modelSelection": { "instanceId": "fake", "model": "fake-model" },
            "runId": null,
            "status": null,
        }),
        "t3_thread_send" => {
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            fake.sent.lock().unwrap().push(message.to_owned());
            *fake.attachments.lock().unwrap() = 0;
            json!({
                "threadId": thread,
                "messageId": format!("msg-{}", fake.sent.lock().unwrap().len()),
                "runId": "run-1",
                "status": "running",
                "delivery": if arguments.get("mode").and_then(Value::as_str) == Some("steer") {
                    "steered"
                } else {
                    "started"
                },
            })
        }
        "t3_thread_send_attachments" => {
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            fake.sent.lock().unwrap().push(message.to_owned());
            let count = arguments
                .get("attachments")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            *fake.attachments.lock().unwrap() = count;
            json!({
                "threadId": thread,
                "messageId": format!("msg-{}", fake.sent.lock().unwrap().len()),
                "runId": "run-1",
                "status": "running",
            })
        }
        "t3_thread_wait" => json!({
            "threadId": thread,
            "runId": "run-1",
            "status": "completed",
            "timedOut": false,
        }),
        "t3_thread_read" => {
            let attachments = *fake.attachments.lock().unwrap();
            let text = match attachments {
                0 => "Hello there.".to_owned(),
                n => format!("Received {n} attachments."),
            };
            json!({
                "thread": { "threadId": thread, "title": "fake thread" },
                "items": [{
                    "position": 0,
                    "itemId": "item-1",
                    "type": "assistant_message",
                    "status": "completed",
                    "title": null,
                    "text": text,
                    "textTruncated": false,
                }],
                "nextPosition": null,
                "hasMore": false,
            })
        }
        "t3_thread_interrupt" => json!({
            "threadId": thread,
            "runId": "run-1",
            "status": "interrupt_requested",
        }),
        "t3_pending_request" => match arguments.get("action").and_then(Value::as_str) {
            Some("list") => json!({ "requestIds": [] }),
            Some("read") => json!({ "requestId": "req-1", "questions": [] }),
            _ => json!({ "sequence": 1 }),
        },
        "t3_attachment_prepare_upload" => {
            let name = arguments
                .pointer("/upload/name")
                .and_then(Value::as_str)
                .unwrap_or("upload");
            json!({
                "attachmentId": format!("att-{name}"),
                "relativeUrl": format!("/api/attachments/upload/att-{name}"),
                "expiresAt": 1_700_000_600_000u64,
            })
        }
        other => {
            return json!({
                "isError": true,
                "content": [{ "type": "text", "text": format!("unknown tool {other}") }],
            });
        }
    };
    json!({
        "structuredContent": structured,
        "content": [{ "type": "text", "text": structured.to_string() }],
    })
}

/// A tool failure the driver reads as an error.
fn failure(text: &str) -> Value {
    json!({
        "isError": true,
        "content": [{ "type": "text", "text": text }],
    })
}

/// One JSON-RPC message answered; `None` for a notification.
pub fn handle(fake: &Fake, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let id = msg.get("id").cloned();
    let id = id.filter(|i| !i.is_null())?;
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "fake-t3", "version": "0.1.10" },
        })),
        "tools/list" => Ok(tools()),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            Ok(call(fake, name, &arguments))
        }
        other => Err(format!("Method not found: {other}")),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": message },
        }),
    })
}

/// Serves MCP on stdin and stdout until stdin closes.
pub fn run_stdio() -> std::io::Result<()> {
    let fake = Fake::default();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(answer) = handle(&fake, &msg) {
            let _ = writeln!(stdout, "{answer}");
            let _ = stdout.flush();
        }
    }
    Ok(())
}

/// Serves MCP over HTTP on `0.0.0.0:port`; prints the address it took, then serves `/mcp` and
/// the attachment upload path.
pub fn run_http(port: u16) -> std::io::Result<()> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let address = listener.local_addr()?;
    println!("http://{address}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let fake = Arc::new(Fake::default());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let fake = fake.clone();
        std::thread::spawn(move || {
            let _ = serve_http(&fake, &mut stream);
        });
    }
    Ok(())
}

fn serve_http(fake: &Fake, stream: &mut std::net::TcpStream) -> std::io::Result<()> {
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default().to_owned();
    if method == "POST" && path.starts_with("/api/attachments/upload/") {
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(head.as_bytes())?;
        return Ok(());
    }
    if method == "DELETE" {
        let head = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
        stream.write_all(head.as_bytes())?;
        return Ok(());
    }
    let msg: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let session = if msg.get("method").and_then(Value::as_str) == Some("initialize") {
        "Mcp-Session-Id: fake-session\r\n"
    } else {
        ""
    };
    match handle(fake, &msg) {
        Some(answer) => {
            let text = answer.to_string();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n",
                text.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(text.as_bytes())?;
        }
        None => {
            let head = format!(
                "HTTP/1.1 202 Accepted\r\n{session}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes())?;
        }
    }
    Ok(())
}
