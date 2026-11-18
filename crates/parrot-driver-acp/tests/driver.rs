//! The ACP driver end to end: JSON-RPC in, `fake-acp` as the agent, updates out.

use std::path::PathBuf;
use std::time::Duration;

use parrot_driver_acp::data;
use parrot_driver_acp::driver::{Config, serve};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

struct Client {
    write: WriteHalf<DuplexStream>,
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next: u64,
    updates: Vec<Value>,
    seen: usize,
    scenario: Option<String>,
}

impl Client {
    fn start(scenario: Option<&str>) -> Client {
        Self::start_with(scenario, vec![])
    }

    fn start_with(scenario: Option<&str>, env: Vec<(String, String)>) -> Client {
        let path = scenario.map(|name| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/scenarios")
                .join(format!("{name}.jsonl"))
        });
        let config = Config {
            env: env.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
            guard: false,
            data: {
                let mut data =
                    data::load_from(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eggs/acp/parrot"));
                // The probe in `agent/list` starts the fake once for its version.
                data.command = Some(vec![env!("CARGO_BIN_EXE_fake-acp").into()]);
                data
            },
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
            scenario: path.map(|p| p.to_string_lossy().into_owned()),
        }
    }

    async fn read(&mut self) -> Value {
        let line = match tokio::time::timeout(Duration::from_secs(15), self.lines.next_line()).await {
            Ok(line) => line.unwrap().expect("the driver is still running"),
            Err(_) => panic!(
                "the driver answers in time; seen {}\n{}",
                self.seen,
                self.updates
                    .iter()
                    .map(|u| u.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
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
        assert_eq!(agents["agents"][0]["id"], "acp");
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let mut command = vec![json!(env!("CARGO_BIN_EXE_fake-acp"))];
        if let Some(scenario) = &self.scenario {
            command.push(json!("--scenario"));
            command.push(json!(scenario));
        }
        let mut params = json!({"session": "s1", "binding": "b1", "agent": "acp", "workspace": cwd, "cwd": cwd,
            "options": {"command": command}, "control": "native", "deny": []});
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

#[tokio::test]
async fn text_turn_streams_and_goes_idle() {
    let mut c = Client::start(Some("text-turn"));
    let start = c.bind().await;
    assert_eq!(start["level"], "read");
    assert_eq!(start["network"], false);
    let options = start["options"].as_array().unwrap();
    assert!(options.iter().any(|o| o["id"] == "mode"));
    assert!(options.iter().any(|o| o["id"] == "model"));
    let r = c.wait(|u| u["update"]["type"] == "ref").await;
    assert_eq!(r["update"]["ref"]["id"], "fake-session-1");
    c.prompt("Say hello").await;
    c.wait(is_state("running")).await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    let a = c
        .wait(|u| {
            u["update"]["item"]["set"]["text"] == "Hello " && u["update"]["item"]["kind"] == "assistant"
        })
        .await;
    assert_eq!(a["update"]["item"]["set"]["text"], "Hello ");
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    assert_eq!(t["update"]["turn"]["stop"], "end");
    c.wait(is_state("idle")).await;
    eprintln!("DEBUG about to stop");
    c.call("binding/stop", json!({"binding": "b1"})).await;
    eprintln!("DEBUG stopped");
}

#[tokio::test]
async fn bash_approval_then_the_tool_finishes() {
    let mut c = Client::start(Some("bash-allowed"));
    c.bind().await;
    c.prompt("List the files").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["id"], "acp-perm-agent-1");
    assert_eq!(request["level"], "host");
    let choices = request["choices"].as_array().unwrap();
    assert!(choices.iter().any(|c| c["id"] == "allow"));
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "acp-perm-agent-1", "answer": {"choice": "allow"}}),
    )
    .await;
    let done = c
        .wait(|u| {
            u["update"]["type"] == "item"
                && u["update"]["item"]["set"]["status"] == "ok"
                && u["update"]["item"]["set"]["output"] == "README.md\nsrc"
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
async fn a_denied_approval_fails_the_tool_and_the_turn_goes_on() {
    let mut c = Client::start(Some("bash-denied"));
    c.bind().await;
    c.prompt("List the files").await;
    c.wait(|u| u["update"]["type"] == "request").await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "acp-perm-agent-1", "answer": {"choice": "deny"}}),
    )
    .await;
    let failed = c
        .wait(|u| u["update"]["type"] == "item" && u["update"]["item"]["set"]["status"] == "error")
        .await;
    assert_eq!(failed["update"]["item"]["set"]["output"], "The user rejected it.");
    let a = c.wait(|u| u["update"]["item"]["set"]["text"] == "I ").await;
    assert_eq!(a["update"]["item"]["set"]["text"], "I ");
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::start(Some("interrupt"));
    c.bind().await;
    c.prompt("Count").await;
    c.wait(|u| u["update"]["item"]["append"]["text"] == "Counted ")
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

#[tokio::test]
async fn options_set_the_mode_and_the_model() {
    let mut c = Client::start(Some("text-turn"));
    c.bind().await;
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"mode": "bypassPermissions", "model": "fake-large"}}),
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
    assert_eq!(model["value"], "fake-large");
    c.wait(|u| u["update"]["type"] == "options" && u["update"]["level"] == "host")
        .await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn an_image_attachment_becomes_an_acp_image_block() {
    let mut c = Client::start(None);
    c.bind().await;
    c.call(
        "binding/prompt",
        json!({
            "binding": "b1",
            "message": "m1",
            "text": "What is this?",
            "attachments": [{ "name": "shot.png", "type": "image/png", "data": "aGVsbG8=" }],
            "steer": false,
        }),
    )
    .await;
    let frame = c
        .wait(|u| {
            u["update"]["type"] == "frame" && u["update"]["data"]["params"]["prompt"][0]["type"] == "image"
        })
        .await;
    let prompt = &frame["update"]["data"]["params"]["prompt"];
    assert_eq!(prompt[0]["mimeType"], "image/png");
    assert_eq!(prompt[1]["type"], "text");
}

#[tokio::test]
async fn a_version_two_agent_negotiates_and_the_turn_runs() {
    let mut c = Client::start_with(
        Some("text-turn"),
        vec![("PARROT_FAKE_ACP_PROTOCOL".into(), "2".into())],
    );
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(agents["agents"][0]["harness"]["version"], "0.4.5");
    let start = c.bind().await;
    assert_eq!(start["level"], "read");
    assert!(
        start["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == "model")
    );
    let r = c.wait(|u| u["update"]["type"] == "ref").await;
    assert_eq!(r["update"]["ref"]["id"], "fake-session-1");
    c.prompt("Say hello").await;
    let a = c.wait(|u| u["update"]["item"]["set"]["text"] == "Hello ").await;
    assert_eq!(a["update"]["item"]["kind"], "assistant");
    c.wait(is_state("idle")).await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn a_version_one_agent_is_asked_again_with_version_one() {
    // The fake answers neither shape when the first ask is version 2: it wants version 1.
    let mut c = Client::start_with(
        Some("text-turn"),
        vec![("PARROT_FAKE_ACP_PROTOCOL".into(), "1".into())],
    );
    let start = c.bind().await;
    assert_eq!(start["level"], "read");
    c.prompt("Say hello").await;
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Hello ").await;
}

#[tokio::test]
async fn a_load_replays_the_stored_conversation_as_items() {
    let mut c = Client::start_with(None, vec![("PARROT_FAKE_ACP_RESUME".into(), "0".into())]);
    let start = c
        .bind_with(json!({"resume": {"id": "fake-session-9", "strength": "strong"}}))
        .await;
    assert_eq!(start["ref"]["id"], "fake-session-9");
    let user = c
        .wait(|u| u["update"]["item"]["set"]["text"] == "What is in the notes?")
        .await;
    assert_eq!(user["update"]["item"]["kind"], "user");
    assert_eq!(user["binding"], "b1");
    let reply = c
        .wait(|u| u["update"]["item"]["set"]["text"] == "Two lines.")
        .await;
    assert_eq!(reply["update"]["item"]["kind"], "assistant");
    let tool = c
        .wait(|u| u["update"]["type"] == "item" && u["update"]["item"]["id"] == "replay-tool")
        .await;
    // The tool's embedded resource is decoded from its base64 blob.
    assert_eq!(tool["update"]["item"]["set"]["output"], "hello");
    c.wait(|u| {
        u["update"]["item"]["id"] == "replay-assistant:assistant" && u["update"]["item"]["streaming"] == false
    })
    .await;
}

#[tokio::test]
async fn embedded_resources_and_links_become_text() {
    let mut c = Client::start(None);
    c.bind().await;
    c.prompt("[[resource:A note from the agent.]] [[resource-blob:aGVsbG8=]] [[resource-link:notes|file:///notes.md]]")
        .await;
    let note = c
        .wait(|u| u["update"]["item"]["set"]["text"] == "A note from the agent.")
        .await;
    assert_eq!(note["update"]["item"]["kind"], "assistant");
    let blob = c.wait(|u| u["update"]["item"]["set"]["text"] == "hello").await;
    assert_eq!(blob["update"]["item"]["kind"], "assistant");
    let link = c
        .wait(|u| u["update"]["item"]["set"]["text"] == "notes: file:///notes.md")
        .await;
    assert_eq!(link["update"]["item"]["kind"], "assistant");
}

#[tokio::test]
async fn an_attachment_becomes_a_resource_with_blob_or_text() {
    let mut c = Client::start(None);
    c.bind().await;
    c.call(
        "binding/prompt",
        json!({
            "binding": "b1", "message": "m1", "text": "Look", "steer": false,
            "attachments": [
                { "name": "notes.txt", "type": "text/plain", "data": "aGVsbG8=" },
                { "name": "shot.png", "type": "image/png", "data": "aGVsbG8=" },
            ],
        }),
    )
    .await;
    let frame = c
        .wait(|u| {
            u["update"]["type"] == "frame"
                && u["update"]["data"]["params"]["prompt"]
                    .as_array()
                    .is_some_and(|p| p.iter().any(|b| b["type"] == "resource"))
        })
        .await;
    let prompt = frame["update"]["data"]["params"]["prompt"].as_array().unwrap();
    let text = prompt.iter().find(|b| b["type"] == "resource").unwrap();
    assert_eq!(text["resource"]["text"], "hello");
    let image = prompt
        .iter()
        .find(|b| b["type"] == "image")
        .expect("the image keeps its block");
    assert_eq!(image["mimeType"], "image/png");
}

#[tokio::test]
async fn the_driver_frames_what_it_sends_without_the_mcp_token() {
    let mut c = Client::start(None);
    c.call(
        "initialize",
        json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
    )
    .await;
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    c.call(
        "binding/start",
        json!({
            "session": "s1", "binding": "b1", "agent": "acp", "workspace": cwd, "cwd": cwd,
            "options": {"command": [env!("CARGO_BIN_EXE_fake-acp")]},
            "control": "native", "deny": [],
            "mcp": {"url": "http://127.0.0.1:9/mcp", "token": "parrot_tok_test"},
        }),
    )
    .await;
    let frame = c
        .wait(|u| u["update"]["type"] == "frame" && u["update"]["data"]["method"] == "session/new")
        .await;
    let servers = &frame["update"]["data"]["params"]["mcpServers"];
    assert_eq!(servers[0]["type"], "http");
    assert!(
        !frame.to_string().contains("parrot_tok_test"),
        "the frame carried the token: {frame}"
    );
}
