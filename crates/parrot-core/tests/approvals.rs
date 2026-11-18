//! Function approvals (E5, 27.2): ceiling patterns, requests for calls above the session's level,
//! `pending_approval` after the wait, and retries with the same key.

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, Peer};
use serde_json::{Value, json};

struct Sim {
    core: Core,
    clock: ManualClock,
    next_id: u64,
}

impl Sim {
    fn new() -> Self {
        let clock = ManualClock::default();
        clock.set(1_700_000_000_000);
        Sim {
            core: Core::new(Box::new(clock.clone()), Box::new(SeqIds::default())),
            clock,
            next_id: 0,
        }
    }

    fn connect(&mut self, conn: &str, provides: Value) {
        self.core.handle(Input::Connected {
            conn: conn.into(),
            peer: Peer::Owner,
        });
        let (r, _) = self.call(
            conn,
            "initialize",
            json!({ "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" },
                "features": {}, "provides": provides }),
        );
        r.expect("initialize");
    }

    fn send(&mut self, conn: &str, method: &str, params: Value) -> (Value, Vec<Effect>) {
        self.next_id += 1;
        let id = json!(self.next_id);
        let effects = self.core.handle(Input::Call {
            conn: conn.into(),
            id: Some(id.clone()),
            method: method.into(),
            params,
        });
        (id, effects)
    }

    fn call(&mut self, conn: &str, method: &str, params: Value) -> (Result<Value, i64>, Vec<Effect>) {
        let (id, effects) = self.send(conn, method, params);
        (reply(&effects, &id).unwrap_or(Err(0)), effects)
    }

    fn advance(&mut self, ms: u64) -> Vec<Effect> {
        let now = self.clock.0.load(std::sync::atomic::Ordering::SeqCst) + ms;
        self.clock.set(now);
        self.core.handle(Input::Tick { now_ms: now })
    }
}

fn reply(effects: &[Effect], id: &Value) -> Option<Result<Value, i64>> {
    effects.iter().find_map(|e| match e {
        Effect::Reply { id: rid, result, .. } if rid == id => Some(result.clone().map_err(|e| e.code)),
        _ => None,
    })
}

fn client_call(effects: &[Effect]) -> Option<(String, u64, Value)> {
    effects.iter().find_map(|e| match e {
        Effect::ClientCall {
            conn, call, params, ..
        } if conn == "con_p" => Some((conn.clone(), *call, params.clone())),
        _ => None,
    })
}

fn spec(name: &str, level: &str, effects: Value) -> Value {
    json!({ "name": name, "description": format!("{name} for tests"), "input": { "type": "object" },
        "output": { "type": "object" }, "effects": effects, "level": level })
}

/// An owner, a provider of three functions, and an idle session whose binding is at `write`.
fn setup() -> (Sim, String) {
    setup_with(json!({ "functions": ["acme.*"] }), "write")
}

fn setup_with(ceiling: Value, level: &str) -> (Sim, String) {
    let mut sim = Sim::new();
    sim.connect("con_o", json!([]));
    sim.connect(
        "con_p",
        json!([
            spec("acme.peek", "read", json!(["read"])),
            spec("acme.deploy", "host", json!(["write"])),
            spec("other.thing", "read", json!(["read"])),
        ]),
    );
    let (r, effects) = sim.call(
        "con_o",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }, "ceiling": ceiling }),
    );
    let session = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    let binding = effects
        .iter()
        .find_map(|e| match e {
            Effect::StartBinding { binding, .. } => Some(binding.clone()),
            _ => None,
        })
        .unwrap();
    sim.core.handle(Input::DriverReply {
        binding: binding.clone(),
        call: u64::MAX,
        result: Ok(
            json!({ "isolation": "none", "level": level, "network": false, "options": [], "actions": [] }),
        ),
    });
    sim.core.state.functions.wait_ms = Some(1_000);
    (sim, session)
}

fn call_params(session: &str, function: &str, key: &str) -> Value {
    json!({ "function": function, "session": session, "input": { "target": "staging" }, "key": key })
}

#[test]
fn calls_outside_the_ceiling_patterns_are_forbidden() {
    let (mut sim, session) = setup();
    let (r, _) = sim.call(
        "con_o",
        "function/call",
        call_params(&session, "other.thing", "k0"),
    );
    assert_eq!(r, Err(-32003));
    // Within the patterns and the level, the call goes straight to the provider.
    let (_, effects) = sim.send("con_o", "function/call", call_params(&session, "acme.peek", "k1"));
    let (_, _, params) = client_call(&effects).expect("the provider is called");
    assert_eq!(params["session"], session.as_str());
}

#[test]
fn a_call_above_the_level_waits_for_an_approval_and_completes_on_retry() {
    let (mut sim, session) = setup();
    let (id, effects) = sim.send(
        "con_o",
        "function/call",
        call_params(&session, "acme.deploy", "k1"),
    );
    assert!(reply(&effects, &id).is_none(), "the call waits");
    assert!(
        client_call(&effects).is_none(),
        "nothing runs before the approval"
    );
    let request = sim
        .core
        .state
        .requests
        .by_id
        .values()
        .find(|r| r.request.session == session)
        .map(|r| serde_json::to_value(&r.request).unwrap())
        .expect("a request is open");
    assert_eq!(request["subject"]["type"], "function", "{request}");
    assert_eq!(request["subject"]["function"], "acme.deploy", "{request}");
    let rid = request["id"].as_str().unwrap().to_string();

    // Unanswered within the wait, the call returns `pending_approval`.
    let effects = sim.advance(1_001);
    let r = reply(&effects, &id).expect("the caller hears back").unwrap();
    assert_eq!(r["error"]["code"], "pending_approval", "{r}");
    assert_eq!(r["error"]["request"], rid.as_str(), "{r}");
    // While open, a retry answers the same at once.
    let (r, _) = sim.call(
        "con_o",
        "function/call",
        call_params(&session, "acme.deploy", "k1"),
    );
    assert_eq!(r.unwrap()["error"]["code"], "pending_approval");

    let (r, effects) = sim.call(
        "con_o",
        "request/answer",
        json!({ "request": rid, "answer": { "choice": "allow" } }),
    );
    r.unwrap();
    let (_, call, _) = client_call(&effects).expect("the approved call runs");
    sim.core.handle(Input::ClientReply {
        conn: "con_p".into(),
        call,
        result: Ok(json!({ "output": { "deployed": true } })),
    });
    let (r, _) = sim.call(
        "con_o",
        "function/call",
        call_params(&session, "acme.deploy", "k1"),
    );
    assert_eq!(r.unwrap()["output"]["deployed"], true);
    let (r, _) = sim.call(
        "con_o",
        "request/answer",
        json!({ "request": rid, "answer": { "choice": "allow" } }),
    );
    assert_eq!(r, Err(-32013));
}

#[test]
fn a_denied_call_returns_denied() {
    let (mut sim, session) = setup();
    let (id, _) = sim.send(
        "con_o",
        "function/call",
        call_params(&session, "acme.deploy", "k2"),
    );
    let rid = sim.core.state.functions.approvals.keys().next().cloned().unwrap();
    let (_, effects) = sim.send(
        "con_o",
        "request/answer",
        json!({ "request": rid, "answer": { "choice": "deny" } }),
    );
    let r = reply(&effects, &id)
        .expect("the waiting caller hears back")
        .unwrap();
    assert_eq!(r["error"]["code"], "denied", "{r}");
    let (r, _) = sim.call(
        "con_o",
        "function/call",
        call_params(&session, "acme.deploy", "k2"),
    );
    assert_eq!(r.unwrap()["error"]["code"], "denied");
}

#[test]
fn effects_the_ceiling_does_not_list_open_a_request() {
    let (mut sim, session) = setup_with(json!({ "functions": ["acme.*"], "effects": ["read"] }), "host");
    let (id, effects) = sim.send(
        "con_o",
        "function/call",
        call_params(&session, "acme.deploy", "k3"),
    );
    assert!(reply(&effects, &id).is_none());
    assert_eq!(sim.core.state.functions.approvals.len(), 1);
}

#[test]
fn the_wait_follows_the_harness_tool_timeout_from_its_egg() {
    use parrot_core::agents::AgentEgg;
    use parrot_proto::agent::{Control, DriverKind};
    let egg = |timeout| AgentEgg {
        id: "claude".into(),
        name: "Claude".into(),
        driver_name: "claude".into(),
        driver_version: "1".into(),
        kind: DriverKind::Process,
        data: "1".into(),
        controls: vec![Control::Native],
        cli_versions: None,
        refused: None,
        tool_timeout: timeout,
    };
    let (mut sim, session) = setup();
    sim.core.state.functions.wait_ms = None;
    assert_eq!(parrot_core::functions::approval_wait_ms(&sim.core.state, &session), 170_000);
    parrot_core::agents::load(&mut sim.core.state, vec![egg(Some(60))]);
    // 60 seconds less a tenth.
    assert_eq!(parrot_core::functions::approval_wait_ms(&sim.core.state, &session), 54_000);
    let (_, effects) = sim.send("con_o", "function/call", call_params(&session, "acme.deploy", "k9"));
    let id = json!(sim.next_id);
    assert!(reply(&effects, &id).is_none(), "the call waits");
    assert!(reply(&sim.advance(53_000), &id).is_none(), "still within the limit");
    let r = reply(&sim.advance(1_000), &id).expect("an answer at the limit").unwrap();
    assert_eq!(r["error"]["code"], "pending_approval", "{r}");
}
