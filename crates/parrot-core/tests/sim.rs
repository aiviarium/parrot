//! Deterministic simulations of the core: scripted inputs in, effects and projections checked.

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, Peer};
use proptest::prelude::*;
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
        Self {
            core: Core::new(Box::new(clock.clone()), Box::new(SeqIds::default())),
            clock,
            next_id: 0,
        }
    }

    fn connect(&mut self, conn: &str) {
        self.core.handle(Input::Connected {
            conn: conn.into(),
            peer: Peer::Owner,
        });
        let r = self.call(
            conn,
            "initialize",
            json!({
                "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" }, "features": {}
            }),
        );
        r.0.expect("initialize");
    }

    /// A call; returns its result and every effect.
    fn call(&mut self, conn: &str, method: &str, params: Value) -> (Result<Value, i64>, Vec<Effect>) {
        self.next_id += 1;
        let id = json!(self.next_id);
        let effects = self.core.handle(Input::Call {
            conn: conn.into(),
            id: Some(id.clone()),
            method: method.into(),
            params,
        });
        let result = effects
            .iter()
            .find_map(|e| match e {
                Effect::Reply { id: rid, result, .. } if *rid == id => {
                    Some(result.clone().map_err(|e| e.code))
                }
                _ => None,
            })
            .unwrap_or(Err(0));
        (result, effects)
    }

    fn driver(&mut self, binding: &str, update: Value) -> Vec<Effect> {
        self.clock
            .set(self.clock.0.load(std::sync::atomic::Ordering::SeqCst) + 1);
        self.core.handle(Input::DriverUpdate {
            binding: binding.into(),
            update: json!({ "binding": binding, "update": update }),
        })
    }

    /// Create a session and bring its binding to idle; returns (session, binding).
    fn idle_session(&mut self, conn: &str) -> (String, String) {
        let (r, effects) = self.call(
            conn,
            "session/new",
            json!({
                "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }
            }),
        );
        let r = r.expect("session/new");
        let session = r["session"]["id"].as_str().unwrap().to_string();
        let binding = effects
            .iter()
            .find_map(|e| match e {
                Effect::StartBinding { binding, .. } => Some(binding.clone()),
                _ => None,
            })
            .expect("StartBinding");
        self.driver(
            &binding,
            json!({ "type": "state", "state": "idle", "background": [] }),
        );
        (session, binding)
    }
}

fn updates(effects: &[Effect], conn: &str) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Notify {
                conn: c,
                method,
                params,
            } if c == conn && method == "update" => Some(params.clone()),
            _ => None,
        })
        .collect()
}

fn driver_calls(effects: &[Effect], method: &str) -> Vec<(u64, Value)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::DriverCall {
                call,
                method: m,
                params,
                ..
            } if m == method => Some((*call, params.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn calls_before_initialize_fail() {
    let mut sim = Sim::new();
    sim.core.handle(Input::Connected {
        conn: "con_a".into(),
        peer: Peer::Owner,
    });
    let (r, _) = sim.call("con_a", "session/list", json!({}));
    assert_eq!(r, Err(-32010));
    let (r, _) = sim.call(
        "con_a",
        "initialize",
        json!({
            "protocol": [7], "peer": { "kind": "client", "name": "x", "version": "1" }, "features": {}
        }),
    );
    assert_eq!(r, Err(-32011));
}

#[test]
fn token_scopes_limit_calls() {
    let mut sim = Sim::new();
    sim.core.handle(Input::Connected {
        conn: "con_t".into(),
        peer: Peer::Token {
            token: "tok_x".into(),
            scopes: vec!["read".into()],
            sessions: None,
        },
    });
    let (r, _) = sim.call(
        "con_t",
        "initialize",
        json!({
            "protocol": [1], "peer": { "kind": "client", "name": "t", "version": "1" }, "features": {}
        }),
    );
    assert_eq!(r.unwrap()["connection"]["scopes"], json!(["read"]));
    let (r, _) = sim.call(
        "con_t",
        "session/new",
        json!({
            "agent": "claude", "workspace": { "kind": "dir", "path": "/w" }
        }),
    );
    assert_eq!(r, Err(-32003));
    assert!(sim.call("con_t", "agent/list", json!({})).0.is_ok());
}

#[test]
fn session_new_emits_start_binding() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (r, effects) = sim.call(
        "con_a",
        "session/new",
        json!({
            "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }, "title": "Demo"
        }),
    );
    let r = r.unwrap();
    assert_eq!(r["session"]["state"], "starting");
    assert_eq!(r["session"]["binding"]["status"], "starting");
    assert!(r["subscription"].is_string());
    let start = effects
        .iter()
        .find_map(|e| match e {
            Effect::StartBinding { agent, start, .. } => Some((agent.clone(), start.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(start.0, "claude");
    let bs: parrot_proto::driver::BindingStart = serde_json::from_value(start.1).unwrap();
    assert_eq!(bs.cwd, "/work/demo");
    assert_eq!(bs.workspace, "/work/demo");
    // The caller is subscribed and sees the session update after the reply.
    let ups = updates(&effects, "con_a");
    assert!(ups.iter().any(|u| u["update"]["type"] == "session"));
    let reply_at = effects
        .iter()
        .position(|e| matches!(e, Effect::Reply { .. }))
        .unwrap();
    let first_notify = effects
        .iter()
        .position(|e| matches!(e, Effect::Notify { .. }))
        .unwrap();
    assert!(reply_at < first_notify);
    let (list, _) = sim.call("con_a", "session/list", json!({}));
    assert_eq!(list.unwrap()["sessions"][0]["title"], "Demo");
}

#[test]
fn state_update_makes_session_idle() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, _) = sim.idle_session("con_a");
    let (r, _) = sim.call("con_a", "session/get", json!({ "session": session }));
    let s = r.unwrap()["session"].clone();
    assert_eq!(s["state"], "idle");
    assert_eq!(s["binding"]["status"], "running");
}

#[test]
fn prompt_emits_binding_prompt() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, binding) = sim.idle_session("con_a");
    let (r, effects) = sim.call(
        "con_a",
        "session/prompt",
        json!({ "session": session, "text": "hello" }),
    );
    let r = r.unwrap();
    assert_eq!(r["disposition"], "started");
    let calls = driver_calls(&effects, "binding/prompt");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1["binding"], binding);
    assert_eq!(calls[0].1["message"], r["message"]);
    assert_eq!(calls[0].1["steer"], false);
    // The runtime creates the user item under the message id; the driver's echo merges into it.
    sim.driver(
        &binding,
        json!({ "type": "item", "item": {
            "id": "u1", "kind": "user", "set": { "text": "hello", "status": "sent", "message": r["message"] }
        }}),
    );
    let (item, _) = sim.call(
        "con_a",
        "item/get",
        json!({ "session": session, "item": r["message"] }),
    );
    let item = item.unwrap()["item"].clone();
    assert_eq!(item["kind"], "user");
    assert_eq!(item["text"], "hello");
    let items = &sim.core.state.items.by_session[&session];
    assert_eq!(items.order.len(), 1);
}

#[test]
fn prompt_while_running_is_queued_then_dispatched() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, binding) = sim.idle_session("con_a");
    let (_, effects) = sim.call(
        "con_a",
        "session/prompt",
        json!({ "session": session, "text": "one" }),
    );
    let (call, _) = driver_calls(&effects, "binding/prompt")[0].clone();
    sim.core.handle(Input::DriverReply {
        binding: binding.clone(),
        call,
        result: Ok(json!({ "disposition": "started" })),
    });
    sim.driver(
        &binding,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let (r, effects) = sim.call(
        "con_a",
        "session/prompt",
        json!({ "session": session, "text": "two" }),
    );
    let r = r.unwrap();
    assert_eq!(r["disposition"], "queued");
    assert!(driver_calls(&effects, "binding/prompt").is_empty());
    let (q, _) = sim.call("con_a", "queue/list", json!({ "session": session }));
    let q = q.unwrap();
    assert_eq!(q["queue"]["entries"][0]["text"], "two");
    assert_eq!(q["queue"]["entries"][0]["kind"], "prompt");
    let effects = sim.driver(
        &binding,
        json!({ "type": "state", "state": "idle", "background": [] }),
    );
    let calls = driver_calls(&effects, "binding/prompt");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1["text"], "two");
    assert_eq!(calls[0].1["message"], r["message"]);
    let (q, _) = sim.call("con_a", "queue/list", json!({ "session": session }));
    assert_eq!(q.unwrap()["queue"]["entries"], json!([]));
}

#[test]
fn cancel_pauses_the_queue_and_resume_dispatches() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, binding) = sim.idle_session("con_a");
    sim.call(
        "con_a",
        "session/prompt",
        json!({ "session": session, "text": "one" }),
    )
    .0
    .unwrap();
    sim.driver(
        &binding,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let (r, _) = sim.call(
        "con_a",
        "session/prompt",
        json!({ "session": session, "text": "two", "delivery": "queue" }),
    );
    assert_eq!(r.unwrap()["disposition"], "queued");
    let effects = sim.core.handle(Input::Call {
        conn: "con_a".into(),
        id: Some(json!("c")),
        method: "session/cancel".into(),
        params: json!({ "session": session }),
    });
    let (call, _) = driver_calls(&effects, "binding/cancel")[0].clone();
    let effects = sim.core.handle(Input::DriverReply {
        binding: binding.clone(),
        call,
        result: Ok(json!({ "interrupted": true })),
    });
    let reply = effects
        .iter()
        .find_map(|e| match e {
            Effect::Reply { result, .. } => Some(result.clone().unwrap()),
            _ => None,
        })
        .unwrap();
    assert_eq!(reply, json!({ "interrupted": true, "queued": 1 }));
    // Paused: idle does not dispatch.
    let effects = sim.driver(
        &binding,
        json!({ "type": "state", "state": "idle", "background": [] }),
    );
    assert!(driver_calls(&effects, "binding/prompt").is_empty());
    let (_, effects) = sim.call("con_a", "queue/resume", json!({ "session": session }));
    assert_eq!(driver_calls(&effects, "binding/prompt").len(), 1);
}

#[test]
fn streamed_appends_are_published_with_increasing_seq() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, binding) = sim.idle_session("con_a");
    let mut seqs = Vec::new();
    let mut all = sim.driver(
        &binding,
        json!({ "type": "item", "item": {
            "id": "a1", "kind": "assistant", "set": { "text": "" }, "streaming": true
        }}),
    );
    for chunk in ["Hel", "lo", "!"] {
        all.extend(sim.driver(
            &binding,
            json!({ "type": "item", "item": { "id": "a1", "append": { "text": chunk } } }),
        ));
    }
    for u in updates(&all, "con_a") {
        assert_eq!(u["update"]["type"], "item");
        assert_eq!(u["session"], session);
        assert_eq!(u["runtime"], "rt_local");
        seqs.push(u["seq"].as_str().unwrap().parse::<u64>().unwrap());
    }
    assert_eq!(seqs.len(), 4);
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    // Item patches are persisted as `append` records with the item id.
    let persisted: Vec<&Value> = all
        .iter()
        .filter_map(|e| match e {
            Effect::Persist { record } if record["op"] == "append" => Some(record),
            _ => None,
        })
        .collect();
    assert!(
        persisted
            .iter()
            .all(|r| r["session"] == session && r["item"].is_string())
    );
    let item_id = updates(&all, "con_a")[0]["update"]["item"]["id"].clone();
    let (item, _) = sim.call(
        "con_a",
        "item/get",
        json!({ "session": session, "item": item_id }),
    );
    assert_eq!(item.unwrap()["item"]["text"], "Hello!");
}

#[test]
fn two_subscribers_and_one_disconnects() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    sim.connect("con_b");
    let (session, binding) = sim.idle_session("con_a");
    let (r, _) = sim.call("con_b", "subscribe", json!({ "target": { "session": session } }));
    let r = r.unwrap();
    assert_eq!(r["snapshot"]["sessions"][0]["session"]["id"], session);
    let effects = sim.driver(
        &binding,
        json!({ "type": "notice", "level": "info", "text": "hi" }),
    );
    assert_eq!(updates(&effects, "con_a").len(), 1);
    assert_eq!(updates(&effects, "con_b").len(), 1);
    sim.core.handle(Input::Disconnected { conn: "con_a".into() });
    let effects = sim.driver(
        &binding,
        json!({ "type": "notice", "level": "info", "text": "again" }),
    );
    assert!(updates(&effects, "con_a").is_empty());
    assert_eq!(updates(&effects, "con_b").len(), 1);
}

#[test]
fn replay_from_a_cursor() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, binding) = sim.idle_session("con_a");
    let head = sim.core.state.seq;
    for text in ["one", "two", "three"] {
        sim.driver(
            &binding,
            json!({ "type": "notice", "level": "info", "text": text }),
        );
    }
    sim.connect("con_b");
    let (r, effects) = sim.call(
        "con_b",
        "subscribe",
        json!({
            "target": { "session": session }, "replayFrom": head.to_string()
        }),
    );
    let r = r.unwrap();
    assert!(r.get("snapshot").is_none());
    let texts: Vec<Value> = updates(&effects, "con_b")
        .iter()
        .map(|u| u["update"]["text"].clone())
        .collect();
    assert_eq!(texts, vec![json!("one"), json!("two"), json!("three")]);
    assert_eq!(r["head"], sim.core.state.seq.to_string());
}

#[test]
fn replay_beyond_the_recent_log_is_unavailable() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (_, binding) = sim.idle_session("con_a");
    for _ in 0..parrot_core::subscriptions::RECENT {
        sim.driver(
            &binding,
            json!({ "type": "notice", "level": "info", "text": "x" }),
        );
    }
    let (r, _) = sim.call(
        "con_a",
        "subscribe",
        json!({ "target": { "index": true }, "replayFrom": "1" }),
    );
    assert_eq!(r, Err(-32012));
}

#[test]
fn exit_marks_lost_or_stopped() {
    let mut sim = Sim::new();
    sim.connect("con_a");
    let (session, binding) = sim.idle_session("con_a");
    sim.core.handle(Input::DriverExited {
        binding: binding.clone(),
        detail: "crashed".into(),
    });
    let (r, _) = sim.call("con_a", "session/get", json!({ "session": session }));
    assert_eq!(r.unwrap()["session"]["state"], "lost");
    let (r, effects) = sim.call("con_a", "session/start", json!({ "session": session }));
    let b2 = r.unwrap()["binding"]["id"].as_str().unwrap().to_string();
    assert!(effects.iter().any(|e| matches!(e, Effect::StartBinding { .. })));
    sim.driver(&b2, json!({ "type": "state", "state": "idle", "background": [] }));
    let (_, effects) = sim.call("con_a", "session/stop", json!({ "session": session }));
    assert!(effects.iter().any(|e| matches!(e, Effect::StopBinding { .. })));
    sim.core.handle(Input::DriverExited {
        binding: b2,
        detail: "exit 0".into(),
    });
    let (r, _) = sim.call("con_a", "session/get", json!({ "session": session }));
    assert_eq!(r.unwrap()["session"]["state"], "stopped");
}

#[derive(Debug, Clone)]
enum Step {
    Prompt,
    Idle,
    Running,
    Item(u8),
    Notice,
    Subscribe,
    Disconnect,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        Just(Step::Prompt),
        Just(Step::Idle),
        Just(Step::Running),
        (0u8..4).prop_map(Step::Item),
        Just(Step::Notice),
        Just(Step::Subscribe),
        Just(Step::Disconnect),
    ]
}

proptest! {
    #[test]
    fn seq_strictly_increases(steps in proptest::collection::vec(step(), 1..60)) {
        let mut sim = Sim::new();
        sim.connect("con_a");
        sim.connect("con_b");
        let (session, binding) = sim.idle_session("con_a");
        let mut last = 0u64;
        for s in steps {
            let effects = match s {
                Step::Prompt => sim.call("con_a", "session/prompt", json!({ "session": session, "text": "p" })).1,
                Step::Idle => sim.driver(&binding, json!({ "type": "state", "state": "idle", "background": [] })),
                Step::Running => sim.driver(&binding, json!({ "type": "state", "state": "running", "background": [] })),
                Step::Item(k) => sim.driver(&binding, json!({ "type": "item", "item": {
                    "id": format!("k{k}"), "kind": "assistant", "append": { "text": "x" } } })),
                Step::Notice => sim.driver(&binding, json!({ "type": "notice", "level": "info", "text": "n" })),
                Step::Subscribe => sim.call("con_b", "subscribe", json!({ "target": { "session": session } })).1,
                Step::Disconnect => {
                    let e = sim.core.handle(Input::Disconnected { conn: "con_b".into() });
                    sim.connect("con_b");
                    e
                }
            };
            for e in &effects {
                if let Effect::Persist { record } = e && record["op"] == "append" {
                    let seq = record["seq"].as_u64().unwrap();
                    prop_assert!(seq > last);
                    last = seq;
                }
            }
            // Per subscription, notifications arrive in seq order too.
            let mut by_sub: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
            for conn in ["con_a", "con_b"] {
                for u in updates(&effects, conn) {
                    by_sub
                        .entry(u["subscription"].as_str().unwrap().to_string())
                        .or_default()
                        .push(u["seq"].as_str().unwrap().parse().unwrap());
                }
            }
            for seqs in by_sub.values() {
                prop_assert!(seqs.windows(2).all(|w| w[0] < w[1]));
            }
        }
    }
}
