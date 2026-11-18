//! The mapping on lines shaped as Codex CLI 0.167.1 sent them, without processes.

use parrot_driver_codex::mapping::{self, MapState, level_of};
use parrot_driver_codex::wire::{Message, RequestId, ServerNotification, short_command};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams, Pin};
use parrot_proto::option::PermissionLevel;
use parrot_proto::request::Answer;
use serde_json::{Value, json};

const T: &str = "01a1de18-b7f0-78a0-aa1a-9082ae1a8fe6";
const U: &str = "01a1de18-b809-7723-900b-c585bea1163d";
const NOW: &str = "2026-01-02T03:04:05.000Z";

fn feed(state: &mut MapState, line: Value) -> Vec<Value> {
    let updates = match Message::from_value(line).unwrap() {
        Message::Notification { method, params } => {
            state.step(&ServerNotification::parse(&method, &params), NOW)
        }
        Message::Request { id, method, params } => state.server_request(&id, &method, &params).0,
        other => panic!("not fed: {other:?}"),
    };
    updates
        .iter()
        .map(|u| serde_json::to_value(u).unwrap()["update"].clone())
        .collect()
}

fn running() -> MapState {
    let mut s = MapState::new("b1");
    s.thread_id = Some(T.into());
    s.turn_started(U, None, NOW);
    s
}

fn prompt(message: &str, text: &str) -> BindingPromptParams {
    serde_json::from_value(json!({"binding": "b1", "message": message, "text": text, "steer": false}))
        .unwrap()
}

#[test]
fn reasoning_shows_only_once_it_has_text() {
    let mut s = running();
    let empty = json!({"type": "reasoning", "id": "rs_1", "summary": [], "content": []});
    assert!(
        feed(
            &mut s,
            json!({"method": "item/started", "params": {"item": empty, "threadId": T, "turnId": U}})
        )
        .is_empty()
    );
    assert!(
        feed(
            &mut s,
            json!({"method": "item/completed", "params": {"item": empty, "threadId": T, "turnId": U}})
        )
        .is_empty()
    );
    let first = feed(
        &mut s,
        json!({"method": "item/reasoning/summaryTextDelta", "params": {"itemId": "rs_2", "delta": "Looking", "threadId": T, "turnId": U}}),
    );
    assert_eq!(first[0]["item"]["kind"], "reasoning");
    assert_eq!(first[0]["item"]["set"]["text"], "Looking");
    let done = feed(
        &mut s,
        json!({"method": "item/completed", "params": {"item": {"type": "reasoning", "id": "rs_2", "summary": ["Looking around"], "content": []}, "threadId": T, "turnId": U}}),
    );
    assert_eq!(done[0]["item"]["set"]["text"], "Looking around");
    assert_eq!(done[0]["item"]["set"]["summary"], true);
    assert_eq!(done[0]["item"]["streaming"], false);
}

#[test]
fn a_file_change_denial() {
    let mut s = running();
    feed(
        &mut s,
        json!({"method": "item/started", "params": {"item": {"type": "fileChange", "id": "patch-1", "status": "inProgress",
            "changes": [{"path": "src/a.rs", "kind": {"type": "update", "move_path": null}, "diff": "@@"},
                        {"path": "b.txt", "kind": {"type": "add"}, "diff": "+b"}]}, "threadId": T, "turnId": U}}),
    );
    let out = feed(
        &mut s,
        json!({"method": "item/fileChange/requestApproval", "id": 4, "params": {"threadId": T, "turnId": U, "itemId": "patch-1", "startedAtMs": 1}}),
    );
    let r = &out[0]["request"];
    assert_eq!(r["id"], "req-4");
    assert_eq!(r["level"], "write");
    assert_eq!(
        r["subject"],
        json!({"type": "files", "paths": ["src/a.rs", "b.txt"]})
    );
    assert_eq!(out[1]["state"], "requires_action");
    let (msg, updates) = s
        .answer(
            "req-4",
            &Answer {
                choice: Some("decline".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        msg,
        Message::Response {
            id: RequestId::Num(4),
            result: json!({"decision": "decline"})
        }
    );
    let u = serde_json::to_value(&updates[1]).unwrap();
    assert_eq!(u["update"]["state"], "running");
}

#[test]
fn file_changes_map_to_ops() {
    let mut s = running();
    let out = feed(
        &mut s,
        json!({"method": "item/completed", "params": {"item": {"type": "fileChange", "id": "p", "status": "completed",
            "changes": [{"path": "old.rs", "kind": {"type": "update", "move_path": "new.rs"}, "diff": ""},
                        {"path": "gone.rs", "kind": {"type": "delete"}}]}, "threadId": T, "turnId": U}}),
    );
    let item = &out[0]["item"];
    assert_eq!(item["kind"], "file_change");
    assert_eq!(item["set"]["status"], "ok");
    assert_eq!(
        item["set"]["changes"][0],
        json!({"path": "new.rs", "op": "move", "from": "old.rs", "diff": ""})
    );
    assert_eq!(
        item["set"]["changes"][1],
        json!({"path": "gone.rs", "op": "delete"})
    );
}

#[test]
fn a_question_from_request_user_input() {
    let mut s = running();
    let out = feed(
        &mut s,
        json!({"method": "item/tool/requestUserInput", "id": 5, "params": {"threadId": T, "turnId": U, "itemId": "q",
            "questions": [{"id": "q1", "header": "H", "question": "Tea?", "isOther": false, "isSecret": false,
                           "options": [{"label": "Tea", "description": ""}, {"label": "Coffee", "description": "Strong"}]}],
            "isBlocking": true}}),
    );
    let r = &out[0]["request"];
    assert_eq!(r["kind"], "question");
    assert_eq!(r["title"], "Tea?");
    assert_eq!(r["input"]["properties"]["q1"]["oneOf"][1]["const"], "Coffee");
    assert_eq!(r["choices"][1]["description"], "Strong");
    let (msg, _) = s
        .answer(
            "req-5",
            &Answer {
                choice: Some("option-0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        msg.to_value(),
        json!({"id": 5, "result": {"answers": {"q1": {"answers": ["Tea"]}}}})
    );
}

#[test]
fn a_failed_turn_ends_with_its_error_and_withdraws_requests() {
    let mut s = running();
    feed(
        &mut s,
        json!({"method": "item/commandExecution/requestApproval", "id": 1, "params": {"threadId": T, "turnId": U, "itemId": "e", "command": "ls"}}),
    );
    let out = feed(
        &mut s,
        json!({"method": "turn/completed", "params": {"threadId": T, "turn": {"id": U, "status": "failed",
            "error": {"message": "Overloaded", "codexErrorInfo": "serverOverloaded"}}}}),
    );
    let withdrawn = out.iter().find(|u| u["type"] == "request").unwrap();
    assert_eq!(withdrawn["request"]["status"], "cancelled");
    let turn = out.iter().find(|u| u["type"] == "turn").unwrap();
    assert_eq!(turn["turn"]["status"], "failed");
    assert_eq!(turn["turn"]["error"]["message"], "Overloaded");
    assert_eq!(turn["turn"]["error"]["code"], "serverOverloaded");
    assert_eq!(out.last().unwrap()["state"], "idle");
    assert!(s.requests.is_empty());
}

#[test]
fn unknown_server_requests_get_an_error() {
    let mut s = running();
    let (updates, reply) = s.server_request(&RequestId::Num(9), "item/tool/call", &json!({}));
    assert!(updates.is_empty());
    let reply = reply.unwrap().to_value();
    assert_eq!(reply["id"], 9);
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
fn the_plan_and_an_echo() {
    let mut s = MapState::new("b1");
    let p = prompt("m1", "Plan it");
    let out = s.prompt_accepted(&p, Some(U), BindingDisposition::Started, NOW);
    let out: Vec<Value> = out
        .iter()
        .map(|u| serde_json::to_value(u).unwrap()["update"].clone())
        .collect();
    assert_eq!(out[1]["turn"]["input"], "m1");
    assert_eq!(out[2]["item"]["id"], "m1");
    assert_eq!(out[2]["item"]["turn"], U);
    // turn/started after turn/start's answer changes nothing.
    assert!(feed(&mut s, json!({"method": "turn/started", "params": {"threadId": T, "turn": {"id": U, "status": "inProgress"}}})).is_empty());
    // The echo comes back by text when Codex leaves out the client id.
    let echo = feed(
        &mut s,
        json!({"method": "item/started", "params": {"item": {"type": "userMessage", "id": "u9", "content": [{"type": "text", "text": "Plan it"}]}, "threadId": T, "turnId": U}}),
    );
    assert_eq!(
        echo[0]["item"],
        json!({"id": "m1", "ref": {"id": "u9", "strength": "strong"}})
    );
    let plan = feed(
        &mut s,
        json!({"method": "turn/plan/updated", "params": {"threadId": T, "turnId": U, "explanation": null,
            "plan": [{"step": "Read", "status": "completed"}, {"step": "Write", "status": "inProgress"}, {"step": "Test", "status": "pending"}]}}),
    );
    assert_eq!(plan[0]["item"]["kind"], "plan");
    assert_eq!(
        plan[0]["item"]["set"]["entries"],
        json!([{"text": "Read", "status": "done"}, {"text": "Write", "status": "active"}, {"text": "Test", "status": "pending"}])
    );
}

#[test]
fn usage_leaves_cache_reads_out_of_input() {
    let usage = mapping::usage_of(
        &serde_json::from_value(json!({
            "total": {"inputTokens": 53660, "cachedInputTokens": 51200, "outputTokens": 223},
            "last": {"inputTokens": 18305, "outputTokens": 100},
            "modelContextWindow": 258400
        }))
        .unwrap(),
    );
    let t = usage.tokens.unwrap();
    assert_eq!((t.input, t.output, t.cache_read), (2460, 223, Some(51200)));
    let c = usage.context.unwrap();
    assert_eq!((c.used, c.window), (18405, 258400));
}

#[test]
fn levels_and_pins() {
    assert_eq!(
        level_of("on-request", "read-only"),
        (PermissionLevel::Read, false)
    );
    assert_eq!(
        level_of("untrusted", "workspace-write"),
        (PermissionLevel::Write, false)
    );
    assert_eq!(
        level_of("never", "workspace-write"),
        (PermissionLevel::Sandboxed, false)
    );
    assert_eq!(
        level_of("never", "danger-full-access"),
        (PermissionLevel::Host, true)
    );
    assert_eq!(
        level_of("sometimes", "workspace-write"),
        (PermissionLevel::Host, true)
    );
    let mut s = MapState::new("b1");
    s.sandbox = "danger-full-access".into();
    s.apply_pin(&Pin {
        level: PermissionLevel::Write,
        network: false,
    });
    assert_eq!(
        (s.approval.as_str(), s.sandbox.as_str()),
        ("untrusted", "workspace-write")
    );
    assert_eq!(s.level(), PermissionLevel::Write);
}

#[test]
fn review_targets() {
    assert_eq!(
        mapping::review_target(None, None).unwrap(),
        json!({"type": "uncommittedChanges"})
    );
    assert_eq!(
        mapping::review_target(None, Some("look at errors")).unwrap(),
        json!({"type": "custom", "instructions": "look at errors"})
    );
    assert_eq!(
        mapping::review_target(Some(&json!({"target": "commit", "sha": "abc123"})), None).unwrap(),
        json!({"type": "commit", "sha": "abc123"})
    );
    assert!(mapping::review_target(Some(&json!({"target": "baseBranch"})), None).is_err());
}

#[test]
fn shell_wrappers_are_taken_off_commands() {
    assert_eq!(short_command("/bin/bash -lc 'echo hi'"), "echo hi");
    assert_eq!(
        short_command("/bin/bash -lc \"printf 'a' > b\""),
        "printf 'a' > b"
    );
    assert_eq!(short_command("/bin/bash -lc ls"), "ls");
    assert_eq!(short_command("ls -la"), "ls -la");
}

#[test]
fn unknown_notifications_and_items_are_kept_apart() {
    let mut s = running();
    assert!(
        feed(
            &mut s,
            json!({"method": "thread/goal/updated", "params": {"x": 1}})
        )
        .is_empty()
    );
    assert!(feed(&mut s, json!({"method": "item/started", "params": {"item": {"type": "sleep", "id": "z", "durationMs": 5}, "threadId": T, "turnId": U}})).is_empty());
}
