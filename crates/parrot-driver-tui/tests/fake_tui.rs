//! The TUI driver against a scripted fake TUI (`tests/fake/fake-tui.sh`) and its profile.

use std::path::{Path, PathBuf};
use std::time::Duration;

use parrot_driver::traits::{Driver, DriverContext};
use parrot_driver_tui::{Config, TranscriptDir, TuiDriver};
use parrot_term::screen::Size;
use parrot_tui::Profile;
use serde_json::{Value, json};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(15);

fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fake")
        .join(name)
}

struct Harness {
    driver: TuiDriver,
    updates: mpsc::UnboundedReceiver<Value>,
    /// Every update seen so far, in order.
    seen: Vec<Value>,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn start(trust: bool) -> (Harness, Result<Value, String>) {
        Harness::start_with(trust, |_| {}).await
    }

    async fn start_with(trust: bool, adjust: impl FnOnce(&mut Config)) -> (Harness, Result<Value, String>) {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::load(fake("fake-tui.kdl")).unwrap();
        let script = fake("fake-tui.sh").to_string_lossy().into_owned();
        let mut config = Config::new("fake-tui", vec!["sh".into(), script], profile);
        config.guard = None;
        config.size = Size::new(12, 60);
        config.unrecognised_after = Duration::from_millis(800);
        config.start_timeout = WAIT;
        config.transcript = Some(TranscriptDir::Dir(dir.path().into()));
        config.env = vec![(
            "FAKE_TUI_TRANSCRIPT".into(),
            dir.path().to_string_lossy().into_owned(),
        )];
        if trust {
            config.env.push(("FAKE_TUI_TRUST".into(), "1".into()));
        }
        adjust(&mut config);
        let (tx, rx) = mpsc::unbounded_channel();
        let mut driver = TuiDriver::new(config, DriverContext { updates: tx });
        let cwd = dir.path().to_string_lossy().into_owned();
        let started = driver
            .call(
                "binding/start",
                json!({
                    "session": "ses_fake", "binding": "b1", "agent": "fake-tui",
                    "workspace": cwd, "cwd": cwd, "options": {}, "control": "tui", "deny": [],
                }),
            )
            .await
            .map_err(|e| e.message);
        (
            Harness {
                driver,
                updates: rx,
                seen: vec![],
                dir,
            },
            started,
        )
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.driver
            .call(method, params)
            .await
            .unwrap_or_else(|e| panic!("{method}: {}", e.message))
    }

    /// Waits for an update `pred` accepts, keeping everything seen on the way.
    async fn until(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(u) = self.seen.iter().find(|u| pred(u)) {
            return u.clone();
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.updates.recv()).await {
                Ok(Some(u)) => {
                    self.seen.push(u.clone());
                    if pred(&u) {
                        return u;
                    }
                }
                _ => panic!(
                    "no {what}; updates: {}",
                    serde_json::to_string_pretty(&self.seen).unwrap()
                ),
            }
        }
    }

    async fn state(&mut self, state: &str) {
        self.until(&format!("state {state}"), |u| {
            u["update"]["type"] == "state" && u["update"]["state"] == state
        })
        .await;
    }

    async fn request(&mut self, status: &str) -> Value {
        let u = self
            .until(&format!("{status} request"), |u| {
                u["update"]["type"] == "request" && u["update"]["request"]["status"] == status
            })
            .await;
        u["update"]["request"].clone()
    }

    async fn turn_done(&mut self, key: &str) -> Value {
        let u = self
            .until(&format!("end of {key}"), |u| {
                u["update"]["type"] == "turn"
                    && u["update"]["turn"]["id"] == key
                    && u["update"]["turn"]["status"] != "running"
            })
            .await;
        u["update"]["turn"].clone()
    }

    /// Forgets what was seen, so the next waits look only at what comes after.
    fn clear(&mut self) {
        self.seen.clear();
    }

    async fn prompt(&mut self, message: &str, text: &str) {
        let r = self
            .call(
                "binding/prompt",
                json!({ "binding": "b1", "message": message, "text": text, "steer": false }),
            )
            .await;
        assert_eq!(r["disposition"], "started");
    }

    async fn screen(&mut self) -> Value {
        self.call("binding/screen", json!({ "binding": "b1" })).await
    }

    async fn stop(mut self) {
        self.call("binding/stop", json!({ "binding": "b1" })).await;
        self.state("stopped").await;
        drop(self.dir);
    }
}

#[tokio::test]
async fn a_prompt_runs_and_ends_idle_with_transcript_items() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.clear();

    h.prompt("msg_1", "hello there").await;
    h.state("running").await;
    let user = h
        .until("user item", |u| {
            u["update"]["type"] == "item" && u["update"]["item"]["id"] == "msg_1"
        })
        .await;
    assert_eq!(user["update"]["item"]["set"]["text"], "hello there");
    let turn = h.turn_done("turn-1").await;
    assert_eq!(turn["status"], "completed");
    assert_eq!(turn["input"], "msg_1");
    h.state("idle").await;

    // The transcript gives the reply and the harness's session id; the echoed prompt is dropped.
    let reply = h
        .until("assistant item", |u| {
            u["update"]["type"] == "item" && u["update"]["item"]["kind"] == "assistant"
        })
        .await;
    assert_eq!(reply["update"]["item"]["set"]["text"], "Hi from the fake.");
    assert_eq!(reply["update"]["item"]["turn"], "turn-1");
    assert_eq!(reply["update"]["item"]["source"], "transcript");
    h.until("ref", |u| {
        u["update"]["type"] == "ref" && u["update"]["ref"]["id"] == "fake-session-0001"
    })
    .await;
    assert!(
        !h.seen
            .iter()
            .any(|u| u["update"]["item"]["source"] == "transcript" && u["update"]["item"]["kind"] == "user"),
        "the echo was reported again"
    );

    let screen = h.screen().await;
    assert!(
        screen["text"].as_str().unwrap().contains("Done: hello there"),
        "{screen}"
    );
    assert_eq!(screen["size"], json!({ "rows": 12, "cols": 60 }));
    assert_eq!(screen["state"], "idle");
    assert!(screen["tree"]["root"].is_object());
    h.stop().await;
}

#[tokio::test]
async fn a_known_approval_is_a_request_answered_with_the_profile_keys() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.clear();

    h.prompt("msg_1", "approve the tool").await;
    let req = h.request("open").await;
    assert_eq!(req["kind"], "approval");
    assert_eq!(req["turn"], "turn-1");
    assert!(
        req["title"]
            .as_str()
            .unwrap()
            .contains("Do you want to run fake-tool?"),
        "{req}"
    );
    let ids: Vec<_> = req["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].clone())
        .collect();
    assert_eq!(ids, [json!("1"), json!("2")]);
    assert!(req["choices"][0]["label"].as_str().unwrap().contains("Yes"));
    h.state("requires_action").await;

    h.call(
        "binding/answer",
        json!({ "binding": "b1", "request": req["id"], "answer": { "choice": "1" } }),
    )
    .await;
    let answered = h.request("answered").await;
    assert_eq!(answered["answer"]["choice"], "1");
    assert_eq!(h.turn_done("turn-1").await["status"], "completed");
    let screen = h.screen().await;
    assert!(
        screen["text"].as_str().unwrap().contains("Tool allowed."),
        "{screen}"
    );
    h.stop().await;
}

#[tokio::test]
async fn an_unknown_dialog_is_answered_from_a_client() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.clear();

    h.prompt("msg_1", "mystery").await;
    h.state("unrecognised").await;
    let req = h.request("open").await;
    assert_eq!(req["kind"], "input");
    let text = req["description"].as_str().unwrap();
    assert!(text.contains("Pick a colour: red, green or blue"), "{req}");
    assert_eq!(req["subject"]["type"], "screen");
    assert_eq!(req["subject"]["text"], text);

    // A client can look at the live screen before answering.
    let screen = h.screen().await;
    assert!(screen["text"].as_str().unwrap().contains("colour:"));
    assert_eq!(screen["unrecognised"], true);

    h.call(
        "binding/answer",
        json!({ "binding": "b1", "request": req["id"], "answer": { "value": "green" } }),
    )
    .await;
    h.request("answered").await;
    assert_eq!(h.turn_done("turn-1").await["status"], "completed");
    h.state("idle").await;
    let screen = h.screen().await;
    assert!(
        screen["text"].as_str().unwrap().contains("You picked green."),
        "{screen}"
    );
    h.stop().await;
}

#[tokio::test]
async fn a_start_time_dialog_is_a_request_on_the_starting_binding() {
    let (mut h, started) = Harness::start(true).await;
    started.unwrap();
    let req = h.request("open").await;
    assert_eq!(req["kind"], "approval");
    assert!(
        req["description"]
            .as_str()
            .unwrap()
            .contains("Do you trust the files")
    );
    h.state("requires_action").await;
    h.call(
        "binding/answer",
        json!({ "binding": "b1", "request": req["id"], "answer": { "choice": "1" } }),
    )
    .await;
    h.state("idle").await;
    h.stop().await;
}

#[tokio::test]
async fn keys_reach_the_terminal() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.call(
        "binding/keys",
        json!({ "binding": "b1", "text": "typed by hand" }),
    )
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let screen = h.screen().await;
        if screen["text"].as_str().unwrap().contains("> typed by hand") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{screen}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.clear();
    h.call("binding/keys", json!({ "binding": "b1", "press": ["enter"] }))
        .await;
    h.state("running").await;
    h.clear();
    h.state("idle").await;
    let r = h.call("binding/cancel", json!({ "binding": "b1" })).await;
    assert_eq!(r["interrupted"], false);
    h.stop().await;
}

#[tokio::test]
async fn agent_list_reports_tui_control() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let profile = Profile::load(fake("fake-tui.kdl")).unwrap();
    let mut d = TuiDriver::new(
        Config::new("fake-tui", vec!["sh".into()], profile),
        DriverContext { updates: tx },
    );
    let r = d.call("agent/list", json!({})).await.unwrap();
    assert_eq!(r["agents"][0]["id"], "fake-tui");
    assert_eq!(r["agents"][0]["controls"], json!(["tui"]));
    assert_eq!(r["agents"][0]["driver"]["data"], "fake-tui");
}

#[tokio::test]
async fn attachments_go_through_the_attach_action_and_frames_rebuild_items() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.clear();
    let r = h
        .call(
            "binding/prompt",
            json!({ "binding": "b1", "message": "msg_1", "text": "look", "steer": false,
                "attachments": [
                    { "name": "dot.png", "type": "image/png", "data": "iVBORw0KGgo=" },
                    { "name": "far.png", "type": "image/png", "uri": "https://example.test/far.png" }
                ] }),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let n = h.until("notice", |u| u["update"]["type"] == "notice").await;
    assert!(n["update"]["text"].as_str().unwrap().contains("far.png"), "{n}");
    h.turn_done("turn-1").await;
    h.state("idle").await;
    h.until("assistant item", |u| u["update"]["item"]["kind"] == "assistant")
        .await;
    let screen = h.screen().await;
    let text = screen["text"].as_str().unwrap();
    assert!(
        text.contains("Done: @") && text.contains("dot.png look"),
        "{screen}"
    );

    let frames: Vec<Value> = h
        .seen
        .iter()
        .filter(|u| u["update"]["type"] == "frame")
        .map(|u| {
            assert_eq!(u["update"]["source"], "transcript");
            u["update"]["data"].clone()
        })
        .collect();
    assert!(!frames.is_empty());
    let echo = "look".to_owned();
    let rebuilt: Vec<Value> = parrot_driver_tui::frame::rebuild("b1", &frames, &[echo])
        .iter()
        .map(|u| serde_json::to_value(u).unwrap())
        .collect();
    let live: Vec<Value> = h
        .seen
        .iter()
        .filter(|u| u["update"]["item"]["source"] == "transcript")
        .cloned()
        .collect();
    assert_eq!(rebuilt, live);
    h.stop().await;
}

#[test]
fn mcp_is_injected_without_the_token_in_argv() {
    use parrot_driver_tui::driver::{McpStyle, mcp_args};
    let mcp = serde_json::from_value(json!({ "url": "http://127.0.0.1:4711/mcp", "token": "tok-made-up" }))
        .unwrap();
    let claude = mcp_args(McpStyle::Claude, &mcp);
    assert_eq!(claude[0], "--mcp-config");
    let config: Value = serde_json::from_str(&claude[1]).unwrap();
    assert_eq!(
        config["mcpServers"]["parrot"]["headers"]["Authorization"],
        "Bearer ${PARROT_MCP_TOKEN}"
    );
    let codex = mcp_args(McpStyle::Codex, &mcp);
    assert_eq!(
        codex[3],
        "mcp_servers.parrot.bearer_token_env_var=\"PARROT_MCP_TOKEN\""
    );
    assert!(!claude.iter().chain(&codex).any(|a| a.contains("tok-made-up")));
}

#[tokio::test]
async fn a_prompt_while_working_steers_the_turn() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.prompt("msg_1", "slow count").await;
    h.state("running").await;
    let r = h
        .call(
            "binding/prompt",
            json!({ "binding": "b1", "message": "msg_2", "text": "also tens", "steer": true }),
        )
        .await;
    assert_eq!(r["disposition"], "steered");
    let steered = h
        .until("steer item", |u| u["update"]["item"]["id"] == "msg_2")
        .await;
    assert_eq!(steered["update"]["item"]["turn"], "turn-1");
    let mut seen = false;
    for _ in 0..30 {
        let screen = h.screen().await;
        if screen["text"].as_str().unwrap().contains("Steered: also tens") {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen, "the steer did not reach the harness");
    // The transcript ends the turn; its echo of the steer is not reported again.
    let turn = h.turn_done("turn-1").await;
    assert_eq!(turn["status"], "completed");
    assert!(
        !h.seen
            .iter()
            .any(|u| u["update"]["item"]["source"] == "transcript" && u["update"]["item"]["kind"] == "user"),
        "an echo was reported again"
    );
    h.stop().await;
}

#[tokio::test]
async fn cancel_runs_the_interrupt_action() {
    let (mut h, started) = Harness::start(false).await;
    started.unwrap();
    h.state("idle").await;
    h.prompt("msg_1", "slow count").await;
    h.state("running").await;
    // The interrupt action waits for the working screen to hold.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let r = h.call("binding/cancel", json!({ "binding": "b1" })).await;
    assert_eq!(r["interrupted"], true);
    let turn = h.turn_done("turn-1").await;
    assert_eq!(turn["status"], "interrupted");
    assert!(
        h.screen().await["text"]
            .as_str()
            .unwrap()
            .contains("Interrupted.")
    );
    h.stop().await;
}

#[tokio::test]
async fn profile_actions_are_offered_and_run() {
    let (mut h, started) = Harness::start(false).await;
    let started = started.unwrap();
    let ids: Vec<&str> = started["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    // The actions the driver runs itself stay out of the list.
    assert_eq!(ids, ["clear-prompt"]);
    assert_eq!(started["actions"][0]["when"], json!(["idle"]));
    assert_eq!(started["actions"][0]["requires"], json!(["terminal"]));
    h.state("idle").await;
    let r = h
        .call(
            "binding/action",
            json!({ "binding": "b1", "action": "clear-prompt" }),
        )
        .await;
    assert_eq!(r["status"], "done");
    let bad = h
        .driver
        .call("binding/action", json!({ "binding": "b1", "action": "submit" }))
        .await;
    assert!(bad.is_err());
    h.stop().await;
}

#[tokio::test]
async fn shadow_mode_reports_a_prompt_the_transcript_recorded_differently() {
    let correlate = parrot_proto::driver_data::parse(
        "correlate.kdl",
        "kind \"user\" {\n  match \"position\" \"text\"\n  prefer \"tui\"\n  history \"transcript\"\n  compare \"text\"\n}\n",
    )
    .unwrap();
    let (mut h, started) = Harness::start_with(false, |c| {
        c.shadow = true;
        c.correlate = Some(correlate);
    })
    .await;
    started.unwrap();
    h.state("idle").await;
    h.prompt("msg_1", "hello there").await;
    h.turn_done("turn-1").await;
    // The turn's comparison has time to run, and the screen settles, before the next prompt.
    tokio::time::sleep(Duration::from_millis(900)).await;
    for _ in 0..30 {
        if h.screen().await["state"] == "idle" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    h.call(
        "binding/prompt",
        json!({ "binding": "b1", "message": "msg_2", "text": "typo here", "steer": false }),
    )
    .await;
    h.turn_done("turn-2").await;
    let notice = h
        .until("shadow notice", |u| u["update"]["code"] == "x.parrot/shadow")
        .await;
    assert!(
        notice["update"]["text"].as_str().unwrap().contains("text"),
        "{notice}"
    );
    let notices = h
        .seen
        .iter()
        .filter(|u| u["update"]["code"] == "x.parrot/shadow")
        .count();
    assert_eq!(notices, 1, "the matching turn reported nothing");
    h.stop().await;
}
