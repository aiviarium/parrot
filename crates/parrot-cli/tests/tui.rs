//! TUI control in the runtime (spec 32.2, E4): `parrot serve` drives the fake TUI of
//! `parrot-driver-tui`, and a client answers a dialog no profile knows.
#![cfg(unix)]

mod common;

use common::{Client, Serve, Setup};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parrot-driver-tui/tests/fake")
        .join(name)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_dialog_is_answered_from_a_client() {
    let transcripts = tempfile::tempdir().unwrap();
    let t = transcripts.path().to_string_lossy().into_owned();
    let config = json!({ "tui": { "fake-tui": {
        "command": ["sh", fake("fake-tui.sh")],
        "profile": fake("fake-tui.toml"),
        "env": { "FAKE_TUI_TRANSCRIPT": t },
        "transcript": t,
        "unrecognisedAfterMs": 800,
        "size": [12, 60],
        "noGuard": true,
    } } });
    let serve = Serve::start(Setup {
        config: Some(config),
        ..Setup::default()
    });
    let mut c = Client::connect(&serve, "phone").await;
    c.initialize().await;
    let agents = c.ok("agent/list", json!({})).await;
    let listed = agents["agents"].as_array().unwrap();
    assert!(
        listed
            .iter()
            .any(|a| a["id"] == "fake-tui" && a["controls"][0] == "tui"),
        "{agents}"
    );

    let work = serve.work_dir();
    let created = c
        .ok(
            "session/new",
            json!({
                "agent": "fake-tui",
                "control": "tui",
                "workspace": { "kind": "dir", "path": work.to_string_lossy() },
            }),
        )
        .await;
    let session = created["session"]["id"].as_str().unwrap().to_string();
    c.wait_update(0, "idle", |u| u["type"] == "state" && u["state"] == "idle")
        .await;

    // The runtime serves the live screen.
    let screen = c.ok("session/screen", json!({ "session": session })).await;
    assert!(
        screen["text"].as_str().unwrap().contains("Type a prompt"),
        "{screen}"
    );

    let from = c.seen.len();
    c.ok("session/prompt", json!({ "session": session, "text": "mystery" }))
        .await;
    let i = c
        .wait_update(from, "the unknown dialog", |u| {
            u["type"] == "request" && u["request"]["status"] == "open"
        })
        .await;
    let request: Value = c.update(i)["request"].clone();
    assert_eq!(request["kind"], "input", "{request}");
    assert!(
        request["description"].as_str().unwrap().contains("Pick a colour"),
        "{request}"
    );
    let screen = c.ok("session/screen", json!({ "session": session })).await;
    assert!(screen["text"].as_str().unwrap().contains("colour:"), "{screen}");

    let from = c.seen.len();
    c.ok(
        "request/answer",
        json!({ "request": request["id"], "answer": { "value": "green" } }),
    )
    .await;
    c.wait_update(from, "the turn's end", |u| {
        u["type"] == "turn" && u["turn"]["status"] == "completed"
    })
    .await;
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let screen = c.ok("session/screen", json!({ "session": session })).await;
        if screen["text"].as_str().unwrap().contains("You picked green.") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{screen}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Keys from a client reach the terminal.
    c.ok(
        "session/keys",
        json!({ "session": session, "text": "hello", "press": ["Enter"] }),
    )
    .await;
    c.wait_update(from, "the keyed turn", |u| {
        u["type"] == "state" && u["state"] == "running"
    })
    .await;
    c.ok("session/stop", json!({ "session": session })).await;
}
