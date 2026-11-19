//! An example third-party Parrot driver. It speaks the driver protocol (spec 30) on stdio and is
//! its own harness: every prompt is answered with `echo: <text>`, prefixed by the first line of
//! `prefix.txt` from the egg's folder when there is one. See `docs/writing-drivers.md`.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn main() {
    let mut out = std::io::stdout().lock();
    let mut turn = 0u64;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications have no id and need no answer.
        let (Some(id), Some(method)) = (msg.get("id").cloned(), msg["method"].as_str()) else {
            continue;
        };
        let p = &msg["params"];
        let mut updates = Vec::new();
        let result = match method {
            "initialize" => json!({
                "protocol": 1,
                "peer": { "kind": "driver", "name": "parrot-driver-echo", "version": env!("CARGO_PKG_VERSION") },
                "features": {},
            }),
            "agent/list" => json!({ "agents": [agent()] }),
            "binding/start" => {
                updates.push(state(p, "idle"));
                json!({ "isolation": "none", "level": "read", "network": false, "options": [], "actions": [] })
            }
            "binding/prompt" => {
                turn += 1;
                let key = format!("t{turn}");
                let at = "2026-01-01T00:00:00.000Z";
                let text = format!("{}echo: {}", prefix(), p["text"].as_str().unwrap_or_default());
                updates.push(state(p, "running"));
                updates.push(update(p, json!({ "type": "turn", "turn": {
                    "id": key, "status": "running", "continuable": false, "input": p["message"], "started": at } })));
                updates.push(update(p, json!({ "type": "item", "item": {
                    "id": format!("a{turn}"), "kind": "assistant", "turn": key, "set": { "text": text } } })));
                updates.push(update(p, json!({ "type": "turn", "turn": {
                    "id": key, "status": "completed", "continuable": false, "input": p["message"],
                    "started": at, "ended": at } })));
                updates.push(state(p, "idle"));
                json!({ "disposition": "started" })
            }
            "binding/cancel" => json!({ "interrupted": false }),
            "binding/stop" | "binding/answer" => json!({}),
            "binding/options" => json!({ "options": [], "level": "read", "network": false }),
            other => {
                send(&mut out, json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {other}") } }));
                continue;
            }
        };
        send(&mut out, json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        for u in updates {
            send(&mut out, u);
        }
    }
}

/// The agent as `agent/list` reports it. Its id and name come from the runtime, which takes them
/// from the egg, so the same binary can serve differently named eggs.
fn agent() -> Value {
    let var = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.into());
    json!({
        "id": var("PARROT_DRIVER_AGENT", "echo"),
        "name": var("PARROT_DRIVER_AGENT_NAME", "Echo"),
        "driver": { "name": "echo", "version": env!("CARGO_PKG_VERSION"), "kind": "process", "data": "1" },
        "installed": true,
        "auth": "signed_in",
        "usable": true,
        "controls": ["native"],
        "features": {},
        "levels": ["read"],
        "isolation": ["none"],
        "options": [],
        "actions": [],
    })
}

/// The first line of `prefix.txt` in the egg's `parrot` folder, followed by a space.
fn prefix() -> String {
    std::env::var_os("PARROT_DRIVER_DATA")
        .and_then(|d| std::fs::read_to_string(std::path::Path::new(&d).join("prefix.txt")).ok())
        .and_then(|t| t.lines().next().map(|l| format!("{l} ")))
        .unwrap_or_default()
}

fn state(p: &Value, state: &str) -> Value {
    update(p, json!({ "type": "state", "state": state, "background": [] }))
}

fn update(p: &Value, update: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "update", "params": { "binding": p["binding"], "update": update } })
}

fn send(out: &mut impl Write, msg: Value) {
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
