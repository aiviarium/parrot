//! Sessions a peer owns, reached through the link (spec 10.2, 24.5): `session/new` in a linked
//! environment, forwarded calls and subscriptions, and what a dropped link does to them.

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, LinkOffer, Peer};
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
        sim.up("con_far");
        sim
    }

    fn connect(&mut self, conn: &str, peer: Peer) -> Vec<Effect> {
        let mut effects = self.core.handle(Input::Connected {
            conn: conn.into(),
            peer,
        });
        let p = json!({ "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" }, "features": {} });
        effects.extend(self.call(conn, "initialize", p).1);
        effects
    }

    /// The link to `env_far` connects as `conn`, and its peer says who it is.
    fn up(&mut self, conn: &str) -> Vec<Effect> {
        let peer = Peer::Link {
            link: "lnk_far".into(),
            scopes: vec!["read".into()],
            offer: LinkOffer::default(),
        };
        let mut effects = self.connect(conn, peer);
        effects.extend(self.core.handle(Input::Call {
            conn: conn.into(),
            id: None,
            method: "link/peer".into(),
            params: json!({ "runtime": "rt_far", "env": "env_far", "name": "server" }),
        }));
        effects
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

    /// The peer answers the call this runtime made to it.
    fn answer(&mut self, call: u64, result: Result<Value, i64>) -> Vec<Effect> {
        self.core.handle(Input::ClientReply {
            conn: "con_far".into(),
            call,
            result: result.map_err(|c| parrot_proto::rpc::RpcError::new(c, "peer")),
        })
    }

    fn update(&mut self, conn: &str, subscription: &str, seq: u64) -> Vec<Effect> {
        self.core.handle(Input::Call {
            conn: conn.into(),
            id: None,
            method: "update".into(),
            params: json!({ "subscription": subscription, "seq": seq.to_string(), "runtime": "rt_far",
                "session": "ses_far", "update": { "type": "state", "state": "running", "background": [] } }),
        })
    }
}

fn to_peer(effects: &[Effect], method: &str) -> Option<(u64, Value)> {
    effects.iter().find_map(|e| match e {
        Effect::ClientCall {
            conn,
            call,
            method: m,
            params,
        } if m == method && conn.starts_with("con_far") => Some((*call, params.clone())),
        _ => None,
    })
}

fn reply(effects: &[Effect], conn: &str) -> Option<Result<Value, i64>> {
    effects.iter().find_map(|e| match e {
        Effect::Reply { conn: c, result, .. } if c == conn => Some(result.clone().map_err(|e| e.code)),
        _ => None,
    })
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

fn envs(sim: &mut Sim) -> Value {
    sim.call("con_owner", "env/list", json!({})).0.unwrap()["envs"].clone()
}

#[test]
fn a_session_in_a_linked_environment_is_the_peers_and_survives_a_drop() {
    let mut sim = Sim::new();
    let far = envs(&mut sim);
    assert_eq!(far[1]["id"], "env_far", "{far}");
    assert_eq!(far[1]["name"], "server");
    assert_eq!(far[1]["status"], "online");

    // `session/new` in the linked environment goes to the peer.
    let (r, effects) = sim.call(
        "con_owner",
        "session/new",
        json!({ "agent": "claude", "env": "env_far", "workspace": { "kind": "dir", "path": "/srv/demo" } }),
    );
    assert_eq!(r, Err(0), "answered once the peer has");
    let (call, params) = to_peer(&effects, "session/new").expect("forwarded");
    assert_eq!(params["env"], "env_far");
    let effects = sim.answer(
        call,
        Ok(json!({ "session": { "id": "ses_far" }, "subscription": "sub_peer1", "head": "7" })),
    );
    let made = reply(&effects, "con_owner").unwrap().unwrap();
    let local = made["subscription"].as_str().unwrap().to_string();
    assert_ne!(
        local, "sub_peer1",
        "the client gets a subscription of its own runtime"
    );

    // Its updates reach the client under that subscription.
    let effects = sim.update("con_far", "sub_peer1", 8);
    let got = updates(&effects, "con_owner");
    assert_eq!(got.len(), 1, "{effects:?}");
    assert_eq!(got[0]["subscription"], local.as_str());
    assert_eq!(got[0]["runtime"], "rt_far");
    // Calls for it go to the peer.
    let (_, effects) = sim.call("con_owner", "session/get", json!({ "session": "ses_far" }));
    assert!(to_peer(&effects, "session/get").is_some(), "{effects:?}");

    // The link drops: the environment is offline, calls fail fast.
    let effects = sim.core.handle(Input::Disconnected {
        conn: "con_far".into(),
    });
    let offline = effects.iter().any(|e| {
        matches!(e, Effect::Persist { record } if record["update"]["type"] == "env" && record["update"]["env"]["status"] == "offline")
    });
    assert!(offline, "{effects:?}");
    assert_eq!(envs(&mut sim)[1]["status"], "offline");
    let (r, _) = sim.call("con_owner", "session/get", json!({ "session": "ses_far" }));
    assert_eq!(r, Err(-32030));

    // It comes back: the subscription is made again from the last update passed on.
    let effects = sim.up("con_far2");
    let (call, params) = to_peer(&effects, "subscribe").expect("subscribed again");
    assert_eq!(params["replayFrom"], "8", "{params}");
    assert_eq!(params["target"]["session"], "ses_far");
    sim.core.handle(Input::ClientReply {
        conn: "con_far2".into(),
        call,
        result: Ok(json!({ "subscription": "sub_peer2", "head": "9" })),
    });
    let effects = sim.update("con_far2", "sub_peer2", 9);
    let got = updates(&effects, "con_owner");
    assert_eq!(got.len(), 1, "{effects:?}");
    assert_eq!(got[0]["subscription"], local.as_str());
    // The old subscription's updates are not passed on any more.
    assert!(updates(&sim.update("con_far2", "sub_peer1", 10), "con_owner").is_empty());
    assert_eq!(envs(&mut sim)[1]["status"], "online");
}

#[test]
fn a_gap_the_peer_no_longer_has_is_reported_and_the_feed_goes_on() {
    let mut sim = Sim::new();
    let (_, effects) = sim.call(
        "con_owner",
        "session/new",
        json!({ "agent": "claude", "env": "env_far", "workspace": { "kind": "dir", "path": "/srv/demo" } }),
    );
    let (call, _) = to_peer(&effects, "session/new").unwrap();
    let effects = sim.answer(
        call,
        Ok(json!({ "session": { "id": "ses_far" }, "subscription": "sub_peer1", "head": "7" })),
    );
    let local = reply(&effects, "con_owner").unwrap().unwrap()["subscription"]
        .as_str()
        .unwrap()
        .to_string();
    sim.core.handle(Input::Disconnected {
        conn: "con_far".into(),
    });
    let effects = sim.up("con_far2");
    let (call, _) = to_peer(&effects, "subscribe").unwrap();
    let effects = sim.core.handle(Input::ClientReply {
        conn: "con_far2".into(),
        call,
        result: Err(parrot_proto::rpc::RpcError::new(-32012, "replay unavailable")),
    });
    let (call, params) = to_peer(&effects, "subscribe").expect("subscribed afresh");
    assert!(params.get("replayFrom").is_none(), "{params}");
    let effects = sim.core.handle(Input::ClientReply {
        conn: "con_far2".into(),
        call,
        result: Ok(json!({ "subscription": "sub_peer3", "head": "40", "snapshot": {} })),
    });
    let got = updates(&effects, "con_owner");
    assert_eq!(got[0]["update"]["type"], "notice", "{effects:?}");
    assert_eq!(got[0]["update"]["code"], "resync");
    assert_eq!(got[0]["subscription"], local.as_str());
    assert_eq!(
        updates(&sim.update("con_far2", "sub_peer3", 41), "con_owner").len(),
        1
    );
}

#[test]
fn a_peer_cannot_reach_a_third_runtime_through_this_one() {
    let mut sim = Sim::new();
    sim.connect(
        "con_other",
        Peer::Link {
            link: "lnk_other".into(),
            scopes: vec!["read".into(), "manage".into()],
            offer: LinkOffer::all(),
        },
    );
    let (r, effects) = sim.call(
        "con_other",
        "session/new",
        json!({ "agent": "claude", "env": "env_far", "workspace": { "kind": "dir", "path": "/srv/x" } }),
    );
    assert_eq!(r, Err(-32002));
    assert!(to_peer(&effects, "session/new").is_none());
    // Nor does it see the linked environment.
    let envs = sim.call("con_other", "env/list", json!({})).0.unwrap();
    assert_eq!(envs["envs"].as_array().map(Vec::len), Some(1), "{envs}");
}
