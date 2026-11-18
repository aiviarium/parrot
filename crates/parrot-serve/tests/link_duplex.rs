//! Two runtimes in one process, linked over an in-memory duplex: they pair with a code, and a
//! workspace moves from one to the other through `workspace/transfer` as `blob/put` chunks.
#![cfg(unix)]

use parrot_core::Core;
use parrot_core::clock::{ManualClock, SeqIds};
use parrot_link::StaticKey;
use parrot_link::carrier::{CarrierRef, Channel, Containment};
use parrot_link::pairing::InviteCode;
use parrot_serve::link_hub::LinkHub;
use parrot_serve::{Runtime, RuntimeBackend, socket};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

struct Side {
    hub: Arc<LinkHub>,
    _rt: Runtime,
    socket: PathBuf,
    root: PathBuf,
}

fn side(dir: &Path, name: &str, key: u8) -> Side {
    let hub = LinkHub::new(StaticKey::from_private([key; 32]));
    let clock = ManualClock::default();
    let mut core = Core::new(Box::new(clock.clone()), Box::new(SeqIds::default()));
    core.state.principals.runtime = hub.runtime_id();
    core.state.principals.env = hub.runtime_id().replacen("rt_", "env_", 1);
    let root = dir.join(name).join("workspaces");
    let (backend_hub, backend_root) = (hub.clone(), root.clone());
    let (rt, _) = Runtime::spawn(core, Box::new(clock), move |rt| {
        RuntimeBackend::new(rt, parrot_store::Store::memory().unwrap())
            .with_links(backend_hub)
            .with_workspace_root(backend_root)
    });
    hub.attach(rt.clone());
    let socket = dir.join(name).join("sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = socket::bind_socket(&socket).unwrap();
    tokio::spawn(socket::serve_socket(rt.clone(), listener));
    Side {
        hub,
        _rt: rt,
        socket,
        root,
    }
}

struct Client {
    lines: Lines<BufReader<OwnedReadHalf>>,
    tx: OwnedWriteHalf,
    id: u64,
}

impl Client {
    async fn connect(side: &Side) -> Client {
        let (r, tx) = UnixStream::connect(&side.socket).await.unwrap().into_split();
        let mut c = Client {
            lines: BufReader::new(r).lines(),
            tx,
            id: 0,
        };
        c.ok(
            "initialize",
            json!({ "protocol": [1], "peer": { "kind": "client", "name": "test", "version": "0" }, "features": {} }),
        )
        .await;
        c
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params });
        self.tx.write_all(format!("{msg}\n").as_bytes()).await.unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(20), self.lines.next_line())
                .await
                .expect("an answer in time")
                .unwrap()
                .expect("the connection stays open");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == self.id && v.get("method").is_none() {
                return v;
            }
        }
    }

    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params).await;
        r.get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{method} failed: {r}"))
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args([
            "-c",
            "user.name=Made Up",
            "-c",
            "user.email=made-up@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_runtimes_pair_over_a_duplex_and_transfer_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let laptop = side(dir.path(), "laptop", 1);
    let server = side(dir.path(), "server", 2);

    // The server invites; the laptop pairs over one end of a duplex, the server accepts the other.
    let mut s = Client::connect(&server).await;
    let invite = s
        .ok(
            "link/invite",
            json!({ "name": "laptop", "grant": ["read", "workspace"] }),
        )
        .await;
    let code = InviteCode::parse(invite["code"].as_str().unwrap()).unwrap();
    let (near, far) = tokio::io::duplex(64 * 1024);
    tokio::spawn(server.hub.clone().accept(far));
    let channel = Channel {
        stream: Box::new(near),
        kind: "duplex".into(),
        containment: Containment::Restricted,
        max_message: 65_535,
        stderr: None,
    };
    // A carrier that can't be dialled again, so nothing reconnects behind the test's back.
    let carrier: CarrierRef = serde_json::from_value(json!({ "kind": "none", "target": {} })).unwrap();
    laptop
        .hub
        .clone()
        .pair_on(
            &json!({ "name": "server", "grant": ["read"] }),
            carrier,
            channel,
            &code,
        )
        .await
        .unwrap();

    let mut l = Client::connect(&laptop).await;
    let server_env = s.ok("initialize", json!({ "protocol": [1], "peer": { "kind": "client", "name": "again", "version": "0" }, "features": {} })).await["runtime"]["env"]
        .as_str()
        .unwrap()
        .to_string();

    // A workspace with a commit and an uncommitted change on the laptop.
    let repo = dir.path().join("laptop").join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(repo.join("a.txt"), "committed\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "one"]);
    std::fs::write(repo.join("a.txt"), "changed\n").unwrap();
    let env = l
        .ok(
            "initialize",
            json!({ "protocol": [1], "peer": { "kind": "client", "name": "x", "version": "0" }, "features": {} }),
        )
        .await["runtime"]["env"]
        .clone();
    let created = l
        .ok(
            "workspace/create",
            json!({ "env": env, "request": { "kind": "dir", "path": repo } }),
        )
        .await;
    let workspace = created["workspace"]["id"].as_str().unwrap().to_string();

    // The laptop learns the server's environment once both sides have initialized.
    let deadline = Instant::now() + Duration::from_secs(20);
    let moved = loop {
        let r = l
            .call(
                "workspace/transfer",
                json!({ "workspace": workspace, "to": server_env }),
            )
            .await;
        if let Some(result) = r.get("result") {
            break result.clone();
        }
        assert!(Instant::now() < deadline, "transfer failed: {r}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let path = PathBuf::from(moved["workspace"]["path"].as_str().unwrap());
    assert_eq!(path, server.root.join(&workspace), "{moved}");
    // Git on Windows may check the file out with CRLF line endings, as its settings say.
    let text = std::fs::read_to_string(path.join("a.txt")).unwrap();
    assert_eq!(text.replace("\r\n", "\n"), "changed\n");
    assert_eq!(moved["workspace"]["branch"], "main", "{moved}");
    let listed = s.ok("workspace/list", json!({})).await;
    assert!(listed.to_string().contains(&workspace), "{listed}");

    // A second transfer of the same workspace is refused rather than mixed into the first.
    let again = l
        .call(
            "workspace/transfer",
            json!({ "workspace": workspace, "to": server_env }),
        )
        .await;
    assert_eq!(again["error"]["code"], -32004, "{again}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_without_the_workspace_scope_cannot_send_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let laptop = side(dir.path(), "laptop", 3);
    let server = side(dir.path(), "server", 4);
    let mut s = Client::connect(&server).await;
    let invite = s
        .ok("link/invite", json!({ "name": "laptop", "grant": ["read"] }))
        .await;
    let code = InviteCode::parse(invite["code"].as_str().unwrap()).unwrap();
    let (near, far) = tokio::io::duplex(64 * 1024);
    tokio::spawn(server.hub.clone().accept(far));
    let channel = Channel {
        stream: Box::new(near),
        kind: "duplex".into(),
        containment: Containment::Restricted,
        max_message: 65_535,
        stderr: None,
    };
    let carrier: CarrierRef = serde_json::from_value(json!({ "kind": "none", "target": {} })).unwrap();
    laptop
        .hub
        .clone()
        .pair_on(&json!({ "name": "server" }), carrier, channel, &code)
        .await
        .unwrap();
    let mut l = Client::connect(&laptop).await;
    let init =
        json!({ "protocol": [1], "peer": { "kind": "client", "name": "x", "version": "0" }, "features": {} });
    let server_env = s.ok("initialize", init.clone()).await["runtime"]["env"]
        .as_str()
        .unwrap()
        .to_string();
    let env = l.ok("initialize", init).await["runtime"]["env"].clone();
    let repo = dir.path().join("laptop").join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "one"]);
    let created = l
        .ok(
            "workspace/create",
            json!({ "env": env, "request": { "kind": "dir", "path": repo } }),
        )
        .await;
    let workspace = created["workspace"]["id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let r = l
            .call(
                "workspace/transfer",
                json!({ "workspace": workspace, "to": server_env }),
            )
            .await;
        // Not found until the peer's environment is known, then refused by the peer's grant.
        if r["error"]["code"] == -32003 {
            break;
        }
        assert!(r.get("result").is_none(), "{r}");
        assert!(Instant::now() < deadline, "never refused: {r}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!server.root.join(&workspace).exists());
}
