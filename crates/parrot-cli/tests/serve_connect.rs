//! `parrot serve` and `parrot connect` end to end over the local socket.
#![cfg(unix)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");

fn serve(state: &Path, socket: &Path) -> Child {
    let child = Command::new(PARROT)
        .args(["serve", "--state-dir"])
        .arg(state)
        .env("PARROT_SOCKET", socket)
        .stdin(Stdio::null())
        .spawn()
        .expect("serve starts");
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}

/// Sends `requests` through `parrot connect` and returns the answers by id.
fn exchange(socket: &Path, requests: &[Value]) -> Vec<Value> {
    let mut connect = Command::new(PARROT)
        .arg("connect")
        .env("PARROT_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("connect starts");
    let mut stdin = connect.stdin.take().unwrap();
    let mut lines = BufReader::new(connect.stdout.take().unwrap()).lines();
    let mut answers = Vec::new();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        loop {
            let line = lines.next().expect("an answer").unwrap();
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == request["id"] {
                answers.push(msg);
                break;
            }
        }
    }
    drop(stdin);
    let status = connect.wait().unwrap();
    assert!(status.success(), "connect exited with {status}");
    answers
}

#[test]
fn unknown_method_through_connect_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let socket = dir.path().join("run/sock");
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocol": [1], "peer": { "kind": "client", "name": "test", "version": "0" }, "features": {} },
    });
    let unknown = json!({ "jsonrpc": "2.0", "id": 2, "method": "nothing/here", "params": {} });

    let mut first = serve(&state, &socket);
    let answers = exchange(&socket, &[init.clone(), unknown.clone()]);
    assert_eq!(answers[1]["error"]["code"], -32601, "{}", answers[1]);
    assert!(state.join("state.db").exists());

    // A killed runtime leaves a stale socket; the next one replaces it.
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_err());
    let mut second = serve(&state, &socket);
    let answers = exchange(&socket, &[init, unknown]);
    assert_eq!(answers[1]["error"]["code"], -32601);

    // A terminated runtime removes its socket.
    terminate(&second);
    second.wait().unwrap();
    assert!(!socket.exists());
}

fn terminate(child: &Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn connect_link_needs_a_link_endpoint() {
    let out = Command::new(PARROT)
        .args(["connect", "--link", "--socket", "/nonexistent/parrot/sock"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("/nonexistent/parrot/link.sock"), "{err}");
}

#[test]
fn conform_is_not_implemented() {
    let out = Command::new(PARROT).arg("conform").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn guard_still_runs_commands() {
    let out = Command::new(PARROT)
        .args(["guard", "--", "sh", "-c", "exit 4"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
}
