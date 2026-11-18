//! Subscriptions, the update sequence and replay (spec 10, Appendix E2).

use crate::principals::{allows, check, check_session, parse, sees};
use crate::{Cx, Effect, Reply, State, items, queue, sessions};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::subscribe::{ItemGetParams, SubscribeParams, Target, UnsubscribeParams};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

/// How many recent updates replay can reach.
pub const RECENT: usize = 10_000;

/// Subscriptions by id, and the recent update log.
#[derive(Debug, Default)]
pub struct Subscriptions {
    pub by_id: BTreeMap<String, Subscription>,
    pub recent: VecDeque<Logged>,
    /// The highest `seq` dropped from `recent`; replay from before it is unavailable.
    pub dropped: u64,
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: String,
    pub conn: String,
    pub target: Watch,
}

/// What a subscription watches. Trees degrade to their root session in the core profile.
#[derive(Debug, Clone, PartialEq)]
pub enum Watch {
    Session(String),
    Index,
}

/// One update as it was sent.
#[derive(Debug, Clone)]
pub struct Logged {
    pub seq: u64,
    pub at_ms: u64,
    pub session: Option<String>,
    pub update: Value,
}

/// Updates the index feed carries (spec 10.3).
fn for_index(update: &Value) -> bool {
    matches!(
        update.get("type").and_then(Value::as_str),
        Some("summary" | "agent" | "env" | "workspace" | "deleted")
    )
}

/// Whether a subscription receives an update.
fn matches(state: &State, sub: &Subscription, session: Option<&str>, update: &Value) -> bool {
    if !allows(state, &sub.conn, Scope::Read) {
        return false;
    }
    if let Some(s) = session
        && !sees(state, &sub.conn, s)
    {
        return false;
    }
    let ty = update.get("type").and_then(Value::as_str);
    match &sub.target {
        Watch::Index => for_index(update),
        Watch::Session(target) => session == Some(target.as_str()) && ty != Some("summary"),
    }
}

fn notify(cx: &mut Cx, sub: &Subscription, logged: &Logged) {
    let mut params = json!({
        "subscription": sub.id,
        "seq": logged.seq.to_string(),
        "runtime": cx.state.principals.runtime,
        "at": sessions::timestamp(logged.at_ms),
        "update": logged.update,
    });
    if let Some(s) = &logged.session {
        params["session"] = json!(s);
    }
    cx.emit(Effect::Notify {
        conn: sub.conn.clone(),
        method: "update".into(),
        params,
    });
}

/// Assign `seq` to the updates in `cx.state.outbox`, persist them and send them to subscribers.
pub fn flush(cx: &mut Cx) {
    let outbox = std::mem::take(&mut cx.state.outbox);
    for (session, update) in outbox {
        let is_item = update.get("type").and_then(Value::as_str) == Some("item");
        let item_id = update
            .pointer("/item/id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if is_item && let (Some(s), Some(patch)) = (&session, update.get("item")) {
            items::apply(cx.state, s, patch);
        }
        let seq = cx.next_seq();
        let mut record = json!({ "op": "append", "seq": seq, "at": cx.now_ms, "update": update });
        if let Some(s) = &session {
            record["session"] = json!(s);
            if is_item && let Some(i) = &item_id {
                record["item"] = json!(i);
            }
        }
        cx.persist(record);
        let logged = Logged {
            seq,
            at_ms: cx.now_ms,
            session,
            update,
        };
        let subs: Vec<Subscription> = cx
            .state
            .subscriptions
            .by_id
            .values()
            .filter(|s| matches(cx.state, s, logged.session.as_deref(), &logged.update))
            .cloned()
            .collect();
        for sub in &subs {
            notify(cx, sub, &logged);
        }
        let subs = &mut cx.state.subscriptions;
        subs.recent.push_back(logged);
        while subs.recent.len() > RECENT {
            if let Some(old) = subs.recent.pop_front() {
                subs.dropped = old.seq;
            }
        }
    }
}

/// Subscribe a connection without a snapshot; returns the subscription id and the head cursor.
pub fn subscribe_conn(cx: &mut Cx, conn: &str, target: Target) -> (String, String) {
    let target = match target {
        Target::Session { session } | Target::Tree { tree: session } => Watch::Session(session),
        Target::Index { .. } => Watch::Index,
    };
    let id = cx.ids.next("sub");
    cx.state.subscriptions.by_id.insert(
        id.clone(),
        Subscription {
            id: id.clone(),
            conn: conn.to_string(),
            target,
        },
    );
    (id, cx.state.seq.to_string())
}

/// The `SessionSnapshot` JSON (spec 10.2). Requests, options, actions and usage are owned by other
/// modules and start empty here.
pub fn session_snapshot(state: &State, session: &str, n: usize) -> Value {
    let subscribers: Vec<Value> = state
        .subscriptions
        .by_id
        .values()
        .filter(|s| s.target == Watch::Session(session.to_string()))
        .map(|s| {
            let name = state
                .principals
                .conns
                .get(&s.conn)
                .and_then(|c| c.info.as_ref())
                .map(|i| i.name.clone())
                .unwrap_or_default();
            json!({ "id": s.conn, "name": name, "principal": crate::principals::principal_of(state, &s.conn) })
        })
        .collect();
    json!({
        "session": sessions::session_json(state, session),
        "items": items::newest(state, session, n),
        "requests": [],
        "queue": queue::queue_state(state, session),
        "options": [],
        "actions": [],
        "subscribers": subscribers,
    })
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "subscribe" => subscribe(cx, conn, id, params),
        "unsubscribe" => Reply::Now(unsubscribe(cx, conn, params)),
        "item/get" => Reply::Now(item_get(cx, conn, params)),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn subscribe(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let p: SubscribeParams = match parse(params) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(e)),
    };
    let checked = match &p.target {
        Target::Session { session } | Target::Tree { tree: session } => {
            check_session(cx.state, conn, Scope::Read, session)
        }
        Target::Index { .. } => check(cx.state, conn, Scope::Read),
    };
    if let Err(e) = checked {
        return Reply::Now(Err(e));
    }
    let from = match p.replay_from.as_deref().map(str::parse::<u64>) {
        None => None,
        Some(Ok(n)) => Some(n),
        Some(Err(_)) => return Reply::Now(Err(RpcError::new(-32602, "invalid replayFrom cursor"))),
    };
    if let Some(from) = from
        && from < cx.state.subscriptions.dropped
    {
        return Reply::Now(Err(RpcError::new(-32012, "replay unavailable")));
    }
    let (sub_id, head) = subscribe_conn(cx, conn, p.target.clone());
    let sub = cx.state.subscriptions.by_id[&sub_id].clone();
    let mut result = json!({ "subscription": sub_id, "head": head });
    match (&sub.target, from) {
        (_, Some(_)) => {}
        (Watch::Session(s), None) => {
            let n = p.items.unwrap_or(50) as usize;
            result["snapshot"] = json!({ "sessions": [session_snapshot(cx.state, s, n)] });
        }
        (Watch::Index, None) => {
            let summaries: Vec<Value> = cx
                .state
                .sessions
                .by_id
                .keys()
                .filter(|s| sees(cx.state, conn, s))
                .filter_map(|s| sessions::summary_json(cx.state, s))
                .collect();
            result["snapshot"] = json!({ "index": {
                "sessions": summaries,
                "agents": sessions::agents(cx.state),
                "envs": [sessions::env_json(cx.state)],
            }});
        }
    }
    // Reply before the replayed updates, so the client knows the subscription they name.
    if let Some(id) = id {
        cx.reply(conn, id.clone(), Ok(result));
    }
    if let Some(from) = from {
        let replay: Vec<Logged> = cx
            .state
            .subscriptions
            .recent
            .iter()
            .filter(|l| l.seq > from && matches(cx.state, &sub, l.session.as_deref(), &l.update))
            .cloned()
            .collect();
        for l in &replay {
            notify(cx, &sub, l);
        }
    }
    Reply::Later
}

fn unsubscribe(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    check(cx.state, conn, Scope::Read)?;
    let p: UnsubscribeParams = parse(params)?;
    match cx.state.subscriptions.by_id.get(&p.subscription) {
        Some(s) if s.conn == conn => {
            cx.state.subscriptions.by_id.remove(&p.subscription);
            Ok(json!({}))
        }
        _ => Err(RpcError::new(
            -32002,
            format!("subscription not found: {}", p.subscription),
        )),
    }
}

fn item_get(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: ItemGetParams = parse(params)?;
    check_session(cx.state, conn, Scope::Read, &p.session)?;
    items::get(cx.state, &p.session, &p.item)
        .map(|item| json!({ "item": item }))
        .ok_or_else(|| RpcError::new(-32002, format!("item not found: {}", p.item)))
}

/// A connection closed.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.subscriptions.by_id.retain(|_, s| s.conn != conn);
}
