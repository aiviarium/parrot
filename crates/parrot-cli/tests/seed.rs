//! The `seed` profile end to end (spec 9.4, 23, 35): a real `parrot serve` with the real Claude
//! and Codex drivers over their fakes. A Claude session forks natively, hands off to Codex with a
//! bundle that carries a summary, the history and the workspace, and rewinds with files.
#![cfg(unix)]

mod common;

use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
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

fn fakes() -> &'static (PathBuf, PathBuf) {
    static PATHS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    PATHS.get_or_init(|| {
        (
            common::workspace_bin("fake-claude"),
            common::workspace_bin("fake-codex"),
        )
    })
}

/// The Claude scenario: two plain text turns, the second answering with `second`.
fn claude_scenario(dir: &Path, second: &str) -> PathBuf {
    let one = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../parrot-driver-claude/fixtures/scenarios/text-turn.jsonl"),
    )
    .unwrap();
    let two = one
        .replace("msg_fake_01", "msg_fake_02")
        .replace("\"uuid\":\"e0", "\"uuid\":\"f0")
        .replace("\"a01\"", "\"a02\"")
        .replace("\"r01\"", "\"r02\"")
        .replace("\"Hello \"", "\"\"")
        .replace("\"there.\"", &serde_json::to_string(second).unwrap())
        .replace("Hello there.", second);
    let path = dir.join("claude.jsonl");
    std::fs::write(&path, format!("{one}\n{two}")).unwrap();
    path
}

/// The Codex scenario: one turn that answers nothing.
fn codex_scenario(dir: &Path) -> PathBuf {
    let thread = "thr-fake-0001";
    let turn = "019a0000-0000-7000-8000-00000000b001";
    let lines = [
        json!({ "expect": { "method": "turn/start" } }),
        json!({ "emit": { "id": "{{id}}", "result": { "turn": { "id": turn, "items": [], "status": "inProgress", "error": null } } } }),
        json!({ "emit": { "method": "turn/started", "params": { "threadId": thread, "turn": { "id": turn, "items": [], "status": "inProgress" } } } }),
        json!({ "emit": { "method": "turn/completed", "params": { "threadId": thread, "turn": { "id": turn, "items": [], "status": "completed", "error": null } } } }),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    let path = dir.join("codex.jsonl");
    std::fs::write(&path, text.join("\n")).unwrap();
    path
}

struct Serve {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Serve {
    fn start(second: &str) -> Self {
        let (claude, codex) = fakes();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        // The Codex fake behind a script that keeps what the driver sends it.
        let wrapper = dir.path().join("codex-logged");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ntee -a '{}' | exec '{}' \"$@\"\n",
                dir.path().join("codex-in.jsonl").display(),
                codex.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(common::with_eggs(dir.path().join("state")))
            .env("PARROT_SOCKET", &socket)
            .env("PARROT_CLAUDE_BIN", claude)
            .env("PARROT_FAKE_CLAUDE_SCENARIO", claude_scenario(dir.path(), second))
            .env(
                "PARROT_FAKE_CLAUDE_ARGS_LOG",
                dir.path().join("claude-args.jsonl"),
            )
            .env("PARROT_CODEX_BIN", &wrapper)
            .env("PARROT_FAKE_CODEX_SCENARIO", codex_scenario(dir.path()))
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

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }

    /// A git repository with one commit.
    fn repo(&self) -> PathBuf {
        let repo = self.dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "demo\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        repo
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test Person",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
                if msgs.send(serde_json::from_str::<Value>(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut c = Client {
            tx,
            rx,
            next_id: 1,
            seen: Vec::new(),
        };
        c.ok(
            "initialize",
            json!({ "protocol": [1], "peer": { "kind": "client", "name": "seed-test", "version": "0" }, "features": {} }),
        )
        .await;
        c
    }

    fn dump(&self) -> String {
        let lines: Vec<String> = self.seen.iter().map(Value::to_string).collect();
        lines.join("\n")
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
        let mut line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
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
            Some(r) => r.clone(),
            None => panic!("{method} failed: {resp}\n{}", self.dump()),
        }
    }

    /// Waits for an update of `session` at or after `from` that matches; returns it.
    async fn wait_update(
        &mut self,
        from: usize,
        session: &str,
        what: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        let hit = |m: &Value| {
            m["method"] == "update" && m["params"]["session"] == session && pred(&m["params"]["update"])
        };
        let mut i = from;
        loop {
            while i < self.seen.len() {
                if hit(&self.seen[i]) {
                    return self.seen[i]["params"]["update"].clone();
                }
                i += 1;
            }
            let msg = self.recv(deadline, what).await;
            self.seen.push(msg);
        }
    }

    /// Prompts and waits for the turn to complete; returns the turn's id.
    async fn turn(&mut self, session: &str, text: &str) -> String {
        let from = self.seen.len();
        self.ok("session/prompt", json!({ "session": session, "text": text }))
            .await;
        let u = self
            .wait_update(from, session, "a completed turn", |u| {
                u["type"] == "turn" && u["turn"]["status"] == "completed"
            })
            .await;
        u["turn"]["id"].as_str().unwrap().to_string()
    }

    /// Polls `session/get` until `pred` holds.
    async fn until(&mut self, session: &str, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let s = self.ok("session/get", json!({ "session": session })).await["session"].clone();
            if pred(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {s}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn id_of(result: &Value) -> String {
    result["session"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_claude_session_forks_natively() {
    let serve = Serve::start("Second answer.");
    let mut c = Client::connect(&serve).await;
    let work = serve.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.to_str().unwrap();
    let src = id_of(
        &c.ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work } }),
        )
        .await,
    );
    c.turn(&src, "hello").await;
    let fork = id_of(
        &c.ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work },
                "seed": { "session": src, "require": "exact" } }),
        )
        .await,
    );
    let s = c
        .until(&fork, "the fork to run", |s| {
            s["binding"]["status"] == "running" && s["state"] == "idle"
        })
        .await;
    assert_eq!(s["binding"]["via"], "fork", "{s}");
    assert_eq!(s["seed"]["via"], "fork", "{s}");
    let starts: Vec<Vec<String>> = serve
        .read("claude-args.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let forked = starts.last().unwrap();
    let at = forked
        .iter()
        .position(|a| a == "--resume")
        .expect("the fork resumes");
    assert_eq!(forked[at + 1], "sess-fake-0001", "{forked:?}");
    assert!(forked.iter().any(|a| a == "--fork-session"), "{forked:?}");
    // The fork takes prompts of its own.
    c.turn(&fork, "go on").await;
}

#[tokio::test]
async fn a_claude_session_hands_off_to_codex_with_a_bundle() {
    let serve = Serve::start("The greeting is done; the parser is next.");
    let mut c = Client::connect(&serve).await;
    let repo = serve.repo();
    let src = id_of(
        &c.ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "worktree", "repo": repo.to_str().unwrap() } }),
        )
        .await,
    );
    let src_dir = PathBuf::from(
        c.until(&src, "the source to run", |s| s["state"] == "idle").await["cwd"]
            .as_str()
            .unwrap(),
    );
    c.turn(&src, "say hello").await;
    // The source's work, committed on its branch.
    std::fs::write(src_dir.join("parser.txt"), "half a parser\n").unwrap();
    git(&src_dir, &["add", "parser.txt"]);
    git(&src_dir, &["commit", "-q", "-m", "parser"]);

    // No workspace: the seed carries it as a worktree on the source's commit.
    let target = id_of(
        &c.ok(
            "session/new",
            json!({ "agent": "codex", "seed": { "session": src, "summary": "source", "brief": "Finish the parser." } }),
        )
        .await,
    );
    let t = c
        .until(&target, "the target's first turn", |s| {
            s["state"] == "idle" && s["binding"]["status"] == "running"
        })
        .await;
    assert_eq!(t["binding"]["via"], "bundle", "{t}");
    let dir = PathBuf::from(t["cwd"].as_str().unwrap());
    assert_ne!(dir, src_dir);
    assert_eq!(
        std::fs::read_to_string(dir.join("parser.txt")).unwrap(),
        "half a parser\n"
    );
    // The brief went to Codex with the bundle: the source's own summary, then its history.
    let deadline = Instant::now() + WAIT;
    let sent = loop {
        let log = serve.read("codex-in.jsonl");
        if let Some(start) = log
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|m| m["method"] == "turn/start")
        {
            break start["params"]["input"][0]["text"].as_str().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "Codex got no turn: {log}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(sent.starts_with("# Continuing an earlier session"), "{sent}");
    assert!(
        sent.contains("## Summary\nThe greeting is done; the parser is next."),
        "{sent}"
    );
    assert!(
        sent.contains("say hello") && sent.contains("Hello there."),
        "{sent}"
    );
    assert!(
        !sent.contains("Summarize this session"),
        "the summary turn is not history: {sent}"
    );
    assert!(sent.trim_end().ends_with("Finish the parser."), "{sent}");
}

#[tokio::test]
async fn session_rebind_hands_a_claude_session_to_codex_and_keeps_its_id() {
    let serve = Serve::start("Second answer.");
    let mut c = Client::connect(&serve).await;
    let work = serve.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let s = id_of(
        &c.ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work.to_str().unwrap() } }),
        )
        .await,
    );
    let first = c
        .until(&s, "the Claude binding to run", |s| s["state"] == "idle")
        .await;
    let old = first["binding"]["id"].as_str().unwrap().to_string();
    c.turn(&s, "say hello").await;

    let from = c.seen.len();
    c.ok("session/rebind", json!({ "session": s, "agent": "codex" }))
        .await;
    let handoff = c
        .wait_update(from, &s, "the handoff item", |u| {
            u["type"] == "item" && u["item"]["kind"] == "handoff"
        })
        .await;
    assert_eq!(handoff["item"]["set"]["status"], "ok", "{handoff}");
    assert_eq!(handoff["item"]["set"]["from"], old.as_str(), "{handoff}");
    let after = c
        .until(&s, "the Codex binding to run", |s| {
            s["binding"]["agent"] == "codex" && s["binding"]["status"] == "running"
        })
        .await;
    assert_eq!(after["id"], s.as_str(), "the session keeps its id");
    assert_eq!(after["binding"]["via"], "bundle", "{after}");
    assert_eq!(after["binding"]["pending"], "bundle", "{after}");
    assert_eq!(after["bindings"].as_array().unwrap().len(), 2, "{after}");
    let old_binding = c.until(&s, "the Claude binding to stop", |_| true).await;
    assert_ne!(old_binding["binding"]["id"], old.as_str());

    // The next prompt reaches Codex behind the bundle of the Claude history.
    c.turn(&s, "Carry on.").await;
    let deadline = Instant::now() + WAIT;
    let sent = loop {
        let log = serve.read("codex-in.jsonl");
        if let Some(start) = log
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|m| m["method"] == "turn/start")
        {
            break start["params"]["input"][0]["text"].as_str().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "Codex got no turn: {log}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(sent.starts_with("# Continuing an earlier session"), "{sent}");
    assert!(
        sent.contains("say hello") && sent.contains("Hello there."),
        "{sent}"
    );
    assert!(sent.trim_end().ends_with("Carry on."), "{sent}");
    let done = c
        .until(&s, "the pending bundle to be used", |s| s["state"] == "idle")
        .await;
    assert!(done["binding"].get("pending").is_none(), "{done}");
}

#[tokio::test]
async fn a_rewind_restores_files_from_a_checkpoint() {
    let serve = Serve::start("Second answer.");
    let mut c = Client::connect(&serve).await;
    let repo = serve.repo();
    let s = id_of(
        &c.ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "worktree", "repo": repo.to_str().unwrap() } }),
        )
        .await,
    );
    let dir = PathBuf::from(
        c.until(&s, "the session to run", |s| s["state"] == "idle").await["cwd"]
            .as_str()
            .unwrap(),
    );
    let first = c.turn(&s, "first").await;
    // What the first turn left: an edit and a new file.
    std::fs::write(dir.join("README.md"), "after the first turn\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "kept\n").unwrap();
    c.turn(&s, "second").await;
    // What the second turn did: more edits and files the checkpoint never saw.
    std::fs::write(dir.join("README.md"), "after the second turn\n").unwrap();
    std::fs::create_dir_all(dir.join("gen")).unwrap();
    std::fs::write(dir.join("gen/out.txt"), "new\n").unwrap();
    std::fs::remove_file(dir.join("notes.txt")).unwrap();

    c.ok(
        "session/rewind",
        json!({ "session": s, "turn": first, "files": true }),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(dir.join("README.md")).unwrap(),
        "after the first turn\n"
    );
    assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "kept\n");
    assert!(
        !dir.join("gen").exists(),
        "files made after the checkpoint are gone"
    );
    // The real index is untouched by the checkpoint and the restore.
    assert_eq!(git(&dir, &["diff", "--cached", "--name-only"]), "");
}
