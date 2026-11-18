//! A tiny out-of-process driver for tests: it is its own fake CLI. Each prompt is answered with
//! the first line of `greeting.txt` from `PARROT_DRIVER_DATA`, so a test can see that the
//! runtime passed the egg's folder. `TINY_CLI_VERSION` sets the harness version it reports.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut turn = 0u64;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(method)) = (msg.get("id").cloned(), msg["method"].as_str()) else {
            continue;
        };
        let p = &msg["params"];
        let mut notes = Vec::new();
        let result = match method {
            "initialize" => json!({
                "protocol": 1,
                "peer": { "kind": "driver", "name": "tiny-driver", "version": "0.1.0" },
                "features": {},
            }),
            "agent/list" => json!({ "agents": [agent()] }),
            "binding/start" => {
                notes.push(update(
                    p,
                    json!({ "type": "state", "state": "idle", "background": [] }),
                ));
                json!({ "isolation": "none", "level": "read", "network": false, "options": [], "actions": [] })
            }
            "binding/prompt" => {
                turn += 1;
                let key = format!("t{turn}");
                let now = "2026-01-01T00:00:00.000Z";
                notes.push(update(
                    p,
                    json!({ "type": "state", "state": "running", "background": [] }),
                ));
                notes.push(update(p, json!({ "type": "turn", "turn": {
                    "id": key, "status": "running", "continuable": false, "input": p["message"], "started": now } })));
                notes.push(update(
                    p,
                    json!({ "type": "frame", "source": "tiny", "data": { "prompt": p["text"] } }),
                ));
                notes.push(update(
                    p,
                    json!({ "type": "item", "item": {
                    "id": format!("a{turn}"), "kind": "assistant", "turn": key,
                    "set": { "text": greeting() } } }),
                ));
                notes.push(update(
                    p,
                    json!({ "type": "turn", "turn": {
                    "id": key, "status": "completed", "continuable": false, "input": p["message"],
                    "started": now, "ended": now } }),
                ));
                notes.push(update(
                    p,
                    json!({ "type": "state", "state": "idle", "background": [] }),
                ));
                json!({ "disposition": "started" })
            }
            "binding/cancel" => json!({ "interrupted": false }),
            "binding/stop" | "binding/answer" => json!({}),
            other => {
                send(
                    &mut out,
                    json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {other}") } }),
                );
                continue;
            }
        };
        send(&mut out, json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        for n in notes {
            send(&mut out, n);
        }
    }
}

fn agent() -> Value {
    let version = std::env::var("TINY_CLI_VERSION").unwrap_or_else(|_| "1.2.0".into());
    json!({
        "id": "tiny",
        "name": "Tiny",
        "driver": { "name": "tiny", "version": "0.1.0", "kind": "process", "data": "1" },
        "harness": { "version": version, "verified": true },
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

fn greeting() -> String {
    std::env::var_os("PARROT_DRIVER_DATA")
        .and_then(|d| std::fs::read_to_string(std::path::Path::new(&d).join("greeting.txt")).ok())
        .and_then(|t| t.lines().next().map(str::to_string))
        .unwrap_or_else(|| "no PARROT_DRIVER_DATA".into())
}

fn update(p: &Value, update: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "update", "params": { "binding": p["binding"], "update": update } })
}

fn send(out: &mut impl Write, msg: Value) {
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
