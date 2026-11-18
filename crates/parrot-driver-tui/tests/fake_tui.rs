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
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::load(fake("fake-tui.toml")).unwrap();
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
    let profile = Profile::load(fake("fake-tui.toml")).unwrap();
    let mut d = TuiDriver::new(
        Config::new("fake-tui", vec!["sh".into()], profile),
        DriverContext { updates: tx },
    );
    let r = d.call("agent/list", json!({})).await.unwrap();
    assert_eq!(r["agents"][0]["id"], "fake-tui");
    assert_eq!(r["agents"][0]["controls"], json!(["tui"]));
    assert_eq!(r["agents"][0]["driver"]["data"], "fake-tui");
}
