//! A link's offer (spec 24.1) is enforced on every path: a peer sees only the sessions, items,
//! trees, workspaces, requests and tokens the offer covers, and only an offered environment.

mod common;

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, LinkOffer, OfferedSessions, Peer};
use serde_json::{Value, json};

const ALL_SCOPES: [&str; 9] = [
    "read",
    "prompt",
    "answer",
    "configure",
    "manage",
    "delegate",
    "workspace",
    "destructive",
    "functions",
];

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

    fn connect(&mut self, conn: &str, peer: Peer) {
        self.core.handle(Input::Connected {
            conn: conn.into(),
            peer,
        });
        let p = json!({ "protocol": [1], "peer": { "kind": "runtime", "name": conn, "version": "1" }, "features": {} });
        self.call(conn, "initialize", p).0.expect("initialize");
    }

    fn link(&mut self, conn: &str, link: &str, env: bool, sessions: OfferedSessions) {
        let peer = Peer::Link {
            link: link.into(),
            scopes: ALL_SCOPES.iter().map(|s| s.to_string()).collect(),
            offer: LinkOffer { env, sessions },
        };
        self.connect(conn, peer);
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

    /// A session with a running binding and one open request.
    fn session(&mut self, conn: &str, path: &str) -> Session {
        let (r, effects) = self.call(
            conn,
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": path } }),
        );
        let r = r.expect("session/new");
        let binding = effects
            .iter()
            .find_map(|e| match e {
                Effect::StartBinding { binding, .. } => Some(binding.clone()),
                _ => None,
            })
            .expect("a binding");
        let id = r["session"]["id"].as_str().unwrap().to_string();
        let request = json!({
            "id": format!("{id}-ask"), "kind": "approval", "title": "Run ls?", "level": "sandboxed",
            "choices": [{ "id": "yes", "label": "Yes", "intent": "allow" }],
            "blocking": true, "answerable": "connection", "notify": "normal", "status": "open",
        });
        self.core.handle(Input::DriverUpdate {
            binding: binding.clone(),
            update: json!({ "binding": binding, "update": { "type": "request", "request": request } }),
        });
        let request = self
            .core
            .state
            .requests
            .by_id
            .values()
            .find(|r| r.request.session == id)
            .map(|r| r.request.id.clone())
            .expect("a request");
        Session {
            workspace: r["session"]["workspace"].as_str().unwrap().to_string(),
            id,
            request,
        }
    }

    fn listed(&mut self, conn: &str) -> Vec<String> {
        let r = self.ok(conn, "session/list", json!({}));
        ids(&r["sessions"], "id")
    }
}

struct Session {
    id: String,
    workspace: String,
    request: String,
}

fn ids(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v[key].as_str().map(str::to_string))
        .collect()
}

/// Every update notification sent to `conn`.
fn updates_to(effects: &[Effect], to: &str) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Notify { conn, method, params } if conn == to && method == "update" => {
                Some(params.clone())
            }
            _ => None,
        })
        .collect()
}

/// What a peer can reach of a session it isn't offered: nothing, on any path.
fn sees_nothing_of(sim: &mut Sim, conn: &str, s: &Session) {
    let sid = s.id.as_str();
    assert!(!sim.listed(conn).contains(&s.id), "session/list shows {sid}");
    let by_session = [
        ("session/get", json!({ "session": sid })),
        ("session/read", json!({ "session": sid })),
        ("item/get", json!({ "session": sid, "item": "itm_1" })),
        ("session/tree", json!({ "session": sid })),
        ("subscribe", json!({ "target": { "session": sid } })),
        ("subscribe", json!({ "target": { "tree": sid } })),
        ("session/prompt", json!({ "session": sid, "text": "hi" })),
        ("session/cancel", json!({ "session": sid })),
        ("session/stop", json!({ "session": sid })),
        ("session/start", json!({ "session": sid })),
        ("session/rename", json!({ "session": sid, "title": "mine now" })),
        ("session/archive", json!({ "session": sid, "archived": true })),
        ("session/delete", json!({ "session": sid })),
        ("session/configure", json!({ "session": sid, "ceiling": {} })),
        ("session/screen", json!({ "session": sid })),
        ("session/keys", json!({ "session": sid, "press": ["enter"] })),
        ("session/bundle", json!({ "session": sid })),
        ("queue/list", json!({ "session": sid })),
        ("queue/clear", json!({ "session": sid })),
        ("queue/pause", json!({ "session": sid })),
        ("queue/resume", json!({ "session": sid })),
        ("queue/remove", json!({ "session": sid, "id": "q_1" })),
        ("queue/edit", json!({ "session": sid, "id": "q_1", "text": "x" })),
        ("queue/move", json!({ "session": sid, "id": "q_1", "index": 0 })),
        ("session/rebind", json!({ "session": sid, "agent": "codex" })),
        (
            "session/rewind",
            json!({ "session": sid, "turn": "trn_1", "files": false }),
        ),
        (
            "workspace/transfer",
            json!({ "workspace": s.workspace, "to": "env_x" }),
        ),
        ("option/list", json!({ "session": sid })),
        (
            "option/set",
            json!({ "session": sid, "values": { "model": "x" } }),
        ),
        (
            "option/complete",
            json!({ "session": sid, "option": "model", "prefix": "" }),
        ),
        ("action/invoke", json!({ "session": sid, "action": "compact" })),
        ("task/wait", json!({ "sessions": [sid] })),
        ("task/assign", json!({ "session": sid, "task": { "brief": "x" } })),
        ("task/fail", json!({ "session": sid, "error": "x" })),
        (
            "session/new",
            json!({ "agent": "claude", "parent": sid, "workspace": { "kind": "dir", "path": "/work/child" } }),
        ),
        (
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "use", "workspace": s.workspace } }),
        ),
        ("workspace/status", json!({ "workspace": s.workspace })),
        ("workspace/remove", json!({ "workspace": s.workspace })),
        (
            "workspace/merge",
            json!({ "from": s.workspace, "into": s.workspace, "method": "merge" }),
        ),
        (
            "request/answer",
            json!({ "request": s.request, "answer": { "choice": "yes" } }),
        ),
    ];
    for (method, params) in by_session {
        let (r, effects) = sim.call(conn, method, params.clone());
        // Refused as not there or forbidden, never for a malformed call that proves nothing.
        assert!(
            matches!(r, Err(-32002 | -32003)),
            "{method} {params} answered {r:?}"
        );
        // Nothing went to a driver or the workspace host on the peer's behalf.
        let acted = effects.iter().any(|e| {
            matches!(
                e,
                Effect::DriverCall { .. } | Effect::StartBinding { .. } | Effect::StopBinding { .. }
            )
        });
        assert!(!acted, "{method} acted: {effects:?}");
    }
    let (r, _) = sim.call(conn, "action/list", json!({ "session": sid }));
    assert_eq!(r, Err(-32002), "action/list");
    let requests = sim.ok(conn, "request/list", json!({}));
    assert!(
        !ids(&requests["requests"], "id").contains(&s.request),
        "request/list"
    );
    let workspaces = sim.ok(conn, "workspace/list", json!({}));
    assert!(
        !ids(&workspaces["workspaces"], "id").contains(&s.workspace),
        "workspace/list"
    );
    let index = sim.ok(conn, "subscribe", json!({ "target": { "index": true } }));
    assert!(
        !ids(&index["snapshot"]["index"]["sessions"], "id").contains(&s.id),
        "index snapshot"
    );
    let (r, _) = sim.call(
        conn,
        "token/create",
        json!({ "name": "phone", "scopes": ["read"], "sessions": [sid] }),
    );
    assert_eq!(r, Err(-32003), "token/create");
}

#[test]
fn a_link_that_offers_nothing_gets_nothing() {
    let mut sim = Sim::new();
    let own = sim.session("con_owner", "/work/own");
    sim.link("con_none", "lnk_none", false, OfferedSessions::None);
    sees_nothing_of(&mut sim, "con_none", &own);

    // No environment: no env, no agents, nothing created or brought in.
    assert_eq!(sim.ok("con_none", "env/list", json!({}))["envs"], json!([]));
    assert_eq!(sim.ok("con_none", "agent/list", json!({}))["agents"], json!([]));
    let index = sim.ok("con_none", "subscribe", json!({ "target": { "index": true } }));
    assert_eq!(index["snapshot"]["index"]["envs"], json!([]));
    assert_eq!(index["snapshot"]["index"]["agents"], json!([]));
    let refused = [
        (
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/peer" } }),
        ),
        (
            "workspace/create",
            json!({ "env": "env_local", "request": { "kind": "dir", "path": "/work/peer" } }),
        ),
        (
            "blob/put",
            json!({ "transfer": "t1", "index": 0, "data": "x", "last": true }),
        ),
        (
            "workspace/receive",
            json!({ "workspace": "wsp_far", "transfer": "t1" }),
        ),
        (
            "session/offer",
            json!({ "session": "ses_far", "record": { "session": {} } }),
        ),
        ("session/commit", json!({ "session": "ses_far", "key": "k" })),
        ("token/list", json!({})),
    ];
    for (method, params) in refused {
        let (r, effects) = sim.call("con_none", method, params);
        assert_eq!(r, Err(-32003), "{method}");
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Workspace { .. })),
            "{method} reached the host"
        );
    }

    // Updates of the owner's sessions and workspaces never reach the peer's index feed.
    let (_, effects) = sim.call(
        "con_owner",
        "session/new",
        json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/later" }, "start": false }),
    );
    assert!(updates_to(&effects, "con_none").is_empty(), "{effects:?}");
}

#[test]
fn a_linked_offer_shows_only_sessions_made_over_the_link() {
    let mut sim = Sim::new();
    let own = sim.session("con_owner", "/work/own");
    sim.link("con_linked", "lnk_a", true, OfferedSessions::Linked);
    sim.link("con_other", "lnk_b", true, OfferedSessions::Linked);
    sees_nothing_of(&mut sim, "con_linked", &own);

    // What the link made, it sees, with its children; another link doesn't.
    let made = sim.session("con_linked", "/work/made");
    let child = sim.ok(
        "con_owner",
        "session/new",
        json!({ "agent": "claude", "parent": made.id, "workspace": { "kind": "dir", "path": "/work/kid" }, "start": false }),
    );
    let child = child["session"]["id"].as_str().unwrap().to_string();
    let listed = sim.listed("con_linked");
    assert!(listed.contains(&made.id) && listed.contains(&child), "{listed:?}");
    assert!(!listed.contains(&own.id));
    sim.ok("con_linked", "session/get", json!({ "session": made.id }));
    let requests = sim.ok("con_linked", "request/list", json!({}));
    assert_eq!(
        ids(&requests["requests"], "id"),
        std::slice::from_ref(&made.request)
    );
    let workspaces = sim.ok("con_linked", "workspace/list", json!({}));
    let ws = ids(&workspaces["workspaces"], "id");
    assert!(
        ws.contains(&made.workspace) && !ws.contains(&own.workspace),
        "{ws:?}"
    );
    sees_nothing_of(&mut sim, "con_other", &made);

    // Updates of its own session reach it; those of the owner's don't.
    sim.ok(
        "con_linked",
        "subscribe",
        json!({ "target": { "session": made.id } }),
    );
    sim.ok("con_linked", "subscribe", json!({ "target": { "index": true } }));
    let (_, effects) = sim.call(
        "con_owner",
        "session/rename",
        json!({ "session": own.id, "title": "private" }),
    );
    assert!(updates_to(&effects, "con_linked").is_empty(), "{effects:?}");
    let (_, effects) = sim.call(
        "con_owner",
        "session/rename",
        json!({ "session": made.id, "title": "shared" }),
    );
    assert!(!updates_to(&effects, "con_linked").is_empty());

    // A token minted over the link covers only what the link made.
    let (r, _) = sim.call(
        "con_linked",
        "token/create",
        json!({ "name": "phone", "scopes": ["read"], "sessions": [made.id] }),
    );
    assert_eq!(r, Err(0), "answered later, by the hub");

    // The owner narrows the offer to nothing: the live connection loses the session and its
    // subscription at once.
    let (_, effects) = sim.call(
        "con_owner",
        "link/update",
        json!({ "link": "lnk_a", "offer": { "env": true, "sessions": "none" } }),
    );
    let call = effects
        .iter()
        .find_map(|e| match e {
            Effect::Workspace { call, op, .. } if op == "link.update" => Some(*call),
            _ => None,
        })
        .expect("link.update");
    let record = json!({ "link": { "id": "lnk_a", "grant": ALL_SCOPES, "offer": { "env": true, "sessions": "none" } } });
    sim.core.handle(Input::WorkspaceReply {
        call,
        result: Ok(record),
    });
    assert!(sim.listed("con_linked").is_empty());
    let (_, effects) = sim.call(
        "con_owner",
        "session/rename",
        json!({ "session": made.id, "title": "gone" }),
    );
    assert!(updates_to(&effects, "con_linked").is_empty(), "{effects:?}");
    sees_nothing_of(&mut sim, "con_linked", &made);
}

#[test]
fn an_all_offer_shows_every_session() {
    let mut sim = Sim::new();
    let own = sim.session("con_owner", "/work/own");
    sim.link("con_all", "lnk_all", true, OfferedSessions::All);
    assert!(sim.listed("con_all").contains(&own.id));
    sim.ok("con_all", "session/get", json!({ "session": own.id }));
    let requests = sim.ok("con_all", "request/list", json!({}));
    assert_eq!(ids(&requests["requests"], "id"), [own.request]);
    assert_eq!(
        ids(&sim.ok("con_all", "env/list", json!({}))["envs"], "id"),
        ["env_local"]
    );
}

fn host_call(effects: &[Effect], want: &str) -> Option<(u64, Value)> {
    effects.iter().find_map(|e| match e {
        Effect::Workspace { call, op, params } if op == want => Some((*call, params.clone())),
        _ => None,
    })
}

#[test]
fn tokens_a_link_minted_go_with_the_link_or_its_offer() {
    for (method, params) in [
        ("link/remove", json!({ "link": "lnk_a" })),
        (
            "link/update",
            json!({ "link": "lnk_a", "offer": { "env": true, "sessions": "none" } }),
        ),
    ] {
        let mut sim = Sim::new();
        sim.link("con_linked", "lnk_a", true, OfferedSessions::Linked);
        // A phone holds a token the link minted.
        sim.connect(
            "con_phone",
            Peer::Token {
                token: "tok_phone".into(),
                scopes: vec!["read".into()],
                sessions: Some(vec!["ses_x".into()]),
                provides: None,
            },
        );
        let (_, effects) = sim.call("con_owner", method, params.clone());
        let op = method.replacen('/', ".", 1);
        let (call, _) = host_call(&effects, &op).expect("the hub is asked");
        let record =
            json!({ "link": { "id": "lnk_a", "grant": [], "offer": { "env": true, "sessions": "none" } } });
        let effects = sim.core.handle(Input::WorkspaceReply {
            call,
            result: Ok(if method == "link/remove" {
                json!({})
            } else {
                record
            }),
        });
        let (call, revoke) = host_call(&effects, "token.revoke").expect("its tokens are revoked");
        assert_eq!(revoke, json!({ "by": "lnk_a", "all": true }), "{method}");
        let effects = sim.core.handle(Input::WorkspaceReply {
            call,
            result: Ok(json!({ "revoked": ["tok_phone"] })),
        });
        let closed = effects
            .iter()
            .any(|e| matches!(e, Effect::Close { conn } if conn == "con_phone"));
        assert!(closed, "{method}: {effects:?}");
    }
}
