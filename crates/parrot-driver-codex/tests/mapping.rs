//! The mapping on lines shaped as Codex CLI 0.167.1 sent them, without processes.

use parrot_driver_codex::mapping::{self, MapState, level_of};
use parrot_driver_codex::wire::{Message, RequestId, ServerNotification, short_command};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams, Pin};
use parrot_proto::option::PermissionLevel;
use parrot_proto::request::Answer;
use serde_json::{Map, Value, json};

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
    // The same question is typed, so a client doesn't read the JSON Schema in `input`.
    assert_eq!(r["questions"][0]["id"], "q1");
    assert_eq!(r["questions"][0]["header"], "H");
    assert_eq!(r["questions"][0]["question"], "Tea?");
    assert_eq!(r["questions"][0]["options"][1]["label"], "Coffee");
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
fn typed_answers_name_each_question() {
    let mut s = running();
    let out = feed(
        &mut s,
        json!({"method": "item/tool/requestUserInput", "id": 6, "params": {"threadId": T, "turnId": U, "itemId": "q",
            "questions": [
                {"id": "q1", "question": "Tea?", "options": [{"label": "Tea"}, {"label": "Coffee"}]},
                {"id": "q2", "question": "Size?", "options": [{"label": "S"}, {"label": "L"}]}
            ],
            "isBlocking": true}}),
    );
    assert_eq!(out[0]["request"]["questions"].as_array().unwrap().len(), 2);
    let mut answers = Map::new();
    answers.insert("q1".into(), json!("option-1"));
    answers.insert("q2".into(), json!(["S"]));
    let (msg, _) = s
        .answer(
            "req-6",
            &Answer {
                answers: Some(answers),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        msg.to_value(),
        json!({"id": 6, "result": {"answers": {"q1": {"answers": ["Coffee"]}, "q2": {"answers": ["S"]}}}})
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

/// Feeds a line through the per-thread routing the driver uses, keeping the whole updates.
fn feed_in(state: &mut MapState, line: Value) -> Vec<Value> {
    let updates = match Message::from_value(line).unwrap() {
        Message::Notification { method, params } => {
            let thread = ServerNotification::thread_id(&params).map(str::to_owned);
            state.step_in(
                thread.as_deref(),
                &ServerNotification::parse(&method, &params),
                NOW,
            )
        }
        Message::Request { id, method, params } => {
            let thread = ServerNotification::thread_id(&params).map(str::to_owned);
            state
                .server_request_in(thread.as_deref(), &id, &method, &params, NOW)
                .0
        }
        other => panic!("not fed: {other:?}"),
    };
    updates.iter().map(|u| serde_json::to_value(u).unwrap()).collect()
}

const SUB: &str = "thr-sub-0002";

#[test]
fn a_spawned_agent_is_a_subagent_and_its_thread_is_routed() {
    let mut s = running();
    let spawn = json!({"type": "collabAgentToolCall", "id": "call_1", "tool": "spawnAgent", "status": "completed",
        "senderThreadId": T, "receiverThreadIds": [SUB], "prompt": "Count the files\nin src", "model": "gpt-fake-mini",
        "agentsStates": {}});
    let out = feed_in(
        &mut s,
        json!({"method": "item/completed", "params": {"item": spawn, "threadId": T, "turnId": U}}),
    );
    let item = &out[0]["update"]["item"];
    assert_eq!(item["kind"], "subagent");
    assert_eq!(item["set"]["key"], SUB);
    assert_eq!(item["set"]["title"], "Count the files");
    assert!(out[0]["subagent"].is_null());

    let out = feed_in(
        &mut s,
        json!({"method": "turn/started", "params": {"threadId": SUB, "turn": {"id": "turn-sub-1", "items": [], "status": "inProgress"}}}),
    );
    assert!(out.iter().all(|u| u["subagent"] == SUB), "{out:?}");
    let out = feed_in(
        &mut s,
        json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "id": "msg_s", "text": "Twelve"},
            "threadId": SUB, "turnId": "turn-sub-1"}}),
    );
    assert_eq!(out[0]["subagent"], SUB);
    assert_eq!(out[0]["update"]["item"]["kind"], "assistant");
    assert_eq!(s.subagents[SUB].turn.as_ref().unwrap().key, "turn-sub-1");
    // The main turn is untouched.
    assert_eq!(s.turn.as_ref().unwrap().key, U);

    let req = feed_in(
        &mut s,
        json!({"method": "item/commandExecution/requestApproval", "id": 9, "params": {"threadId": SUB, "turnId": "turn-sub-1",
            "itemId": "c1", "command": "ls", "cwd": "/fake"}}),
    );
    assert_eq!(req[0]["subagent"], SUB);
    assert_eq!(req[0]["update"]["type"], "request");
    let key = req[0]["update"]["request"]["id"].as_str().unwrap().to_owned();
    assert!(
        s.answer(
            &key,
            &Answer {
                choice: Some("accept".into()),
                ..Default::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn other_collab_tools_are_tools() {
    let mut s = running();
    let wait = json!({"type": "collabAgentToolCall", "id": "call_2", "tool": "wait", "status": "completed",
        "senderThreadId": T, "receiverThreadIds": [SUB], "prompt": null, "model": null,
        "agentsStates": {SUB: {"status": "completed", "message": "Twelve"}}});
    let out = feed(
        &mut s,
        json!({"method": "item/completed", "params": {"item": wait, "threadId": T, "turnId": U}}),
    );
    let item = &out[0]["item"];
    assert_eq!(item["kind"], "tool");
    assert_eq!(item["set"]["output"][SUB]["message"], "Twelve");
}

#[test]
fn auto_resolution_sets_expiry_and_a_fallback() {
    let mut s = running();
    let out = feed_in(
        &mut s,
        json!({"method": "item/tool/requestUserInput", "id": 6, "params": {"threadId": T, "turnId": U, "itemId": "q",
            "questions": [{"id": "q1", "header": "H", "question": "Tea?", "isOther": true, "isSecret": false, "options": null}],
            "isBlocking": false, "autoResolutionMs": 30000}}),
    );
    let r = &out[0]["update"]["request"];
    assert_eq!(r["expires"], "2026-01-02T03:04:35.000Z");
    assert_eq!(r["blocking"], false);
    assert!(r["fallback"].is_object(), "{r}");
    let out = feed_in(
        &mut s,
        json!({"method": "serverRequest/resolved", "params": {"threadId": T, "requestId": 6}}),
    );
    let status = &out[0]["update"]["request"];
    assert_eq!(status["status"], "expired", "{status}");
    assert_eq!(status["answeredBy"], "fallback");
}

#[test]
fn diffs_warnings_status_and_mcp_are_mapped() {
    let mut s = running();
    let patch = json!({"type": "fileChange", "id": "fc_1", "status": "inProgress",
        "changes": [{"path": "/fake/a.txt", "kind": {"type": "add"}, "diff": "hello\n"}]});
    feed(
        &mut s,
        json!({"method": "item/started", "params": {"item": patch, "threadId": T, "turnId": U}}),
    );
    let out = feed(
        &mut s,
        json!({"method": "item/fileChange/outputDelta", "params": {"threadId": T, "turnId": U, "itemId": "fc_1", "delta": "Success.\n"}}),
    );
    assert_eq!(out[0]["item"]["append"]["output"], "Success.\n", "{out:?}");
    let out = feed(
        &mut s,
        json!({"method": "item/fileChange/patchUpdated", "params": {"threadId": T, "turnId": U, "itemId": "fc_1",
            "changes": [{"path": "/fake/a.txt", "kind": {"type": "add"}, "diff": "hello\nworld\n"}]}}),
    );
    assert!(!out.is_empty());
    assert!(
        feed(
            &mut s,
            json!({"method": "turn/diff/updated", "params": {"threadId": T, "turnId": U, "diff": "+hello\n"}})
        )
        .is_empty()
    );
    let w = feed(
        &mut s,
        json!({"method": "warning", "params": {"threadId": T, "message": "Disk nearly full"}}),
    );
    assert_eq!(w[0]["type"], "notice");
    assert_eq!(w[0]["text"], "Disk nearly full");
    let d = feed(
        &mut s,
        json!({"method": "deprecationNotice", "params": {"summary": "Old flag", "details": "Use the new one"}}),
    );
    assert_eq!(d[0]["type"], "notice");
    let e = feed(
        &mut s,
        json!({"method": "thread/status/changed", "params": {"threadId": T, "status": {"type": "systemError"}}}),
    );
    assert_eq!(e[0]["level"], "error");
    let m = feed(
        &mut s,
        json!({"method": "mcpServer/startupStatus/updated", "params": {"name": "fake-docs", "status": "failed", "error": "no such command"}}),
    );
    assert_eq!(m[0]["type"], "mcp", "{m:?}");
    let done = feed(
        &mut s,
        json!({"method": "turn/completed", "params": {"threadId": T, "turn": {"id": U, "items": [], "status": "completed", "error": null}}}),
    );
    let turn = done.iter().find(|u| u["type"] == "turn").unwrap();
    assert_eq!(turn["turn"]["_meta"]["diff"], "+hello\n", "{turn}");
}

#[test]
fn a_device_code_login_is_an_auth_request() {
    let mut s = running();
    let (key, out) = s.login_started(
        "login-fake-1",
        "https://auth.example.test/device",
        Some("FAKE-1234"),
    );
    let r = serde_json::to_value(&out[0]).unwrap();
    assert_eq!(r["update"]["request"]["kind"], "auth");
    assert_eq!(r["update"]["request"]["subject"]["code"], "FAKE-1234");
    let done = feed(
        &mut s,
        json!({"method": "account/login/completed", "params": {"loginId": "login-fake-1", "success": true, "error": null}}),
    );
    assert_eq!(done[0]["request"]["status"], "answered", "{done:?}");
    assert!(s.take_login(&key).is_none());
}

#[test]
fn skills_become_actions_and_turn_input() {
    let mut s = running();
    let listed = json!({"data": [{"cwd": "/fake", "skills": [{"name": "fake-lint", "description": "Lints",
        "shortDescription": "Lint", "path": "/fake/skills/fake-lint/SKILL.md", "enabled": true},
        {"name": "off", "description": "", "path": "/fake/off/SKILL.md", "enabled": false}]}]});
    let u = serde_json::to_value(s.skills_listed(&listed).unwrap()).unwrap();
    let ids: Vec<&Value> = u["update"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["id"])
        .collect();
    assert!(ids.contains(&&json!("skill:fake-lint")), "{ids:?}");
    assert!(!ids.contains(&&json!("skill:off")));
    assert!(
        s.skills_listed(&listed).is_none(),
        "an unchanged list sends nothing"
    );
    let p = s.skill_turn_params("fake-lint", "check src", "m1").unwrap();
    assert_eq!(
        p["input"][1],
        json!({"type": "skill", "name": "fake-lint", "path": "/fake/skills/fake-lint/SKILL.md"})
    );
}
