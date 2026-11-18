//! The `link` profile proof: two runtimes pair over the `command` carrier, a session moves from
//! the first to the second, and it keeps running after the first runtime stops.
#![cfg(unix)]
// The helpers are shared in shape with `e2e.rs`; this test uses only some of them.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const WAIT: Duration = Duration::from_secs(20);
/// Builds `fake-claude` once and returns its path, beside the `parrot` binary.
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
    /// Starts serve; with `scenario`, the `claude` agent runs the fake CLI on it.
    fn start(scenario: Option<&Path>) -> Self {
        Self::start_in(tempfile::tempdir().unwrap(), scenario)
    }

    /// Starts serve on the state of `dir`, as after a restart.
    fn start_in(dir: tempfile::TempDir, scenario: Option<&Path>) -> Self {
        let socket = dir.path().join("sock");
        let mut cmd = Command::new(PARROT);
        cmd.args(["serve", "--state-dir"])
            .arg(dir.path().join("state"))
            .env("PARROT_SOCKET", &socket)
            .stdin(Stdio::null());
        if let Some(scenario) = scenario {
            cmd.env("PARROT_CLAUDE_BIN", fake_claude())
                .env("PARROT_FAKE_CLAUDE_SCENARIO", scenario);
        }
        let child = cmd.spawn().expect("serve starts");
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "serve never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve { child, socket, dir }
    }

    /// Kills serve and starts it again on the same state.
    fn restart(&mut self, scenario: Option<&Path>) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let dir = std::mem::replace(&mut self.dir, tempfile::tempdir().unwrap());
        *self = Self::start_in(dir, scenario);
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

    #[allow(dead_code)]
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

impl Serve {
    /// Runs `parrot link` against this runtime and returns its standard output.
    fn link(&self, args: &[&str], stdin: Option<&str>) -> String {
        let mut child = Command::new(PARROT)
            .arg("link")
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("parrot link runs");
        let mut input = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            writeln!(input, "{text}").unwrap();
        }
        drop(input);
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "parrot link {args:?} failed\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_moves_over_a_link_and_outlives_its_first_runtime() {
    let mut laptop = Serve::start(Some(&scenario("text-turn.jsonl")));
    let server = Serve::start(Some(&scenario("text-turn.jsonl")));

    // The server invites; the laptop pairs through `parrot connect --link` as a command carrier.
    let invite: Value = serde_json::from_str(&server.link(
        &[
            "invite",
            "--name",
            "laptop",
            "--grant",
            "read,prompt,manage,delegate",
            "--json",
        ],
        None,
    ))
    .expect("invite prints JSON");
    let code = invite["code"].as_str().expect("a code");
    let relay = server.socket.to_str().unwrap();
    laptop.link(
        &[
            "add",
            "--name",
            "server",
            "--grant",
            "read",
            "--code-stdin",
            "--command",
            PARROT,
            "connect",
            "--link",
            "--socket",
            relay,
            ";",
        ],
        Some(code),
    );

    let mut s = Client::connect(&server, "server-client").await;
    let init = s.initialize().await;
    let server_env = init["runtime"]["env"].as_str().unwrap().to_string();
    let server_runtime = init["runtime"]["id"].as_str().unwrap().to_string();
    assert!(server_runtime.starts_with("rt_"), "{init}");
    let deadline = Instant::now() + WAIT;
    loop {
        let links = s.ok("link/list", json!({})).await;
        if links["links"][0]["status"] == "connected" {
            break;
        }
        assert!(Instant::now() < deadline, "the link never connected: {links}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A session runs one turn on the laptop.
    // Both runtimes reach this directory; moving the workspace itself is `workspace/transfer`.
    let shared = tempfile::tempdir().unwrap();
    let work = shared.path().to_path_buf();
    let work_str = work.to_str().unwrap();
    let mut l = Client::connect(&laptop, "laptop-client").await;
    l.initialize().await;
    let created = l
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work_str }, "cwd": work_str }),
        )
        .await;
    let session = session_id(&created);
    l.ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    l.ok("session/prompt", json!({ "session": session, "text": "hello" }))
        .await;
    l.wait_update(0, "the first turn completing", |u| {
        u["type"] == "turn" && u["turn"]["status"] == "completed"
    })
    .await;

    // It moves to the server's environment, and the laptop keeps a redirect.
    let deadline = Instant::now() + WAIT;
    let moved = loop {
        let resp = l
            .call("session/rebind", json!({ "session": session, "env": server_env }))
            .await;
        if let Some(r) = resp.get("result") {
            break r.clone();
        }
        // The peer's environment is known once both sides have initialized over the link.
        assert!(Instant::now() < deadline, "rebind failed: {resp}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(moved["runtime"], server_runtime.as_str(), "{moved}");
    let redirect = l.call("session/get", json!({ "session": session })).await;
    assert_eq!(redirect["error"]["code"], -32050, "{redirect}");
    assert_eq!(redirect["error"]["data"]["runtime"], server_runtime.as_str());

    // The redirect outlives a restart of the laptop.
    drop(l);
    laptop.restart(Some(&scenario("text-turn.jsonl")));
    let mut l = Client::connect(&laptop, "laptop-again").await;
    l.initialize().await;
    let redirect = l.call("session/get", json!({ "session": session })).await;
    assert_eq!(redirect["error"]["code"], -32050, "{redirect}");

    // The laptop closes; the session runs on at the server.
    drop(l);
    drop(laptop);
    let got = s.ok("session/get", json!({ "session": session })).await;
    assert_eq!(got["session"]["runtime"], server_runtime.as_str(), "{got}");
    let from = s.seen.len();
    s.ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    s.ok(
        "session/prompt",
        json!({ "session": session, "text": "still there?" }),
    )
    .await;
    s.wait_update(from, "a turn on the server", |u| {
        u["type"] == "turn" && u["turn"]["status"] == "completed"
    })
    .await;
}
