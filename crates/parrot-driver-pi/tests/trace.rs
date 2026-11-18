//! The mapping on the recorded pi 1.5.1 trace (`parrot-conform/fixtures/traces`): the real CLI's
//! lines, played as the driver would play them.

use std::path::PathBuf;

use parrot_driver_pi::data::DriverData;
use parrot_driver_pi::mapping::{MapState, map};
use parrot_driver_pi::wire::{self, LevelList, ModelList};
use parrot_proto::driver::BindingPromptParams;
use parrot_proto::open::Open;
use parrot_proto::prompt::Attachment;
use serde_json::Value;

const NOW: &str = "2026-01-01T00:00:00.000Z";

fn trace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../parrot-conform/fixtures/traces/pi-1.5.1.jsonl")
}

fn records() -> Vec<Value> {
    std::fs::read_to_string(trace())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The data of the first answer to `command`, or an empty object.
fn answer(records: &[Value], command: &str) -> Value {
    records
        .iter()
        .find(|r| r["dir"] == "out" && r["msg"]["command"] == command)
        .map(|r| r["msg"]["data"].clone())
        .unwrap_or(Value::Null)
}

/// Plays the recorded lines through the mapping: commands as the driver sends them, events as they
/// arrive.
fn play() -> Vec<Value> {
    let records = records();
    let models: ModelList = serde_json::from_value(answer(&records, "get_available_models")).unwrap();
    let levels: LevelList =
        serde_json::from_value(answer(&records, "get_available_thinking_levels")).unwrap();
    let commands: wire::CommandList = serde_json::from_value(answer(&records, "get_commands")).unwrap();
    let mut state = MapState::new("b1", DriverData::default());
    let mut updates = Vec::new();
    let mut prompts = 0;
    for record in records {
        let msg = &record["msg"];
        if record["dir"] == "in" {
            match msg["type"].as_str() {
                Some("prompt") => {
                    prompts += 1;
                    let images: Vec<Attachment> = msg["images"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|image| Attachment {
                            name: "image.png".into(),
                            media_type: image["mimeType"].as_str().unwrap_or("image/png").into(),
                            data: image["data"].as_str().map(str::to_owned),
                            uri: None,
                        })
                        .collect();
                    let params = BindingPromptParams {
                        binding: "b1".into(),
                        subagent: None,
                        message: format!("m{prompts}"),
                        text: msg["message"].as_str().unwrap_or_default().into(),
                        context: None,
                        attachments: (!images.is_empty()).then_some(images),
                        steer: false,
                    };
                    let u = state.prompt(&params, parrot_proto::driver::BindingDisposition::Started, NOW);
                    updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
                }
                Some("abort") => state.cancel(),
                _ => {}
            }
            continue;
        }
        if msg["type"] == "response" {
            if msg["command"] == "get_state" && state.session_id.is_none() {
                let session: wire::SessionState = serde_json::from_value(msg["data"].clone()).unwrap();
                let u = state.initialized(&session, &models.models, &levels.levels, &commands.commands);
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
            continue;
        }
        let (u, next) = map(&state, msg, NOW);
        state = next;
        updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
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
fn every_line_pi_sends_is_known() {
    for record in records() {
        if record["dir"] != "out" {
            continue;
        }
        let msg = wire::parse_inbound(&record["msg"].to_string()).unwrap();
        assert!(
            matches!(msg, Open::Known(_)),
            "unknown message {}",
            &record["msg"].to_string()[..200.min(record["msg"].to_string().len())]
        );
    }
}

#[test]
fn the_trace_maps_to_items_tools_usage_and_turns() {
    let updates = play();
    let ended: Vec<&Value> = of_type(&updates, "turn")
        .into_iter()
        .filter(|t| t["turn"]["status"] != "running")
        .collect();
    assert_eq!(ended.len(), 4, "four turns end: {:?}", ended);
    assert_eq!(ended[0]["turn"]["status"], "completed");
    assert_eq!(ended[1]["turn"]["status"], "completed");
    assert_eq!(ended[2]["turn"]["status"], "completed");
    assert_eq!(ended[3]["turn"]["status"], "interrupted");
    assert_eq!(ended[3]["turn"]["stop"], "cancelled");

    // The reply streams as deltas and the message end is authoritative.
    let items = of_type(&updates, "item");
    assert!(
        items.iter().any(|i| i["item"]["kind"] == "assistant"
            && i["item"]["streaming"] == true
            && i["item"]["set"]["text"] == ""),
        "a text block starts empty and streams"
    );
    let streamed = items
        .iter()
        .find(|i| i["item"]["append"]["text"] == "Let me read ")
        .expect("a text delta");
    assert_eq!(streamed["item"]["id"], "pi-1-0");
    assert!(
        items
            .iter()
            .any(|i| i["item"]["set"]["text"] == "Let me read that file.\n"),
        "the final message sets the text"
    );

    // A tool keeps pi's own call id, and its result carries pi's duration.
    let tool = items
        .iter()
        .find(|i| i["item"]["id"] == "call_stub_1" && i["item"]["set"]["durationMs"] == 5)
        .expect("the read tool result with its duration");
    assert_eq!(tool["item"]["set"]["output"], "hello\n");
    assert_eq!(tool["item"]["set"]["status"], "ok");

    // Usage is the message's own tokens, and the window comes from the model list.
    let usage = &of_type(&updates, "usage")[0]["usage"];
    assert_eq!(usage["tokens"]["input"], 136_000);
    assert_eq!(usage["tokens"]["output"], 1_500);
    assert_eq!(usage["tokens"]["cacheRead"], 64_000);
    assert_eq!(usage["context"]["used"], 201_500);
    assert_eq!(usage["context"]["window"], 1_000_000);

    // The second prompt carried an image, which the user item records.
    let image = items
        .iter()
        .find(|i| i["item"]["set"]["attachments"].is_array())
        .expect("the image prompt");
    assert_eq!(image["item"]["set"]["attachments"][0]["type"], "image/png");

    let refs = of_type(&updates, "ref");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["ref"]["id"], "01a1eeb0-0000-4000-8000-000000000001");
}

#[test]
fn a_manual_compaction_becomes_one_item_and_a_failure_a_notice() {
    let updates = play();
    let compactions: Vec<&Value> = of_type(&updates, "item")
        .into_iter()
        .filter(|i| i["item"]["kind"] == "compaction")
        .collect();
    assert_eq!(compactions.len(), 1, "one compaction item: {compactions:?}");
    let compaction = &compactions[0]["item"]["set"];
    assert_eq!(compaction["trigger"], "manual");
    assert_eq!(compaction["tokensBefore"], 201_500);
    assert_eq!(compaction["tokensAfter"], 2_564);
    assert!(
        compaction["summary"].as_str().unwrap().contains("The user asked"),
        "the summary is kept: {compaction}"
    );

    // The second compaction was refused; pi says so in `compaction_end`, and the driver warns.
    let notices = of_type(&updates, "notice");
    let failed = notices
        .iter()
        .find(|n| n["code"] == "compaction_failed")
        .expect("a notice for the refused compaction");
    assert!(
        failed["text"].as_str().unwrap().contains("Already compacted"),
        "the notice names the reason: {failed}"
    );
}

#[test]
fn a_thinking_level_change_updates_the_effort_option() {
    let updates = play();
    // The session opened on `medium`; `set_thinking_level low` reports the new level.
    let options = of_type(&updates, "options");
    let low = options
        .iter()
        .find(|o| {
            o["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["id"] == "effort" && o["value"] == "low")
        })
        .expect("an options update carrying the new level");
    let effort = low["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "effort")
        .unwrap();
    assert_eq!(
        effort["schema"]["enum"],
        serde_json::json!(["minimal", "low", "medium", "high"]),
        "the levels pi reported, without off"
    );
}
