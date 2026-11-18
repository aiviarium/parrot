//! The driver end to end: JSON-RPC in, `fake-claude` as the CLI, updates out.

use std::path::PathBuf;
use std::time::Duration;

use parrot_driver_claude::driver::{Config, serve};
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
    fn start(scenario: &str) -> Client {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/scenarios")
            .join(format!("{scenario}.jsonl"));
        let config = Config {
            claude_bin: env!("CARGO_BIN_EXE_fake-claude").into(),
            env: vec![("PARROT_FAKE_CLAUDE_SCENARIO".into(), path.into())],
            guard: false,
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
            .call("initialize", json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}))
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "claude");
        assert!(agents["agents"][0]["features"]["session.steer"].is_object());
        let cwd = std::env::temp_dir();
        self.call(
            "binding/start",
            json!({"session": "s1", "binding": "b1", "agent": "claude", "workspace": cwd, "cwd": cwd,
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

#[tokio::test]
async fn text_turn_streams_and_goes_idle() {
    let mut c = Client::start("text-turn");
    let start = c.bind().await;
    assert_eq!(start["level"], "read");
    assert_eq!(start["actions"][0]["id"], "review");
    assert_eq!(start["options"][0]["schema"]["enum"], json!(["default"]));
    c.prompt("Say hello").await;
    c.wait(is_state("running")).await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    let r = c.wait(|u| u["update"]["type"] == "ref").await;
    assert_eq!(r["update"]["ref"]["id"], "sess-fake-0001");
    let a = c.wait(|u| u["update"]["item"]["append"].is_object()).await;
    assert_eq!(a["update"]["item"]["append"]["text"], "Hello ");
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    c.wait(is_state("idle")).await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn bash_approval_then_completes() {
    let mut c = Client::start("bash-allowed");
    c.bind().await;
    c.prompt("List the files").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["id"], "perm-1");
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "perm-1", "answer": {"choice": "allow"}}),
    )
    .await;
    let done = c
        .wait(|u| {
            u["update"]["item"]["id"] == "toolu_fake_01" && u["update"]["item"]["set"]["status"] == "ok"
        })
        .await;
    assert_eq!(done["update"]["item"]["set"]["output"], "README.md\nsrc");
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::start("interrupt");
    c.bind().await;
    c.prompt("Count").await;
    c.wait(|u| u["update"]["item"]["append"]["text"] == "One, two, ")
        .await;
    let r = c.call("binding/cancel", json!({"binding": "b1"})).await;
    assert_eq!(r["interrupted"], true);
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "interrupted");
    assert_eq!(t["update"]["turn"]["stop"], "cancelled");
    c.wait(is_state("idle")).await;
}
