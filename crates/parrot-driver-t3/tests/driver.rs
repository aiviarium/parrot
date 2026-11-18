//! The T3 driver end to end: JSON-RPC in, `fake-t3` as the MCP server, updates out.

use std::time::Duration;

use parrot_driver_t3::driver::{Config, serve};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

struct Client {
    write: WriteHalf<DuplexStream>,
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next: u64,
    updates: Vec<Value>,
    seen: usize,
}

impl Client {
    fn start() -> Client {
        Client::with(Config {
            env: vec![],
            guard: false,
            command: Some(vec![env!("CARGO_BIN_EXE_fake-t3").into()]),
            url: None,
            token: None,
        })
    }

    fn with(config: Config) -> Client {
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(theirs);
        tokio::spawn(serve(config, r, w));
        let (r, write) = tokio::io::split(ours);
        Client {
            write,
            lines: BufReader::new(r).lines(),
            next: 1,
            updates: vec![],
            seen: 0,
        }
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(15), self.lines.next_line())
            .await
            .expect("the driver answers in time")
            .unwrap()
            .expect("the driver is still running");
        serde_json::from_str(&line).unwrap()
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
        self.write.write_all(line.as_bytes()).await.unwrap();
        loop {
            let msg = self.read().await;
            if msg["method"] == "update" {
                self.updates.push(msg["params"].clone());
            } else if msg["id"] == id {
                assert!(msg["error"].is_null(), "{method} failed: {msg}");
                return msg["result"].clone();
            }
        }
    }

    async fn wait(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            if let Some(i) = self.updates[self.seen..].iter().position(&pred) {
                self.seen += i + 1;
                return self.updates[self.seen - 1].clone();
            }
            let msg = self.read().await;
            if msg["method"] == "update" {
                self.updates.push(msg["params"].clone());
            }
        }
    }

    async fn bind(&mut self) -> Value {
        self.bind_with(json!({"session": "s1", "binding": "b1", "agent": "t3:fake", "workspace": std::env::temp_dir().to_string_lossy(), "cwd": std::env::temp_dir().to_string_lossy(),
                "options": {"title": "Test"}, "control": "native", "deny": []}))
        .await
    }

    async fn bind_with(&mut self, params: Value) -> Value {
        let init = self
            .call(
                "initialize",
                json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
            )
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "t3");
        self.call("binding/start", params).await
    }
}

/// A fake T3 server over HTTP, killed on drop.
struct HttpFake {
    child: std::process::Child,
    url: String,
}

impl HttpFake {
    fn start() -> Self {
        use std::io::BufRead;
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fake-t3-http"))
            .arg("--port")
            .arg("0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("the fake T3 server starts");
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("the server prints its address");
        HttpFake {
            child,
            url: line.trim().to_owned(),
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for HttpFake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn a_prompt_sends_reads_and_completes_the_turn() {
    let mut c = Client::start();
    let start = c.bind().await;
    assert_eq!(start["ref"]["id"], "thread-1");
    assert_eq!(start["level"], "read");
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m1", "text": "Say hello", "steer": false}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    c.wait(|u| u["update"]["type"] == "state" && u["update"]["state"] == "running")
        .await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["status"], "sent");
    let assistant = c.wait(|u| u["update"]["item"]["kind"] == "assistant").await;
    assert_eq!(assistant["update"]["item"]["set"]["text"], "Hello there.");
    let turn = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(turn["update"]["turn"]["status"], "completed");
    c.wait(|u| u["update"]["type"] == "state" && u["update"]["state"] == "idle")
        .await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn the_http_mcp_endpoint_serves_the_thread_tools() {
    let mut http = HttpFake::start();
    let mut c = Client::with(Config {
        env: vec![],
        guard: false,
        command: None,
        url: Some(format!("{}/mcp", http.url)),
        token: None,
    });
    let init = c
        .call(
            "initialize",
            json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
        )
        .await;
    assert_eq!(init["peer"]["kind"], "driver");
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(agents["agents"][0]["harness"]["version"], "0.1.10");
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let start = c
        .call(
            "binding/start",
            json!({"session": "s1", "binding": "b1", "agent": "t3:fake", "workspace": cwd, "cwd": cwd,
                "options": {"title": "Test", "model": "fake-model", "mode": "full-access"},
                "control": "native", "deny": []}),
        )
        .await;
    assert_eq!(start["ref"]["id"], "thread-1");
    assert_eq!(start["level"], "host");
    c.call(
        "binding/prompt",
        json!({"binding": "b1", "message": "m1", "text": "Say hello", "steer": false}),
    )
    .await;
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Hello there.")
        .await;
    c.wait(|u| u["update"]["type"] == "state" && u["update"]["state"] == "idle")
        .await;
    http.stop();
}

#[tokio::test]
async fn attachments_are_uploaded_then_named_to_t3() {
    let mut http = HttpFake::start();
    let mut c = Client::with(Config {
        env: vec![],
        guard: false,
        command: None,
        url: Some(format!("{}/mcp", http.url)),
        token: None,
    });
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    c.bind_with(
        json!({"session": "s1", "binding": "b1", "agent": "t3:fake", "workspace": cwd, "cwd": cwd,
            "options": {"title": "Test"}, "control": "native", "deny": []}),
    )
    .await;
    c.call(
        "binding/prompt",
        json!({
            "binding": "b1", "message": "m1", "text": "What is this?", "steer": false,
            "attachments": [
                { "name": "shot.png", "type": "image/png", "data": "aGVsbG8=" },
                { "name": "empty.bin", "type": "application/octet-stream" },
            ],
        }),
    )
    .await;
    let notice = c.wait(|u| u["update"]["type"] == "notice").await;
    assert_eq!(notice["update"]["code"], "attachment_unsupported");
    assert!(notice["update"]["text"].as_str().unwrap().contains("empty.bin"));
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Received 1 attachments.")
        .await;
    c.wait(|u| u["update"]["type"] == "state" && u["update"]["state"] == "idle")
        .await;
    http.stop();
}

#[tokio::test]
async fn options_set_the_runtime_mode() {
    let mut c = Client::start();
    c.bind().await;
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"mode": "full-access", "model": "fake-model"}}),
        )
        .await;
    assert_eq!(r["level"], "host");
    assert_eq!(r["network"], true);
    let model = r["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "model")
        .unwrap();
    assert_eq!(model["value"], "fake-model");
    c.call("binding/stop", json!({"binding": "b1"})).await;
}
