//! End to end: a real `parrot serve`, the real pi driver over a fake `pi --mode rpc`, and a
//! client on the local socket.
#![cfg(unix)]

mod common;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const WAIT: Duration = Duration::from_secs(10);

fn fake_pi() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| common::workspace_bin("fake-pi"))
}

/// A running `parrot serve` in a temporary directory, killed on drop.
struct Serve {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Serve {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(common::with_eggs(dir.path().join("state")))
            .env("PARROT_SOCKET", &socket)
            .env("PARROT_PI_BIN", fake_pi())
            .env("FAKE_PI_HOME", dir.path().join("pi-sessions"))
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

/// A JSON-RPC client over the socket, one message per line.
struct Client {
    tx: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    seen: Vec<Value>,
}

impl Client {
    async fn connect(serve: &Serve) -> Self {
        let stream = UnixStream::connect(&serve.socket).await.expect("connects");
        let (read, tx) = stream.into_split();
        let (msgs, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(msg) = serde_json::from_str::<Value>(&line)
                    && msgs.send(msg).is_err()
                {
                    break;
                }
            }
        });
        let mut client = Client {
            tx,
            rx,
            next_id: 1,
            seen: Vec::new(),
        };
        client
            .ok(
                "initialize",
                json!({"protocol": [1], "peer": {"kind": "client", "name": "pi-test", "version": "0"}, "features": {}}),
            )
            .await;
        client
    }

    fn dump(&self) -> String {
        self.seen
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn recv(&mut self, deadline: tokio::time::Instant, what: &str) -> Value {
        match tokio::time::timeout_at(deadline, self.rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => panic!("connection closed waiting for {what}\n{}", self.dump()),
            Err(_) => panic!("timed out waiting for {what}\n{}", self.dump()),
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        self.tx.write_all(line.as_bytes()).await.expect("send");
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let msg = self.recv(deadline, method).await;
            if msg.get("method").is_none() && msg["id"] == id {
                return msg;
            }
            self.seen.push(msg);
        }
    }

    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let resp = self.call(method, params).await;
        match resp.get("result") {
            Some(result) => result.clone(),
            None => panic!("{method} failed: {resp}\n{}", self.dump()),
        }
    }

    /// Waits for an `update` notification at or after `from` that `pred` accepts.
    async fn wait_update(&mut self, from: usize, what: &str, pred: impl Fn(&Value) -> bool) -> usize {
        let deadline = tokio::time::Instant::now() + WAIT;
        let is_match = |msg: &Value| msg["method"] == "update" && pred(&msg["params"]["update"]);
        let mut i = from;
        loop {
            while i < self.seen.len() {
                if is_match(&self.seen[i]) {
                    return i;
                }
                i += 1;
            }
            let msg = self.recv(deadline, what).await;
            self.seen.push(msg);
        }
    }

    fn update(&self, i: usize) -> &Value {
        &self.seen[i]["params"]["update"]
    }
}

fn session_id(result: &Value) -> String {
    let session = &result["session"];
    session
        .as_str()
        .or_else(|| session["id"].as_str())
        .unwrap_or_else(|| panic!("no session id in {result}"))
        .to_string()
}

#[tokio::test]
async fn a_pi_turn_streams_through_the_runtime() {
    let serve = Serve::start();
    let work = serve.work_dir();
    let work_str = work.to_str().unwrap();

    let mut a = Client::connect(&serve).await;
    let created = a
        .ok(
            "session/new",
            json!({
                "agent": "pi",
                "workspace": { "kind": "dir", "path": work_str },
                "cwd": work_str,
            }),
        )
        .await;
    let session = session_id(&created);
    let prompted = a
        .ok(
            "session/prompt",
            json!({ "session": session, "text": "[[text:Hello there.]]" }),
        )
        .await;
    assert!(prompted["message"].is_string(), "{prompted}");

    let reply = a
        .wait_update(0, "the assistant text", |u| {
            u["type"] == "item"
                && u["item"]["kind"] == "assistant"
                && u["item"]["set"]["text"] == "Hello there."
        })
        .await;
    assert_eq!(a.update(reply)["item"]["streaming"], false);
    let turn = a
        .wait_update(reply, "the turn completing", |u| {
            u["type"] == "turn" && u["turn"]["status"] == "completed"
        })
        .await;
    let options = a.update(turn)["turn"]["id"].clone();
    assert!(options.is_string());
    a.wait_update(turn, "the idle state", |u| {
        u["type"] == "state" && u["state"] == "idle"
    })
    .await;
}

#[tokio::test]
async fn a_pi_extension_dialog_is_answered_through_the_runtime() {
    let serve = Serve::start();
    let work = serve.work_dir();
    let work_str = work.to_str().unwrap();

    let mut a = Client::connect(&serve).await;
    let created = a
        .ok(
            "session/new",
            json!({
                "agent": "pi",
                "workspace": { "kind": "dir", "path": work_str },
                "cwd": work_str,
            }),
        )
        .await;
    let session = session_id(&created);
    a.ok(
        "session/prompt",
        json!({ "session": session, "text": "[[confirm:Run the migration?]]" }),
    )
    .await;
    let open = a
        .wait_update(0, "the approval request", |u| {
            u["type"] == "request" && u["request"]["kind"] == "approval" && u["request"]["status"] == "open"
        })
        .await;
    let request = a.update(open)["request"].clone();
    assert_eq!(request["title"], "Run the migration?");
    a.ok(
        "request/answer",
        json!({ "request": request["id"], "answer": { "choice": "allow" } }),
    )
    .await;
    let said = a
        .wait_update(open, "the confirmed reply", |u| {
            u["type"] == "item" && u["item"]["set"]["text"] == "Confirmed."
        })
        .await;
    let turn = a
        .wait_update(said, "the turn completing", |u| {
            u["type"] == "turn" && u["turn"]["status"] == "completed"
        })
        .await;
    a.wait_update(turn, "the idle state", |u| {
        u["type"] == "state" && u["state"] == "idle"
    })
    .await;
}
