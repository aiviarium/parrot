//! The mapping on the recorded Claude Code trace (`parrot-conform/fixtures/traces`): the real
//! CLI's lines, played as the driver would play them.

use std::path::PathBuf;

use parrot_driver_claude::fake::{Step, load_scenario};
use parrot_driver_claude::mapping::{MapState, map};
use parrot_driver_claude::wire;
use parrot_proto::driver::BindingPromptParams;
use parrot_proto::request::Answer;
use serde_json::Value;

const NOW: &str = "2026-01-01T00:00:00.000Z";

pub fn trace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../parrot-conform/fixtures/traces/claude-2.1.327.jsonl")
}

fn play() -> Vec<Value> {
    let steps = load_scenario(&trace()).unwrap();
    let mut state = MapState::new("b1");
    let mut updates = Vec::new();
    let mut prompts = 0;
    for step in steps {
        match step {
            Step::Expect(p) if p["type"] == "user" => {
                prompts += 1;
                let params = BindingPromptParams {
                    binding: "b1".into(),
                    subagent: None,
                    message: format!("m{prompts}"),
                    text: "prompt".into(),
                    context: None,
                    attachments: None,
                    steer: false,
                };
                let (u, _) = state.prompt(&params, NOW);
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
            Step::Expect(p) if p["request"]["subtype"] == "interrupt" => state.cancel(),
            Step::Expect(_) | Step::Exit(_) => {}
            Step::Choose(_) | Step::McpCall { .. } | Step::Shell(_) => {}
            Step::Emit(m) => {
                if m["type"] == "control_response" && m["response"]["response"]["models"].is_array() {
                    state.initialized(&m["response"]["response"]);
                    continue;
                }
                let msg = wire::parse_line(&m.to_string()).unwrap();
                let (u, next) = map(&state, &msg, NOW);
                state = next;
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
            Step::AwaitControlResponse { request_id, .. } => {
                let answer = Answer {
                    choice: Some("allow".into()),
                    ..Default::default()
                };
                let (_, u) = state.answer(&request_id, &answer).unwrap();
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
        }
    }
    updates
}

fn of_type<'a>(updates: &'a [Value], t: &str) -> Vec<&'a Value> {
    updates
        .iter()
        .filter(|u| u["update"]["type"] == t)
        .map(|u| &u["update"])
        .collect()
}

#[test]
fn every_line_parses_as_a_known_message() {
    let text = std::fs::read_to_string(trace()).unwrap();
    for line in text.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        if v["dir"] != "out" {
            continue;
        }
        let msg = wire::parse_line(&v["msg"].to_string()).unwrap();
        let t = v["msg"]["type"].as_str().unwrap();
        // Status, rate-limit and thinking-token lines carry nothing an item needs.
        if matches!(t, "rate_limit_event")
            || v["msg"]["subtype"] == "status"
            || v["msg"]["subtype"] == "thinking_tokens"
        {
            continue;
        }
        assert!(
            matches!(msg, parrot_proto::open::Open::Known(_)),
            "unknown message {}",
            &line[..line.len().min(200)]
        );
    }
}

#[test]
fn trace_maps_to_items_tools_requests_and_turns() {
    let updates = play();
    let turns: Vec<_> = of_type(&updates, "turn")
        .into_iter()
        .filter(|t| t["turn"]["status"] != "running")
        .collect();
    assert_eq!(turns.len(), 2, "two turns end");
    assert_eq!(turns[0]["turn"]["status"], "completed");
    assert_eq!(turns[1]["turn"]["status"], "interrupted");

    let requests = of_type(&updates, "request");
    let opened: Vec<_> = requests
        .iter()
        .filter(|r| r["request"]["status"] == "open")
        .collect();
    assert_eq!(opened.len(), 1, "one permission request (Read outside the cwd)");
    assert_eq!(opened[0]["request"]["subject"]["tool"], "Read");
    assert_eq!(opened[0]["request"]["item"], "toolu_01Hk9hZZXtjdaoDcb2Gc4rgp");
    assert!(
        opened[0]["request"]["choices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["scope"] == "session"),
        "the session suggestion is a choice"
    );

    let items = of_type(&updates, "item");
    for tool in ["toolu_01Hk9hZZXtjdaoDcb2Gc4rgp", "toolu_013EQ9AxUpG6gVLMW4DBHeDN"] {
        let last = items
            .iter()
            .rfind(|i| i["item"]["id"] == tool && i["item"]["set"]["status"].is_string())
            .unwrap();
        assert_eq!(last["item"]["set"]["status"], "ok", "{tool} ends ok");
    }
    // The CLI's interruption marker is not a user message.
    assert!(
        !items
            .iter()
            .any(|i| i["item"]["set"]["text"] == "[Request interrupted by user]"),
        "the interrupt marker is no item"
    );

    // Usage: cost is the CLI's total, the context is the last request's, not the sum of all.
    let usage = of_type(&updates, "usage");
    let first = &usage[0]["usage"];
    assert!(first["cost"]["amount"].as_f64().unwrap() > 0.0);
    let used = first["context"]["used"].as_u64().unwrap();
    assert!(used < 50_000, "context used is one request's, got {used}");
    assert_eq!(first["context"]["window"], 1_000_000);

    let refs = of_type(&updates, "ref");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["ref"]["id"], "1047c330-5f7f-46a0-9341-bc7d57588a73");
}

/// The image block the driver builds has the shape Claude Code accepted in the trace.
#[test]
fn image_blocks_match_the_trace() {
    let text = std::fs::read_to_string(trace()).unwrap();
    let sent: Value = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["dir"] == "in" && v["msg"]["type"] == "user")
        .unwrap();
    let recorded = &sent["msg"]["message"]["content"][0];
    let data = recorded["source"]["data"].as_str().unwrap();
    let att: parrot_proto::prompt::Attachment = serde_json::from_value(serde_json::json!({
        "name": "red.png", "type": "image/png", "data": data
    }))
    .unwrap();
    let input = parrot_driver_claude::input::content("Name the color", &[], &[att]);
    assert!(input.skipped.is_empty());
    let image = input.blocks.iter().find(|b| b["type"] == "image").unwrap();
    assert_eq!(image, recorded);
    // A kind Claude Code takes no block for is named in a notice, not dropped silently.
    let odd: parrot_proto::prompt::Attachment = serde_json::from_value(serde_json::json!({
        "name": "clip.mp4", "type": "video/mp4", "data": "AAAA"
    }))
    .unwrap();
    let input = parrot_driver_claude::input::content("x", &[], &[odd]);
    assert_eq!(input.skipped.len(), 1, "{:?}", input.blocks);
}
