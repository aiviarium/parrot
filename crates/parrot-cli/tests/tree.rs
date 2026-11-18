//! The `tree` profile end to end: a real `parrot serve` and the real Claude driver over a fake
//! `claude`. A parent delegates to two children in worktrees, waits for their tasks and merges
//! their branches, calling as the parent session's principal over the protocol.
#![cfg(unix)]

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
const WAIT: Duration = Duration::from_secs(20);

fn fake_claude() -> &'static Path {
    common::fake_claude()
}

struct Serve {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Serve {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        let scenario = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../parrot-driver-claude/fixtures/scenarios/text-turn.jsonl");
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(common::with_eggs(dir.path().join("state")))
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
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    tx: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
}

impl Client {
    async fn connect(serve: &Serve, act_as: Option<&str>) -> Self {
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
        let mut c = Client { tx, rx, next_id: 1 };
        let mut init = json!({ "protocol": [1], "peer": { "kind": "client", "name": "test", "version": "0" }, "features": {} });
        if let Some(s) = act_as {
            init["_meta"] = json!({ "x.parrot/actAs": s });
        }
        c.ok("initialize", init).await;
        c
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        self.tx.write_all(line.as_bytes()).await.expect("send");
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let msg = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {method}"))
                .expect("connection open");
            if msg.get("method").is_none() && msg["id"] == id {
                return msg;
            }
        }
    }

    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let resp = self.call(method, params).await;
        resp.get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{method} failed: {resp}"))
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
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn a_parent_delegates_to_two_children_in_worktrees_waits_and_merges() {
    let serve = Serve::start();
    let repo = serve.dir.path().join("work").join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Test Person"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(repo.join("README.md"), "demo\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    let repo_str = repo.to_str().unwrap();

    let mut owner = Client::connect(&serve, None).await;
    let parent = owner
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": repo_str }, "start": false,
                "grant": ["read", "prompt", "delegate", "workspace"], "title": "Orchestrator" }),
        )
        .await;
    let parent_id = parent["session"]["id"].as_str().unwrap().to_string();
    let parent_ws = parent["session"]["workspace"].as_str().unwrap().to_string();

    // The parent's agent calls as the parent session's principal.
    let mut agent = Client::connect(&serve, Some(&parent_id)).await;
    let mut children = Vec::new();
    for name in ["alpha", "beta"] {
        let r = agent
            .ok(
                "session/new",
                json!({ "agent": "claude", "parent": parent_id, "title": name,
                    "workspace": { "kind": "worktree", "repo": repo_str },
                    "task": { "brief": format!("Write {name}.txt"), "completeOn": "idle" } }),
            )
            .await;
        assert_eq!(r["session"]["kind"], "child", "{r}");
        children.push((
            r["session"]["id"].as_str().unwrap().to_string(),
            r["session"]["workspace"].as_str().unwrap().to_string(),
            name,
        ));
    }
    let ids: Vec<&str> = children.iter().map(|(s, _, _)| s.as_str()).collect();

    // Wait until both tasks are done, with the opaque cursor.
    let first = agent.ok("task/wait", json!({ "sessions": ids })).await;
    let mut cursor = first["cursor"].clone();
    let mut done: Vec<String> = first["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["task"]["status"] == "done")
        .map(|c| c["session"].as_str().unwrap().to_string())
        .collect();
    let deadline = Instant::now() + WAIT;
    while done.len() < 2 {
        if Instant::now() > deadline {
            let a = owner.ok("session/get", json!({ "session": ids[0] })).await;
            let items = owner
                .ok("subscribe", json!({ "target": { "session": ids[0] } }))
                .await;
            panic!("the tasks never finished: {a}\n{items}");
        }
        let r = agent
            .ok(
                "task/wait",
                json!({ "sessions": ids, "since": cursor, "timeout": 5 }),
            )
            .await;
        for c in r["changes"].as_array().unwrap() {
            if c["task"]["status"] == "done" && !done.contains(&c["session"].as_str().unwrap().to_string()) {
                done.push(c["session"].as_str().unwrap().to_string());
            }
        }
        cursor = r["cursor"].clone();
    }

    // The children's work, committed in their worktrees, merges into the parent's workspace.
    for (_, ws, name) in &children {
        let status = agent.ok("workspace/status", json!({ "workspace": ws })).await;
        assert!(
            status["branch"].as_str().unwrap().starts_with("parrot/ses_"),
            "{status}"
        );
        let list = owner.ok("workspace/list", json!({})).await;
        let path = list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == ws.as_str())
            .unwrap()["path"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::write(Path::new(&path).join(format!("{name}.txt")), format!("{name}\n")).unwrap();
        git(Path::new(&path), &["add", "."]);
        git(Path::new(&path), &["commit", "-q", "-m", &format!("add {name}")]);
        let merged = agent
            .ok(
                "workspace/merge",
                json!({ "from": ws, "into": parent_ws, "method": "merge" }),
            )
            .await;
        assert!(merged["commit"].is_string(), "{merged}");
    }
    assert!(repo.join("alpha.txt").exists() && repo.join("beta.txt").exists());

    // A child's workspace is not the parent agent's to merge into.
    let (_, child_ws, _) = &children[0];
    let refused = agent
        .call(
            "workspace/merge",
            json!({ "from": parent_ws, "into": child_ws, "method": "merge" }),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32003, "{refused}");
    let tree = owner.ok("session/tree", json!({ "session": parent_id })).await;
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 3, "{tree}");
}
