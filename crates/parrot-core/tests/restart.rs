//! A simulated crash and restart (spec 34, Appendix E7, E8): the store a first core wrote is fed
//! to a second one, which must start nothing, hold its queues, mark unconfirmed prompts
//! `uncertain` and expire `connection` requests.

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, Peer, Restore, Stored};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What a store would hold after the persist records so far.
#[derive(Default)]
struct FakeStore {
    projections: BTreeMap<(String, String), Value>,
    log: Vec<Stored>,
}

impl FakeStore {
    fn apply(&mut self, effects: &[Effect]) {
        for e in effects {
            let Effect::Persist { record: r } = e else {
                continue;
            };
            let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
            match r["op"].as_str() {
                Some("put") => {
                    self.projections.insert((s("kind"), s("id")), r["value"].clone());
                }
                Some("delete") => {
                    self.projections.remove(&(s("kind"), s("id")));
                }
                Some("append") => self.log.push(Stored {
                    seq: r["seq"].as_u64().unwrap(),
                    session: r["session"].as_str().map(str::to_string),
                    at_ms: r["at"].as_u64().unwrap(),
                    update: r["update"].clone(),
                }),
                _ => {}
            }
        }
    }

    fn restore(&self) -> Restore {
        Restore {
            head: self.log.last().map_or(0, |s| s.seq),
            projections: self
                .projections
                .iter()
                .map(|((k, i), v)| (k.clone(), i.clone(), v.clone()))
                .collect(),
            recent: self.log.clone(),
        }
    }
}

struct Sim {
    core: Core,
    clock: ManualClock,
    store: FakeStore,
    next_id: u64,
}

impl Sim {
    fn new(ids: u64) -> Self {
        let clock = ManualClock::default();
        clock.set(1_700_000_000_000);
        Self {
            core: Core::new(Box::new(clock.clone()), Box::new(SeqIds::starting_at(ids))),
            clock,
            store: FakeStore::default(),
            next_id: 0,
        }
    }

    fn handle(&mut self, input: Input) -> Vec<Effect> {
        let effects = self.core.handle(input);
        self.store.apply(&effects);
        effects
    }

    fn connect(&mut self, conn: &str) {
        self.connect_with(conn, json!([]));
    }

    fn connect_with(&mut self, conn: &str, provides: Value) {
        self.handle(Input::Connected {
            conn: conn.into(),
            peer: Peer::Owner,
        });
        let params = json!({ "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" },
            "features": {}, "provides": provides });
        self.call(conn, "initialize", params).0.expect("initialize");
    }

    fn call(&mut self, conn: &str, method: &str, params: Value) -> (Result<Value, i64>, Vec<Effect>) {
        self.next_id += 1;
        let id = json!(self.next_id);
        let effects = self.handle(Input::Call {
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call("con_a", method, params.clone())
            .0
            .unwrap_or_else(|e| panic!("{method} {params} failed with {e}"))
    }

    fn driver(&mut self, binding: &str, update: Value) -> Vec<Effect> {
        let now = self.clock.0.load(std::sync::atomic::Ordering::SeqCst);
        self.clock.set(now + 1);
        self.handle(Input::DriverUpdate {
            binding: binding.into(),
            update: json!({ "binding": binding, "update": update }),
        })
    }

    fn started(&mut self, binding: &str) -> Vec<Effect> {
        self.handle(Input::DriverReply {
            binding: binding.into(),
            call: u64::MAX,
            result: Ok(json!({
                "isolation": "none", "level": "write", "network": false,
                "options": [{ "id": "model", "role": "model", "label": "Model",
                    "schema": { "type": "string", "enum": ["fast", "deep"] }, "value": "fast", "applies": "next_turn" }],
                "actions": [],
            })),
        })
    }
}

fn start_of(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::StartBinding { binding, .. } => Some(binding.clone()),
        _ => None,
    })
}

fn prompts(effects: &[Effect]) -> Vec<(u64, Value)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::DriverCall {
                call, method, params, ..
            } if method == "binding/prompt" => Some((*call, params.clone())),
            _ => None,
        })
        .collect()
}

fn request(key: &str, answerable: &str) -> Value {
    json!({ "type": "request", "request": {
        "id": key, "kind": "question", "title": format!("Question {key}"), "level": "read",
        "choices": [{ "id": "yes", "label": "Yes" }, { "id": "no", "label": "No" }],
        "blocking": false, "answerable": answerable, "notify": "normal", "status": "open",
    }})
}

#[test]
fn a_restart_starts_nothing_and_keeps_what_was_stored() {
    let mut a = Sim::new(0);
    a.connect("con_a");
    let (r, effects) = a.call(
        "con_a",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }, "title": "Demo" }),
    );
    let session = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    let binding = start_of(&effects).expect("a binding starts");
    a.started(&binding);
    a.driver(
        &binding,
        json!({ "type": "state", "state": "idle", "background": [] }),
    );

    // A first prompt is sent; the driver never confirms it before the crash.
    let (r, effects) = a.call(
        "con_a",
        "session/prompt",
        json!({ "session": session, "text": "first" }),
    );
    let first = r.unwrap()["message"].as_str().unwrap().to_string();
    assert_eq!(prompts(&effects).len(), 1);
    a.driver(
        &binding,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let queued = a.ok(
        "session/prompt",
        json!({ "session": session, "text": "second", "delivery": "queue" }),
    );
    assert_eq!(queued["disposition"], "queued");
    a.driver(&binding, request("q-conn", "connection"));
    a.driver(&binding, request("q-msg", "message"));
    a.driver(
        &binding,
        json!({ "type": "item", "item": { "id": "k1", "kind": "assistant", "set": { "text": "Hel" } } }),
    );
    a.driver(
        &binding,
        json!({ "type": "item", "item": { "id": "k1", "append": { "text": "lo" } } }),
    );
    a.driver(
        &binding,
        json!({ "type": "usage", "usage": { "tokens": { "input": 3, "output": 4 } } }),
    );
    a.driver(
        &binding,
        json!({ "type": "actions", "actions": [{ "id": "compact", "label": "Compact", "scope": "session",
            "when": ["idle"], "effects": ["turn"], "source": "harness" }] }),
    );
    let head_before = a.core.state.seq;

    // The crash: a second core starts from what the first one stored.
    let mut b = Sim::new(10_000);
    b.store = std::mem::take(&mut a.store);
    let restore = b.store.restore();
    let effects = b.handle(Input::Restore(restore));
    assert!(start_of(&effects).is_none(), "a restart starts nothing");
    assert!(b.core.state.seq > head_before, "the sequence continues");
    b.connect("con_a");

    let s = b.ok("session/get", json!({ "session": session }))["session"].clone();
    assert_eq!(s["state"], "stopped", "{s}");
    assert_eq!(s["binding"]["status"], "stopped", "{s}");
    assert_eq!(s["title"], "Demo");

    let sub = b.ok("subscribe", json!({ "target": { "session": session } }));
    let snap = &sub["snapshot"]["sessions"][0];
    assert_eq!(snap["queue"]["paused"], true, "{snap}");
    assert_eq!(snap["queue"]["entries"][0]["text"], "second", "{snap}");
    let items = snap["items"].as_array().unwrap();
    let user = items
        .iter()
        .find(|i| i["id"] == first.as_str())
        .expect("the first prompt");
    assert_eq!(user["status"], "uncertain", "{user}");
    assert!(
        items
            .iter()
            .any(|i| i["kind"] == "assistant" && i["text"] == "Hello"),
        "{snap}"
    );
    assert_eq!(snap["usage"]["tokens"]["output"], 4, "{snap}");
    assert_eq!(snap["options"][0]["id"], "model", "{snap}");
    assert!(
        snap["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "compact"),
        "{snap}"
    );
    let open: Vec<&Value> = snap["requests"].as_array().unwrap().iter().collect();
    assert_eq!(open.len(), 1, "only the message request stays open: {snap}");
    assert_eq!(open[0]["answerable"], "message");
    let all = b.ok("request/list", json!({ "session": session }))["requests"].clone();
    let conn_req = all
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["answerable"] == "connection")
        .unwrap();
    assert_eq!(conn_req["status"], "expired");

    // Replay reaches across the restart.
    let (r, effects) = b.call(
        "con_a",
        "subscribe",
        json!({ "target": { "session": session }, "replayFrom": "1" }),
    );
    assert!(r.is_ok());
    assert!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Notify { .. }))
            .count()
            > 5
    );

    // `retry` resends the uncertain prompt: the session starts, the held queue goes first.
    let retried = b.ok("action/invoke", json!({ "session": session, "action": "retry" }));
    assert_eq!(retried["status"], "queued", "{retried}");
    let binding2 = b
        .core
        .state
        .sessions
        .by_id
        .get(&session)
        .and_then(|s| s.binding.clone())
        .unwrap();
    assert_ne!(binding2, binding);
    let queue = b.ok("queue/list", json!({ "session": session }))["queue"].clone();
    assert_eq!(queue["paused"], false, "{queue}");
    let effects = b.started(&binding2);
    let sent = prompts(&effects);
    assert_eq!(
        sent.len(),
        1,
        "the start reply makes the session idle and sends one entry"
    );
    assert_eq!(sent[0].1["text"], "second");
    b.driver(
        &binding2,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let effects = b.driver(
        &binding2,
        json!({ "type": "state", "state": "idle", "background": [] }),
    );
    let sent = prompts(&effects);
    assert_eq!(
        sent[0].1["text"], "first",
        "retry sends the uncertain prompt's text"
    );

    // The message request, answered now, becomes a prompt.
    let msg_req = open[0]["id"].clone();
    b.ok(
        "request/answer",
        json!({ "request": msg_req, "answer": { "choice": "yes" } }),
    );
    let queue = b.ok("queue/list", json!({ "session": session }))["queue"].clone();
    let text = queue["entries"][0]["text"].as_str().unwrap();
    assert!(text.contains("Question q-msg") && text.contains("Yes"), "{queue}");
}

#[test]
fn a_restart_without_a_prompt_keeps_the_queue_held() {
    let mut a = Sim::new(0);
    a.connect("con_a");
    let (r, effects) = a.call(
        "con_a",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" } }),
    );
    let session = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    let binding = start_of(&effects).unwrap();
    a.started(&binding);
    a.driver(
        &binding,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    a.ok(
        "session/prompt",
        json!({ "session": session, "text": "later", "delivery": "queue" }),
    );

    let mut b = Sim::new(10_000);
    b.store = std::mem::take(&mut a.store);
    let restore = b.store.restore();
    b.handle(Input::Restore(restore));
    b.connect("con_a");
    // Time passing and other calls don't release the queue.
    b.handle(Input::Tick {
        now_ms: 1_800_000_000_000,
    });
    let queue = b.ok("queue/list", json!({ "session": session }))["queue"].clone();
    assert_eq!(queue["paused"], true);
    // `session/start` does, and the entry goes once the harness is ready.
    let (r, effects) = b.call("con_a", "session/start", json!({ "session": session }));
    r.unwrap();
    let binding2 = start_of(&effects).unwrap();
    let effects = b.started(&binding2);
    assert_eq!(prompts(&effects)[0].1["text"], "later");
}

#[test]
fn model_is_set_on_a_stopped_session_and_used_at_the_next_start() {
    let mut a = Sim::new(0);
    a.connect("con_a");
    let (r, effects) = a.call(
        "con_a",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" } }),
    );
    let session = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    let binding = start_of(&effects).unwrap();
    a.started(&binding);
    let mut b = Sim::new(10_000);
    b.store = std::mem::take(&mut a.store);
    let restore = b.store.restore();
    b.handle(Input::Restore(restore));
    b.connect("con_a");
    assert_eq!(
        b.call(
            "con_a",
            "option/set",
            json!({ "session": session, "values": { "model": "huge" } })
        )
        .0,
        Err(-32602)
    );
    let set = b.ok(
        "option/set",
        json!({ "session": session, "values": { "model": "deep" } }),
    );
    assert_eq!(set["options"][0]["value"], "deep");
    let (_, effects) = b.call("con_a", "session/start", json!({ "session": session }));
    let start = effects
        .iter()
        .find_map(|e| match e {
            Effect::StartBinding { start, .. } => Some(start.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(start["options"]["model"], "deep");
}

/// A session with a started, idle binding; returns the session and binding ids.
fn idle_session(a: &mut Sim, extra: Value) -> (String, String) {
    let mut params = json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" } });
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    let (r, effects) = a.call("con_a", "session/new", params);
    let session = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    let binding = start_of(&effects).unwrap();
    a.started(&binding);
    a.driver(
        &binding,
        json!({ "type": "state", "state": "idle", "background": [] }),
    );
    (session, binding)
}

/// Carry the store of `a` over to a new core, as a crash and restart would.
fn restart(a: &mut Sim) -> Sim {
    let mut b = Sim::new(10_000);
    b.store = std::mem::take(&mut a.store);
    let restore = b.store.restore();
    b.handle(Input::Restore(restore));
    b.connect("con_a");
    b
}

fn turn_updates(effects: &[Effect]) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Persist { record } if record["op"] == "append" && record["update"]["type"] == "turn" => {
                Some(record["update"]["turn"].clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_turn_running_at_a_restart_ends_interrupted_and_can_continue() {
    let mut a = Sim::new(0);
    a.connect("con_a");
    let (session, binding) = idle_session(&mut a, json!({}));
    a.ok("session/prompt", json!({ "session": session, "text": "work" }));
    a.driver(&binding, json!({ "type": "turn", "turn": { "id": "t1", "status": "running", "continuable": false, "started": "2023-11-14T22:13:20Z" } }));

    let mut b = Sim::new(10_000);
    b.store = std::mem::take(&mut a.store);
    let restore = b.store.restore();
    let effects = b.handle(Input::Restore(restore));
    let turns = turn_updates(&effects);
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert_eq!(turns[0]["status"], "interrupted");
    assert_eq!(turns[0]["continuable"], true);
    assert!(turns[0]["ended"].is_string());
    // It is ended once: a second restart has nothing left to interrupt.
    let mut c = Sim::new(20_000);
    c.store = std::mem::take(&mut b.store);
    let restore = c.store.restore();
    assert!(turn_updates(&c.handle(Input::Restore(restore))).is_empty());
    c.connect("con_a");
    let done = c.ok(
        "action/invoke",
        json!({ "session": session, "action": "continue" }),
    );
    assert_eq!(done["status"], "queued", "{done}");
}

#[test]
fn retry_resends_a_failed_turn_after_a_restart() {
    let mut a = Sim::new(0);
    a.connect("con_a");
    let (session, binding) = idle_session(&mut a, json!({}));
    let sent = a.ok(
        "session/prompt",
        json!({ "session": session, "text": "build it" }),
    );
    let message = sent["message"].as_str().unwrap().to_string();
    a.driver(
        &binding,
        json!({ "type": "item", "item": { "id": "u1", "kind": "user",
        "set": { "text": "build it", "status": "sent", "message": message } } }),
    );
    a.driver(
        &binding,
        json!({ "type": "turn", "turn": { "id": "t1", "status": "running", "input": "u1",
        "continuable": false, "started": "2023-11-14T22:13:20Z" } }),
    );
    a.driver(
        &binding,
        json!({ "type": "turn", "turn": { "id": "t1", "status": "failed", "input": "u1",
        "continuable": true, "started": "2023-11-14T22:13:20Z",
        "error": { "code": "overloaded", "message": "try later", "retryable": true } } }),
    );
    a.driver(
        &binding,
        json!({ "type": "state", "state": "idle", "background": [] }),
    );

    let mut b = restart(&mut a);
    let retried = b.ok("action/invoke", json!({ "session": session, "action": "retry" }));
    assert_eq!(retried["status"], "queued", "{retried}");
    let binding2 = b.core.state.sessions.by_id[&session].binding.clone().unwrap();
    let effects = b.started(&binding2);
    let sent = prompts(&effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["text"], "build it");
}

#[test]
fn an_approval_open_at_a_restart_is_answered_and_collected_with_its_key() {
    let spec = json!({ "name": "acme.deploy", "description": "Deploy for tests", "input": { "type": "object" },
        "output": { "type": "object" }, "effects": ["write"], "level": "host" });
    let mut a = Sim::new(0);
    a.connect("con_a");
    a.connect_with("con_p", json!([spec.clone()]));
    let (session, _) = idle_session(&mut a, json!({}));
    let call = json!({ "function": "acme.deploy", "session": session, "input": {}, "key": "deploy-1" });
    let (r, _) = a.call("con_a", "function/call", call.clone());
    assert_eq!(r, Err(0), "the call waits for its approval");
    let request = a.core.state.functions.approvals.keys().next().unwrap().clone();

    let mut b = restart(&mut a);
    b.connect_with("con_p", json!([spec]));
    let open = b.ok("request/list", json!({ "session": session }))["requests"].clone();
    let req = open
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == request.as_str())
        .unwrap();
    assert_eq!(req["status"], "open", "{open}");
    b.ok(
        "request/answer",
        json!({ "request": request, "answer": { "choice": "allow" } }),
    );
    // The caller comes back with its key; the call goes to the provider now.
    let (_, effects) = b.call("con_a", "function/call", call);
    let forwarded = effects
        .iter()
        .any(|e| matches!(e, Effect::ClientCall { conn, .. } if conn == "con_p"));
    assert!(forwarded, "the approved call runs on the repeat");
}

#[test]
fn seconds_spent_are_kept_across_a_restart() {
    let mut a = Sim::new(0);
    a.connect("con_a");
    let (session, binding) = idle_session(&mut a, json!({ "ceiling": { "budget": { "seconds": 600 } } }));
    a.ok("session/prompt", json!({ "session": session, "text": "work" }));
    a.driver(&binding, json!({ "type": "turn", "turn": { "id": "t1", "status": "running", "continuable": false, "started": "2023-11-14T22:13:20Z" } }));
    let now = a.clock.0.load(std::sync::atomic::Ordering::SeqCst);
    a.clock.set(now + 42_000);
    a.handle(Input::Tick { now_ms: now + 42_000 });
    let mut b = restart(&mut a);
    let s = b.ok("session/get", json!({ "session": session }))["session"].clone();
    assert_eq!(s["budget"]["used"]["seconds"], 42, "{s}");
}
