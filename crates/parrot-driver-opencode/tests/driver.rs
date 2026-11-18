//! The opencode driver end to end: JSON-RPC in, `fake-opencode` as the HTTP server, updates out.

use std::time::Duration;

use parrot_driver_opencode::driver::{Config, serve};
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
    /// Starts the driver with a fake opencode on a port of its own.
    fn start() -> Client {
        let config = Config {
            bin: env!("CARGO_BIN_EXE_fake-opencode").into(),
            args: vec!["serve".into(), "--hostname".into(), "127.0.0.1".into()],
            env: vec![],
            guard: false,
            data: parrot_driver_opencode::driver::default_data(),
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
        let line = tokio::time::timeout(Duration::from_secs(20), self.lines.next_line())
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
        self.bind_with(json!({})).await
    }

    async fn bind_with(&mut self, extra: Value) -> Value {
        let init = self
            .call(
                "initialize",
                json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
            )
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "opencode");
        assert_eq!(agents["agents"][0]["levels"], json!(["read", "write", "host"]));
        assert!(agents["agents"][0]["features"]["functions.mcp"].is_object());
        let cwd = std::env::temp_dir();
        let mut params = json!({"session": "s1", "binding": "b1", "agent": "opencode",
            "workspace": cwd, "cwd": cwd, "options": {}, "control": "native", "deny": []});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            params[k] = v;
        }
        self.call("binding/start", params).await
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
async fn text_then_a_tool_streams_and_goes_idle() {
    let mut c = Client::start();
    let start = c.bind().await;
    assert_eq!(start["level"], "host");
    assert!(
        start["ref"]["id"].as_str().unwrap().starts_with("ses_"),
        "the session id comes from opencode: {}",
        start["ref"]["id"]
    );
    let actions: Vec<&str> = start["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["review", "test", "compact"]);
    c.prompt("[[text:Hello there.]] [[tool:bash {\"command\":\"ls\"}]]")
        .await;
    c.wait(is_state("running")).await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    c.wait(|u| u["update"]["item"]["append"]["text"] == "Hello ")
        .await;
    let text = c
        .wait(|u| u["update"]["item"]["kind"] == "assistant" && u["update"]["item"]["set"]["text"] == "Hello there.")
        .await;
    assert_eq!(text["update"]["item"]["streaming"], false);
    let tool = c
        .wait(|u| u["update"]["item"]["set"]["status"] == "ok")
        .await;
    assert_eq!(tool["update"]["item"]["set"]["output"], "ran bash");
    assert!(
        c.updates
            .iter()
            .any(|u| u["update"]["item"]["set"]["tool"] == "bash")
    );
    let done = c.wait(ended("completed")).await;
    assert_eq!(done["update"]["turn"]["stop"], "end");
    let usage = c.wait(|u| u["update"]["type"] == "usage").await;
    assert_eq!(usage["update"]["usage"]["tokens"]["input"], 10);
    assert_eq!(usage["update"]["usage"]["tokens"]["cacheRead"], 2);
    c.wait(is_state("idle")).await;
    // Every event is also a frame, so the raw stream can be stored.
    assert!(
        c.updates
            .iter()
            .any(|u| u["update"]["type"] == "frame" && u["update"]["source"] == "opencode")
    );
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn a_permission_is_answered_over_http() {
    let mut c = Client::start();
    c.bind().await;
    c.prompt("[[permission:bash|ls -la]]").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["title"], "Run bash?");
    assert_eq!(request["level"], "host");
    assert_eq!(request["choices"][0]["id"], "allow");
    assert_eq!(request["choices"][1]["id"], "allow-session");
    let id = request["id"].as_str().unwrap().to_owned();
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": id, "answer": {"choice": "allow"}}),
    )
    .await;
    c.wait(|u| u["update"]["item"]["set"]["output"] == "allowed")
        .await;
    c.wait(|u| u["update"]["item"]["set"]["text"] == "It ran.")
        .await;
    c.wait(ended("completed")).await;
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn a_denied_permission_marks_the_tool_failed() {
    let mut c = Client::start();
    c.bind().await;
    c.prompt("[[permission:bash|rm -rf build]]").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let id = req["update"]["request"]["id"].as_str().unwrap().to_owned();
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": id, "answer": {"choice": "deny"}}),
    )
    .await;
    let tool = c
        .wait(|u| u["update"]["item"]["set"]["status"] == "error")
        .await;
    assert_eq!(tool["update"]["item"]["set"]["output"], "denied");
    c.wait(|u| u["update"]["item"]["set"]["text"] == "It did not run.")
        .await;
    c.wait(ended("completed")).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::start();
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
async fn an_image_reaches_opencode_and_a_file_without_data_gets_a_notice() {
    let mut c = Client::start();
    c.bind().await;
    let r = c
        .call(
            "binding/prompt",
            json!({
                "binding": "b1", "message": "m1", "text": "What is this?", "steer": false,
                "attachments": [
                    {"name": "shot.png", "type": "image/png", "data": "iVBORw0KGgo="},
                    {"name": "notes.txt", "type": "text/plain", "data": "aGVsbG8="},
                    {"name": "empty.bin", "type": "application/octet-stream"},
                ],
            }),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let notice = c.wait(|u| u["update"]["type"] == "notice").await;
    assert_eq!(notice["update"]["code"], "attachment_unsupported");
    assert!(
        notice["update"]["text"].as_str().unwrap().contains("empty.bin"),
        "the notice names the file: {}",
        notice["update"]["text"]
    );
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Received 2 images.")
        .await;
    c.wait(ended("completed")).await;
}

#[tokio::test]
async fn options_come_from_opencode_and_complete() {
    let mut c = Client::start();
    let start = c.bind().await;
    let options = start["options"].as_array().unwrap();
    let model = options.iter().find(|o| o["id"] == "model").unwrap();
    assert_eq!(model["role"], "model");
    assert_eq!(model["complete"], true);
    assert_eq!(model["value"], "fake/fake-1");
    let provider = options.iter().find(|o| o["id"] == "provider").unwrap();
    assert_eq!(provider["schema"]["enum"], json!(["fake", "other"]));
    assert_eq!(provider["value"], "fake");

    let complete = c
        .call(
            "option/complete",
            json!({"binding": "b1", "option": "model", "prefix": "fake/fake-"}),
        )
        .await;
    let values = complete["values"].as_array().unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["const"], "fake/fake-1");
    assert_eq!(values[0]["title"], "Fake 1 (fake)");

    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"model": "fake/fake-2"}}),
        )
        .await;
    let model = r["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "model")
        .unwrap()
        .clone();
    assert_eq!(model["value"], "fake/fake-2");
    assert_eq!(r["level"], "host");
    let update = c.wait(|u| u["update"]["type"] == "options").await;
    assert_eq!(update["update"]["level"], "host");
}

#[tokio::test]
async fn mcp_reaches_opencode_with_the_token_in_the_environment() {
    let mut c = Client::start();
    c.bind_with(json!({"mcp": {"url": "http://127.0.0.1:9/mcp", "token": "test-token-123"}}))
        .await;
    c.prompt("[[mcp]]").await;
    let tool = c
        .wait(|u| u["update"]["item"]["set"]["status"] == "ok")
        .await;
    let output = tool["update"]["item"]["set"]["output"].as_str().unwrap();
    let output: Value = serde_json::from_str(output).expect("the fake reports JSON");
    assert_eq!(output["url"], "http://127.0.0.1:9/mcp");
    assert_eq!(output["authorization"], "Bearer test-token-123");
}
