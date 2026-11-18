//! Subscriptions, the update sequence and replay (spec 10, Appendix E2).

use crate::principals::{allows, check, check_session, parse, sees};
use crate::{Cx, Effect, Reply, State, items, queue, sessions};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::subscribe::{
    ItemGetParams, ReadDirection, SessionReadParams, SubscribeParams, Target, UnsubscribeParams,
};
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
    /// A session and its descendants (spec 10.2, 21).
    Tree(String),
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
    if session.is_none() {
        // Updates outside any session are filtered by what they are about (spec 24.1, E8).
        match ty {
            Some("agent" | "env") if !crate::principals::env_offered(state, &sub.conn) => return false,
            Some("deleted") if crate::principals::limited(state, &sub.conn) => return false,
            Some("workspace") => {
                let w = update
                    .pointer("/workspace/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !crate::tree::visible_workspace(state, &sub.conn, w) {
                    return false;
                }
            }
            _ => {}
        }
    }
    match &sub.target {
        Watch::Index => for_index(update),
        Watch::Session(target) => session == Some(target.as_str()) && ty != Some("summary"),
        Watch::Tree(root) => {
            session.is_some_and(|s| crate::directory::path_of(state, s).iter().any(|p| p == root))
                && (ty != Some("summary") || session != Some(root.as_str()))
        }
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
        let seq = cx.next_seq();
        if is_item && let (Some(s), Some(patch), Some(i)) = (&session, update.get("item"), &item_id) {
            let key = format!("{s}/{i}");
            match items::apply(cx.state, s, patch, seq) {
                Some(item) => {
                    let created = items::created(cx.state, s, i);
                    cx.persist(json!({ "op": "put", "kind": "item", "id": key,
                        "value": { "session": s, "created": created, "item": item } }));
                }
                None => cx.persist(json!({ "op": "delete", "kind": "item", "id": key })),
            }
        }
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
        Target::Session { session } => Watch::Session(session),
        Target::Tree { tree } => Watch::Tree(tree),
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

/// The `SessionSnapshot` JSON (spec 10.2).
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
    let mut v = json!({
        "session": sessions::session_json(state, session),
        "items": items::newest(state, session, n),
        "requests": crate::requests::open_in(state, session),
        "queue": queue::queue_state(state, session),
        "options": crate::options::options_of(state, session),
        "actions": crate::actions::actions_of(state, session),
        "subscribers": subscribers,
    });
    if let Some(u) = state.usage.by_session.get(session) {
        v["usage"] = json!(u.total);
    }
    v
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "subscribe" => subscribe(cx, conn, id, params),
        "unsubscribe" => Reply::Now(unsubscribe(cx, conn, params)),
        "item/get" => Reply::Now(item_get(cx, conn, params)),
        "session/read" => Reply::Now(read(cx, conn, params)),
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
        (Watch::Tree(root), None) => {
            // Paged like `session/tree` (E8).
            let n = p.items.unwrap_or(50) as usize;
            let all = crate::tree::subtree(cx.state, root);
            let page: Vec<Value> = all
                .iter()
                .take(100)
                .map(|s| session_snapshot(cx.state, s, n))
                .collect();
            let mut snap = json!({ "sessions": page });
            if all.len() > 100 {
                snap["next"] = json!("100");
            }
            result["snapshot"] = snap;
        }
        (Watch::Index, None) => {
            // The first page of what `session/list` returns; `next` continues it there (10.3).
            let listed = sessions::list_page(cx.state, conn, &Default::default())
                .unwrap_or_else(|_| json!({ "sessions": [] }));
            // A link that doesn't offer the environment sees neither it nor its agents (24.1).
            let env = crate::principals::env_offered(cx.state, conn);
            let mut envs: Vec<Value> = env.then(|| sessions::env_json(cx.state)).into_iter().collect();
            envs.extend(crate::links::peer_envs(cx, conn));
            let mut index = json!({
                "sessions": listed["sessions"],
                "agents": if env { json!(sessions::agents(cx.state)) } else { json!([]) },
                "envs": envs,
            });
            if let Some(next) = listed.get("next") {
                index["next"] = next.clone();
            }
            result["snapshot"] = json!({ "index": index });
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

/// Ends the subscriptions of `conn` to sessions it no longer sees, after its grant changed.
pub fn drop_unseen(cx: &mut Cx, conn: &str) {
    let state = &*cx.state;
    let gone: Vec<String> = state
        .subscriptions
        .by_id
        .values()
        .filter(|s| s.conn == conn)
        .filter(|s| match &s.target {
            Watch::Session(t) | Watch::Tree(t) => !sees(state, conn, t),
            Watch::Index => false,
        })
        .map(|s| s.id.clone())
        .collect();
    for id in gone {
        cx.state.subscriptions.by_id.remove(&id);
    }
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

fn read(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionReadParams = parse(params)?;
    check_session(cx.state, conn, Scope::Read, &p.session)?;
    if !cx.state.sessions.by_id.contains_key(&p.session) {
        return Err(RpcError::new(-32002, format!("session not found: {}", p.session)));
    }
    let forward = p.direction == Some(ReadDirection::Forward);
    let limit = p.limit.unwrap_or(50).clamp(1, 500) as usize;
    Ok(items::read(
        cx.state,
        &p.session,
        p.cursor.as_deref(),
        forward,
        limit,
    ))
}

/// A connection closed.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.subscriptions.by_id.retain(|_, s| s.conn != conn);
}
