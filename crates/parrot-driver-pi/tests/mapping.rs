//! The pure mapping: pi RPC events to driver updates.

use parrot_driver_pi::data::DriverData;
use parrot_driver_pi::mapping::MapState;
use parrot_proto::driver::DriverUpdateKind;
use parrot_proto::open::Open;
use parrot_proto::request::Answer;
use serde_json::{Value, json};

const NOW: &str = "2026-01-01T00:00:00.000Z";

fn state() -> MapState {
    MapState::new("b1", DriverData::default())
}

/// The updates one event line causes, as JSON.
fn step(state: &mut MapState, line: Value) -> Vec<Value> {
    state
        .step(&line, NOW)
        .into_iter()
        .map(|u| serde_json::to_value(u.update).unwrap())
        .collect()
}

#[test]
fn text_deltas_share_one_item() {
    let mut s = state();
    step(
        &mut s,
        json!({"type": "message_start", "message": {"role": "assistant", "content": []}}),
    );
    step(
        &mut s,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_start", "contentIndex": 0}}),
    );
    let delta = step(
        &mut s,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "Hi "}}),
    );
    let item = delta.iter().find(|u| u["type"] == "item").unwrap();
    assert_eq!(item["item"]["id"], "pi-1-0");
    assert_eq!(item["item"]["append"]["text"], "Hi ");
    let end = step(
        &mut s,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_end", "contentIndex": 0, "content": "Hi there"}}),
    );
    let item = end.iter().find(|u| u["type"] == "item").unwrap();
    assert_eq!(item["item"]["set"]["text"], "Hi there");
    assert_eq!(item["item"]["streaming"], false);
}

#[test]
fn tools_keep_pis_ids() {
    let mut s = state();
    let updates = step(
        &mut s,
        json!({"type": "tool_execution_start", "toolCallId": "call_9", "toolName": "bash", "args": {"command": "ls"}}),
    );
    let item = updates.iter().find(|u| u["type"] == "item").unwrap();
    assert_eq!(item["item"]["id"], "call_9");
    assert_eq!(item["item"]["kind"], "tool");
    assert_eq!(item["item"]["set"]["title"], "bash: ls");
    assert_eq!(item["item"]["set"]["status"], "running");
    let updates = step(
        &mut s,
        json!({"type": "tool_execution_end", "toolCallId": "call_9", "result": {"content": [{"type": "text", "text": "README.md"}]}, "isError": false}),
    );
    let item = updates.iter().find(|u| u["type"] == "item").unwrap();
    assert_eq!(item["item"]["id"], "call_9");
    assert_eq!(item["item"]["set"]["status"], "ok");
    assert_eq!(item["item"]["set"]["output"], "README.md");
}

#[test]
fn a_confirm_dialog_becomes_an_approval_and_an_answer() {
    let mut s = state();
    let updates = step(
        &mut s,
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "confirm", "title": "Run it?", "message": "An extension asks"}),
    );
    let request = updates.iter().find(|u| u["type"] == "request").unwrap();
    assert_eq!(request["request"]["id"], "pi-ui-ui-1");
    assert_eq!(request["request"]["kind"], "approval");
    assert_eq!(request["request"]["level"], "host");
    assert_eq!(request["request"]["choices"][1]["intent"], "deny");
    assert!(updates.iter().any(|u| u["state"] == "requires_action"));
    let (line, updates) = s
        .answer(
            "pi-ui-ui-1",
            &Answer {
                choice: Some("allow".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        line,
        json!({"type": "extension_ui_response", "id": "ui-1", "confirmed": true})
    );
    let answered: Vec<Value> = updates
        .iter()
        .map(|u| serde_json::to_value(&u.update).unwrap())
        .collect();
    let request = answered.iter().find(|u| u["type"] == "request").unwrap();
    assert_eq!(request["request"]["status"], "answered");
    assert!(s.requests.is_empty());
}

#[test]
fn a_select_answer_picks_the_label() {
    let mut s = state();
    step(
        &mut s,
        json!({"type": "extension_ui_request", "id": "ui-2", "method": "select", "title": "Pick", "options": ["A", "B"]}),
    );
    let (line, _) = s
        .answer(
            "pi-ui-ui-2",
            &Answer {
                choice: Some("option-1".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(line["value"], "B");
    assert!(line.get("cancelled").is_none());
}

#[test]
fn settling_ends_the_turn() {
    let mut s = state();
    step(&mut s, json!({"type": "agent_start"}));
    let updates = step(&mut s, json!({"type": "agent_settled", "aborted": true}));
    let turn = updates.iter().find(|u| u["type"] == "turn").unwrap();
    assert_eq!(turn["turn"]["status"], "interrupted");
    assert_eq!(turn["turn"]["stop"], "cancelled");
    assert_eq!(turn["turn"]["continuable"], true);
    assert!(updates.iter().any(|u| u["state"] == "idle"));
}

#[test]
fn unknown_events_are_kept_as_frames() {
    let mut s = state();
    let updates = step(&mut s, json!({"type": "x.acme/new", "detail": 4}));
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["type"], "frame");
    assert_eq!(updates[0]["source"], "pi");
    assert_eq!(updates[0]["data"]["detail"], 4);
}

#[test]
fn usage_adds_up_and_names_the_context() {
    let mut s = state();
    s.models = vec![parrot_driver_pi::wire::Model {
        id: "fake-1".into(),
        provider: "fake".into(),
        context_window: Some(1000),
        ..Default::default()
    }];
    s.model = Some("fake/fake-1".into());
    step(
        &mut s,
        json!({"type": "message_end", "message": {"role": "assistant", "content": [], "stopReason": "stop",
            "usage": {"input": 10, "output": 5, "totalTokens": 15, "cost": {"total": 0.5}}}}),
    );
    let updates = step(
        &mut s,
        json!({"type": "message_end", "message": {"role": "assistant", "content": [], "stopReason": "stop",
            "usage": {"input": 2, "output": 1, "totalTokens": 3, "cost": {"total": 0.25}}}}),
    );
    let usage = updates.iter().find(|u| u["type"] == "usage").unwrap();
    assert_eq!(usage["usage"]["tokens"]["input"], 12);
    assert_eq!(usage["usage"]["tokens"]["output"], 6);
    assert_eq!(usage["usage"]["cost"]["amount"], 0.75);
    assert_eq!(usage["usage"]["context"]["used"], 3);
    assert_eq!(usage["usage"]["context"]["window"], 1000);
}

#[test]
fn an_error_ends_the_turn_failed() {
    let mut s = state();
    step(&mut s, json!({"type": "agent_start"}));
    step(
        &mut s,
        json!({"type": "message_end", "message": {"role": "assistant", "content": [], "stopReason": "error", "errorMessage": "Overloaded"}}),
    );
    let updates = step(&mut s, json!({"type": "agent_settled", "aborted": false}));
    let turn = updates.iter().find(|u| u["type"] == "turn").unwrap();
    assert_eq!(turn["turn"]["status"], "failed");
    assert_eq!(turn["turn"]["error"]["message"], "Overloaded");
    assert_eq!(turn["turn"]["error"]["retryable"], true);
}

#[test]
fn a_handled_prompt_starts_no_turn() {
    let mut s = state();
    let params = parrot_proto::driver::BindingPromptParams {
        binding: "b1".into(),
        subagent: None,
        message: "m1".into(),
        text: "/status".into(),
        context: None,
        attachments: None,
        steer: false,
    };
    let updates = s.prompt(&params, parrot_proto::driver::BindingDisposition::Handled, NOW);
    assert!(s.turn.is_none());
    assert!(
        updates
            .iter()
            .all(|u| !matches!(&u.update, Open::Known(DriverUpdateKind::Turn { .. })))
    );
    let handled = s.handled(&"m1".to_string(), NOW);
    let item: Vec<_> = handled
        .iter()
        .filter(|u| matches!(&u.update, Open::Known(DriverUpdateKind::Item { .. })))
        .collect();
    assert_eq!(item.len(), 1);
}
