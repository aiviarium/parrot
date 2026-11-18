//! The `core` profile proof (spec 40) in one run of a real `parrot serve` over `fake-claude`:
//! two clients on one session, one prompts and disconnects while a request is open, the other
//! answers it; models are discovered with `agent/list` and `agent/refresh` and set with
//! `option/set`; after a crash and a restart nothing runs until asked.
#![cfg(unix)]
// Looks for the harness process with pgrep.

mod common;

use common::{Client, Serve, Setup, fake_claude, scenario};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Whether a process whose command line contains `needle` runs.
fn running(needle: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", needle])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn setup(bin: &Path, scenario: &Path) -> Setup {
    Setup {
        env: vec![
            ("PARROT_CLAUDE_BIN".into(), bin.display().to_string()),
            (
                "PARROT_FAKE_CLAUDE_SCENARIO".into(),
                scenario.display().to_string(),
            ),
        ],
        ..Default::default()
    }
}

fn model_value(options: &Value) -> Value {
    options
        .as_array()
        .and_then(|o| o.iter().find(|o| o["role"] == "model"))
        .map(|o| o["value"].clone())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn two_clients_share_a_session_and_a_restart_leaves_nothing_running() {
    // A copy of the fake under a unique path, so `pgrep` finds this test's harness only.
    let dir = tempfile::tempdir().unwrap();
    let marked = dir.path().join("fake-claude-core-proof");
    std::fs::copy(fake_claude(), &marked).unwrap();
    let scenario = scenario("bash-allowed.jsonl");
    let serve = Serve::start(setup(&marked, &scenario));
    let work = serve.work_dir();

    let mut a = Client::connect(&serve, "client-a").await;
    let init = a.initialize().await;
    assert!(init["features"].get("profile.core").is_some(), "{init}");
    let mut b = Client::connect(&serve, "client-b").await;
    b.initialize().await;

    // Models are discovered from the agent.
    let agents = b.ok("agent/list", json!({})).await;
    let claude = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "claude")
        .cloned()
        .unwrap_or_else(|| panic!("no claude agent: {agents}"));
    assert!(claude["usable"] == true, "{claude}");
    let refreshed = b.ok("agent/refresh", json!({ "agent": "claude" })).await;
    let model = refreshed["agent"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["role"] == "model")
        .cloned()
        .unwrap_or_else(|| panic!("no model option: {refreshed}"));
    // Before a binding has asked the harness, the agent lists the models it knows of.
    let choice = model["schema"]["examples"][0].clone();
    assert_eq!(choice, "default", "{model}");

    let created = a
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work }, "title": "Core proof" }),
        )
        .await;
    let session = created["session"]["id"].as_str().unwrap().to_string();
    let sub = b
        .ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    assert_eq!(sub["snapshot"]["sessions"][0]["session"]["id"], session.as_str());

    // A prompts, sees the approval request, and goes away while it is open.
    a.ok("session/prompt", json!({ "session": session, "text": "run ls" }))
        .await;
    a.wait_update(0, "the request on a", |u| {
        u["type"] == "request" && u["request"]["status"] == "open"
    })
    .await;
    drop(a);

    // B sees the same request and answers it; the turn finishes.
    let i = b
        .wait_update(0, "the request on b", |u| {
            u["type"] == "request" && u["request"]["status"] == "open"
        })
        .await;
    let request = b.update(i)["request"].clone();
    b.ok(
        "request/answer",
        json!({ "request": request["id"], "answer": { "choice": "allow" } }),
    )
    .await;
    b.wait_update(i, "the turn's end", |u| {
        u["type"] == "turn" && u["turn"]["status"] == "completed"
    })
    .await;
    let requests = b.ok("request/list", json!({ "session": session })).await;
    assert_eq!(requests["requests"][0]["status"], "answered", "{requests}");
    let read = b.ok("session/read", json!({ "session": session })).await;
    let items = read["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["kind"] == "tool"), "{read}");
    assert!(
        items
            .iter()
            .any(|i| i["kind"] == "assistant" && i["text"] == "There are two entries."),
        "{read}"
    );

    // The binding reports the harness's own models; B sets one of them.
    let snap = b
        .ok("subscribe", json!({ "target": { "session": session } }))
        .await;
    let options = &snap["snapshot"]["sessions"][0]["options"];
    let offered: Vec<&Value> = options
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["role"] == "model")
        .and_then(|o| o["schema"]["oneOf"].as_array())
        .map(|c| c.iter().map(|c| &c["const"]).collect())
        .unwrap_or_default();
    assert!(offered.contains(&&choice), "{options}");
    let set = b
        .ok(
            "option/set",
            json!({ "session": session, "values": { "model": choice } }),
        )
        .await;
    assert_eq!(model_value(&set["options"]), choice, "{set}");
    let listed = b.ok("option/list", json!({ "session": session })).await;
    assert_eq!(model_value(&listed["options"]), choice, "{listed}");
    assert!(
        running(marked.to_str().unwrap()),
        "the harness runs before the crash"
    );

    // A crash takes the harness with it, and a restart starts nothing.
    drop(b);
    let dir_back = serve.kill();
    let deadline = std::time::Instant::now() + common::WAIT;
    while running(marked.to_str().unwrap()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the harness outlived the runtime"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let serve = Serve::start_in(dir_back, setup(&marked, &scenario));
    let mut c = Client::connect(&serve, "client-c").await;
    c.initialize().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = c.ok("session/get", json!({ "session": session })).await;
    assert_eq!(got["session"]["state"], "stopped", "{got}");
    assert_eq!(got["session"]["binding"]["status"], "stopped", "{got}");
    assert!(!running(marked.to_str().unwrap()), "a harness started by itself");
    let read = c.ok("session/read", json!({ "session": session })).await;
    assert!(
        read["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["text"] == "run ls"),
        "{read}"
    );
}
