//! The `link` profile proof (spec 40): two runtimes with their own state and workspace roots
//! pair over the `command` carrier without a code. A session with a git worktree, an open request
//! and a queued prompt moves from the first to the second; its workspace arrives through the
//! link; the first runtime is killed, as when a laptop closes; a client of the second resumes the
//! queue, the session runs its queued prompt there, and the client answers the request it raises.
#![cfg(unix)]
// Waits for the runtime on a Unix socket.
// The helpers are shared in shape with `link_move.rs`; this test uses only some of them.
#![allow(dead_code)]

mod common;

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const WAIT: Duration = Duration::from_secs(20);
fn fake_claude() -> &'static Path {
    common::fake_claude()
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
            .arg(common::with_eggs(dir.path().join("state")))
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Made Up",
            "-c",
            "user.email=made-up@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Polls `method` until `pred` holds for its result.
async fn until(
    c: &mut Client,
    method: &str,
    params: Value,
    what: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let r = c.ok(method, params.clone()).await;
        if pred(&r) {
            return r;
        }
        assert!(
            Instant::now() < deadline,
            "{}: never {what}: {r}\n{}",
            c.name,
            c.dump()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_with_a_worktree_moves_to_a_server_and_runs_on_after_the_laptop_closes() {
    let laptop = Serve::start(Some(&scenario("bash-allowed.jsonl")));
    let server = Serve::start(Some(&scenario("bash-allowed.jsonl")));

    // The laptop pairs through `parrot connect --link` as a command carrier, with no code: the
    // invite runs on the server through the same carrier.
    let relay = server.socket.to_str().unwrap();
    laptop.link(
        &[
            "add",
            "--name",
            "server",
            "--grant",
            "read",
            "--ask-grant",
            "read,prompt,answer,manage,delegate,workspace",
            "--command",
            PARROT,
            "connect",
            "--link",
            "--socket",
            relay,
            ";",
        ],
        None,
    );
    let mut s = Client::connect(&server, "server-client").await;
    let init = s.initialize().await;
    let server_env = init["runtime"]["env"].as_str().unwrap().to_string();
    let server_runtime = init["runtime"]["id"].as_str().unwrap().to_string();
    until(&mut s, "link/list", json!({}), "a connected link", |r| {
        r["links"][0]["status"] == "connected"
    })
    .await;

    // The laptop's session works in a worktree of a repository only the laptop has.
    let repo = laptop.dir.path().join("work").join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "made-up project\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    let mut l = Client::connect(&laptop, "laptop-client").await;
    l.initialize().await;
    let created = l
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "worktree", "repo": repo.to_str().unwrap() } }),
        )
        .await;
    let session = session_id(&created);
    let ws = created["session"]["workspace"].as_str().unwrap().to_string();
    let listed = l.ok("workspace/list", json!({ "session": session })).await;
    let wt = std::path::PathBuf::from(listed["workspaces"][0]["path"].as_str().unwrap());
    // A commit and an uncommitted change, both of which travel.
    std::fs::write(wt.join("notes.txt"), "committed on the laptop\n").unwrap();
    git(&wt, &["add", "notes.txt"]);
    git(&wt, &["commit", "-q", "-m", "notes"]);
    std::fs::write(wt.join("README.md"), "made-up project, edited\n").unwrap();

    // The first prompt raises a request and stays open; the second waits in the queue.
    l.ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    l.ok("session/prompt", json!({ "session": session, "text": "run ls" }))
        .await;
    let i = l
        .wait_update(0, "the request", |u| {
            u["type"] == "request" && u["request"]["status"] == "open"
        })
        .await;
    let first_request = l.update(i)["request"]["id"].as_str().unwrap().to_string();
    // The queue is paused first, so the prompt waits whatever the turn's state is.
    l.ok("queue/pause", json!({ "session": session })).await;
    l.ok(
        "session/prompt",
        json!({ "session": session, "text": "run ls again", "delivery": "queue" }),
    )
    .await;
    let queued = l.ok("queue/list", json!({ "session": session })).await;
    assert_eq!(
        queued["queue"]["entries"].as_array().map(Vec::len),
        Some(1),
        "{queued}"
    );

    // It moves to the server's environment.
    let deadline = Instant::now() + WAIT;
    let moved = loop {
        let resp = l
            .call(
                "session/rebind",
                json!({ "session": session, "env": server_env, "interrupt": true }),
            )
            .await;
        if let Some(r) = resp.get("result") {
            break r.clone();
        }
        // The peer's environment is known once both sides have initialized over the link.
        assert!(Instant::now() < deadline, "rebind failed: {resp}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(moved["runtime"], server_runtime.as_str(), "{moved}");

    // The workspace arrived through the link, under the server's own workspace root.
    let arrived = s.ok("workspace/list", json!({ "session": session })).await;
    let path = std::path::PathBuf::from(arrived["workspaces"][0]["path"].as_str().unwrap_or_default());
    assert_eq!(arrived["workspaces"][0]["id"], ws.as_str(), "{arrived}");
    assert!(
        path.starts_with(server.dir.path().join("state").join("workspaces")),
        "{arrived}"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("notes.txt")).unwrap(),
        "committed on the laptop\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "made-up project, edited\n"
    );
    let got = s.ok("session/get", json!({ "session": session })).await;
    assert!(
        got["session"]["cwd"]
            .as_str()
            .unwrap_or_default()
            .starts_with(path.to_str().unwrap()),
        "{got}"
    );

    // The laptop closes.
    drop(l);
    drop(laptop);

    // The queue arrived as it was: paused, with the prompt in it. A client of the server
    // resumes it.
    let arrived_queue = s.ok("queue/list", json!({ "session": session })).await;
    assert_eq!(arrived_queue["queue"]["paused"], true, "{arrived_queue}");
    assert_eq!(
        arrived_queue["queue"]["entries"][0]["text"], "run ls again",
        "{arrived_queue}"
    );
    s.ok("queue/resume", json!({ "session": session })).await;

    // The queued prompt runs on the server and raises a request there, which a client of the
    // server answers. The first request belonged to the laptop's harness and came along expired.
    let open = until(
        &mut s,
        "request/list",
        json!({ "session": session }),
        "a request on the server",
        |r| {
            r["requests"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|q| q["status"] == "open" && q["id"] != first_request.as_str())
            })
        },
    )
    .await;
    let requests = open["requests"].as_array().unwrap();
    let carried = requests.iter().find(|q| q["id"] == first_request.as_str());
    assert_eq!(
        carried.map(|q| q["status"].clone()),
        Some(json!("expired")),
        "{open}"
    );
    let request = requests.iter().find(|q| q["status"] == "open").unwrap()["id"].clone();
    s.ok(
        "request/answer",
        json!({ "request": request, "answer": { "choice": "allow" } }),
    )
    .await;
    until(
        &mut s,
        "session/get",
        json!({ "session": session }),
        "idle after the queued prompt",
        |r| r["session"]["state"] == "idle",
    )
    .await;
    let queue = s.ok("queue/list", json!({ "session": session })).await;
    assert_eq!(
        queue["queue"]["entries"].as_array().map(Vec::len),
        Some(0),
        "{queue}"
    );
    let read = s
        .ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    assert!(read.to_string().contains("There are two entries."), "{read}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_carrier_egg_carries_a_link() {
    let server = Serve::start(None);
    // The laptop has the example carrier egg installed before it starts.
    let dir = tempfile::tempdir().unwrap();
    let egg = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eggs/relay");
    common::with_eggs(dir.path().join("state"));
    let out = Command::new(PARROT)
        .args(["eggs", "install", egg.to_str().unwrap(), "--state-dir"])
        .arg(dir.path().join("state"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let laptop = Serve::start_in(dir, None);

    let mut l = Client::connect(&laptop, "laptop-client").await;
    l.initialize().await;
    let carriers = l.ok("carrier/list", json!({})).await;
    let relay = carriers["carriers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "relay")
        .unwrap_or_else(|| panic!("no relay carrier: {carriers}"))
        .clone();
    assert_eq!(relay["containment"], "restricted", "{relay}");
    // The egg is a carrier, not an agent.
    let agents = l.ok("agent/list", json!({})).await;
    assert!(!agents.to_string().contains("\"relay\""), "{agents}");

    let invite: Value =
        serde_json::from_str(&server.link(&["invite", "--name", "laptop", "--json"], None)).unwrap();
    let carrier = json!({ "kind": "relay", "target": { "parrot": PARROT, "socket": server.socket } });
    laptop.link(
        &[
            "add",
            "--name",
            "server",
            "--code-stdin",
            "--carrier",
            &carrier.to_string(),
        ],
        Some(invite["code"].as_str().unwrap()),
    );
    let links = until(&mut l, "link/list", json!({}), "a connected link", |r| {
        r["links"][0]["status"] == "connected"
    })
    .await;
    assert_eq!(links["links"][0]["carrier"]["kind"], "relay", "{links}");
    assert_eq!(links["links"][0]["containment"], "restricted", "{links}");
}
