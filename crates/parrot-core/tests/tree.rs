//! Deterministic simulations of the `tree` and `seed` profiles: children, tasks, waking,
//! ceilings, budgets, workspaces, rebinds, rewinds and bundles.

mod common;

use parrot_core::ceilings::{Ceiling, intersect, narrows};
use parrot_core::clock::{Clock, ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, Peer, Restore, Stored};
use parrot_proto::rpc::RpcError;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Store {
    projections: BTreeMap<(String, String), Value>,
    log: Vec<Stored>,
}

impl Store {
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
    store: Store,
    next_id: u64,
    /// Every effect so far.
    seen: Vec<Effect>,
}

const OWNER: &str = "con_owner";

impl Sim {
    fn new() -> Self {
        Self::with_ids(0)
    }

    fn with_ids(ids: u64) -> Self {
        let clock = ManualClock::default();
        clock.set(1_700_000_000_000);
        let mut sim = Self {
            core: common::core(Box::new(clock.clone()), Box::new(SeqIds::starting_at(ids))),
            clock,
            store: Store::default(),
            next_id: 0,
            seen: Vec::new(),
        };
        sim.connect(OWNER, None);
        sim
    }

    fn handle(&mut self, input: Input) -> Vec<Effect> {
        let effects = self.core.handle(input);
        self.store.apply(&effects);
        self.seen.extend(effects.iter().cloned());
        effects
    }

    /// Connect as the owner, or as a session principal with `act_as`.
    fn connect(&mut self, conn: &str, act_as: Option<&str>) {
        self.handle(Input::Connected {
            conn: conn.into(),
            peer: Peer::Owner,
        });
        let mut p = json!({ "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" }, "features": {} });
        if let Some(s) = act_as {
            p["_meta"] = json!({ "x.parrot/actAs": s });
        }
        self.call(conn, "initialize", p).0.expect("initialize");
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
        let result = reply(&effects, &id).unwrap_or(Err(0));
        (result, effects)
    }

    fn ok(&mut self, conn: &str, method: &str, params: Value) -> Value {
        let (r, _) = self.call(conn, method, params.clone());
        r.unwrap_or_else(|e| panic!("{method} {params} failed with {e}"))
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
            result: Ok(json!({ "isolation": "none", "level": "write", "network": false, "options": [], "actions": [] })),
        })
    }

    fn idle(&mut self, binding: &str) -> Vec<Effect> {
        self.driver(
            binding,
            json!({ "type": "state", "state": "idle", "background": [] }),
        )
    }

    /// Start a binding and bring it to idle.
    fn bring_up(&mut self, binding: &str) -> Vec<Effect> {
        let mut e = self.started(binding);
        e.extend(self.idle(binding));
        e
    }

    /// A session from `session/new`; returns (session, binding).
    fn new_session(&mut self, conn: &str, params: Value) -> (String, String) {
        let (r, effects) = self.call(conn, "session/new", params.clone());
        let r = r.unwrap_or_else(|e| panic!("session/new {params} failed with {e}"));
        let session = r["session"]["id"].as_str().unwrap().to_string();
        let binding = start_of(&effects).expect("StartBinding");
        (session, binding)
    }

    fn root(&mut self, extra: Value) -> (String, String) {
        let mut p = json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" },
            "grant": ["read", "prompt", "delegate", "workspace", "destructive"] });
        merge(&mut p, extra);
        let (s, b) = self.new_session(OWNER, p);
        self.bring_up(&b);
        (s, b)
    }

    /// Run one turn on a binding: running, an assistant message, completed, idle.
    fn turn(&mut self, binding: &str, key: &str, text: &str) -> Vec<Effect> {
        let at = "2026-01-01T00:00:00.000Z";
        let mut e = self.driver(
            binding,
            json!({ "type": "state", "state": "running", "background": [] }),
        );
        e.extend(self.driver(binding, json!({ "type": "turn", "turn": { "id": key, "status": "running", "continuable": false, "started": at } })));
        e.extend(self.driver(binding, json!({ "type": "item", "item": { "id": format!("{key}-a"), "kind": "assistant", "turn": key, "set": { "text": text } } })));
        e.extend(self.driver(binding, json!({ "type": "turn", "turn": { "id": key, "status": "completed", "continuable": false, "started": at, "ended": at } })));
        e.extend(self.idle(binding));
        e
    }

    fn get(&mut self, session: &str) -> Value {
        self.ok(OWNER, "session/get", json!({ "session": session }))["session"].clone()
    }

    fn task_status(&mut self, session: &str) -> String {
        self.get(session)["task"]["status"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }
}

fn merge(into: &mut Value, extra: Value) {
    if let (Some(a), Value::Object(b)) = (into.as_object_mut(), extra) {
        a.extend(b);
    }
}

fn reply(effects: &[Effect], id: &Value) -> Option<Result<Value, i64>> {
    effects.iter().find_map(|e| match e {
        Effect::Reply { id: rid, result, .. } if rid == id => Some(result.clone().map_err(|e| e.code)),
        _ => None,
    })
}

fn start_of(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::StartBinding { binding, .. } => Some(binding.clone()),
        _ => None,
    })
}

fn start_params(effects: &[Effect]) -> Option<Value> {
    effects.iter().find_map(|e| match e {
        Effect::StartBinding { start, .. } => Some(start.clone()),
        _ => None,
    })
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

fn workspace_ops(effects: &[Effect], op: &str) -> Vec<(u64, Value)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Workspace { call, op: o, params } if o == op => Some((*call, params.clone())),
            _ => None,
        })
        .collect()
}

fn updates_of(effects: &[Effect], ty: &str) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Notify { method, params, .. } if method == "update" && params["update"]["type"] == ty => {
                Some(params["update"].clone())
            }
            _ => None,
        })
        .collect()
}

/// A parent and a session-principal connection for it.
fn parent(sim: &mut Sim, extra: Value) -> (String, String) {
    let (p, b) = sim.root(extra);
    sim.connect("con_agent", Some(&p));
    (p, b)
}

fn child_params(parent: &str, extra: Value) -> Value {
    let mut p =
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }, "parent": parent });
    merge(&mut p, extra);
    p
}

#[test]
fn a_parent_agent_creates_a_child_in_its_subtree() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (c, b) = sim.new_session("con_agent", child_params(&p, json!({ "title": "Child" })));
    sim.bring_up(&b);
    let child = sim.get(&c);
    assert_eq!(child["kind"], "child");
    assert_eq!(child["parent"]["session"], p.as_str());
    assert_eq!(child["origin"]["kind"], "session");
    // The parent has a subagent item for it.
    let items = updates_of(&sim.seen, "item");
    assert!(
        items
            .iter()
            .any(|i| i["item"]["kind"] == "subagent" && i["item"]["set"]["session"] == c.as_str())
    );
    let tree = sim.ok(OWNER, "session/tree", json!({ "session": p }));
    let nodes = tree["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[1]["depth"], 1);
    let page = sim.ok(OWNER, "session/tree", json!({ "session": p, "limit": 1 }));
    assert_eq!(
        page["next"],
        p.as_str(),
        "the cursor is the last session of the page"
    );
    // A child that joins between pages doesn't repeat what the first page had.
    let (late, b2) = sim.new_session("con_agent", child_params(&p, json!({ "title": "Late" })));
    sim.bring_up(&b2);
    let rest = sim.ok(
        OWNER,
        "session/tree",
        json!({ "session": p, "cursor": page["next"] }),
    );
    let ids: Vec<&str> = rest["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["session"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [c.as_str(), late.as_str()]);
    assert!(rest.get("next").is_none());
}

#[test]
fn session_list_pages_by_cursor_without_shifting_when_sessions_change() {
    let mut sim = Sim::new();
    let ids: Vec<String> = (0..5).map(|_| sim.root(json!({})).0).collect();
    let first = sim.ok(OWNER, "session/list", json!({ "limit": 2 }));
    let seen: Vec<String> = first["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen.len(), 2);
    // A session on a later page changes and moves to the front; the rest stay in order.
    let moved = ids.iter().find(|i| !seen.contains(i)).unwrap().clone();
    sim.ok(
        OWNER,
        "session/rename",
        json!({ "session": moved, "title": "Renamed" }),
    );
    let mut rest = Vec::new();
    let mut cursor = first["next"].clone();
    while let Some(c) = cursor.as_str().map(str::to_string) {
        let page = sim.ok(OWNER, "session/list", json!({ "limit": 2, "cursor": c }));
        rest.extend(
            page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["id"].as_str().unwrap().to_string()),
        );
        cursor = page.get("next").cloned().unwrap_or(Value::Null);
    }
    assert!(
        rest.iter().all(|r| !seen.contains(r)),
        "no repeats: {seen:?} then {rest:?}"
    );
    assert_eq!(
        seen.len() + rest.len() + 1,
        5,
        "only the renamed one moved ahead: {rest:?}"
    );
    let (r, _) = sim.call(OWNER, "session/list", json!({ "cursor": "nonsense" }));
    assert_eq!(r, Err(-32602));
}

#[test]
fn a_session_principal_acts_only_on_its_subtree_and_grant() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (other, _) = sim.root(json!({}));
    let (r, _) = sim.call("con_agent", "session/get", json!({ "session": other }));
    assert_eq!(r, Err(-32002));
    let (r, _) = sim.call(
        "con_agent",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/x" } }),
    );
    assert_eq!(r, Err(-32003), "a root needs manage, which the grant lacks");
    // The child's grant is at most its parent's.
    let (c, _) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "grant": ["read", "manage"] })),
    );
    assert_eq!(sim.get(&c)["grant"], json!(["read"]));
    // Only the owner may act as a session.
    sim.connect("con_child", Some(&c));
    let (r, _) = sim.call("con_child", "session/get", json!({ "session": p }));
    assert_eq!(r, Err(-32002), "the parent is outside the child's subtree");
}

#[test]
fn depth_and_running_children_limit_new_children() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({ "ceiling": { "depth": 1, "children": 1 } }));
    let (c, _) = sim.new_session("con_agent", child_params(&p, json!({})));
    assert_eq!(sim.get(&c)["ceiling"]["depth"], 0);
    let (r, _) = sim.call("con_agent", "session/new", child_params(&p, json!({})));
    assert_eq!(r, Err(-32003), "one child runs already");
    let (r, _) = sim.call(OWNER, "session/new", child_params(&c, json!({})));
    assert_eq!(r, Err(-32003), "no depth left below the child");
}

#[test]
fn a_task_completes_on_idle_wakes_the_parent_and_returns_from_wait() {
    let mut sim = Sim::new();
    let (p, pb) = parent(&mut sim, json!({ "wake": { "on": ["done"] } }));
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(
            &p,
            json!({ "task": { "brief": "Count the files", "completeOn": "idle" } }),
        ),
    );
    let first = sim.ok("con_agent", "task/wait", json!({ "sessions": [c] }));
    assert_eq!(first["changes"][0]["task"]["status"], "running");
    let cursor = first["cursor"].as_str().unwrap().to_string();
    // The brief is the child's first prompt.
    let e = sim.bring_up(&b);
    let prompts = driver_calls(&e, "binding/prompt");
    assert_eq!(prompts[0].1["text"], "Count the files");
    // A wait with nothing new is held.
    let (r, _) = sim.call(
        "con_agent",
        "task/wait",
        json!({ "sessions": [c], "since": cursor, "timeout": 30 }),
    );
    assert_eq!(r, Err(0));
    let held = json!(sim.next_id);
    let e = sim.turn(&b, "t1", "There are 3 files.");
    let answer = reply(&e, &held).expect("the wait returns").unwrap();
    assert_eq!(answer["changes"][0]["task"]["status"], "done");
    assert_eq!(
        answer["changes"][0]["task"]["output"]["text"],
        "There are 3 files."
    );
    assert_eq!(sim.task_status(&c), "done");
    // The wait returned the change, so the parent gets no wake entry for it.
    assert!(
        driver_calls(&e, "binding/prompt")
            .iter()
            .all(|(_, p)| p["binding"] != pb.as_str())
    );
}

#[test]
fn a_wake_entry_reaches_an_idle_parent() {
    let mut sim = Sim::new();
    let (p, pb) = parent(&mut sim, json!({ "wake": { "on": ["done", "awaiting"] } }));
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(
            &p,
            json!({ "title": "Counter", "task": { "brief": "Go", "completeOn": "finish" } }),
        ),
    );
    sim.bring_up(&b);
    let e = sim.turn(&b, "t1", "Should I count hidden files?");
    assert_eq!(sim.task_status(&c), "awaiting");
    let wakes: Vec<Value> = driver_calls(&e, "binding/prompt")
        .into_iter()
        .filter(|(_, p)| p["binding"] == pb.as_str())
        .map(|(_, p)| p)
        .collect();
    assert_eq!(wakes.len(), 1, "one wake entry");
    let text = wakes[0]["text"].as_str().unwrap();
    assert!(
        text.contains(&c) && text.contains("awaiting") && text.contains("Counter"),
        "{text}"
    );
}

#[test]
fn finish_is_for_the_own_principal_and_checks_the_result_schema() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let schema =
        json!({ "type": "object", "required": ["count"], "properties": { "count": { "type": "integer" } } });
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "task": { "brief": "Count", "result": schema } })),
    );
    sim.bring_up(&b);
    let (r, _) = sim.call(
        "con_agent",
        "task/finish",
        json!({ "session": c, "output": { "count": 3 } }),
    );
    assert_eq!(r, Err(-32003), "the parent can't finish its child's task");
    sim.connect("con_child", Some(&c));
    let (r, _) = sim.call(
        "con_child",
        "task/finish",
        json!({ "session": c, "output": { "count": "three" } }),
    );
    assert_eq!(r, Err(-32602));
    sim.ok(
        "con_child",
        "task/finish",
        json!({ "session": c, "output": { "count": 3 } }),
    );
    assert_eq!(sim.get(&c)["task"]["output"]["count"], 3);
    // A new task after the last one ended; the old one goes to history.
    let t = sim.ok(
        "con_agent",
        "task/assign",
        json!({ "session": c, "task": { "brief": "Again" } }),
    );
    assert_eq!(t["task"]["history"][0]["status"], "done");
    let (r, _) = sim.call(
        "con_agent",
        "task/assign",
        json!({ "session": c, "task": { "brief": "Third" } }),
    );
    assert_eq!(r, Err(-32004));
    sim.ok(
        "con_child",
        "task/fail",
        json!({ "session": c, "error": "stuck" }),
    );
    assert_eq!(sim.task_status(&c), "failed");
}

#[test]
fn stopping_a_child_cancels_its_task_and_children_that_end_with_it() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "task": { "brief": "Work" }, "onParentEnd": "stop" })),
    );
    sim.bring_up(&b);
    sim.ok(OWNER, "session/stop", json!({ "session": p }));
    assert_eq!(sim.task_status(&c), "cancelled");
}

#[test]
fn budgets_are_allotments_from_the_parent() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({ "budget": { "tokens": 100 } }));
    let (r, _) = sim.call(
        "con_agent",
        "session/new",
        child_params(&p, json!({ "task": { "brief": "x" } })),
    );
    assert_eq!(r, Err(-32602), "an explicit budget is needed");
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(
            &p,
            json!({ "budget": { "tokens": 60 }, "task": { "brief": "x" } }),
        ),
    );
    assert_eq!(sim.get(&p)["budget"]["allotted"]["tokens"], 60);
    let (r, _) = sim.call(
        "con_agent",
        "session/new",
        child_params(&p, json!({ "budget": { "tokens": 60 } })),
    );
    assert_eq!(r, Err(-32003), "only 40 tokens are left");
    sim.bring_up(&b);
    sim.driver(
        &b,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let e = sim.driver(
        &b,
        json!({ "type": "usage", "usage": { "tokens": { "input": 50, "output": 20 } } }),
    );
    assert_eq!(sim.get(&c)["task"]["error"], "budget");
    assert_eq!(
        driver_calls(&e, "binding/cancel").len(),
        1,
        "the running turn is interrupted"
    );
    // The task ended: the allotment went back, with what was spent.
    let pb = sim.get(&p)["budget"].clone();
    assert_eq!(pb["allotted"]["tokens"], 0, "{pb}");
    assert_eq!(pb["used"]["tokens"], 70, "{pb}");
}

#[test]
fn a_request_above_the_ceiling_level_is_denied_by_the_ceiling() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (_, b) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "ceiling": { "level": "read" } })),
    );
    let start = start_params(&sim.seen).unwrap();
    let _ = start;
    sim.bring_up(&b);
    let e = sim.driver(
        &b,
        json!({ "type": "request", "request": {
        "id": "k1", "kind": "approval", "title": "Write a file", "level": "write",
        "choices": [{ "id": "allow", "label": "Allow", "intent": "allow", "scope": "always" },
                    { "id": "deny", "label": "Deny", "intent": "deny" }],
        "blocking": true, "answerable": "connection", "notify": "normal", "status": "open" } }),
    );
    let answers = driver_calls(&e, "binding/answer");
    assert_eq!(answers[0].1["answer"]["choice"], "deny");
}

#[test]
fn unattended_requests_are_denied_when_nobody_can_answer() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (_, b) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "ceiling": { "unattended": 5 } })),
    );
    sim.bring_up(&b);
    sim.driver(
        &b,
        json!({ "type": "request", "request": {
        "id": "k1", "kind": "approval", "title": "Run", "level": "read",
        "choices": [{ "id": "deny", "label": "Deny", "intent": "deny" }],
        "blocking": true, "answerable": "message", "notify": "normal", "status": "open" } }),
    );
    // The owner is connected, so nothing happens.
    let e = sim.handle(Input::Tick {
        now_ms: 1_700_000_100_000,
    });
    assert!(driver_calls(&e, "binding/answer").is_empty());
    sim.handle(Input::Disconnected { conn: OWNER.into() });
    let (s2, _) = (1, 2);
    let _ = s2;
    sim.driver(
        &b,
        json!({ "type": "request", "request": {
        "id": "k2", "kind": "approval", "title": "Run", "level": "read",
        "choices": [{ "id": "deny", "label": "Deny", "intent": "deny" }],
        "blocking": true, "answerable": "message", "notify": "normal", "status": "open" } }),
    );
    let e = sim.handle(Input::Tick {
        now_ms: 1_800_000_000_000,
    });
    assert_eq!(driver_calls(&e, "binding/answer").len(), 1);
}

#[test]
fn configure_only_narrows_and_wakes() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (c, _) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "ceiling": { "level": "write" } })),
    );
    sim.ok(
        "con_agent",
        "session/configure",
        json!({ "session": c, "ceiling": { "level": "host" }, "wake": { "on": ["done"] } }),
    );
    assert_eq!(
        sim.get(&c)["ceiling"]["level"],
        "write",
        "a ceiling can't be raised"
    );
    sim.ok(
        "con_agent",
        "session/configure",
        json!({ "session": c, "ceiling": { "level": "read" } }),
    );
    assert_eq!(sim.get(&c)["ceiling"]["level"], "read");
    let (r, _) = sim.call(
        "con_agent",
        "session/configure",
        json!({ "session": p, "wake": { "on": [] } }),
    );
    assert_eq!(r, Err(-32003), "a session can't configure itself");
}

#[test]
fn spawning_with_a_worktree_creates_it_first() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let (r, e) = sim.call(
        "con_agent",
        "session/new",
        child_params(
            &p,
            json!({ "workspace": { "kind": "worktree", "repo": "/work/repo" } }),
        ),
    );
    assert_eq!(r, Err(0), "the reply waits for the worktree");
    let (call, params) = workspace_ops(&e, "worktree.create").remove(0);
    assert_eq!(params["path"].as_str().unwrap().rsplit('/').nth(1), Some("repo"));
    assert!(params["branch"].as_str().unwrap().starts_with("parrot/ses_"));
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Ok(json!({ "path": params["path"], "branch": params["branch"], "base": "abc123" })),
    });
    let id = json!(sim.next_id);
    let s = reply(&e, &id).unwrap().unwrap();
    let ws = s["session"]["workspace"].as_str().unwrap().to_string();
    let list = sim.ok(OWNER, "workspace/list", json!({}));
    let w = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == ws.as_str())
        .unwrap()
        .clone();
    assert_eq!(w["kind"], "worktree");
    assert_eq!(w["owner"], s["session"]["id"]);
    // Without the `workspace` scope, spawning with a worktree fails.
    let (q, _) = sim.root(json!({ "grant": ["delegate"] }));
    sim.connect("con_q", Some(&q));
    let (r, _) = sim.call(
        "con_q",
        "session/new",
        child_params(
            &q,
            json!({ "workspace": { "kind": "worktree", "repo": "/work/repo" } }),
        ),
    );
    assert_eq!(r, Err(-32003));
}

#[test]
fn merge_is_from_the_subtree_into_the_callers_own_workspace() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({}));
    let parent_ws = sim.get(&p)["workspace"].as_str().unwrap().to_string();
    let (_, e) = sim.call(
        "con_agent",
        "session/new",
        child_params(
            &p,
            json!({ "workspace": { "kind": "worktree", "repo": "/work/demo" } }),
        ),
    );
    let (call, params) = workspace_ops(&e, "worktree.create").remove(0);
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Ok(json!({ "path": params["path"] })),
    });
    let s = reply(&e, &json!(sim.next_id)).unwrap().unwrap();
    let child_ws = s["session"]["workspace"].as_str().unwrap().to_string();
    let (r, _) = sim.call(
        "con_agent",
        "workspace/merge",
        json!({ "from": parent_ws, "into": child_ws, "method": "merge" }),
    );
    assert_eq!(r, Err(-32003), "into must be the caller's own");
    let (r, e) = sim.call(
        "con_agent",
        "workspace/merge",
        json!({ "from": child_ws, "into": parent_ws, "method": "merge" }),
    );
    assert_eq!(r, Err(0));
    let (call, m) = workspace_ops(&e, "merge").remove(0);
    assert_eq!(m["into"], "/work/demo");
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Ok(json!({ "commit": "def456" })),
    });
    assert_eq!(
        reply(&e, &json!(sim.next_id)).unwrap().unwrap()["commit"],
        "def456"
    );
}

#[test]
fn a_restart_interrupts_running_tasks_and_wakes() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({ "wake": { "on": ["done"], "start": true } }));
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(&p, json!({ "task": { "brief": "Work" } })),
    );
    sim.bring_up(&b);
    let restore = sim.store.restore();
    let mut next = Sim::with_ids(1000);
    let e = next.handle(Input::Restore(restore));
    assert_eq!(next.task_status(&c), "interrupted");
    // `Wake.start` starts the stopped parent for the wake (E7).
    let start = start_params(&e).expect("the parent starts");
    assert_eq!(start["session"], p.as_str());
    // Prompting the stopped child needs only `prompt` for its parent's principal.
    next.connect("con_agent", Some(&p));
    next.ok(
        "con_agent",
        "session/prompt",
        json!({ "session": c, "text": "Go on" }),
    );
}

#[test]
fn rebinds_hold_the_queue_and_never_overlap() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.turn(&b, "t1", "Hello");
    sim.driver(
        &b,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let r = sim.ok(OWNER, "session/rebind", json!({ "session": s, "agent": "codex" }));
    assert_eq!(
        r["binding"]["id"],
        b.as_str(),
        "the turn runs, so the old binding stays for now"
    );
    let (again, _) = sim.call(OWNER, "session/rebind", json!({ "session": s, "agent": "codex" }));
    assert_eq!(again, Err(-32004));
    // A prompt meanwhile is queued, not sent.
    sim.ok(OWNER, "session/prompt", json!({ "session": s, "text": "Next" }));
    let e = sim.idle(&b);
    let nb = start_of(&e).expect("the new binding starts once the turn ended");
    assert!(driver_calls(&e, "binding/prompt").is_empty());
    assert!(
        driver_calls(&e, "binding/stop").is_empty(),
        "a bundle starts the new binding first"
    );
    let e = sim.bring_up(&nb);
    assert_eq!(driver_calls(&e, "binding/stop")[0].1["binding"], b.as_str());
    let handoff = updates_of(&e, "item")
        .into_iter()
        .find(|u| u["item"]["kind"] == "handoff")
        .unwrap();
    assert_eq!(handoff["item"]["set"]["to"], nb.as_str());
    let prompt = &driver_calls(&e, "binding/prompt")[0].1;
    assert_eq!(prompt["binding"], nb.as_str());
    let text = prompt["text"].as_str().unwrap();
    assert!(text.starts_with("# Continuing an earlier session"), "{text}");
    assert!(
        text.contains("**Assistant:** Hello") && text.ends_with("## Next\nNext\n"),
        "{text}"
    );
    assert_eq!(sim.get(&s)["binding"]["agent"], "codex");
}

#[test]
fn an_initial_prompt_is_sent_when_the_binding_starts() {
    let mut sim = Sim::new();
    let (r, effects) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" },
            "prompt": "Hello" }),
    );
    r.unwrap();
    let b = start_of(&effects).expect("StartBinding");
    let e = sim.bring_up(&b);
    let prompt = &driver_calls(&e, "binding/prompt")[0].1;
    assert_eq!(prompt["text"], "Hello");
    assert_eq!(prompt["binding"], b.as_str());
    assert!(prompt["message"].as_str().is_some_and(|m| m.starts_with("msg_")));
}

#[test]
fn a_rebind_can_switch_the_control() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    let e = sim
        .call(OWNER, "session/rebind", json!({ "session": s, "control": "tui" }))
        .1;
    let start = start_params(&e).expect("the new binding starts");
    assert_eq!(start["control"], "tui");
    assert!(
        start.get("resume").is_none(),
        "another control doesn't resume the old run: {start}"
    );
    let nb = start_of(&e).unwrap();
    let e = sim.bring_up(&nb);
    // The old binding stops once the new one runs, and the session keeps its history as a bundle.
    assert_eq!(driver_calls(&e, "binding/stop")[0].1["binding"], b.as_str());
    assert_eq!(sim.get(&s)["binding"]["control"], "tui");
}

#[test]
fn a_failed_handoff_starts_the_old_binding_again() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.driver(
        &b,
        json!({ "type": "ref", "ref": { "id": "h1", "strength": "strong" } }),
    );
    let e = sim
        .call(OWNER, "session/rebind", json!({ "session": s, "agent": "codex" }))
        .1;
    let nb = start_of(&e).unwrap();
    let e = sim.handle(Input::DriverReply {
        binding: nb.clone(),
        call: u64::MAX,
        result: Err(parrot_proto::rpc::RpcError::new(-32603, "no codex")),
    });
    let handoff = updates_of(&e, "item")
        .into_iter()
        .find(|u| u["item"]["kind"] == "handoff")
        .unwrap();
    assert_eq!(handoff["item"]["set"]["status"], "error");
    assert_eq!(sim.get(&s)["binding"]["id"], b.as_str());
    assert_eq!(sim.get(&s)["binding"]["agent"], "claude");
}

#[test]
fn fork_by_bundle_seeds_a_new_session() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.turn(&b, "t1", "First answer");
    sim.turn(&b, "t2", "Second answer");
    let turns = sim.ok(OWNER, "session/bundle", json!({ "session": s }));
    assert!(turns["rendered"].as_str().unwrap().contains("Second answer"));
    let t1 = turns["bundle"]["items"][0]["turn"].as_str().unwrap().to_string();
    let (f, fb) = sim.new_session(
        OWNER,
        json!({ "agent": "codex", "workspace": { "kind": "dir", "path": "/work/demo" },
            "seed": { "session": s, "at": t1, "brief": "Carry on", "source": "stop" } }),
    );
    let forked = sim.get(&f);
    assert_eq!(forked["seed"]["via"], "bundle");
    assert_eq!(forked["binding"]["via"], "bundle");
    assert_eq!(forked["binding"]["pending"], "bundle");
    let e = sim.bring_up(&fb);
    let text = driver_calls(&e, "binding/prompt")[0].1["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("First answer") && !text.contains("Second answer"),
        "{text}"
    );
    assert!(text.ends_with("## Next\nCarry on\n"));
    // `source: "stop"` stopped the source once the fork started.
    assert_eq!(driver_calls(&e, "binding/stop")[0].1["binding"], b.as_str());
    let (r, _) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "codex", "workspace": { "kind": "dir", "path": "/w" },
        "seed": { "session": s, "require": "exact" } }),
    );
    assert_eq!(r, Err(-32001));
}

#[test]
fn a_native_fork_passes_the_ref() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.driver(
        &b,
        json!({ "type": "ref", "ref": { "id": "h1", "strength": "strong" } }),
    );
    sim.turn(&b, "t1", "the source's answer");
    sim.core.state.sessions.agents.get_mut("claude").unwrap()["features"] = json!({ "session.fork": {} });
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }, "seed": { "session": s, "require": "exact" } }),
    );
    let f = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    // The source's binding forks first; the new binding starts from the ref it returns.
    assert!(start_of(&e).is_none());
    let (call, fp) = driver_calls(&e, "binding/fork").remove(0);
    assert_eq!(fp["binding"], b.as_str());
    let e = sim.handle(Input::DriverReply {
        binding: b.clone(),
        call,
        result: Ok(json!({ "ref": { "id": "fork:h1", "strength": "strong" } })),
    });
    let start = start_params(&e).unwrap();
    assert_eq!(start["resume"]["id"], "fork:h1");
    assert_eq!(sim.get(&f)["binding"]["via"], "fork");
    assert_eq!(sim.get(&f)["seed"]["via"], "fork");

    // A driver that refuses the fork leaves the new session with the bundle.
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" }, "seed": { "session": s } }),
    );
    let g = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    let (call, _) = driver_calls(&e, "binding/fork").remove(0);
    let e = sim.handle(Input::DriverReply {
        binding: b.clone(),
        call,
        result: Err(RpcError::new(-32001, "not at that turn")),
    });
    let start = start_params(&e).unwrap();
    assert!(start.get("resume").is_none(), "{start}");
    assert_eq!(sim.get(&g)["binding"]["via"], "bundle");
    assert_eq!(sim.get(&g)["seed"]["via"], "bundle");
}

#[test]
fn checkpoints_are_taken_per_turn_and_rewind_restores_them() {
    let mut sim = Sim::new();
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "worktree", "repo": "/work/repo" },
        "grant": ["read"], "ceiling": {} }),
    );
    assert_eq!(r, Err(0));
    let (call, params) = workspace_ops(&e, "worktree.create").remove(0);
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Ok(json!({ "path": params["path"] })),
    });
    let s = reply(&e, &json!(sim.next_id)).unwrap().unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = start_of(&e).unwrap();
    sim.bring_up(&b);
    let mut turn_ids = Vec::new();
    for (i, key) in ["t1", "t2"].iter().enumerate() {
        let (_, e) = sim.call(
            OWNER,
            "session/prompt",
            json!({ "session": s, "text": format!("step {i}") }),
        );
        let (call, cp) = workspace_ops(&e, "checkpoint.record").remove(0);
        assert_eq!(cp["turn"], i);
        // The prompt waits for its checkpoint.
        assert!(driver_calls(&e, "binding/prompt").is_empty());
        let e = sim.handle(Input::WorkspaceReply {
            call,
            result: Ok(json!({})),
        });
        assert_eq!(driver_calls(&e, "binding/prompt").len(), 1);
        sim.turn(&b, key, &format!("did {i}"));
        let items = sim.ok(OWNER, "session/bundle", json!({ "session": s }));
        turn_ids.push(
            items["bundle"]["items"].as_array().unwrap().last().unwrap()["turn"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    // The second turn's checkpoint is the state after the first.
    let (r, e) = sim.call(
        OWNER,
        "session/rewind",
        json!({ "session": s, "turn": turn_ids[0], "files": true }),
    );
    assert_eq!(r, Err(0));
    let (call, rp) = workspace_ops(&e, "checkpoint.restore").remove(0);
    assert_eq!(rp["turn"], 1);
    let removed: Vec<Value> = updates_of(&e, "item")
        .into_iter()
        .filter(|u| u["item"]["removed"] == true)
        .collect();
    assert!(!removed.is_empty());
    let nb = start_of(&e).expect("the binding restarts with a bundle");
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Ok(json!({})),
    });
    assert_eq!(reply(&e, &json!(sim.next_id)).unwrap(), Ok(json!({})));
    sim.bring_up(&nb);
    let (_, e) = sim.call(OWNER, "session/prompt", json!({ "session": s, "text": "redo" }));
    // Checkpoint refs only grow, so the redone turn doesn't overwrite one a rewind might want.
    let (call, cp) = workspace_ops(&e, "checkpoint.record").remove(0);
    assert_eq!(cp["turn"], 2);
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Err(RpcError::new(-32004, "disk full")),
    });
    assert!(
        updates_of(&e, "notice").iter().any(|u| u["code"] == "checkpoint"),
        "a failed checkpoint is reported"
    );
    let text = driver_calls(&e, "binding/prompt")[0].1["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("did 0") && !text.contains("did 1"), "{text}");
    // Deleting the session prunes its checkpoints.
    let (_, e) = sim.call(OWNER, "session/delete", json!({ "session": s }));
    assert_eq!(workspace_ops(&e, "checkpoint.prune").len(), 1);
}

#[test]
fn rewind_with_files_is_refused_for_dir_workspaces() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.turn(&b, "t1", "x");
    let t = sim.ok(OWNER, "session/bundle", json!({ "session": s }))["bundle"]["items"][0]["turn"].clone();
    let (r, _) = sim.call(
        OWNER,
        "session/rewind",
        json!({ "session": s, "turn": t, "files": true }),
    );
    assert_eq!(r, Err(-32004));
}

#[test]
fn delete_is_refused_while_children_run_and_returns_the_allotment() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({ "budget": { "turns": 10 } }));
    let (c, b) = sim.new_session("con_agent", child_params(&p, json!({ "budget": { "turns": 4 } })));
    sim.bring_up(&b);
    let (r, _) = sim.call(OWNER, "session/delete", json!({ "session": p }));
    assert_eq!(r, Err(-32004));
    sim.ok(OWNER, "session/delete", json!({ "session": c }));
    assert_eq!(sim.get(&p)["budget"]["allotted"]["turns"], 0);
}

fn level() -> impl Strategy<Value = Option<&'static str>> {
    prop::option::of(prop::sample::select(vec!["read", "write", "sandboxed", "host"]))
}

fn ceiling() -> impl Strategy<Value = Ceiling> {
    (
        level(),
        prop::option::of(any::<bool>()),
        prop::option::of(prop::sample::subsequence(
            vec!["claude".to_string(), "codex".into(), "pi".into()],
            0..3,
        )),
        prop::option::of(prop::sample::subsequence(
            vec!["/a".to_string(), "/a/b".into(), "/c".into()],
            0..3,
        )),
        prop::option::of(0u32..5),
        prop::option::of(0u32..5),
        prop::option::of(0u64..100),
    )
        .prop_map(|(l, n, agents, paths, depth, children, unattended)| Ceiling {
            level: l.map(|l| serde_json::from_value(json!(l)).unwrap()),
            network: n,
            agents,
            paths,
            depth,
            children,
            unattended,
            ..Default::default()
        })
}

proptest! {
    #[test]
    fn ceilings_only_narrow(parent in ceiling(), asked in ceiling(), again in ceiling()) {
        let child = intersect(&parent, &asked);
        prop_assert!(narrows(&child, &parent), "{child:?} widens {parent:?}");
        let grandchild = intersect(&child, &again);
        prop_assert!(narrows(&grandchild, &child));
    }

    #[test]
    fn budgets_are_never_overspent_by_allotments(asks in prop::collection::vec((0usize..3, 0u64..60), 1..12)) {
        let mut sim = Sim::new();
        let (p, _) = parent(&mut sim, json!({ "budget": { "tokens": 100, "sessions": 5 } }));
        let mut nodes = vec![p.clone()];
        for (who, tokens) in asks {
            let at = nodes[who % nodes.len()].clone();
            let (r, _) = sim.call(OWNER, "session/new", child_params(&at, json!({ "budget": { "tokens": tokens, "sessions": 2 }, "start": false })));
            if let Ok(v) = r {
                nodes.push(v["session"]["id"].as_str().unwrap().to_string());
            }
            for n in &nodes {
                let b = sim.get(n)["budget"].clone();
                let num = |k: &str, d: &str| b[k][d].as_u64().unwrap_or(0);
                for d in ["tokens", "sessions"] {
                    prop_assert!(num("used", d) + num("allotted", d) <= num("limit", d), "{n}: {b}");
                }
            }
        }
    }

    #[test]
    fn one_binding_runs_per_session_through_rebinds(steps in prop::collection::vec(0u8..4, 1..20)) {
        let mut sim = Sim::new();
        let (s, _) = sim.root(json!({}));
        for step in steps {
            match step {
                0 => { let _ = sim.call(OWNER, "session/rebind", json!({ "session": s, "agent": "codex" })); }
                1 => { let _ = sim.call(OWNER, "session/rebind", json!({ "session": s, "agent": "claude" })); }
                2 => {
                    let starting: Vec<String> = sim.core.state.bindings.info.iter()
                        .filter(|(_, b)| b.status == parrot_proto::session::BindingStatus::Starting)
                        .map(|(id, _)| id.clone()).collect();
                    for b in starting { sim.bring_up(&b); }
                }
                _ => {
                    let stopping: Vec<String> = sim.core.state.bindings.info.iter()
                        .filter(|(_, b)| b.stop_requested && b.status == parrot_proto::session::BindingStatus::Running)
                        .map(|(id, _)| id.clone()).collect();
                    for b in stopping { sim.handle(Input::DriverExited { binding: b, detail: "stopped".into() }); }
                }
            }
            // At most one binding of the session runs that wasn't asked to stop.
            let live = sim.core.state.bindings.info.iter()
                .filter(|(id, b)| sim.core.state.bindings.by_id[*id].session == s
                    && b.status == parrot_proto::session::BindingStatus::Running && !b.stop_requested)
                .count();
            prop_assert!(live <= 1, "{live} bindings run");
            prop_assert!(sim.core.state.seeds.rebinding.len() <= 1);
        }
    }
}

#[test]
fn seconds_are_counted_from_turn_wall_time_and_enforced() {
    let mut sim = Sim::new();
    let (p, _) = parent(&mut sim, json!({ "budget": { "seconds": 100 } }));
    let (c, b) = sim.new_session(
        "con_agent",
        child_params(
            &p,
            json!({ "budget": { "seconds": 10 }, "task": { "brief": "x" } }),
        ),
    );
    sim.bring_up(&b);
    let at = "2026-01-01T00:00:00.000Z";
    sim.driver(
        &b,
        json!({ "type": "state", "state": "running", "background": [] }),
    );
    let start = sim.clock.now_ms();
    let e = sim.driver(&b, json!({ "type": "turn", "turn": { "id": "t1", "status": "running", "continuable": false, "started": at } }));
    let timers: Vec<u64> = e
        .iter()
        .filter_map(|e| match e {
            Effect::TimerAt { at_ms } => Some(*at_ms),
            _ => None,
        })
        .collect();
    assert!(
        timers
            .iter()
            .any(|t| (start + 10_000..start + 10_100).contains(t)),
        "a timer for when the budget runs out: {timers:?}"
    );
    // Four seconds in, the running turn has spent four.
    sim.clock.set(start + 4_200);
    sim.handle(Input::Tick {
        now_ms: start + 4_200,
    });
    assert_eq!(sim.get(&c)["budget"]["used"]["seconds"], 4);
    assert_eq!(sim.get(&c)["task"]["status"], "running");
    // At ten seconds the turn is interrupted and the task fails.
    sim.clock.set(start + 10_100);
    let e = sim.handle(Input::Tick {
        now_ms: start + 10_100,
    });
    assert_eq!(
        driver_calls(&e, "binding/cancel").len(),
        1,
        "the turn is interrupted"
    );
    assert_eq!(sim.get(&c)["task"]["error"], "budget");
    // What the turn spent came back to the parent with the allotment.
    sim.clock.set(start + 10_400);
    sim.driver(&b, json!({ "type": "turn", "turn": { "id": "t1", "status": "interrupted", "continuable": false, "started": at, "ended": at } }));
    let pb = sim.get(&p)["budget"].clone();
    assert!(pb["used"]["seconds"].as_u64().unwrap() >= 10, "{pb}");
}

#[test]
fn a_failed_prompt_does_not_shift_checkpoints() {
    let mut sim = Sim::new();
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "worktree", "repo": "/work/repo" } }),
    );
    assert_eq!(r, Err(0));
    let (call, params) = workspace_ops(&e, "worktree.create").remove(0);
    let e = sim.handle(Input::WorkspaceReply {
        call,
        result: Ok(json!({ "path": params["path"] })),
    });
    let s = reply(&e, &json!(sim.next_id)).unwrap().unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = start_of(&e).unwrap();
    sim.bring_up(&b);
    let prompt = |sim: &mut Sim, text: &str| -> u64 {
        let (_, e) = sim.call(OWNER, "session/prompt", json!({ "session": s, "text": text }));
        let (call, cp) = workspace_ops(&e, "checkpoint.record").remove(0);
        let e = sim.handle(Input::WorkspaceReply {
            call,
            result: Ok(json!({})),
        });
        let _ = cp;
        driver_calls(&e, "binding/prompt")[0].0
    };
    prompt(&mut sim, "first");
    sim.turn(&b, "t1", "did first");
    // The second prompt fails before the harness starts a turn.
    let call = prompt(&mut sim, "lost");
    sim.handle(Input::DriverReply {
        binding: b.clone(),
        call,
        result: Err(RpcError::new(-32603, "harness crashed")),
    });
    prompt(&mut sim, "third");
    sim.turn(&b, "t3", "did third");
    let bundle = sim.ok(OWNER, "session/bundle", json!({ "session": s }));
    let first = bundle["bundle"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["text"] == "did first")
        .unwrap()["turn"]
        .clone();
    let (_, e) = sim.call(
        OWNER,
        "session/rewind",
        json!({ "session": s, "turn": first, "files": true }),
    );
    // The checkpoint before "third" (ref 2) holds the state after t1, not the one before "lost".
    let (_, rp) = workspace_ops(&e, "checkpoint.restore").remove(0);
    assert_eq!(rp["turn"], 2);
}

#[test]
fn a_source_summary_is_asked_of_the_source_harness() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.turn(&b, "t1", "wrote the parser");
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "codex", "workspace": { "kind": "dir", "path": "/work/demo" },
            "seed": { "session": s, "summary": "source" } }),
    );
    assert_eq!(r, Err(0), "the call waits for the summary");
    let prompt = driver_calls(&e, "binding/prompt").remove(0).1;
    assert!(
        prompt["text"]
            .as_str()
            .unwrap()
            .starts_with("Summarize this session")
    );
    let e = sim.turn(&b, "t2", "The parser is done; tests are next.");
    let created = reply(&e, &json!(sim.next_id)).unwrap().unwrap();
    let f = created["session"]["id"].as_str().unwrap().to_string();
    let nb = start_of(&e).expect("the new session starts");
    sim.bring_up(&nb);
    let (_, e) = sim.call(OWNER, "session/prompt", json!({ "session": f, "text": "go on" }));
    let text = driver_calls(&e, "binding/prompt")[0].1["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("## Summary\nThe parser is done; tests are next."),
        "{text}"
    );
    assert!(text.contains("wrote the parser"), "{text}");
    assert!(
        !text.contains("Summarize this session"),
        "the summary turn is cut: {text}"
    );
}

/// Stop a session and let its driver exit.
fn stop_and_exit(sim: &mut Sim, session: &str, binding: &str) {
    sim.ok(OWNER, "session/stop", json!({ "session": session }));
    sim.handle(Input::DriverExited {
        binding: binding.into(),
        detail: "stopped".into(),
    });
    assert_eq!(sim.get(session)["state"], "stopped");
}

#[test]
fn a_native_fork_of_a_stopped_source_resumes_it_to_fork_then_stops_it() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.driver(
        &b,
        json!({ "type": "ref", "ref": { "id": "h1", "strength": "strong" } }),
    );
    sim.turn(&b, "t1", "first");
    sim.turn(&b, "t2", "second");
    let t1 = sim.ok(OWNER, "session/bundle", json!({ "session": s }))["bundle"]["items"][0]["turn"]
        .as_str()
        .unwrap()
        .to_string();
    stop_and_exit(&mut sim, &s, &b);
    sim.core.state.sessions.agents.get_mut("claude").unwrap()["features"] = json!({ "session.fork": {} });
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" },
            "seed": { "session": s, "at": t1, "require": "exact" } }),
    );
    let f = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
    // The source is resumed first.
    let woken = start_params(&e).expect("the source starts");
    assert_eq!(woken["session"], s.as_str());
    assert_eq!(woken["resume"]["id"], "h1");
    let b2 = woken["binding"].as_str().unwrap().to_string();
    let e = sim.bring_up(&b2);
    let (call, fp) = driver_calls(&e, "binding/fork").remove(0);
    assert_eq!(fp["binding"], b2.as_str());
    assert_eq!(fp["at"], "t1", "the turn's key from the binding that ran it");
    let e = sim.handle(Input::DriverReply {
        binding: b2.clone(),
        call,
        result: Ok(json!({ "ref": { "id": "fork:h1", "strength": "strong" } })),
    });
    let start = start_params(&e).unwrap();
    assert_eq!(start["session"], f.as_str());
    assert_eq!(start["resume"]["id"], "fork:h1");
    assert_eq!(
        driver_calls(&e, "binding/stop")[0].1["binding"],
        b2.as_str(),
        "the source stops again"
    );
    assert_eq!(sim.get(&f)["seed"]["via"], "fork");
}

#[test]
fn a_stopped_source_with_a_paused_queue_summarizes_itself_and_stops_again() {
    let mut sim = Sim::new();
    let (s, b) = sim.root(json!({}));
    sim.driver(
        &b,
        json!({ "type": "ref", "ref": { "id": "h1", "strength": "strong" } }),
    );
    sim.turn(&b, "t1", "wrote the parser");
    stop_and_exit(&mut sim, &s, &b);
    sim.ok(OWNER, "queue/pause", json!({ "session": s }));
    let (r, e) = sim.call(
        OWNER,
        "session/new",
        json!({ "agent": "codex", "workspace": { "kind": "dir", "path": "/work/demo" },
            "seed": { "session": s, "summary": "source" } }),
    );
    assert_eq!(r, Err(0), "the call waits for the summary");
    let id = json!(sim.next_id);
    let woken = start_params(&e).expect("the source starts");
    assert_eq!(woken["resume"]["id"], "h1");
    let b2 = woken["binding"].as_str().unwrap().to_string();
    let e = sim.bring_up(&b2);
    let prompt = driver_calls(&e, "binding/prompt").remove(0).1;
    assert!(
        prompt["text"]
            .as_str()
            .unwrap()
            .starts_with("Summarize this session")
    );
    let e = sim.turn(&b2, "t2", "The parser is done.");
    let created = reply(&e, &id).unwrap().unwrap();
    assert!(created["session"]["id"].is_string());
    assert_eq!(
        driver_calls(&e, "binding/stop")[0].1["binding"],
        b2.as_str(),
        "the source stops again"
    );
    let q = sim.ok(OWNER, "queue/list", json!({ "session": s }))["queue"].clone();
    assert_eq!(q["paused"], true, "{q}");
}
