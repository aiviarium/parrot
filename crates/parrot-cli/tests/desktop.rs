//! The desktop functions (spec 29, E5) through `parrot serve`, with a fake `box` on `PATH`.
#![cfg(unix)]
// Writes a POSIX fake command and sets its file mode.

mod common;

use common::{Client, Serve, Setup};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

/// A `box` that logs its arguments and writes a screenshot file when asked for one.
const FAKE_BOX: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_BOX_LOG"
if [ "$2" = screenshot ]; then printf 'png' > "$4"; fi
"#;

#[tokio::test(flavor = "multi_thread")]
async fn desktop_functions_run_box_and_input_needs_an_approval() {
    let bin = tempfile::tempdir().unwrap();
    let fake = bin.path().join("box");
    std::fs::write(&fake, FAKE_BOX).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = bin.path().join("box.log");
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut env = common::fake_claude_env(&common::scenario("text-turn.jsonl"));
    env.push(("PATH".into(), path));
    env.push(("FAKE_BOX_LOG".into(), log.display().to_string()));
    let serve = Serve::start(Setup {
        env,
        ..Setup::default()
    });
    let mut c = Client::connect(&serve, "owner").await;
    c.initialize().await;
    let listed = c.ok("function/list", json!({})).await;
    let names: Vec<&str> = listed["functions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(names.contains(&"parrot.desktop.screenshot"), "{listed}");
    let keys = listed["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "parrot.desktop.keys")
        .unwrap();
    assert_eq!(keys["level"], "host");

    let shot = c
        .ok(
            "function/call",
            json!({ "function": "parrot.desktop.screenshot", "input": {} }),
        )
        .await;
    let uri = shot["output"]["uri"].as_str().expect("a uri");
    let file = uri.strip_prefix("file://").unwrap();
    assert_eq!(std::fs::read_to_string(file).unwrap(), "png");
    c.ok(
        "function/call",
        json!({ "function": "parrot.desktop.keys", "input": { "text": "hi", "press": ["Control", "a"] } }),
    )
    .await;
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains(r#"desktop type --args {"text":"hi"}"#),
        "{logged}"
    );
    assert!(
        logged.contains(r#"desktop key --args {"key":"ctrl+a"}"#),
        "{logged}"
    );

    // For a session, input at `host` is above its level: a request, then the result on retry.
    let work = serve.work_dir();
    let created = c
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work.to_string_lossy() } }),
        )
        .await;
    let session = created["session"]["id"].as_str().unwrap().to_string();
    c.wait_update(0, "idle", |u| u["type"] == "state" && u["state"] == "idle")
        .await;
    let call = json!({ "function": "parrot.desktop.pointer", "session": session, "key": "click-1",
        "timeout": 1, "input": { "action": "click", "x": 10, "y": 20 } });
    let waited = c.ok("function/call", call.clone()).await;
    assert_eq!(waited["error"]["code"], "pending_approval", "{waited}");
    let request = waited["error"]["request"].clone();
    assert!(!std::fs::read_to_string(&log).unwrap().contains("click"));
    let open = c.ok("request/list", json!({ "session": session })).await;
    assert_eq!(
        open["requests"][0]["subject"]["function"], "parrot.desktop.pointer",
        "{open}"
    );

    c.ok(
        "request/answer",
        json!({ "request": request, "answer": { "choice": "allow" } }),
    )
    .await;
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let r = c.ok("function/call", call.clone()).await;
        if r["error"]["code"] != "pending_approval" {
            assert!(r["error"].is_null(), "{r}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{r}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains(r#"desktop click --args {"x":10,"y":20}"#),
        "{logged}"
    );
}
