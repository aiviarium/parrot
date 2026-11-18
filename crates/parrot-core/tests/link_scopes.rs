//! What a link peer and a session-limited token may do: a peer mints tokens only within its own
//! grant and for sessions it created here (spec 5, 24.4, E8), and a token limited to sessions
//! sees only their workspaces.

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, Peer};
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
            core: Core::new(Box::new(clock), Box::new(SeqIds::default())),
            next: 0,
        };
        sim.connect("con_owner", Peer::Owner);
        sim
    }

    fn connect(&mut self, conn: &str, peer: Peer) {
        self.core.handle(Input::Connected {
            conn: conn.into(),
            peer,
        });
        let p = json!({ "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" }, "features": {} });
        self.call(conn, "initialize", p).0.expect("initialize");
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
                Effect::Reply { id: rid, result, .. } if *rid == id => Some(result.clone().map_err(|e| e.code)),
                _ => None,
            })
            .unwrap_or(Err(0));
        (result, effects)
    }

    fn session(&mut self, conn: &str, path: &str) -> (String, String) {
        let (r, _) = self.call(
            conn,
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": path }, "start": false }),
        );
        let r = r.expect("session/new");
        (
            r["session"]["id"].as_str().unwrap().to_string(),
            r["session"]["workspace"].as_str().unwrap().to_string(),
        )
    }
}

fn token_op(effects: &[Effect]) -> Option<Value> {
    effects.iter().find_map(|e| match e {
        Effect::Workspace { op, params, .. } if op == "token.create" => Some(params.clone()),
        _ => None,
    })
}

fn link(scopes: &[&str]) -> Peer {
    Peer::Link {
        link: "lnk_peer".into(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn a_link_mints_tokens_only_within_what_it_holds() {
    let mut sim = Sim::new();
    sim.connect("con_link", link(&["read", "answer", "manage"]));
    let (own, _) = sim.session("con_owner", "/work/owner");
    let (linked, _) = sim.session("con_link", "/work/linked");

    // More scopes than the link holds.
    let (r, e) = sim.call(
        "con_link",
        "token/create",
        json!({ "name": "phone", "scopes": ["read", "prompt"], "sessions": [linked] }),
    );
    assert_eq!(r, Err(-32003));
    assert!(token_op(&e).is_none());
    // No sessions: a token for everything here.
    let (r, _) = sim.call("con_link", "token/create", json!({ "name": "phone", "scopes": ["read"] }));
    assert_eq!(r, Err(-32003));
    // A session the owner made, not the link.
    let (r, _) = sim.call(
        "con_link",
        "token/create",
        json!({ "name": "phone", "scopes": ["read"], "sessions": [own] }),
    );
    assert_eq!(r, Err(-32003));
    // Within its grant, for its own session: the hub mints it, marked as the link's.
    let (r, e) = sim.call(
        "con_link",
        "token/create",
        json!({ "name": "phone", "scopes": ["read", "answer"], "sessions": [linked], "by": "con_owner" }),
    );
    assert_eq!(r, Err(0), "answered later, by the hub");
    assert_eq!(token_op(&e).expect("token.create")["by"], "lnk_peer");

    // The owner's tokens never carry a `by` a caller sent.
    let (_, e) = sim.call(
        "con_owner",
        "token/create",
        json!({ "name": "mine", "scopes": ["read"], "by": "lnk_peer" }),
    );
    assert!(token_op(&e).expect("token.create").get("by").is_none());
}

#[test]
fn a_session_limited_token_sees_only_its_sessions_workspaces() {
    let mut sim = Sim::new();
    let (mine, my_ws) = sim.session("con_owner", "/work/mine");
    let (_, other_ws) = sim.session("con_owner", "/work/other");
    sim.connect(
        "con_token",
        Peer::Token {
            token: "tok_a".into(),
            scopes: vec!["read".into(), "workspace".into()],
            sessions: Some(vec![mine]),
            provides: None,
        },
    );
    let (r, _) = sim.call("con_token", "workspace/list", json!({}));
    let listed = r.unwrap();
    let ids: Vec<&str> = listed["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert_eq!(ids, [my_ws.as_str()]);
    let (r, _) = sim.call("con_token", "workspace/status", json!({ "workspace": other_ws }));
    assert_eq!(r, Err(-32002));
    let (r, _) = sim.call(
        "con_token",
        "workspace/merge",
        json!({ "from": other_ws, "into": my_ws, "method": "merge" }),
    );
    assert_eq!(r, Err(-32002));
    let (r, _) = sim.call(
        "con_token",
        "workspace/create",
        json!({ "env": "", "request": { "kind": "dir", "path": "/work/new" } }),
    );
    assert_eq!(r, Err(-32003));
    let (r, _) = sim.call(
        "con_token",
        "workspace/transfer",
        json!({ "workspace": other_ws, "to": "env_far" }),
    );
    assert_eq!(r, Err(-32002));
}
