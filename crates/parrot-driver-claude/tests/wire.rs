//! Every fixture message parses and writes back to the same JSON.

use std::path::PathBuf;

use parrot_driver_claude::fake::{Step, load_scenario};
use parrot_driver_claude::wire::*;
use parrot_proto::open::Open;
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn lines(name: &str) -> Vec<String> {
    std::fs::read_to_string(fixtures().join(name))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn round_trip_out(line: &str) -> ClaudeOut {
    let msg = parse_line(line).unwrap();
    let back: Value = serde_json::from_str(&out_to_line(&msg)).unwrap();
    assert_eq!(back, serde_json::from_str::<Value>(line).unwrap(), "{line}");
    msg
}

fn round_trip_in(line: &str) -> ClaudeIn {
    let msg = parse_in_line(line).unwrap();
    let back: Value = serde_json::from_str(&to_line(&msg)).unwrap();
    assert_eq!(back, serde_json::from_str::<Value>(line).unwrap(), "{line}");
    msg
}

#[test]
fn scenario_messages_parse_as_known_and_round_trip() {
    let dir = fixtures().join("scenarios");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        for step in load_scenario(&path).unwrap() {
            if let Step::Emit(m) = step {
                let line = m.to_string();
                let msg = round_trip_out(&line);
                assert!(matches!(msg, Open::Known(_)), "{}: {line}", path.display());
                count += 1;
            }
        }
    }
    assert!(count > 40, "only {count} messages");
}

#[test]
fn cli_messages_round_trip() {
    let msgs: Vec<ClaudeOut> = lines("messages/out.jsonl")
        .iter()
        .map(|l| round_trip_out(l))
        .collect();

    let Open::Known(ClaudeOutMsg::System(Open::Known(System::Init(init)))) = &msgs[0] else {
        panic!("init: {:?}", msgs[0]);
    };
    assert_eq!(init.tools.as_deref(), Some(&["Task".into(), "Bash".into()][..]));
    assert_eq!(init.permission_mode, Some(Open::Known(PermissionMode::Plan)));
    assert_eq!(init.extra["apiKeySource"], "none");

    let Open::Known(ClaudeOutMsg::System(Open::Known(System::Init(init)))) = &msgs[1] else {
        panic!("init with a new mode: {:?}", msgs[1]);
    };
    assert!(matches!(init.permission_mode, Some(Open::Unknown(_))));

    assert!(matches!(
        msgs[2],
        Open::Known(ClaudeOutMsg::System(Open::Known(System::CompactBoundary(_))))
    ));
    assert!(matches!(
        msgs[3],
        Open::Known(ClaudeOutMsg::System(Open::Unknown(_)))
    ));

    let Open::Known(ClaudeOutMsg::Assistant(a)) = &msgs[4] else {
        panic!()
    };
    let Content::Blocks(blocks) = &a.message.content else {
        panic!()
    };
    assert!(matches!(blocks[0], Open::Known(ContentBlock::Thinking(_))));
    assert!(matches!(blocks[1], Open::Unknown(_)));
    assert!(matches!(&blocks[2], Open::Known(ContentBlock::ToolUse(t)) if t.name == "Task"));
    assert_eq!(a.parent_tool_use_id, Some(None));

    let Open::Known(ClaudeOutMsg::Assistant(a)) = &msgs[5] else {
        panic!()
    };
    assert_eq!(a.parent_tool_use_id, Some(Some("toolu_task".into())));
    assert_eq!(a.message.content.text(), "From the subagent.");

    let Open::Known(ClaudeOutMsg::StreamEvent(e)) = &msgs[7] else {
        panic!()
    };
    assert!(matches!(
        &e.event,
        Open::Known(Event::ContentBlockDelta {
            delta: Open::Unknown(_),
            ..
        })
    ));
    let Open::Known(ClaudeOutMsg::StreamEvent(e)) = &msgs[8] else {
        panic!()
    };
    assert!(matches!(e.event, Open::Unknown(_)));

    let Open::Known(ClaudeOutMsg::Result(r)) = &msgs[9] else {
        panic!()
    };
    assert_eq!(r.subtype, Open::Known(ResultSubtype::ErrorMaxBudgetUsd));
    assert_eq!(
        (r.is_error, r.num_turns, r.cost_usd()),
        (true, Some(3), Some(1.5))
    );
    let Open::Known(ClaudeOutMsg::Result(r)) = &msgs[10] else {
        panic!()
    };
    assert!(matches!(r.subtype, Open::Unknown(_)));

    let Open::Known(ClaudeOutMsg::ControlRequest(c)) = &msgs[11] else {
        panic!()
    };
    assert_eq!(c.request_id, "perm-9");
    let Open::Known(ControlRequestBody::CanUseTool(t)) = &c.request else {
        panic!()
    };
    assert_eq!(t.tool_name, "Write");
    assert_eq!(t.blocked_path.as_deref(), Some("/work/demo/notes.txt"));
    let Open::Known(ClaudeOutMsg::ControlRequest(c)) = &msgs[12] else {
        panic!()
    };
    assert!(matches!(c.request, Open::Unknown(_)));

    let Open::Known(ClaudeOutMsg::ControlResponse(r)) = &msgs[14] else {
        panic!()
    };
    assert_eq!(r.request_id(), Some("model-1"));
    assert!(matches!(
        msgs[15],
        Open::Known(ClaudeOutMsg::ControlCancelRequest(_))
    ));
    assert!(matches!(msgs[16], Open::Known(ClaudeOutMsg::KeepAlive(_))));
    assert!(matches!(msgs[17], Open::Unknown(_)));
}

#[test]
fn host_messages_round_trip() {
    let msgs: Vec<ClaudeIn> = lines("messages/in.jsonl")
        .iter()
        .map(|l| round_trip_in(l))
        .collect();
    let Open::Known(ClaudeInMsg::User(u)) = &msgs[0] else {
        panic!()
    };
    assert_eq!(u.message.content.text(), "Say hello");
    let Open::Known(ClaudeInMsg::ControlRequest(c)) = &msgs[4] else {
        panic!()
    };
    assert!(matches!(
        c.request,
        Open::Known(ControlRequestBody::SetPermissionMode {
            mode: Open::Known(PermissionMode::AcceptEdits),
            ..
        })
    ));
    let Open::Known(ClaudeInMsg::ControlRequest(c)) = &msgs[6] else {
        panic!()
    };
    assert!(matches!(
        c.request,
        Open::Known(ControlRequestBody::SetModel { model: None, .. })
    ));
    let Open::Known(ClaudeInMsg::ControlRequest(c)) = &msgs[7] else {
        panic!()
    };
    assert!(matches!(c.request, Open::Unknown(_)));
    let Open::Known(ClaudeInMsg::ControlResponse(r)) = &msgs[9] else {
        panic!()
    };
    let Open::Known(ControlResponseBody::Success {
        response: Some(body), ..
    }) = &r.response
    else {
        panic!()
    };
    let p: PermissionResult = serde_json::from_value(body.clone()).unwrap();
    assert!(matches!(
        p,
        PermissionResult::Deny {
            interrupt: Some(false),
            ..
        }
    ));
    assert!(matches!(msgs[12], Open::Unknown(_)));
}

#[test]
fn builders_write_the_cli_shapes() {
    let prompt: ClaudeIn = Open::Known(ClaudeInMsg::User(User::prompt("Hi")));
    assert_eq!(
        serde_json::from_str::<Value>(&to_line(&prompt)).unwrap(),
        json!({"type":"user","message":{"role":"user","content":"Hi"},"parent_tool_use_id":null})
    );
    let allow = PermissionResult::Allow {
        updated_input: Some(json!({"command":"ls"})),
        updated_permissions: None,
        extra: Extra::new(),
    };
    let reply: ClaudeIn = Open::Known(ClaudeInMsg::ControlResponse(ControlResponse::success(
        "perm-1",
        allow.to_value(),
    )));
    assert_eq!(
        serde_json::from_str::<Value>(&to_line(&reply)).unwrap(),
        json!({"type":"control_response","response":{"subtype":"success","request_id":"perm-1",
            "response":{"behavior":"allow","updatedInput":{"command":"ls"}}}})
    );
    let interrupt: ClaudeIn = Open::Known(ClaudeInMsg::ControlRequest(ControlRequest::new(
        "int-1",
        ControlRequestBody::Interrupt { extra: Extra::new() },
    )));
    assert_eq!(
        to_line(&interrupt),
        r#"{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}"#
    );
}

#[test]
fn bad_json_is_an_error() {
    assert!(parse_line("{not json").is_err());
}
