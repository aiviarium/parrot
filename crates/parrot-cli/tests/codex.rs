//! End to end: a real `parrot serve`, the real Codex driver over a fake `codex app-server`, and a
//! client on the local socket.
#![cfg(unix)]

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

/// Builds `fake-codex` once and returns its path, beside the `parrot` binary.
fn fake_codex() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--locked",
                "-p",
                "parrot-driver-codex",
                "--bin",
                "fake-codex",
            ])
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building fake-codex failed");
        let path = Path::new(PARROT).with_file_name(format!("fake-codex{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "no fake-codex at {}", path.display());
        path
    })
}

fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parrot-driver-codex/fixtures/scenarios")
        .join(name)
}

/// A running `parrot serve` in a temporary directory, killed on drop.
struct Serve {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Serve {
    /// Starts serve; with `scenario`, the `codex` agent runs the fake app-server on it.
    fn start(scenario: Option<&Path>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        let mut cmd = Command::new(PARROT);
        cmd.args(["serve", "--state-dir"])
            .arg(dir.path().join("state"))
            .env("PARROT_SOCKET", &socket)
            .stdin(Stdio::null());
        if let Some(scenario) = scenario {
            cmd.env("PARROT_CODEX_BIN", fake_codex())
                .env("PARROT_FAKE_CODEX_SCENARIO", scenario);
        }
        let child = cmd.spawn().expect("serve starts");
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

    fn update(&self, i: usize) -> &Value {
        &self.seen[i]["params"]["update"]
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

#[tokio::test]
async fn a_codex_approval_is_answered_through_the_runtime() {
    let serve = Serve::start(Some(&scenario("command-approval.jsonl")));
    let work = serve.work_dir();
    let work_str = work.to_str().unwrap();

    let mut a = Client::connect(&serve, "client-a").await;
    a.initialize().await;
    let created = a
        .ok(
            "session/new",
            json!({
                "agent": "codex",
                "workspace": { "kind": "dir", "path": work_str },
                "cwd": work_str,
            }),
        )
        .await;
    let session = session_id(&created);
    let prompted = a
        .ok(
            "session/prompt",
            json!({ "session": session, "text": "write live.txt" }),
        )
        .await;
    assert!(prompted["message"].is_string(), "{prompted}");

    let open = a
        .wait_update(0, "approval request", |u| {
            u["type"] == "request" && u["request"]["kind"] == "approval" && u["request"]["status"] == "open"
        })
        .await;
    let request = a.update(open)["request"].clone();
    assert_eq!(request["subject"]["type"], "command", "{request}");
    assert_eq!(
        request["subject"]["command"], "printf 'live' > live.txt",
        "{request}"
    );
    a.ok(
        "request/answer",
        json!({ "request": request["id"], "answer": { "choice": "accept" } }),
    )
    .await;
    let done = a
        .wait_update(open, "the command finishing", |u| {
            u["type"] == "item" && u["item"]["set"]["status"] == "ok"
        })
        .await;
    let turn = a
        .wait_update(done, "the turn completing", |u| {
            u["type"] == "turn" && u["turn"]["status"] == "completed"
        })
        .await;
    a.wait_update(turn, "the idle state", |u| {
        u["type"] == "state" && u["state"] == "idle"
    })
    .await;
}
