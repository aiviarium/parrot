//! `parrot acp` end to end: an ACP client, the export, a real `parrot serve`, and `fake-claude`.
#![cfg(unix)]

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const WAIT: Duration = Duration::from_secs(20);

fn fake_claude() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--locked",
                "-p",
                "parrot-driver-claude",
                "--bin",
                "fake-claude",
            ])
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building fake-claude failed");
        let path = Path::new(PARROT).with_file_name(format!("fake-claude{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "no fake-claude at {}", path.display());
        path
    })
}

fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parrot-driver-claude/fixtures/scenarios")
        .join(name)
}

/// A running `parrot serve` in a temporary directory, killed on drop.
struct Serve {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Serve {
    fn start(scenario: &Path) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(dir.path().join("state"))
            .env("PARROT_SOCKET", &socket)
            .env("PARROT_CLAUDE_BIN", fake_claude())
            .env("PARROT_FAKE_CLAUDE_SCENARIO", scenario)
            .stdin(Stdio::null())
            .spawn()
            .expect("serve starts");
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "serve never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve { child, socket, dir }
    }

    fn work_dir(&self) -> PathBuf {
        let path = self.dir.path().join("work");
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `parrot acp` as a child process, spoken to over its stdio.
struct Acp {
    child: tokio::process::Child,
    write: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next: u64,
    /// Every notification and request received, in order.
    seen: Vec<Value>,
}

impl Acp {
    async fn start(socket: &Path) -> Self {
        let mut child = tokio::process::Command::new(PARROT)
            // Named, since the first usable agent depends on which eggs are installed.
            .args(["acp", "--agent", "claude", "--socket"])
            .arg(socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("parrot acp starts");
        let write = child.stdin.take().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Acp {
            child,
            write,
            lines,
            next: 1,
            seen: vec![],
        }
    }

    async fn read(&mut self, what: &str) -> Value {
        match tokio::time::timeout(WAIT, self.lines.next_line()).await {
            Ok(Ok(Some(line))) => serde_json::from_str(&line).expect("ACP lines are JSON"),
            Ok(Ok(None)) => panic!("parrot acp closed while waiting for {what}"),
            Ok(Err(e)) => panic!("reading parrot acp: {e}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        loop {
            let msg = self.read(method).await;
            if msg.get("method").is_none() && msg["id"] == id {
                assert!(msg.get("error").is_none(), "{method} failed: {msg}");
                return msg.get("result").cloned().unwrap_or(Value::Null);
            }
            self.seen.push(msg);
        }
    }

    async fn send(&mut self, msg: Value) {
        let mut line = msg.to_string();
        line.push('\n');
        self.write.write_all(line.as_bytes()).await.expect("send");
    }

    /// Waits for a message that `pred` accepts, past the ones already waited for.
    async fn wait(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let mut from = 0;
        loop {
            while from < self.seen.len() {
                if pred(&self.seen[from]) {
                    let found = self.seen[from].clone();
                    self.seen.drain(0..=from);
                    return found;
                }
                from += 1;
            }
            let msg = self.read(what).await;
            self.seen.push(msg);
        }
    }

    async fn open(&mut self, cwd: &Path) -> String {
        let init = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                    "clientInfo": { "name": "test", "version": "0" },
                }),
            )
            .await;
        assert_eq!(init["protocolVersion"], 1);
        let created = self
            .call(
                "session/new",
                json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
            )
            .await;
        created["sessionId"].as_str().expect("a session id").to_owned()
    }
}

impl Drop for Acp {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn is_update(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |m| {
        m.get("method").and_then(Value::as_str) == Some("session/update")
            && m.pointer("/params/update/sessionUpdate").and_then(Value::as_str) == Some(kind)
    }
}

#[tokio::test]
async fn text_updates_then_the_stop_reason() {
    let serve = Serve::start(&scenario("text-turn.jsonl"));
    let work = serve.work_dir();
    let mut acp = Acp::start(&serve.socket).await;
    let session = acp.open(&work).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "session/prompt",
        "params": { "sessionId": session, "prompt": [{ "type": "text", "text": "Say hello." }] },
    }))
    .await;
    let first = acp
        .wait("the first chunk", is_update("agent_message_chunk"))
        .await;
    assert_eq!(first["params"]["update"]["content"]["text"], "Hello ");
    let second = acp.wait("the rest", is_update("agent_message_chunk")).await;
    assert_eq!(second["params"]["update"]["content"]["text"], "there.");
    let response = acp.wait("the prompt's answer", |m| m["id"] == 99).await;
    assert_eq!(response["result"]["stopReason"], "end_turn");
    assert_eq!(response["result"]["usage"]["outputTokens"], 4);
}

#[tokio::test]
async fn a_permission_request_travels_to_the_client_and_back() {
    let serve = Serve::start(&scenario("bash-allowed.jsonl"));
    let work = serve.work_dir();
    let mut acp = Acp::start(&serve.socket).await;
    let session = acp.open(&work).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "session/prompt",
        "params": { "sessionId": session, "prompt": [{ "type": "text", "text": "Run ls." }] },
    }))
    .await;
    let request = acp
        .wait("a permission request", |m| {
            m.get("method").and_then(Value::as_str) == Some("session/request_permission")
        })
        .await;
    let options = request["params"]["options"].as_array().unwrap();
    assert!(options.iter().any(|o| o["kind"] == "allow_once"), "{request}");
    let id = request["id"].clone();
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "outcome": { "outcome": "selected", "optionId": "allow" } },
    }))
    .await;
    let done = acp
        .wait("the tool result", |m| {
            is_update("tool_call_update")(m)
                && m.pointer("/params/update/status").and_then(Value::as_str) == Some("completed")
        })
        .await;
    let content = &done["params"]["update"]["content"][0]["content"]["text"];
    assert_eq!(content, "README.md\nsrc");
    let response = acp.wait("the prompt's answer", |m| m["id"] == 100).await;
    assert_eq!(response["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn cancel_answers_the_prompt_with_cancelled() {
    let serve = Serve::start(&scenario("interrupt.jsonl"));
    let work = serve.work_dir();
    let mut acp = Acp::start(&serve.socket).await;
    let session = acp.open(&work).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 101,
        "method": "session/prompt",
        "params": { "sessionId": session, "prompt": [{ "type": "text", "text": "Count slowly." }] },
    }))
    .await;
    acp.wait("the first chunk", |m| {
        is_update("agent_message_chunk")(m) || is_update("agent_thought_chunk")(m)
    })
    .await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": { "sessionId": session },
    }))
    .await;
    let response = acp.wait("the prompt's answer", |m| m["id"] == 101).await;
    assert_eq!(response["result"]["stopReason"], "cancelled");
}
