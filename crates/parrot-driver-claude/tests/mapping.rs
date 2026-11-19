//! The mapping on every fixture. Each scenario's update sequence is compared with
//! `fixtures/updates/<scenario>.jsonl`; set `PARROT_UPDATE_FIXTURES=1` to rewrite those files.

use std::path::PathBuf;

use parrot_driver_claude::fake::{Step, load_scenario};
use parrot_driver_claude::mapping::{MapState, map};
use parrot_driver_claude::wire::{self, ClaudeInMsg};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams};
use parrot_proto::open::Open;
use parrot_proto::request::Answer;
use serde_json::{Value, json};

const NOW: &str = "2026-01-01T00:00:00.000Z";

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn prompt(text: &str, message: &str) -> BindingPromptParams {
    BindingPromptParams {
        binding: "b1".into(),
        subagent: None,
        message: message.into(),
        text: text.into(),
        context: None,
        attachments: None,
        steer: false,
    }
}

/// Plays a scenario against the mapping as the driver would, and returns the updates and the
/// control responses written.
fn play(name: &str) -> (Vec<Value>, Vec<Value>) {
    let steps = load_scenario(&dir().join("scenarios").join(format!("{name}.jsonl"))).unwrap();
    let mut state = MapState::new("b1");
    let mut updates = Vec::new();
    let mut written = Vec::new();
    let mut prompts = 0;
    for step in steps {
        match step {
            Step::Expect(p) if p["type"] == "user" => {
                prompts += 1;
                let (u, d) = state.prompt(&prompt("Say hello", &format!("m{prompts}")), NOW);
                assert_eq!(d, BindingDisposition::Started);
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
            Step::Expect(p) if p["request"]["subtype"] == "interrupt" => state.cancel(),
            Step::Expect(_) => {}
            Step::Emit(m) => {
                let msg = wire::parse_line(&m.to_string()).unwrap();
                let (u, next) = map(&state, &msg, NOW);
                state = next;
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
            Step::AwaitControlResponse { request_id, pattern } => {
                let choice = if pattern.unwrap()["behavior"] == "allow" {
                    "allow"
                } else {
                    "deny"
                };
                let answer = Answer {
                    choice: Some(choice.into()),
                    ..Default::default()
                };
                let (line, u) = state.answer(&request_id, &answer).unwrap();
                let Open::Known(ClaudeInMsg::ControlResponse(r)) = &line else {
                    panic!("an answer is a control response");
                };
                written.push(serde_json::to_value(r).unwrap());
                updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
            }
            Step::Exit(_) => break,
            Step::Choose(_) | Step::McpCall { .. } | Step::Shell(_) => {}
        }
    }
    (updates, written)
}

fn golden(name: &str, updates: &[Value]) {
    let path = dir().join("updates").join(format!("{name}.jsonl"));
    let text: String = updates.iter().map(|u| format!("{u}\n")).collect();
    if std::env::var_os("PARROT_UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let want: Vec<Value> = want.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(updates.len(), want.len(), "{name}: update count");
    for (i, (got, want)) in updates.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "{name}: update {i}");
    }
}

fn kinds(updates: &[Value]) -> Vec<String> {
    updates
        .iter()
        .map(|u| {
            let u = &u["update"];
            match u["type"].as_str().unwrap() {
                "state" => format!("state:{}", u["state"].as_str().unwrap()),
                "item" if u["item"]["append"].is_object() => {
                    format!("append:{}", u["item"]["id"].as_str().unwrap())
                }
                "item" => format!("item:{}", u["item"]["id"].as_str().unwrap()),
                "turn" => format!("turn:{}", u["turn"]["status"].as_str().unwrap()),
                "request" => format!("request:{}", u["request"]["status"].as_str().unwrap()),
                t => t.to_owned(),
            }
        })
        .collect()
}

#[test]
fn text_turn() {
    let (updates, _) = play("text-turn");
    golden("text-turn", &updates);
    assert_eq!(
        kinds(&updates),
        [
            "state:running",
            "turn:running",
            "item:m1",
            "ref",
            "options",
            "actions",
            "item:msg_fake_01:0",
            "append:msg_fake_01:0",
            "append:msg_fake_01:0",
            "item:msg_fake_01:0",
            "turn:completed",
            "usage",
            "state:idle",
        ]
    );
    let user = &updates[2]["update"]["item"];
    assert_eq!(user["kind"], "user");
    assert_eq!(user["set"]["message"], "m1");
    assert_eq!(
        updates[3]["update"]["ref"],
        json!({"id": "sess-fake-0001", "strength": "strong"})
    );
    assert_eq!(updates[7]["update"]["item"]["append"], json!({"text": "Hello "}));
    assert_eq!(updates[9]["update"]["item"]["set"]["text"], "Hello there.");
    let turn = &updates[10]["update"]["turn"];
    assert_eq!(turn["stop"], "end");
    assert_eq!(turn["input"], "m1");
    let usage = &updates[11]["update"]["usage"];
    assert_eq!(
        usage["tokens"],
        json!({"input": 3, "output": 4, "cacheRead": 1200, "cacheWrite": 0})
    );
    assert_eq!(usage["cost"], json!({"amount": 0.0012, "currency": "USD"}));
    assert_eq!(usage["context"]["window"], 200000);
}

#[test]
fn bash_allowed() {
    let (updates, written) = play("bash-allowed");
    golden("bash-allowed", &updates);
    let k = kinds(&updates);
    let req = k.iter().position(|k| k == "request:open").unwrap();
    assert_eq!(k[req + 1], "state:requires_action");
    assert_eq!(k[req + 2], "request:answered");
    assert_eq!(k[req + 3], "state:running");
    assert_eq!(k.last().unwrap(), "state:idle");
    let request = &updates[req]["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["title"], "Bash: ls");
    assert_eq!(request["item"], "toolu_fake_01");
    assert_eq!(request["answerable"], "connection");
    assert_eq!(request["level"], "host");
    let choices: Vec<_> = request["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].clone())
        .collect();
    assert_eq!(choices, [json!("allow"), json!("suggestion-0"), json!("deny")]);
    assert_eq!(request["choices"][1]["scope"], "always");
    assert_eq!(request["choices"][1]["amends"]["type"], "addRules");
    assert_eq!(
        written,
        [
            json!({"response": {"subtype": "success", "request_id": "perm-1", "response": {
            "behavior": "allow", "updatedInput": {"command": "ls", "description": "List files"}}}})
        ]
    );
    let result = updates
        .iter()
        .find(|u| {
            u["update"]["item"]["id"] == "toolu_fake_01" && u["update"]["item"]["set"]["status"] == "ok"
        })
        .expect("the tool result");
    assert_eq!(result["update"]["item"]["set"]["output"], "README.md\nsrc");
}

#[test]
fn bash_denied() {
    let (updates, written) = play("bash-denied");
    golden("bash-denied", &updates);
    assert_eq!(written[0]["response"]["response"]["behavior"], "deny");
    assert!(
        updates
            .iter()
            .any(|u| u["update"]["item"]["id"] == "toolu_fake_01"
                && u["update"]["item"]["set"]["status"] == "error")
    );
}

#[test]
fn interrupt() {
    let (updates, _) = play("interrupt");
    golden("interrupt", &updates);
    let k = kinds(&updates);
    assert!(k.contains(&"append:msg_fake_04:0".to_owned()), "thinking streams");
    assert!(k.contains(&"append:msg_fake_04:1".to_owned()), "text streams");
    let reasoning = updates
        .iter()
        .find(|u| u["update"]["item"]["id"] == "msg_fake_04:0")
        .unwrap();
    assert_eq!(reasoning["update"]["item"]["kind"], "reasoning");
    let turn = updates
        .iter()
        .rev()
        .find(|u| u["update"]["type"] == "turn")
        .unwrap();
    assert_eq!(turn["update"]["turn"]["status"], "interrupted");
    assert_eq!(turn["update"]["turn"]["stop"], "cancelled");
    assert_eq!(k.last().unwrap(), "state:idle");
}

#[test]
fn error_result() {
    let (updates, _) = play("error-result");
    golden("error-result", &updates);
    assert!(updates.iter().any(|u| u["update"]["type"] == "notice"));
    let turns: Vec<_> = updates
        .iter()
        .filter(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .map(|u| {
            (
                u["update"]["turn"]["status"].clone(),
                u["update"]["turn"]["stop"].clone(),
            )
        })
        .collect();
    assert_eq!(
        turns,
        [
            (json!("failed"), json!("error")),
            (json!("completed"), json!("turn_limit"))
        ]
    );
}

#[test]
fn message_fixture() {
    let text = std::fs::read_to_string(dir().join("messages/out.jsonl")).unwrap();
    let mut state = MapState::new("b1");
    let mut updates = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let msg = wire::parse_line(line).unwrap();
        let (u, next) = map(&state, &msg, NOW);
        state = next;
        updates.extend(u.iter().map(|u| serde_json::to_value(u).unwrap()));
    }
    golden("messages", &updates);
    // Plan mode reads as `read`; an unknown mode keeps the level it maps to.
    let options = updates.iter().find(|u| u["update"]["type"] == "options").unwrap();
    assert_eq!(options["update"]["level"], "read");
    assert!(
        updates
            .iter()
            .any(|u| u["update"]["item"]["kind"] == "compaction")
    );
    let sub = updates
        .iter()
        .find(|u| u["update"]["item"]["set"]["text"] == "From the subagent.")
        .unwrap();
    assert_eq!(
        sub["subagent"], "toolu_task",
        "a sidechain message belongs to its subagent"
    );
    let turn = updates.iter().find(|u| u["update"]["turn"]["stop"] == "budget");
    assert!(turn.is_some());
    // The Write request is opened, then cancelled by the CLI.
    let req: Vec<_> = updates
        .iter()
        .filter(|u| u["update"]["request"]["id"] == "perm-9")
        .map(|u| u["update"]["request"]["status"].clone())
        .collect();
    assert_eq!(req, [json!("open"), json!("cancelled")]);
}

#[test]
fn questions_and_plans() {
    let mut state = MapState::new("b1");
    let ask = json!({"type": "control_request", "request_id": "q-1", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "tool_use_id": "toolu_q",
        "input": {"questions": [{"question": "Which colour?", "header": "Colour",
            "options": [{"label": "Red"}, {"label": "Blue"}], "multiSelect": false}]}}});
    let u = state.step(&wire::parse_line(&ask.to_string()).unwrap(), NOW);
    let req = u
        .iter()
        .map(|u| serde_json::to_value(u).unwrap())
        .find(|u| u["update"]["type"] == "request")
        .unwrap();
    assert_eq!(req["update"]["request"]["kind"], "question");
    assert_eq!(req["update"]["request"]["title"], "Which colour?");
    assert_eq!(req["update"]["request"]["choices"][1]["label"], "Blue");
    // The question is typed too, so a client doesn't read Claude Code's raw input.
    assert_eq!(req["update"]["request"]["questions"][0]["id"], "question-0");
    assert_eq!(
        req["update"]["request"]["questions"][0]["question"],
        "Which colour?"
    );
    assert_eq!(req["update"]["request"]["questions"][0]["header"], "Colour");
    assert_eq!(
        req["update"]["request"]["questions"][0]["options"][1]["label"],
        "Blue"
    );
    let answer = Answer {
        choice: Some("option-1".into()),
        ..Default::default()
    };
    let (line, _) = state.answer("q-1", &answer).unwrap();
    let line: Value = serde_json::from_str(&wire::to_line(&line)).unwrap();
    assert_eq!(
        line["response"]["response"]["updatedInput"]["answers"],
        json!({"Which colour?": "Blue"})
    );

    // Two questions answered by id: each becomes Claude Code's answer for its question text.
    let multi = json!({"type": "control_request", "request_id": "q-2", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "tool_use_id": "toolu_q2",
        "input": {"questions": [
            {"question": "Which colour?", "header": "Colour",
             "options": [{"label": "Red"}, {"label": "Blue"}], "multiSelect": false},
            {"question": "Which size?", "header": "Size",
             "options": [{"label": "S"}, {"label": "L"}], "multiSelect": false}]}}});
    let u = state.step(&wire::parse_line(&multi.to_string()).unwrap(), NOW);
    let req = u
        .iter()
        .map(|u| serde_json::to_value(u).unwrap())
        .find(|u| u["update"]["type"] == "request")
        .unwrap();
    assert_eq!(req["update"]["request"]["questions"].as_array().unwrap().len(), 2);
    let mut answers = serde_json::Map::new();
    answers.insert("question-0".into(), json!("option-1"));
    answers.insert("question-1".into(), json!("L"));
    let answer = Answer {
        answers: Some(answers),
        ..Default::default()
    };
    let (line, _) = state.answer("q-2", &answer).unwrap();
    let line: Value = serde_json::from_str(&wire::to_line(&line)).unwrap();
    assert_eq!(
        line["response"]["response"]["updatedInput"]["answers"],
        json!({"Which colour?": "Blue", "Which size?": "L"})
    );

    let plan = json!({"type": "control_request", "request_id": "p-1", "request": {
        "subtype": "can_use_tool", "tool_name": "ExitPlanMode", "tool_use_id": "toolu_p",
        "input": {"plan": "1. Do it"}}});
    let u: Vec<Value> = state
        .step(&wire::parse_line(&plan.to_string()).unwrap(), NOW)
        .iter()
        .map(|u| serde_json::to_value(u).unwrap())
        .collect();
    assert_eq!(u[0]["update"]["item"]["kind"], "plan");
    assert_eq!(u[0]["update"]["item"]["set"]["text"], "1. Do it");
    assert_eq!(u[1]["update"]["request"]["kind"], "plan");
    assert_eq!(
        u[1]["update"]["request"]["subject"],
        json!({"type": "plan", "text": "1. Do it"})
    );
}

#[test]
fn steering_and_echoes() {
    let mut state = MapState::new("b1");
    let (_, d) = state.prompt(&prompt("first", "m1"), NOW);
    assert_eq!(d, BindingDisposition::Started);
    let (u, d) = state.prompt(&prompt("second", "m2"), NOW);
    assert_eq!(d, BindingDisposition::Steered);
    assert_eq!(u.len(), 1, "a steer adds only the user item");
    // A replayed echo of a runtime prompt is already reported.
    let echo = json!({"type": "user", "message": {"role": "user", "content": "second"}, "uuid": "u-echo"});
    assert!(
        state
            .step(&wire::parse_line(&echo.to_string()).unwrap(), NOW)
            .is_empty()
    );
}

#[test]
fn a_task_call_is_a_subagent() {
    let (updates, _) = play("subagent");
    let sub: Vec<&Value> = updates
        .iter()
        .filter(|u| u["update"]["item"]["kind"] == "subagent")
        .collect();
    assert_eq!(sub.len(), 1, "one subagent item");
    let item = &sub[0]["update"]["item"];
    assert_eq!(item["set"]["key"], "toolu_task_01");
    assert_eq!(item["set"]["title"], "Count the files");
    assert!(
        sub[0].get("subagent").is_none(),
        "the subagent item is in the parent"
    );
    let inside: Vec<&Value> = updates
        .iter()
        .filter(|u| u["subagent"] == "toolu_task_01")
        .collect();
    assert!(inside.iter().any(|u| u["update"]["item"]["id"] == "toolu_ls_01"));
    assert!(
        inside
            .iter()
            .any(|u| u["update"]["item"]["set"]["text"] == "Two files.")
    );
    assert!(inside.iter().all(|u| u["update"]["item"]["parent"].is_null()));
    let done = updates
        .iter()
        .rfind(|u| u["update"]["item"]["id"] == "toolu_task_01:subagent")
        .unwrap();
    assert_eq!(done["update"]["item"]["set"]["status"], "ok");
    let top = updates
        .iter()
        .find(|u| u["update"]["item"]["set"]["text"] == "There are two files.")
        .unwrap();
    assert!(top.get("subagent").is_none());
}
