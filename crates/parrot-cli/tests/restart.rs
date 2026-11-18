//! A restart end to end (spec 34): a real `parrot serve` with the Claude driver over a fake
//! `claude` is killed mid-session and started again on the same state directory.
#![cfg(unix)]
// Waits for the runtime on a Unix socket.

mod common;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const WAIT: Duration = Duration::from_secs(10);

fn fake_claude() -> &'static Path {
    common::fake_claude()
}

fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parrot-driver-claude/fixtures/scenarios")
        .join(name)
}

/// A running `parrot serve` on a state directory that outlives it.
struct Serve {
    child: Child,
    socket: PathBuf,
}

impl Serve {
    /// Starts serve in `dir`; the `claude` agent runs the fake CLI on `scenario`.
    fn start(dir: &Path, scenario: &Path) -> Self {
        let socket = dir.join("sock");
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(common::with_eggs(dir.join("state")))
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
        Serve { child, socket }
    }

    /// Kills serve without letting it shut down, as a crash would.
    fn crash(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
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
    name: &'static str,
    tx: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    /// Every notification received, in order.
    seen: Vec<Value>,
}

impl Client {
    async fn connect(serve: &Serve, name: &'static str) -> Self {
        let stream = UnixStream::connect(&serve.socket).await.expect("connects");
        let (read, tx) = stream.into_split();
        let (msgs, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<Value>(&line) {
                    Ok(msg) => {
                        if msgs.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(e) => panic!("bad line from runtime: {e}: {line}"),
                }
            }
        });
        Client {
            name,
            tx,
            rx,
            next_id: 1,
            seen: Vec::new(),
        }
    }

    fn dump(&self) -> String {
        let lines: Vec<String> = self.seen.iter().map(Value::to_string).collect();
        format!("{} received:\n{}", self.name, lines.join("\n"))
    }

    async fn recv(&mut self, deadline: tokio::time::Instant, what: &str) -> Value {
        match tokio::time::timeout_at(deadline, self.rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => panic!(
                "{}: connection closed waiting for {what}\n{}",
                self.name,
                self.dump()
            ),
            Err(_) => panic!("{}: timed out waiting for {what}\n{}", self.name, self.dump()),
        }
    }

    /// Sends a request and returns the whole response, keeping notifications that arrive first.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = msg.to_string();
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

    /// Like [`Client::call`], but fails on an error response and returns the result.
    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let resp = self.call(method, params).await;
        match resp.get("result") {
            Some(result) => result.clone(),
            None => panic!("{}: {method} failed: {resp}\n{}", self.name, self.dump()),
        }
    }

    async fn initialize(&mut self) -> Value {
        self.ok("initialize", initialize_params(self.name)).await
    }

    /// Waits for an `update` notification at or after index `from` in [`Client::seen`] whose
    /// `params.update` matches, and returns its index.
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
}

fn initialize_params(name: &str) -> Value {
    json!({
        "protocol": [1],
        "peer": { "kind": "client", "name": name, "version": "0" },
        "features": {},
    })
}

fn session_id(result: &Value) -> String {
    let session = &result["session"];
    session
        .as_str()
        .or_else(|| session["id"].as_str())
        .unwrap_or_else(|| panic!("no session id in {result}"))
        .to_string()
}

/// The `seq` of the newest update a client has seen.
fn last_seq(client: &Client) -> u64 {
    client
        .seen
        .iter()
        .filter_map(|m| m["params"]["seq"].as_str().and_then(|s| s.parse().ok()))
        .max()
        .unwrap_or(0)
}

#[tokio::test]
async fn a_killed_runtime_comes_back_with_nothing_running() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work_str = work.to_str().unwrap();

    let serve = Serve::start(dir.path(), &scenario("bash-allowed.jsonl"));
    let mut a = Client::connect(&serve, "client-a").await;
    a.initialize().await;
    let created = a
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work_str }, "cwd": work_str, "title": "Restart demo" }),
        )
        .await;
    let session = session_id(&created);
    a.ok("session/prompt", json!({ "session": session, "text": "run ls" }))
        .await;
    a.wait_update(0, "approval request", |u| {
        u["type"] == "request" && u["request"]["status"] == "open"
    })
    .await;
    let queued = a
        .ok(
            "session/prompt",
            json!({ "session": session, "text": "after the restart", "delivery": "queue" }),
        )
        .await;
    assert_eq!(queued["disposition"], "queued", "{queued}");
    let seq_before = last_seq(&a);
    drop(a);
    serve.crash();

    let serve = Serve::start(dir.path(), &scenario("text-turn.jsonl"));
    let mut b = Client::connect(&serve, "client-b").await;
    b.initialize().await;

    let got = b.ok("session/get", json!({ "session": session })).await;
    assert_eq!(got["session"]["state"], "stopped", "{got}");
    assert_eq!(got["session"]["binding"]["status"], "stopped", "{got}");
    assert_eq!(got["session"]["title"], "Restart demo", "{got}");

    let sub = b
        .ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    let head: u64 = sub["head"].as_str().unwrap().parse().unwrap();
    assert!(
        head > seq_before,
        "the sequence continues: {head} after {seq_before}"
    );
    let snap = &sub["snapshot"]["sessions"][0];
    assert_eq!(snap["queue"]["paused"], true, "{snap}");
    assert_eq!(snap["queue"]["entries"][0]["text"], "after the restart", "{snap}");
    assert_eq!(
        snap["requests"],
        json!([]),
        "the connection request expired: {snap}"
    );
    let items = snap["items"].as_array().unwrap();
    assert!(
        items.iter().any(|i| i["kind"] == "user" && i["text"] == "run ls"),
        "{snap}"
    );
    assert!(items.iter().any(|i| i["kind"] == "tool"), "{snap}");
    let requests = b.ok("request/list", json!({ "session": session })).await;
    assert_eq!(requests["requests"][0]["status"], "expired", "{requests}");

    // Nothing starts by itself: a while later the session is still stopped.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = b.ok("session/get", json!({ "session": session })).await;
    assert_eq!(got["session"]["state"], "stopped", "{got}");

    // Models and options come from the driver.
    let refreshed = b.ok("agent/refresh", json!({ "agent": "claude" })).await;
    let options = refreshed["agent"]["options"].as_array().unwrap();
    assert!(options.iter().any(|o| o["id"] == "model"), "{refreshed}");

    // `session/start` resumes: the binding starts and the held queue sends its entry.
    let started = b.ok("session/start", json!({ "session": session })).await;
    assert_eq!(started["binding"]["via"], "resume", "{started}");
    let from = b.seen.len();
    b.wait_update(from, "the queued prompt's turn", |u| {
        u["type"] == "turn" && u["turn"]["status"] == "completed"
    })
    .await;
    let queue = b.ok("queue/list", json!({ "session": session })).await;
    assert_eq!(queue["queue"]["entries"], json!([]), "{queue}");
}
