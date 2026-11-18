//! The pi driver end to end: JSON-RPC in, `fake-pi` as the CLI, updates out.

use std::path::PathBuf;
use std::time::Duration;

use parrot_driver_pi::driver::{Config, serve};
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
    /// Starts the driver with a fake pi whose sessions live in a scratch folder.
    fn start(home: PathBuf) -> Client {
        let config = Config {
            pi_bin: env!("CARGO_BIN_EXE_fake-pi").into(),
            args: vec!["--mode".into(), "rpc".into()],
            env: vec![("FAKE_PI_HOME".into(), home.into_os_string())],
            guard: false,
            data: parrot_driver_pi::driver::default_data(),
        };
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

    fn temp() -> Client {
        let dir = tempfile::tempdir().expect("a scratch folder");
        // The fake keeps its sessions here for as long as the process runs.
        let home = dir.keep();
        Client::start(home)
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
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

    /// Waits for an update that `pred` accepts, past the ones already waited for.
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
        let init = self
            .call(
                "initialize",
                json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
            )
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "pi");
        assert_eq!(agents["agents"][0]["levels"], json!(["host"]));
        assert!(agents["agents"][0]["features"]["session.steer"].is_object());
        let cwd = std::env::temp_dir();
        self.call(
            "binding/start",
            json!({"session": "s1", "binding": "b1", "agent": "pi", "workspace": cwd, "cwd": cwd,
                "options": {}, "control": "native", "deny": []}),
        )
        .await
    }

    async fn prompt(&mut self, text: &str) {
        let r = self
            .call(
                "binding/prompt",
                json!({"binding": "b1", "message": "m1", "text": text, "steer": false}),
            )
            .await;
        assert_eq!(r["disposition"], "started");
    }
}

fn is_state(state: &'static str) -> impl Fn(&Value) -> bool {
    move |u| u["update"]["type"] == "state" && u["update"]["state"] == state
}

fn ended(status: &'static str) -> impl Fn(&Value) -> bool {
    move |u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] == status
}

#[tokio::test]
async fn text_turn_streams_and_goes_idle() {
    let mut c = Client::temp();
    let start = c.bind().await;
    assert_eq!(start["level"], "host");
    assert!(
        start["ref"]["id"].as_str().unwrap().starts_with("fake-"),
        "the session id comes from pi: {}",
        start["ref"]["id"]
    );
    assert_eq!(start["actions"][0]["id"], "review");
    c.prompt("[[text:Hello there.]]").await;
    c.wait(is_state("running")).await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    let turn = c
        .updates
        .iter()
        .find(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] == "running")
        .expect("the turn started")
        .clone();
    assert_eq!(turn["update"]["turn"]["input"], "m1");
    c.wait(|u| u["update"]["item"]["append"]["text"] == "Hello ")
        .await;
    // A usage update is the message's own tokens; the runtime adds them up.
    let usage = c.wait(|u| u["update"]["type"] == "usage").await;
    assert_eq!(usage["update"]["usage"]["tokens"]["input"], 10);
    let done = c.wait(ended("completed")).await;
    assert_eq!(done["update"]["turn"]["stop"], "end");
    c.wait(is_state("idle")).await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn an_extension_confirm_is_answered() {
    let mut c = Client::temp();
    c.bind().await;
    c.prompt("[[confirm:Run the migration?]]").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["title"], "Run the migration?");
    assert_eq!(request["choices"][0]["id"], "allow");
    let id = request["id"].as_str().unwrap().to_owned();
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": id, "answer": {"choice": "allow"}}),
    )
    .await;
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Confirmed.")
        .await;
    c.wait(ended("completed")).await;
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::temp();
    c.bind().await;
    c.prompt("[[stream-until-abort]]").await;
    c.wait(|u| u["update"]["item"]["append"]["text"] == "One, ").await;
    let r = c.call("binding/cancel", json!({"binding": "b1"})).await;
    assert_eq!(r["interrupted"], true);
    let t = c.wait(ended("interrupted")).await;
    assert_eq!(t["update"]["turn"]["stop"], "cancelled");
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn an_image_attachment_reaches_pi_and_a_file_gets_a_notice() {
    let mut c = Client::temp();
    c.bind().await;
    let r = c
        .call(
            "binding/prompt",
            json!({
                "binding": "b1", "message": "m1", "text": "What is this?", "steer": false,
                "attachments": [
                    {"name": "shot.png", "type": "image/png", "data": "iVBORw0KGgo="},
                    {"name": "notes.txt", "type": "text/plain", "data": "aGVsbG8="},
                ],
            }),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let notice = c.wait(|u| u["update"]["type"] == "notice").await;
    assert_eq!(notice["update"]["code"], "attachment_unsupported");
    assert!(
        notice["update"]["text"].as_str().unwrap().contains("notes.txt"),
        "the notice names the file: {}",
        notice["update"]["text"]
    );
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Received 1 image.")
        .await;
    c.wait(ended("completed")).await;
}

#[tokio::test]
async fn options_come_from_pi_and_apply() {
    let mut c = Client::temp();
    let start = c.bind().await;
    let options = start["options"].as_array().unwrap();
    let model = options.iter().find(|o| o["id"] == "model").unwrap();
    assert_eq!(model["schema"]["enum"], json!(["fake/fake-1", "fake/fake-2"]));
    let effort = options.iter().find(|o| o["id"] == "effort").unwrap();
    assert_eq!(effort["schema"]["enum"], json!(["low", "medium", "high"]));
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"model": "fake/fake-2", "effort": "high"}}),
        )
        .await;
    let options = r["options"].as_array().unwrap();
    assert_eq!(
        options.iter().find(|o| o["id"] == "model").unwrap()["value"],
        "fake/fake-2"
    );
    assert_eq!(
        options.iter().find(|o| o["id"] == "effort").unwrap()["value"],
        "high"
    );
    assert_eq!(r["level"], "host");
    let update = c.wait(|u| u["update"]["type"] == "options").await;
    assert_eq!(update["update"]["level"], "host");
}

#[tokio::test]
async fn mcp_reaches_pi_with_the_token_in_the_environment() {
    let mut c = Client::temp();
    let cwd = std::env::temp_dir();
    c.call(
        "initialize",
        json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
    )
    .await;
    c.call(
        "binding/start",
        json!({"session": "s1", "binding": "b1", "agent": "pi", "workspace": cwd, "cwd": cwd,
            "options": {}, "control": "native", "deny": [],
            "mcp": {"url": "http://127.0.0.1:9/mcp", "token": "test-token-123"}}),
    )
    .await;
    c.prompt("[[mcp]]").await;
    let tool = c.wait(|u| u["update"]["item"]["set"]["status"] == "ok").await;
    let output = tool["update"]["item"]["set"]["output"].as_str().unwrap();
    let output: Value = serde_json::from_str(output).expect("the fake reports JSON");
    assert_eq!(output["url"], "http://127.0.0.1:9/mcp");
    assert_eq!(output["authorization"], "Bearer test-token-123");
}
