//! A child session that moved to another runtime keeps reporting its task to its parent's
//! runtime: through the link when it is up, through the link's durable outbox when it is down
//! (E2). Only the runtime the child moved to may speak for it.

mod common;

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::sessions::SessionRecord;
use parrot_core::{Core, Effect, Input, LinkOffer, OfferedSessions, Peer};
use serde_json::{Value, json};

struct Sim {
    core: Core,
    next: u64,
}

impl Sim {
    fn new() -> Sim {
        let clock = ManualClock::default();
        clock.set(1_700_000_000_000);
        let mut sim = Sim {
            core: common::core(Box::new(clock), Box::new(SeqIds::default())),
            next: 0,
        };
        sim.connect("con_owner", Peer::Owner);
        sim
    }

    fn connect(&mut self, conn: &str, peer: Peer) -> Vec<Effect> {
        let mut effects = self.core.handle(Input::Connected {
            conn: conn.into(),
            peer,
        });
        let p = json!({ "protocol": [1], "peer": { "kind": "runtime", "name": conn, "version": "1" }, "features": {} });
        let (r, more) = self.call(conn, "initialize", p);
        r.expect("initialize");
        effects.extend(more);
        effects
    }

    fn link(&mut self, conn: &str, link: &str) -> Vec<Effect> {
        self.connect(
            conn,
            Peer::Link {
                link: link.into(),
                scopes: vec!["read".into(), "delegate".into()],
                offer: LinkOffer {
                    env: true,
                    sessions: OfferedSessions::Linked,
                },
            },
        )
    }

    fn call(&mut self, conn: &str, method: &str, params: Value) -> (Result<Value, i64>, Vec<Effect>) {
        self.next += 1;
        let id = json!(self.next);
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

    fn ok(&mut self, conn: &str, method: &str, params: Value) -> Value {
        let (r, _) = self.call(conn, method, params.clone());
        r.unwrap_or_else(|e| panic!("{method} {params}: {e}"))
    }
}

fn sent(effects: &[Effect], to: &str, method: &str) -> Option<Value> {
    effects.iter().find_map(|e| match e {
        Effect::Notify {
            conn,
            method: m,
            params,
        } if conn == to && m == method => Some(params.clone()),
        _ => None,
    })
}

fn outboxed(effects: &[Effect]) -> bool {
    effects.iter().any(
        |e| matches!(e, Effect::Persist { record } if record["kind"] == "outbox" && record["op"] == "put"),
    )
}

#[test]
fn a_moved_child_reports_its_task_through_the_outbox() {
    // The new owner holds the child; its parent stayed on the runtime it came from.
    let mut sim = Sim::new();
    sim.core.state.sessions.by_id.insert(
        "ses_child".into(),
        SessionRecord {
            id: "ses_child".into(),
            parent: Some("ses_parent".into()),
            binding: None,
            agent: "claude".into(),
        },
    );
    sim.core
        .state
        .sessions
        .info
        .insert("ses_child".into(), Default::default());
    parrot_core::tree::restore(
        &mut sim.core.state,
        "ses_child",
        &json!({ "kind": "child", "task": { "id": "tsk_a", "brief": "count", "completeOn": "finish", "status": "running" } }),
    );
    sim.core
        .state
        .links
        .arrived
        .insert("ses_child".into(), "lnk_old".into());

    // The link is down: the change waits in the outbox.
    let (r, effects) = sim.call(
        "con_owner",
        "task/fail",
        json!({ "session": "ses_child", "error": "no input" }),
    );
    assert_eq!(r, Ok(json!({})));
    assert!(outboxed(&effects), "{effects:?}");

    // The link comes back: the change is delivered, with this runtime named.
    let effects = sim.link("con_old", "lnk_old");
    let change = sent(&effects, "con_old", "task/changed").expect("delivered on connect");
    assert_eq!(change["session"], "ses_child");
    assert_eq!(change["parent"], "ses_parent");
    assert_eq!(change["status"], "failed");
    assert_eq!(change["task"]["error"], "no input");
    assert_eq!(change["runtime"], "rt_local");
    let cleared = effects.iter().any(
        |e| matches!(e, Effect::Persist { record } if record["kind"] == "outbox" && record["op"] == "delete"),
    );
    assert!(cleared, "{effects:?}");

    // While it is up, changes go straight over it.
    sim.ok(
        "con_owner",
        "task/assign",
        json!({ "session": "ses_child", "task": { "brief": "again" } }),
    );
    let (_, effects) = sim.call(
        "con_owner",
        "task/fail",
        json!({ "session": "ses_child", "error": "x" }),
    );
    assert!(sent(&effects, "con_old", "task/changed").is_some(), "{effects:?}");
    assert!(!outboxed(&effects));
}

#[test]
fn the_parents_runtime_takes_the_change_from_the_new_owner_only() {
    let mut sim = Sim::new();
    let parent = sim.ok(
        "con_owner",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/p" }, "start": false }),
    );
    let parent = parent["session"]["id"].as_str().unwrap().to_string();
    let child = sim.ok(
        "con_owner",
        "session/new",
        json!({ "agent": "claude", "parent": parent, "task": { "brief": "count" },
                "workspace": { "kind": "dir", "path": "/work/c" }, "start": false }),
    );
    let child = child["session"]["id"].as_str().unwrap().to_string();
    // The child moved over `lnk_new`.
    sim.core.state.links.moved.insert(child.clone(), "rt_new".into());
    sim.core
        .state
        .links
        .moved_via
        .insert(child.clone(), "lnk_new".into());
    sim.link("con_new", "lnk_new");
    sim.link("con_other", "lnk_other");
    let first = sim.ok("con_owner", "task/wait", json!({ "sessions": [child] }));
    let cursor = first["cursor"].clone();

    // Another peer can't speak for the child.
    let (r, _) = sim.call(
        "con_other",
        "task/changed",
        json!({ "session": child, "status": "done", "task": { "output": { "text": "forged" } } }),
    );
    assert_eq!(r, Err(-32002));
    let wait = sim.ok(
        "con_owner",
        "task/wait",
        json!({ "sessions": [child], "since": cursor, "timeout": 0 }),
    );
    assert_eq!(wait["changes"], json!([]), "{wait}");

    // The runtime it moved to can, and the parent's waits see it.
    let effects = sim.core.handle(Input::Call {
        conn: "con_new".into(),
        id: None,
        method: "task/changed".into(),
        params: json!({ "session": child, "status": "done", "task": { "output": { "text": "42" } }, "runtime": "rt_new" }),
    });
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::Reply { .. })),
        "a notification gets no answer"
    );
    let wait = sim.ok(
        "con_owner",
        "task/wait",
        json!({ "sessions": [child], "since": cursor, "timeout": 0 }),
    );
    let change = &wait["changes"][0];
    assert_eq!(change["task"]["status"], "done", "{wait}");
    assert_eq!(change["task"]["output"]["text"], "42", "{wait}");
}
