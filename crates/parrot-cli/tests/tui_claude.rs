//! The `tui` profile proof with a real profile (spec 32.2, 40): `control: "tui"` for the
//! `claude` agent uses the Claude egg's `profile.kdl` and command, with no `config.json` entry.
//! The harness is a fake that replays Claude Code's screens from a development cast; a dialog
//! no profile knows is answered from a client, and the screen and keys go through the runtime.
#![cfg(unix)]
// Starts the cast fake by its shebang, which Windows does not run.

mod common;

use common::{Client, Serve, Setup};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

fn here(path: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(path)
        .display()
        .to_string()
}

async fn screen_with(c: &mut Client, session: &str, text: &str) -> Value {
    let deadline = Instant::now() + common::WAIT;
    loop {
        let screen = c.ok("session/screen", json!({ "session": session })).await;
        if screen["text"].as_str().unwrap_or_default().contains(text) {
            return screen;
        }
        assert!(Instant::now() < deadline, "no {text:?} on {screen}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_under_tui_control_uses_its_egg_profile() {
    let home = tempfile::tempdir().unwrap();
    let keys = home.path().join("keys.log");
    let serve = Serve::start(Setup {
        env: vec![
            ("PARROT_CLAUDE_BIN".into(), here("tests/fake/claude-cast.py")),
            (
                "FAKE_CLAUDE_CAST".into(),
                here("../parrot-tui/fixtures/casts/claude-prompt.cast"),
            ),
            ("FAKE_CLAUDE_LOG".into(), keys.display().to_string()),
            // The size the cast was recorded at.
            ("PARROT_TUI_SIZE".into(), "30x100".into()),
            // Claude Code's transcript folder is under HOME; keep it out of the real one.
            ("HOME".into(), home.path().display().to_string()),
        ],
        ..Setup::default()
    });
    assert!(!serve.state_dir().join("config.json").exists());
    let mut c = Client::connect(&serve, "phone").await;
    c.initialize().await;
    let agents = c.ok("agent/list", json!({})).await;
    let claude = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "claude")
        .cloned()
        .unwrap();
    assert!(
        claude["controls"].as_array().unwrap().contains(&json!("tui")),
        "{claude}"
    );

    let work = serve.work_dir();
    let created = c
        .ok(
            "session/new",
            json!({ "agent": "claude", "control": "tui",
                "workspace": { "kind": "dir", "path": work.to_string_lossy() } }),
        )
        .await;
    let session = created["session"]["id"].as_str().unwrap().to_string();
    c.wait_update(0, "idle", |u| u["type"] == "state" && u["state"] == "idle")
        .await;
    let screen = screen_with(&mut c, &session, "Claude Code").await;
    assert_eq!(screen["size"]["cols"], 100, "{screen}");

    // A prompt runs a turn the profile follows from the screens: working, then idle.
    let from = c.seen.len();
    c.ok(
        "session/prompt",
        json!({ "session": session, "text": "Say hello." }),
    )
    .await;
    c.wait_update(from, "working", |u| {
        u["type"] == "state" && u["state"] == "running"
    })
    .await;
    c.wait_update(from, "the turn's end", |u| {
        u["type"] == "turn" && u["turn"]["status"] == "completed"
    })
    .await;

    // A dialog no profile knows becomes an input request with the screen text.
    let from = c.seen.len();
    c.ok("session/prompt", json!({ "session": session, "text": "mystery" }))
        .await;
    let i = c
        .wait_update(from, "the unknown dialog", |u| {
            u["type"] == "request" && u["request"]["status"] == "open"
        })
        .await;
    let request = c.update(i)["request"].clone();
    assert_eq!(request["kind"], "input", "{request}");
    assert!(
        request["description"].as_str().unwrap().contains("Pick a colour"),
        "{request}"
    );
    screen_with(&mut c, &session, "colour:").await;
    c.ok(
        "request/answer",
        json!({ "request": request["id"], "answer": { "value": "green" } }),
    )
    .await;
    c.wait_update(i, "idle again", |u| u["type"] == "state" && u["state"] == "idle")
        .await;
    screen_with(&mut c, &session, "Claude Code").await;
    let typed = std::fs::read_to_string(&keys).unwrap_or_default();
    assert!(typed.contains("green"), "{typed:?}");

    // Keys from a client reach the terminal: Escape, plain or in the kitty keyboard protocol
    // the cast turned on.
    let before = std::fs::read_to_string(&keys).unwrap_or_default().len();
    c.ok("session/keys", json!({ "session": session, "press": ["Escape"] }))
        .await;
    let deadline = Instant::now() + common::WAIT;
    let escaped = |all: &str| {
        let new = all.get(before..).unwrap_or_default();
        new == "\x1b" || new.contains("\x1b[27u")
    };
    while !escaped(&std::fs::read_to_string(&keys).unwrap_or_default()) {
        assert!(Instant::now() < deadline, "the key never arrived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    c.ok("session/stop", json!({ "session": session })).await;
}
