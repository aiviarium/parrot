//! A fake T3 MCP server for tests: the orchestration tools over stdio, without T3.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn tool(name: &str, description: &str, properties: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties },
    })
}

fn tools() -> Value {
    json!({ "tools": [
        tool("t3_thread_launch", "Create a top-level thread", json!({ "title": {"type": "string"}, "message": {"type": "string"} })),
        tool("t3_thread_send", "Send to a thread", json!({ "threadId": {"type": "string"}, "message": {"type": "string"}, "mode": {"type": "string"} })),
        tool("t3_thread_read", "Read a thread", json!({ "threadId": {"type": "string"} })),
        tool("t3_thread_wait", "Wait for a thread's run", json!({ "threadId": {"type": "string"}, "timeoutMs": {"type": "number"} })),
        tool("t3_thread_interrupt", "Interrupt a thread", json!({ "threadId": {"type": "string"} })),
        tool("t3_pending_request", "List, read or answer a pending request", json!({ "action": {"type": "string"} })),
    ]})
}

/// One thread's run: sending starts it, and waiting says it is done.
fn call(name: &str, arguments: &Value) -> Value {
    let thread = arguments
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("thread-1");
    let structured = match name {
        "t3_thread_launch" => json!({
            "threadId": "thread-1",
            "projectId": "project-1",
            "modelSelection": { "model": "fake-model" },
            "runId": null,
            "status": null,
        }),
        "t3_thread_send" => json!({
            "threadId": thread,
            "messageId": "msg-1",
            "runId": "run-1",
            "status": "running",
            "delivery": "started",
        }),
        "t3_thread_wait" => json!({
            "threadId": thread,
            "runId": "run-1",
            "status": "completed",
            "timedOut": false,
        }),
        "t3_thread_read" => json!({
            "thread": { "threadId": thread, "title": "fake thread" },
            "items": [{
                "position": 0,
                "itemId": "item-1",
                "type": "assistant_message",
                "status": "completed",
                "title": null,
                "text": "Hello there.",
            }],
            "nextPosition": null,
            "hasMore": false,
        }),
        "t3_thread_interrupt" => json!({ "threadId": thread, "interrupted": true }),
        "t3_pending_request" => match arguments.get("action").and_then(Value::as_str) {
            Some("list") => json!({ "requestIds": [] }),
            Some("read") => json!({ "requestId": "req-1", "questions": [] }),
            _ => json!({ "sequence": 1 }),
        },
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

fn main() {
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
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let id = msg.get("id").cloned();
        if id.is_none() {
            continue;
        }
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake-t3", "version": "0.0.0" },
            })),
            "tools/list" => Ok(tools()),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                Ok(call(name, &arguments))
            }
            other => Err(format!("Method not found: {other}")),
        };
        let response = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": message },
            }),
        };
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}
