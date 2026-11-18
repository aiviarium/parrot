//! A move waits for the running turn to end: up to 30 seconds, then it interrupts the turn, and
//! the old binding stops only after that (E9).

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Effect, Input, LinkOffer, Peer};
use serde_json::{Value, json};

const T0: u64 = 1_700_000_000_000;

struct Sim {
    core: Core,
    clock: ManualClock,
    next: u64,
    session: String,
    binding: String,
}

impl Sim {
    /// A session whose binding runs a turn, and a connected link to `env_far`.
    fn new() -> Sim {
        let clock = ManualClock::default();
        clock.set(T0);
        let mut sim = Sim {
            core: Core::new(Box::new(clock.clone()), Box::new(SeqIds::default())),
            clock,
            next: 0,
            session: String::new(),
            binding: String::new(),
        };
        sim.connect("con_owner", Peer::Owner);
        sim.connect(
            "con_far",
            Peer::Link {
                link: "lnk_far".into(),
                scopes: vec!["read".into()],
                offer: LinkOffer::default(),
            },
        );
        sim.core.handle(Input::Call {
            conn: "con_far".into(),
            id: None,
            method: "link/peer".into(),
            params: json!({ "runtime": "rt_far", "env": "env_far" }),
        });
        let (r, effects) = sim.call(
            "con_owner",
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/work/demo" } }),
        );
        sim.session = r.unwrap()["session"]["id"].as_str().unwrap().to_string();
        sim.binding = effects
            .iter()
            .find_map(|e| match e {
                Effect::StartBinding { binding, .. } => Some(binding.clone()),
                _ => None,
            })
            .unwrap();
        sim.core.handle(Input::DriverReply {
            binding: sim.binding.clone(),
            call: u64::MAX,
            result: Ok(json!({ "isolation": "none", "level": "write", "network": false, "options": [], "actions": [] })),
        });
        sim.state("running");
        sim
    }

    fn connect(&mut self, conn: &str, peer: Peer) {
        self.core.handle(Input::Connected {
            conn: conn.into(),
            peer,
        });
        let p = json!({ "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" }, "features": {} });
        self.call(conn, "initialize", p).0.unwrap();
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

    fn state(&mut self, state: &str) -> Vec<Effect> {
        self.core.handle(Input::DriverUpdate {
            binding: self.binding.clone(),
            update: json!({ "binding": self.binding, "update": { "type": "state", "state": state, "background": [] } }),
        })
    }

    fn tick(&mut self, at: u64) -> Vec<Effect> {
        self.clock.set(at);
        self.core.handle(Input::Tick { now_ms: at })
    }

    fn rebind(&mut self, extra: Value) -> Vec<Effect> {
        let mut params = json!({ "session": self.session, "env": "env_far" });
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (r, effects) = self.call("con_owner", "session/rebind", params);
        assert_eq!(r, Err(0), "the move answers once it is done");
        effects
    }
}

fn cancels(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::DriverCall { method, .. } if method == "binding/cancel"))
}

fn stops(effects: &[Effect]) -> bool {
    effects.iter().any(|e| matches!(e, Effect::StopBinding { .. }))
}

/// The move went on: the session's workspace is packed for the target, then offered to it.
fn offers(effects: &[Effect]) -> bool {
    effects.iter().any(|e| {
        matches!(e, Effect::ClientCall { conn, method, .. } if conn == "con_far" && method == "session/offer")
            || matches!(e, Effect::Workspace { op, .. } if op == "transfer.pack")
    })
}

#[test]
fn a_move_waits_for_the_turn_then_interrupts_it_after_30_seconds() {
    let mut sim = Sim::new();
    let effects = sim.rebind(json!({}));
    assert!(
        !stops(&effects) && !offers(&effects) && !cancels(&effects),
        "{effects:?}"
    );

    // Nothing happens before 30 seconds have passed.
    let effects = sim.tick(T0 + 29_000);
    assert!(!cancels(&effects) && !stops(&effects), "{effects:?}");
    // Then the turn is interrupted, and the binding still runs.
    let effects = sim.tick(T0 + 30_001);
    assert!(cancels(&effects), "{effects:?}");
    assert!(!stops(&effects) && !offers(&effects));
    // The turn ends: the old binding stops and the session is offered to the target.
    let effects = sim.state("idle");
    assert!(stops(&effects), "{effects:?}");
    assert!(offers(&effects), "{effects:?}");
}

#[test]
fn a_turn_that_ends_in_time_is_not_interrupted() {
    let mut sim = Sim::new();
    sim.rebind(json!({}));
    let effects = sim.state("idle");
    assert!(offers(&effects) && stops(&effects), "{effects:?}");
    let effects = sim.tick(T0 + 31_000);
    assert!(!cancels(&effects), "{effects:?}");
}

#[test]
fn an_interrupting_move_cancels_at_once_and_stops_the_binding_if_the_turn_hangs() {
    let mut sim = Sim::new();
    let effects = sim.rebind(json!({ "interrupt": true }));
    assert!(cancels(&effects) && !stops(&effects), "{effects:?}");
    let effects = sim.tick(T0 + parrot_core::links::INTERRUPT_GRACE_MS + 1);
    assert!(stops(&effects) && offers(&effects), "{effects:?}");
}

#[test]
fn nothing_is_dispatched_while_a_move_waits() {
    let mut sim = Sim::new();
    sim.rebind(json!({}));
    let (r, effects) = sim.call(
        "con_owner",
        "session/prompt",
        json!({ "session": sim.session, "text": "more", "delivery": "queue" }),
    );
    assert!(r.is_ok(), "{r:?}");
    let effects = [effects, sim.state("idle")].concat();
    let prompted = effects
        .iter()
        .any(|e| matches!(e, Effect::DriverCall { method, .. } if method == "binding/prompt"));
    assert!(!prompted, "{effects:?}");
}
