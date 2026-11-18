//! Functions through `parrot serve`: a client provides one, another calls it with a key.
#![cfg(unix)]
// The helpers are shared in shape with `e2e.rs`; this test uses only some of them.
#![allow(dead_code)]

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
        let dir = tempfile::tempdir().unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn a_client_function_is_listed_and_called_once_per_key() {
    let serve = Serve::start(None);
    let mut provider = Client::connect(&serve, "provider").await;
    let mut init = initialize_params("provider");
    init["provides"] = json!([{
        "name": "acme.echo",
        "description": "Echoes its input.",
        "input": { "type": "object" },
        "output": { "type": "object" },
        "effects": ["read"],
        "level": "read",
    }]);
    provider.ok("initialize", init).await;

    let mut caller = Client::connect(&serve, "caller").await;
    caller.initialize().await;
    let listed = caller.ok("function/search", json!({ "query": "echo" })).await;
    assert_eq!(listed["functions"][0]["name"], "acme.echo", "{listed}");
    assert_eq!(listed["functions"][0]["provider"], provider_id(&serve).await);

    // A second provider of the same name is refused.
    let mut other = Client::connect(&serve, "other").await;
    other.initialize().await;
    let refused = other
        .call(
            "function/provide",
            json!({ "functions": [listed["functions"][0].clone()] }),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32004, "{refused}");

    let call = |key: &str| json!({ "function": "acme.echo", "input": { "say": "hi" }, "key": key });
    let id = caller.next_id;
    caller.next_id += 1;
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": "function/call", "params": call("k1") });
    caller.tx.write_all(format!("{line}\n").as_bytes()).await.unwrap();

    // The provider gets the call and answers it.
    let deadline = tokio::time::Instant::now() + WAIT;
    let asked = loop {
        let msg = provider.recv(deadline, "function/call").await;
        if msg["method"] == "function/call" {
            break msg;
        }
    };
    assert_eq!(asked["params"]["input"]["say"], "hi", "{asked}");
    let answer = json!({ "jsonrpc": "2.0", "id": asked["id"], "result": { "output": { "said": "hi" } } });
    provider
        .tx
        .write_all(format!("{answer}\n").as_bytes())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    let result = loop {
        let msg = caller.recv(deadline, "the call's result").await;
        if msg["id"] == id {
            break msg;
        }
    };
    assert_eq!(result["result"]["output"]["said"], "hi", "{result}");

    // The same key returns the first result without asking the provider again.
    let again = caller.ok("function/call", call("k1")).await;
    assert_eq!(again["output"]["said"], "hi", "{again}");
}

async fn provider_id(serve: &Serve) -> String {
    let mut c = Client::connect(serve, "lister").await;
    c.initialize().await;
    let list = c.ok("function/list", json!({})).await;
    list["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "acme.echo")
        .and_then(|f| f["provider"].as_str())
        .unwrap()
        .to_string()
}
