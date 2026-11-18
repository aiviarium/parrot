//! Helpers shared by the runtime tests: a `parrot serve` in a temporary directory and a
//! JSON-RPC client over its socket.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

pub const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
pub const WAIT: Duration = Duration::from_secs(30);

/// What a `parrot serve` starts with.
#[derive(Default)]
pub struct Setup {
    /// Written to `config.json` in the state directory.
    pub config: Option<Value>,
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
}

/// A running `parrot serve` in a temporary directory, killed on drop.
pub struct Serve {
    pub child: Child,
    pub socket: PathBuf,
    pub dir: tempfile::TempDir,
}

impl Serve {
    pub fn start(setup: Setup) -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::start_in(dir, setup)
    }

    /// Starts serve with its state in `dir/state` and its socket at `dir/sock`.
    pub fn start_in(dir: tempfile::TempDir, setup: Setup) -> Self {
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        if let Some(config) = &setup.config {
            std::fs::write(state.join("config.json"), config.to_string()).unwrap();
        }
        let socket = dir.path().join("sock");
        let _ = std::fs::remove_file(&socket);
        let mut cmd = Command::new(PARROT);
        cmd.args(["serve", "--state-dir"])
            .arg(&state)
            .args(&setup.args)
            .env("PARROT_SOCKET", &socket)
            .stdin(Stdio::null());
        for (k, v) in &setup.env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("serve starts");
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "serve never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve { child, socket, dir }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    pub fn work_dir(&self) -> PathBuf {
        let path = self.dir.path().join("work");
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Kills serve and hands back its directory, for a restart.
    pub fn kill(mut self) -> tempfile::TempDir {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let dir = tempfile::tempdir().unwrap();
        std::mem::replace(&mut self.dir, dir)
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A JSON-RPC client, one message per line.
pub struct Client {
    pub name: &'static str,
    pub tx: Box<dyn Send + Unpin + FnMut(String) -> futures_lite::BoxFuture>,
    pub rx: mpsc::UnboundedReceiver<Value>,
    pub next_id: u64,
    /// Every message received that was not a response to `call`, in order.
    pub seen: Vec<Value>,
}

/// The boxed send future, without pulling in a futures crate.
pub mod futures_lite {
    pub type BoxFuture = std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;
}

impl Client {
    pub async fn connect(serve: &Serve, name: &'static str) -> Self {
        Self::connect_path(&serve.socket, name).await
    }

    pub async fn connect_path(socket: &Path, name: &'static str) -> Self {
        let stream = UnixStream::connect(socket).await.expect("connects");
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
        let tx = std::sync::Arc::new(tokio::sync::Mutex::new(tx));
        Client {
            name,
            tx: Box::new(move |line: String| {
                let tx = tx.clone();
                Box::pin(async move {
                    let mut tx: tokio::sync::MutexGuard<'_, OwnedWriteHalf> = tx.lock().await;
                    tx.write_all(line.as_bytes()).await.is_ok()
                })
            }),
            rx,
            next_id: 1,
            seen: Vec::new(),
        }
    }

    /// A client over any channel pair: lines to send and messages received.
    pub fn from_channels(
        name: &'static str,
        out: mpsc::UnboundedSender<String>,
        rx: mpsc::UnboundedReceiver<Value>,
    ) -> Self {
        Client {
            name,
            tx: Box::new(move |line: String| {
                let ok = out.send(line).is_ok();
                Box::pin(async move { ok })
            }),
            rx,
            next_id: 1,
            seen: Vec::new(),
        }
    }

    pub fn dump(&self) -> String {
        let lines: Vec<String> = self.seen.iter().map(Value::to_string).collect();
        format!("{} received:\n{}", self.name, lines.join("\n"))
    }

    pub async fn recv(&mut self, deadline: tokio::time::Instant, what: &str) -> Value {
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

    pub async fn send(&mut self, msg: Value) {
        let mut line = msg.to_string();
        line.push('\n');
        assert!((self.tx)(line).await, "{}: send failed", self.name);
    }

    /// Sends a request without waiting; returns its id.
    pub async fn start_call(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        id
    }

    /// Waits for the response to `id`, keeping other messages.
    pub async fn response(&mut self, id: u64, what: &str) -> Value {
        if let Some(i) = self
            .seen
            .iter()
            .position(|m| m.get("method").is_none() && m["id"] == id)
        {
            return self.seen.remove(i);
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let msg = self.recv(deadline, what).await;
            if msg.get("method").is_none() && msg["id"] == id {
                return msg;
            }
            self.seen.push(msg);
        }
    }

    /// Sends a request and returns the whole response.
    pub async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.start_call(method, params).await;
        self.response(id, method).await
    }

    /// Like [`Client::call`], but fails on an error response and returns the result.
    pub async fn ok(&mut self, method: &str, params: Value) -> Value {
        let resp = self.call(method, params).await;
        match resp.get("result") {
            Some(result) => result.clone(),
            None => panic!("{}: {method} failed: {resp}\n{}", self.name, self.dump()),
        }
    }

    pub async fn initialize(&mut self) -> Value {
        self.ok("initialize", initialize_params(self.name)).await
    }

    /// Waits for a message `pred` accepts, at or after index `from` of [`Client::seen`].
    pub async fn wait_msg(&mut self, from: usize, what: &str, pred: impl Fn(&Value) -> bool) -> usize {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut i = from;
        loop {
            while i < self.seen.len() {
                if pred(&self.seen[i]) {
                    return i;
                }
                i += 1;
            }
            let msg = self.recv(deadline, what).await;
            self.seen.push(msg);
        }
    }

    /// Waits for an `update` whose `params.update` matches; returns its index in `seen`.
    pub async fn wait_update(&mut self, from: usize, what: &str, pred: impl Fn(&Value) -> bool) -> usize {
        self.wait_msg(from, what, |m| {
            m["method"] == "update" && pred(&m["params"]["update"])
        })
        .await
    }

    pub fn update(&self, i: usize) -> &Value {
        &self.seen[i]["params"]["update"]
    }

    /// Answers a request the runtime sent to this client.
    pub async fn answer(&mut self, request: &Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }))
            .await;
    }
}

pub fn initialize_params(name: &str) -> Value {
    json!({
        "protocol": [1],
        "peer": { "kind": "client", "name": name, "version": "0" },
        "features": {},
    })
}
