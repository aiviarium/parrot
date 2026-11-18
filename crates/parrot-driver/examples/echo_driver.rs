//! A tiny process driver for tests. It answers `initialize`, `binding/start` and `binding/stop`,
//! sends one `update` per started binding, and exits without replying to `test/exit`.
//! With `ECHO_DRIVER_HANG` set it ignores the end of its input.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut send = |v: Value| {
        writeln!(out, "{v}").unwrap();
        out.flush().unwrap();
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = msg["id"].clone();
        let params = &msg["params"];
        match msg["method"].as_str().unwrap_or("") {
            "initialize" => send(json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "peer": { "kind": "driver", "name": "echo" } },
            })),
            "binding/start" => {
                let binding = params["binding"].clone();
                send(json!({
                    "jsonrpc": "2.0", "method": "update",
                    "params": { "binding": binding, "update": { "type": "state", "state": "idle", "background": [] } },
                }));
                send(json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "isolation": "none", "level": "read", "network": false, "options": [], "actions": [] },
                }));
            }
            "binding/stop" => {
                eprintln!("stopped {}", params["binding"]);
                send(json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
            }
            "test/exit" => {
                eprintln!("echo driver giving up");
                std::process::exit(3);
            }
            _ => send(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": "method not found" },
            })),
        }
    }
    if std::env::var_os("ECHO_DRIVER_HANG").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
