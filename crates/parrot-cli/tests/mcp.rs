//! `parrot mcp` end to end: a harness on stdio, a fake runtime on a local socket.
#![cfg(unix)]
// Starts a Unix socket listener.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const SESSION: &str = "ses_01FAKESESSION0000000000000";

/// Serves one connection: answers what `parrot mcp` asks and reports each request.
fn fake_runtime(listener: UnixListener, seen: mpsc::Sender<Value>) {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut out = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let msg: Value = serde_json::from_str(&line).unwrap();
            let result = match msg["method"].as_str().unwrap() {
                "initialize" => json!({
                    "protocol": 1,
                    "peer": { "kind": "runtime", "name": "fake", "version": "0" },
                    "runtime": { "id": "rt_FAKE", "env": "env_FAKE" },
                    "features": {},
                    "connection": { "id": "con_FAKE", "principal": SESSION, "scopes": ["read"] },
                }),
                "session/get" => json!({ "session": { "id": SESSION, "workspace": "wsp_FAKE" } }),
                "function/list" => json!({ "functions": [] }),
                "env/list" => json!({ "envs": [{ "id": "env_FAKE", "name": "made-up-host" }] }),
                _ => json!({}),
            };
            seen.send(msg.clone()).unwrap();
            let reply = json!({ "jsonrpc": "2.0", "id": msg["id"], "result": result });
            writeln!(out, "{reply}").unwrap();
        }
    });
}

#[test]
fn serves_tools_for_a_binding() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    let token = dir.path().join("token");
    std::fs::write(&token, "made-up-binding-secret\n").unwrap();
    let (tx, rx) = mpsc::channel();
    fake_runtime(UnixListener::bind(&socket).unwrap(), tx);

    let mut mcp = Command::new(PARROT)
        .args(["mcp", "--binding", "bnd_FAKE", "--socket"])
        .arg(&socket)
        .arg("--token-file")
        .arg(&token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("parrot mcp starts");
    let mut stdin = mcp.stdin.take().unwrap();
    let mut lines = BufReader::new(mcp.stdout.take().unwrap()).lines();
    let mut ask = |id: u64, method: &str, params: Value| -> Value {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        loop {
            let reply: Value = serde_json::from_str(&lines.next().expect("a reply").unwrap()).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    };

    let init = ask(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    let tools = ask(2, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"parrot__envs__list"));
    assert!(
        !names.contains(&"parrot__sessions__spawn"),
        "only read is granted"
    );
    let call = ask(
        3,
        "tools/call",
        json!({ "name": "parrot__envs__list", "arguments": {} }),
    );
    assert_eq!(call["result"]["structuredContent"]["envs"][0]["id"], "env_FAKE");
    drop(stdin);
    assert!(mcp.wait().unwrap().success());

    let first: Value = rx.recv().unwrap();
    assert_eq!(first["method"], "initialize");
    assert_eq!(
        first["params"]["binding"],
        json!({ "id": "bnd_FAKE", "token": "made-up-binding-secret" })
    );
}

#[test]
fn listens_on_loopback_http_with_the_binding_credential() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    let (tx, _rx) = mpsc::channel();
    fake_runtime(UnixListener::bind(&socket).unwrap(), tx);

    let mut mcp = Command::new(PARROT)
        .args([
            "mcp",
            "--binding",
            "bnd_FAKE",
            "--listen",
            "127.0.0.1:0",
            "--socket",
        ])
        .arg(&socket)
        .env("PARROT_MCP_TOKEN", "made-up-binding-secret")
        .stdout(Stdio::piped())
        .spawn()
        .expect("parrot mcp starts");
    let mut out = BufReader::new(mcp.stdout.take().unwrap());
    let mut url = String::new();
    out.read_line(&mut url).unwrap();
    let addr = url
        .trim()
        .strip_prefix("http://")
        .and_then(|u| u.strip_suffix("/mcp"))
        .unwrap_or_else(|| panic!("a URL, not {url:?}"))
        .to_string();

    let post = |token: &str, body: &str| -> String {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut answer = String::new();
        s.read_to_string(&mut answer).unwrap_or_default();
        answer
    };
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    assert!(post("not-it", list).starts_with("HTTP/1.1 401"));
    let answer = post("made-up-binding-secret", list);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains("parrot__envs__list"), "{answer}");
    mcp.kill().unwrap();
    let _ = mcp.wait();
}
