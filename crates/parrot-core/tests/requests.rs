//! Requests, options and actions, driven through `Core::handle` with a manual clock.

use parrot_core::bindings::BindingRecord;
use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::sessions::SessionRecord;
use parrot_core::{Core, Effect, Input};
use parrot_proto::request::RequestStatus;
use serde_json::{Value, json};

fn core() -> (Core, ManualClock) {
    let clock = ManualClock::default();
    clock.set(1_000);
    let mut core = Core::new(Box::new(clock.clone()), Box::new(SeqIds::default()));
    for (id, parent, binding) in [
        ("ses_root", None, None),
        ("ses_a", Some("ses_root"), Some("bnd_a")),
    ] {
        core.state.sessions.by_id.insert(
            id.into(),
            SessionRecord {
                id: id.into(),
                parent: parent.map(Into::into),
                binding: binding.map(Into::into),
                agent: "fake".into(),
            },
        );
    }
    core.state.bindings.by_id.insert(
        "bnd_a".into(),
        BindingRecord {
            id: "bnd_a".into(),
            session: "ses_a".into(),
            agent: "fake".into(),
        },
    );
    (core, clock)
}

fn driver_request(core: &mut Core, key: &str, answerable: &str, extra: Value) -> String {
    let mut request = json!({
        "id": key, "kind": "approval", "title": "Run ls?", "level": "sandboxed",
        "choices": [{ "id": "yes", "label": "Yes", "intent": "allow" }, { "id": "no", "label": "No", "intent": "deny" }],
        "blocking": true, "answerable": answerable, "notify": "normal", "status": "open",
    });
    request
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    core.handle(Input::DriverUpdate {
        binding: "bnd_a".into(),
        update: json!({ "binding": "bnd_a", "update": { "type": "request", "request": request } }),
    });
    core.state
        .requests
        .by_id
        .values()
        .find(|r| r.key == key)
        .map(|r| r.request.id.clone())
        .expect("request recorded")
}

fn call(core: &mut Core, id: u64, method: &str, params: Value) -> Vec<Effect> {
    core.handle(Input::Call {
        conn: "con_1".into(),
        id: Some(json!(id)),
        method: method.into(),
        params,
    })
}

fn reply_of(effects: &[Effect], want: u64) -> Option<Result<Value, parrot_proto::rpc::RpcError>> {
    effects.iter().find_map(|e| match e {
        Effect::Reply { id, result, .. } if *id == json!(want) => Some(result.clone()),
        _ => None,
    })
}

fn driver_call(effects: &[Effect]) -> (u64, String, Value) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::DriverCall {
                call, method, params, ..
            } => Some((*call, method.clone(), params.clone())),
            _ => None,
        })
        .expect("a driver call")
}

fn driver_reply(core: &mut Core, call: u64, result: Value) -> Vec<Effect> {
    core.handle(Input::DriverReply {
        binding: "bnd_a".into(),
        call,
        result: Ok(result),
    })
}

#[test]
fn driver_request_gets_session_and_path() {
    let (mut core, _) = core();
    let id = driver_request(&mut core, "k1", "connection", json!({}));
    assert!(id.starts_with("req_"));
    let rec = &core.state.requests.by_id[&id];
    assert_eq!(rec.request.session, "ses_a");
    assert_eq!(rec.request.path, vec!["ses_root", "ses_a"]);
    let listed = call(&mut core, 1, "request/list", json!({ "tree": "ses_root" }));
    let list = reply_of(&listed, 1).unwrap().unwrap();
    assert_eq!(list["requests"][0]["id"], json!(id));
    assert_eq!(list["requests"][0]["path"], json!(["ses_root", "ses_a"]));
}

#[test]
fn answer_goes_to_driver_and_replies_later() {
    let (mut core, _) = core();
    let id = driver_request(&mut core, "k1", "message", json!({}));
    let out = call(
        &mut core,
        7,
        "request/answer",
        json!({ "request": id, "answer": { "choice": "yes" } }),
    );
    assert!(reply_of(&out, 7).is_none(), "no reply before the driver confirms");
    let (call_id, method, params) = driver_call(&out);
    assert_eq!(method, "binding/answer");
    assert_eq!(params["request"], "k1");
    assert_eq!(params["answer"]["choice"], "yes");

    let out = driver_reply(&mut core, call_id, json!({}));
    assert_eq!(reply_of(&out, 7), Some(Ok(json!({}))));
    let req = &core.state.requests.by_id[&id].request;
    assert_eq!(req.status, RequestStatus::Answered);
    assert_eq!(req.answered_by.as_deref(), Some("con_1"));
}

#[test]
fn second_answer_is_rejected() {
    let (mut core, _) = core();
    let id = driver_request(&mut core, "k1", "message", json!({}));
    let out = call(
        &mut core,
        1,
        "request/answer",
        json!({ "request": id, "answer": { "choice": "yes" } }),
    );
    // While the first answer is in flight, and after it is confirmed.
    let again = call(
        &mut core,
        2,
        "request/answer",
        json!({ "request": id, "answer": { "choice": "no" } }),
    );
    assert_eq!(reply_of(&again, 2).unwrap().unwrap_err().code, -32013);
    driver_reply(&mut core, driver_call(&out).0, json!({}));
    let again = call(
        &mut core,
        3,
        "request/answer",
        json!({ "request": id, "answer": { "choice": "no" } }),
    );
    assert_eq!(reply_of(&again, 3).unwrap().unwrap_err().code, -32013);
}

#[test]
fn disconnect_expires_connection_requests_only() {
    let (mut core, _) = core();
    parrot_core::requests::note_connection(&mut core.state, "ses_a", "con_9");
    let conn_req = driver_request(&mut core, "k1", "connection", json!({}));
    let msg_req = driver_request(&mut core, "k2", "message", json!({}));
    core.handle(Input::Disconnected { conn: "con_9".into() });
    assert_eq!(
        core.state.requests.by_id[&conn_req].request.status,
        RequestStatus::Expired
    );
    assert_eq!(
        core.state.requests.by_id[&msg_req].request.status,
        RequestStatus::Open
    );
}

#[test]
fn expiry_applies_the_fallback() {
    let (mut core, _) = core();
    let id = driver_request(
        &mut core,
        "k1",
        "message",
        json!({ "expires": "1970-01-01T00:00:05Z", "fallback": { "choice": "no" } }),
    );
    let none = core.handle(Input::Tick { now_ms: 4_000 });
    assert!(none.is_empty());
    let out = core.handle(Input::Tick { now_ms: 5_000 });
    let (call_id, method, params) = driver_call(&out);
    assert_eq!(method, "binding/answer");
    assert_eq!(params["answer"]["choice"], "no");
    driver_reply(&mut core, call_id, json!({}));
    let req = &core.state.requests.by_id[&id].request;
    assert_eq!(req.status, RequestStatus::Answered);
    assert_eq!(req.answered_by.as_deref(), Some("fallback"));
}

fn with_options(core: &mut Core) {
    core.handle(Input::DriverUpdate {
        binding: "bnd_a".into(),
        update: json!({ "binding": "bnd_a", "update": { "type": "options", "level": "write", "network": false, "options": [
            { "id": "model", "label": "Model", "applies": "next_turn", "value": "small",
              "schema": { "type": "string", "oneOf": [{ "const": "small", "title": "Small" }, { "const": "large", "title": "Large" }] } },
            { "id": "isolation", "label": "Isolation", "applies": "start", "value": "none", "schema": { "type": "string" } },
        ] } }),
    });
}

#[test]
fn option_set_validates() {
    let (mut core, _) = core();
    with_options(&mut core);
    let cases = [
        (json!({ "model": "huge" }), -32602),
        (json!({ "model": 3 }), -32602),
        (json!({ "missing": "x" }), -32602),
        (json!({ "isolation": "full" }), -32004),
    ];
    for (i, (values, code)) in cases.into_iter().enumerate() {
        let out = call(
            &mut core,
            i as u64,
            "option/set",
            json!({ "session": "ses_a", "values": values }),
        );
        assert_eq!(
            reply_of(&out, i as u64).unwrap().unwrap_err().code,
            code,
            "case {i}"
        );
    }
    let out = call(
        &mut core,
        9,
        "option/complete",
        json!({ "session": "ses_a", "option": "model", "prefix": "s" }),
    );
    assert_eq!(reply_of(&out, 9).unwrap().unwrap_err().code, -32001);
}

#[test]
fn option_set_round_trip() {
    let (mut core, _) = core();
    with_options(&mut core);
    let out = call(
        &mut core,
        1,
        "option/set",
        json!({ "session": "ses_a", "values": { "model": "large" } }),
    );
    assert!(reply_of(&out, 1).is_none());
    let (call_id, method, params) = driver_call(&out);
    assert_eq!(method, "binding/options");
    assert_eq!(params["values"], json!({ "model": "large" }));
    let options = json!([{ "id": "model", "label": "Model", "applies": "next_turn", "value": "large", "schema": { "type": "string" } }]);
    let out = driver_reply(
        &mut core,
        call_id,
        json!({ "options": options, "level": "write", "network": false }),
    );
    let result = reply_of(&out, 1).unwrap().unwrap();
    assert_eq!(result["options"][0]["value"], "large");
    assert_eq!(result["reset"], json!([]));
    let out = call(&mut core, 2, "option/list", json!({ "session": "ses_a" }));
    assert_eq!(
        reply_of(&out, 2).unwrap().unwrap()["options"][0]["value"],
        "large"
    );
}

#[test]
fn continue_falls_back_to_a_prompt() {
    let (mut core, _) = core();
    let out = call(
        &mut core,
        1,
        "action/invoke",
        json!({ "session": "ses_a", "action": "continue", "input": { "note": "tests too" } }),
    );
    let (call_id, method, params) = driver_call(&out);
    assert_eq!(method, "binding/prompt");
    assert_eq!(params["text"], "Continue.\ntests too");
    assert_eq!(params["origin"]["action"], "continue");
    let out = driver_reply(&mut core, call_id, json!({ "disposition": "started" }));
    let result = reply_of(&out, 1).unwrap().unwrap();
    assert_eq!(result["status"], "started");
    assert_eq!(result["message"], params["message"]);

    parrot_core::actions::note_turn(&mut core.state, "ses_a", true, None);
    let out = call(
        &mut core,
        2,
        "action/invoke",
        json!({ "session": "ses_a", "action": "continue" }),
    );
    assert_eq!(reply_of(&out, 2).unwrap().unwrap_err().code, -32004);
}

#[test]
fn retry_needs_a_failed_turn_with_one_input() {
    let (mut core, _) = core();
    let out = call(
        &mut core,
        1,
        "action/invoke",
        json!({ "session": "ses_a", "action": "retry" }),
    );
    assert_eq!(reply_of(&out, 1).unwrap().unwrap_err().code, -32004);

    parrot_core::actions::note_turn(
        &mut core.state,
        "ses_a",
        false,
        Some(vec!["fix the build".into()]),
    );
    let out = call(
        &mut core,
        2,
        "action/invoke",
        json!({ "session": "ses_a", "action": "retry" }),
    );
    let (_, method, params) = driver_call(&out);
    assert_eq!(method, "binding/prompt");
    assert_eq!(params["text"], "fix the build");
    assert_eq!(params["origin"]["action"], "retry");
}
