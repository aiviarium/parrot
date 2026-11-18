//! The rollout as a passive source: history from it, and shadow mode against the recorded trace.

use std::path::PathBuf;

use parrot_driver::correlate::{Fold, correlate, differences, notice};
use parrot_driver_codex::data::data;
use parrot_driver_codex::mapping::MapState;
use parrot_driver_codex::rollout;
use parrot_driver_codex::wire::{Message, ServerNotification};
use parrot_proto::request::Answer;
use parrot_proto::session::SessionDiscoverParams;
use serde_json::Value;

const THREAD: &str = "01a1edab-5a86-75f0-b42a-57127da3693a";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn rollout_text() -> String {
    std::fs::read_to_string(
        fixtures()
            .join("rollouts")
            .join(format!("rollout-2026-11-18T08-23-03-{THREAD}.jsonl")),
    )
    .unwrap()
}

/// The trace's app-server output through the live mapping, approving every request.
fn live() -> Vec<Value> {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../parrot-conform/fixtures/traces/codex-0.167.1.jsonl"),
    )
    .unwrap();
    let mut state = MapState::new("b1");
    let mut out = Vec::new();
    for line in text.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        if v["dir"] != "out" || v["msg"].get("method").is_none() {
            continue;
        }
        let updates = match Message::from_value(v["msg"].clone()).unwrap() {
            Message::Notification { method, params } => state.step(
                &ServerNotification::parse(&method, &params),
                "2026-11-18T08:23:03.000Z",
            ),
            Message::Request { id, method, params } => {
                let (mut u, _) = state.server_request(&id, &method, &params);
                let open = u
                    .iter()
                    .map(|u| serde_json::to_value(u).unwrap())
                    .find(|u| u["update"]["type"] == "request" && u["update"]["request"]["status"] == "open");
                if let Some(r) = open {
                    let key = r["update"]["request"]["id"].as_str().unwrap().to_owned();
                    let answer = Answer {
                        choice: Some("accept".into()),
                        ..Default::default()
                    };
                    u.extend(state.answer(&key, &answer).unwrap().1);
                }
                u
            }
            _ => vec![],
        };
        out.extend(updates.iter().map(|u| serde_json::to_value(u).unwrap()));
    }
    out
}

fn fold(updates: &[Value]) -> Fold {
    let mut f = Fold::new();
    f.extend(updates);
    f
}

fn passive(text: &str) -> Vec<Value> {
    rollout::updates("b1", text)
        .iter()
        .map(|u| serde_json::to_value(u).unwrap())
        .collect()
}

#[test]
fn rollout_matches_the_live_run() {
    let (live, disk) = (fold(&live()), fold(&passive(&rollout_text())));
    assert_eq!(live.turn_count(), 2);
    assert_eq!(disk.turn_count(), 2);
    let (c, p) = (live.turn(0), disk.turn(0));
    let pairs = correlate(&data().correlate, &c, &p);
    let diffs = differences(&data().correlate, &pairs, "native", "transcript");
    assert!(diffs.is_empty(), "{diffs:#?}");
    // Commands and messages pair by their ids, the user message by position.
    assert!(pairs.iter().all(|p| p.control.is_some() && p.passive.is_some()));
}

#[test]
fn shadow_reports_a_deliberate_difference() {
    // The rollout says the second command failed; the app-server said it exited 0.
    let text = rollout_text();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let i = lines
        .iter()
        .rposition(|l| l.contains("CommandExecution"))
        .unwrap();
    lines[i] = lines[i].replace(r#""exit_code":0"#, r#""exit_code":2"#);
    let (live, disk) = (fold(&live()), fold(&passive(&lines.join("\n"))));
    let (c, p) = (live.turn(0), disk.turn(0));
    let pairs = correlate(&data().correlate, &c, &p);
    let diffs = differences(&data().correlate, &pairs, "native", "transcript");
    let texts: Vec<_> = diffs.iter().map(|d| d.text.as_str()).collect();
    assert_eq!(texts.len(), 2, "{diffs:#?}");
    assert!(texts[0].ends_with("exit is 0 in the native and 2 in the transcript"));
    assert!(texts[1].ends_with(r#"status is "ok" in the native and "error" in the transcript"#));
    let n = notice("b1", &diffs[0]);
    assert_eq!(n["update"]["code"], "x.parrot/shadow");
}

#[test]
fn history_discovers_and_reads_rollouts() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join("2026/11/18");
    std::fs::create_dir_all(&day).unwrap();
    let name = format!("rollout-2026-11-18T08-23-03-{THREAD}.jsonl");
    std::fs::write(day.join(&name), rollout_text()).unwrap();
    let found = rollout::discover(
        root.path(),
        &SessionDiscoverParams {
            agent: "codex".into(),
            cwd: Some("/home/user/project".into()),
            cursor: None,
            limit: None,
        },
    );
    assert_eq!(found.found.len(), 1);
    assert_eq!(found.found[0].reference.id, THREAD);
    assert!(
        found.found[0]
            .title
            .as_deref()
            .unwrap()
            .starts_with("Name the color")
    );
    let none = rollout::discover(
        root.path(),
        &SessionDiscoverParams {
            agent: "codex".into(),
            cwd: Some("/elsewhere".into()),
            cursor: None,
            limit: None,
        },
    );
    assert!(none.found.is_empty());
    let path = rollout::find(root.path(), THREAD).unwrap();
    let r = rollout::read(&std::fs::read_to_string(path).unwrap(), None);
    let updates: Vec<Value> = r
        .updates
        .iter()
        .map(|u| serde_json::to_value(u).unwrap())
        .collect();
    assert!(updates.iter().all(|u| u["binding"] == ""));
    assert!(updates.iter().any(|u| u["update"]["ref"]["id"] == THREAD));
    let turns: Vec<_> = updates
        .iter()
        .filter(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .map(|u| u["update"]["turn"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(turns, ["completed", "interrupted"]);
    assert!(rollout::find(root.path(), "../x").is_none());
}
