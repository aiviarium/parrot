//! Links to other runtimes in the core (spec 24, E1, E2, E9): link methods, link connections,
//! calls forwarded to sessions another runtime owns, moving sessions with `session/offer` and
//! `session/commit`, blob transfers and the per-link outbox.
//!
//! Link records, keys and carriers are I/O, so they live in `parrot-serve`'s link hub. The core
//! asks the hub for them with `Effect::Workspace` operations named `link.*` and answers the
//! client when the hub has.

use crate::{CallOwner, Cx, Effect, Peer, Reply, principals, sessions};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// How long an offer waits for its commit before it expires and nothing moved (E1).
pub const OFFER_TTL_MS: u64 = 60_000;
/// The largest `blob/put` chunk (spec 24.2).
pub const CHUNK: usize = 1024 * 1024;
/// The error code of a session that moved to another runtime.
pub const MOVED: i64 = -32050;

#[derive(Debug, Default)]
pub struct Links {
    /// The connection of each connected link, by link id.
    pub conns: BTreeMap<String, String>,
    /// What each connected peer said about itself: its runtime id and environment id.
    pub peers: BTreeMap<String, PeerInfo>,
    /// Sessions this runtime moved away, with the runtime that owns them now.
    pub moved: BTreeMap<String, String>,
    /// Sessions another runtime owns, reached through a link, by session id.
    pub remote: BTreeMap<String, String>,
    /// Calls waiting for the hub or a peer.
    pub pending: BTreeMap<u64, Pending>,
    /// Blob transfers arriving from peers, by transfer id.
    pub blobs: BTreeMap<String, Blob>,
    /// Offers received and not yet committed, by session.
    pub offers: BTreeMap<String, Offer>,
    /// Commit keys already applied, so a repeated commit answers the same (E9).
    pub commits: BTreeMap<String, Value>,
    /// Messages for peers that are down, delivered when the link connects again (E2).
    pub outbox: BTreeMap<String, Vec<(String, Value)>>,
}

#[derive(Debug, Clone, Default)]
pub struct PeerInfo {
    pub runtime: String,
    pub env: String,
}

#[derive(Debug, Clone)]
pub enum Pending {
    /// A hub operation answering a client call.
    Hub { conn: String, id: Option<RequestId> },
    /// A call forwarded to the session's owner.
    Forward { conn: String, id: Option<RequestId> },
    /// A chunk of a move's log.
    Chunk { session: String },
    /// The offer of a move; the commit follows its answer.
    Offer(Move),
    /// The commit of a move.
    Commit(Move),
}

#[derive(Debug, Clone)]
pub struct Move {
    pub session: String,
    pub link: String,
    pub key: String,
    pub conn: String,
    pub id: Option<RequestId>,
    pub binding: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct Blob {
    pub link: String,
    pub parts: BTreeMap<u64, String>,
    pub last: Option<u64>,
    pub expires_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub link: String,
    pub expires_ms: u64,
    pub binding: Option<String>,
}

fn link_of(cx: &Cx, conn: &str) -> Option<String> {
    match cx.state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Link { link, .. }) => Some(link.clone()),
        _ => None,
    }
}

fn owner_only(cx: &Cx, conn: &str) -> Result<(), RpcError> {
    principals::check(cx.state, conn, Scope::Read)?;
    match cx.state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Owner) => Ok(()),
        _ => Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "owner" }))),
    }
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "link/peer" => {
            peer_said(cx, conn, &params);
            Reply::Now(Ok(json!({})))
        }
        "link/list" | "link/invite" | "link/add" | "link/update" | "link/remove" | "carrier/list" => {
            if let Err(e) = owner_only(cx, conn) {
                return Reply::Now(Err(e));
            }
            let op = method.replacen('/', ".", 1);
            let call = cx.call_workspace(CallOwner::Links, &op, params);
            cx.state.links.pending.insert(
                call,
                Pending::Hub {
                    conn: conn.to_string(),
                    id: id.cloned(),
                },
            );
            Reply::Later
        }
        "blob/put" => Reply::Now(blob_put(cx, conn, params)),
        "session/offer" => Reply::Now(offer(cx, conn, params)),
        "session/commit" => Reply::Now(commit(cx, conn, params)),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

/// Calls the links answer before the area that would own them: sessions that moved away, and
/// sessions another runtime owns. `session/rebind` to a linked environment is a move.
pub fn intercept(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    method: &str,
    params: &Value,
) -> Option<Reply> {
    let session = params.get("session").and_then(Value::as_str)?.to_string();
    if let Some(runtime) = cx.state.links.moved.get(&session) {
        return Some(Reply::Now(Err(RpcError::new(MOVED, "the session moved")
            .with_data(json!({ "runtime": runtime, "session": session })))));
    }
    if let Some(link) = cx.state.links.remote.get(&session).cloned() {
        if link_of(cx, conn).is_some() {
            // No transitivity: a peer reaches only this runtime's own sessions.
            return Some(Reply::Now(Err(RpcError::new(-32002, "session not found"))));
        }
        return Some(forward(cx, conn, id, &link, method, params.clone()));
    }
    if method == "session/rebind" {
        let env = params.get("env").and_then(Value::as_str)?;
        let link = cx
            .state
            .links
            .peers
            .iter()
            .find(|(_, p)| p.env == env)
            .map(|(l, _)| l.clone())?;
        return Some(Reply::Now(start_move(cx, conn, id, &session, &link, params)));
    }
    None
}

fn forward(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    link: &str,
    method: &str,
    params: Value,
) -> Reply {
    if let Err(e) = principals::check(cx.state, conn, Scope::Read) {
        return Reply::Now(Err(e));
    }
    let Some(link_conn) = cx.state.links.conns.get(link).cloned() else {
        return Reply::Now(Err(RpcError::new(-32030, "environment offline")));
    };
    let call = client_call(cx, &link_conn, method, params);
    cx.state.links.pending.insert(
        call,
        Pending::Forward {
            conn: conn.to_string(),
            id: id.cloned(),
        },
    );
    Reply::Later
}

fn client_call(cx: &mut Cx, conn: &str, method: &str, params: Value) -> u64 {
    let call = cx.next_call();
    cx.state.pending_calls.insert(call, CallOwner::Links);
    cx.emit(Effect::ClientCall {
        conn: conn.to_string(),
        call,
        method: method.to_string(),
        params,
    });
    call
}

/// The stored form of a session, as `sessions::changed` persists it.
fn session_value(cx: &Cx, session: &str) -> Option<Value> {
    let s = sessions::session_json(cx.state, session)?;
    let info = cx.state.sessions.info.get(session)?;
    let agent = cx.state.sessions.by_id.get(session).map(|r| r.agent.clone());
    Some(json!({
        "session": s,
        "agent": agent,
        "core": { "options": info.options, "control": info.control,
                  "createdMs": info.created_ms, "updatedMs": info.updated_ms },
    }))
}

fn start_move(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    session: &str,
    link: &str,
    params: &Value,
) -> Result<Value, RpcError> {
    principals::check_session(cx.state, conn, Scope::Delegate, session)?;
    let Some(link_conn) = cx.state.links.conns.get(link).cloned() else {
        return Err(RpcError::new(-32030, "environment offline"));
    };
    let value = session_value(cx, session).ok_or_else(|| RpcError::new(-32002, "session not found"))?;
    let workspace = cx
        .state
        .sessions
        .info
        .get(session)
        .and_then(|i| cx.state.sessions.workspaces.get(&i.workspace))
        .map(|w| json!({ "id": w.id, "path": w.path, "created": sessions::timestamp(w.created_ms) }));
    let log = serde_json::to_string(&crate::items::newest(cx.state, session, usize::MAX))
        .unwrap_or_else(|_| "[]".into());
    let key = params
        .get("key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| cx.ids.next("mov"));
    let transfer = format!("{key}.log");
    // The harness session resumes on the other side, so the old binding stops first (E9).
    sessions::stop_session(cx, session);
    let chunks = split(&log, CHUNK);
    let n = chunks.len() as u64;
    for (index, data) in chunks.into_iter().enumerate() {
        let call = client_call(
            cx,
            &link_conn,
            "blob/put",
            json!({ "transfer": transfer, "index": index, "data": data, "last": index as u64 + 1 == n }),
        );
        cx.state.links.pending.insert(
            call,
            Pending::Chunk {
                session: session.to_string(),
            },
        );
    }
    let offer = json!({
        "session": session,
        "record": value,
        "workspace": workspace,
        "log": transfer,
        "queue": [],
        "requests": [],
        "turnKeys": [],
        "tasks": [],
    });
    let call = client_call(cx, &link_conn, "session/offer", offer);
    cx.state.links.pending.insert(
        call,
        Pending::Offer(Move {
            session: session.to_string(),
            link: link.to_string(),
            key,
            conn: conn.to_string(),
            id: id.cloned(),
            binding: None,
        }),
    );
    Ok(Value::Null)
}

/// Splits text into parts of at most `max` bytes on character boundaries; at least one part.
fn split(text: &str, max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    parts.push(rest.to_string());
    parts
}

fn blob_put(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let Some(link) = link_of(cx, conn) else {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" })));
    };
    let transfer = params["transfer"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "transfer is required"))?
        .to_string();
    let index = params["index"]
        .as_u64()
        .ok_or_else(|| RpcError::new(-32602, "index is required"))?;
    let data = params["data"].as_str().unwrap_or_default();
    if data.len() > CHUNK {
        return Err(RpcError::new(-32602, "a chunk is at most 1 MiB"));
    }
    let expires_ms = cx.now_ms + OFFER_TTL_MS;
    let blob = cx.state.links.blobs.entry(transfer).or_insert_with(|| Blob {
        link: link.clone(),
        expires_ms,
        ..Blob::default()
    });
    if blob.link != link {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "transfer" })));
    }
    // Chunks are keyed by index, so a resent chunk replaces itself.
    blob.parts.insert(index, data.to_string());
    if params["last"].as_bool() == Some(true) {
        blob.last = Some(index);
    }
    Ok(json!({}))
}

fn blob_text(cx: &mut Cx, transfer: &str) -> Option<String> {
    let blob = cx.state.links.blobs.get(transfer)?;
    let last = blob.last?;
    if (0..=last).any(|i| !blob.parts.contains_key(&i)) {
        return None;
    }
    let blob = cx.state.links.blobs.remove(transfer)?;
    Some(blob.parts.into_values().collect())
}

/// The target of a move: take the session and start its binding idle (E1).
fn offer(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let Some(link) = link_of(cx, conn) else {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" })));
    };
    principals::check(cx.state, conn, Scope::Delegate)?;
    let session = params["session"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "session is required"))?
        .to_string();
    if let Some(o) = cx.state.links.offers.get(&session) {
        // A repeated offer answers with the binding it already started.
        return Ok(json!({ "binding": o.binding }));
    }
    if cx.state.sessions.by_id.contains_key(&session) {
        return Err(RpcError::new(-32004, "wrong state").with_data(json!({ "state": "exists" })));
    }
    let record = params
        .get("record")
        .filter(|r| r.get("session").is_some())
        .ok_or_else(|| RpcError::new(-32602, "record is required"))?;
    if let Some(w) = params.get("workspace").filter(|w| w.is_object())
        && let Some(wid) = w["id"].as_str()
    {
        sessions::restore_workspace(cx.state, wid, w);
        cx.persist(json!({ "op": "put", "kind": "workspace", "id": wid, "value": w }));
    }
    sessions::restore(cx.state, &session, record);
    // Only clients of this runtime reach it now; the session is no longer the peer's.
    cx.state.links.remote.remove(&session);
    if let Some(transfer) = params["log"].as_str()
        && let Some(text) = blob_text(cx, transfer)
        && let Ok(items) = serde_json::from_str::<Vec<Value>>(&text)
    {
        for item in items {
            if let Value::Object(map) = item {
                let seq = cx.next_seq();
                crate::items::restore(cx.state, &session, map, seq);
            }
        }
    }
    let binding = sessions::start_binding(cx, &session, parrot_proto::session::BindingVia::New);
    cx.state.links.offers.insert(
        session.clone(),
        Offer {
            link,
            expires_ms: cx.now_ms + OFFER_TTL_MS,
            binding: binding.clone(),
        },
    );
    cx.emit(Effect::TimerAt {
        at_ms: cx.now_ms + OFFER_TTL_MS,
    });
    Ok(json!({ "binding": binding }))
}

/// The commit of a move: this runtime owns the session from now on (E9).
fn commit(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let Some(link) = link_of(cx, conn) else {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" })));
    };
    let key = params["key"].as_str().unwrap_or_default().to_string();
    if let Some(done) = cx.state.links.commits.get(&key) {
        return Ok(done.clone());
    }
    let session = params["session"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "session is required"))?
        .to_string();
    let Some(o) = cx.state.links.offers.get(&session).filter(|o| o.link == link) else {
        return Err(RpcError::new(-32004, "wrong state").with_data(json!({ "state": "no offer" })));
    };
    let result = json!({ "binding": o.binding });
    cx.state.links.offers.remove(&session);
    if !key.is_empty() {
        cx.state.links.commits.insert(key, result.clone());
    }
    sessions::changed(cx, &session);
    Ok(result)
}

/// Offers and blobs that outlived their 60 seconds: the session didn't move (E1).
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let expired: Vec<String> = cx
        .state
        .links
        .offers
        .iter()
        .filter(|(_, o)| o.expires_ms <= now)
        .map(|(s, _)| s.clone())
        .collect();
    for session in expired {
        cx.state.links.offers.remove(&session);
        sessions::stop_session(cx, &session);
        cx.state.sessions.by_id.remove(&session);
        cx.state.sessions.info.remove(&session);
        cx.persist(json!({ "op": "delete", "kind": "session", "id": session }));
    }
    cx.state.links.blobs.retain(|_, b| b.expires_ms > now);
}

/// A link connection opened: deliver what its outbox holds (E2).
pub fn connected(cx: &mut Cx, conn: &str, link: &str) {
    cx.state.links.conns.insert(link.to_string(), conn.to_string());
    for (method, params) in cx.state.links.outbox.remove(link).unwrap_or_default() {
        cx.emit(Effect::Notify {
            conn: conn.to_string(),
            method,
            params,
        });
    }
}

/// A link connection closed.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.links.conns.retain(|_, c| c != conn);
}

/// Send a notification to a peer, or keep it in the link's outbox until it connects (E2).
pub fn notify_peer(cx: &mut Cx, link: &str, method: &str, mut params: Value) {
    params["runtime"] = json!(cx.state.principals.runtime);
    match cx.state.links.conns.get(link).cloned() {
        Some(conn) => cx.emit(Effect::Notify {
            conn,
            method: method.to_string(),
            params,
        }),
        None => {
            let entry = (method.to_string(), params);
            cx.persist(
                json!({ "op": "put", "kind": "outbox", "id": format!("{link}/{}", cx.now_ms),
                "value": { "link": link, "method": entry.0, "params": entry.1 } }),
            );
            cx.state
                .links
                .outbox
                .entry(link.to_string())
                .or_default()
                .push(entry);
        }
    }
}

fn peer_said(cx: &mut Cx, conn: &str, params: &Value) {
    let Some(link) = link_of(cx, conn) else {
        return;
    };
    let info = PeerInfo {
        runtime: params["runtime"].as_str().unwrap_or_default().to_string(),
        env: params["env"].as_str().unwrap_or_default().to_string(),
    };
    cx.state.links.peers.insert(link, info);
}

/// A hub or peer reply to a call this module made.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(pending) = cx.state.links.pending.remove(&call) else {
        return;
    };
    match pending {
        Pending::Hub { conn, id } | Pending::Forward { conn, id } => {
            if let Some(id) = id {
                cx.reply(&conn, id, result);
            }
        }
        Pending::Chunk { .. } => {}
        Pending::Offer(mut m) => match result {
            Ok(v) => {
                m.binding = v.get("binding").cloned();
                let Some(link_conn) = cx.state.links.conns.get(&m.link).cloned() else {
                    fail_move(cx, &m, RpcError::new(-32040, "link failed"));
                    return;
                };
                let call = client_call(
                    cx,
                    &link_conn,
                    "session/commit",
                    json!({ "session": m.session, "key": m.key }),
                );
                cx.state.links.pending.insert(call, Pending::Commit(m));
            }
            Err(e) => fail_move(cx, &m, e),
        },
        Pending::Commit(m) => match result {
            Ok(v) => {
                let runtime = cx
                    .state
                    .links
                    .peers
                    .get(&m.link)
                    .map(|p| p.runtime.clone())
                    .unwrap_or_default();
                cx.state.links.moved.insert(m.session.clone(), runtime.clone());
                cx.persist(json!({ "op": "put", "kind": "moved", "id": m.session,
                    "value": { "runtime": runtime, "link": m.link } }));
                cx.publish(
                    Some(m.session.clone()),
                    json!({ "type": "moved", "session": m.session, "runtime": runtime }),
                );
                if let Some(id) = m.id {
                    let binding = v.get("binding").cloned().or(m.binding).unwrap_or(Value::Null);
                    cx.reply(&m.conn, id, Ok(json!({ "binding": binding, "runtime": runtime })));
                }
            }
            // A lost answer: ask again with the same key (E9).
            Err(e) if e.code == -32030 => {
                if let Some(link_conn) = cx.state.links.conns.get(&m.link).cloned() {
                    let call = client_call(
                        cx,
                        &link_conn,
                        "session/commit",
                        json!({ "session": m.session, "key": m.key }),
                    );
                    cx.state.links.pending.insert(call, Pending::Commit(m));
                } else {
                    fail_move(cx, &m, e);
                }
            }
            Err(e) => fail_move(cx, &m, e),
        },
    }
}

fn fail_move(cx: &mut Cx, m: &Move, e: RpcError) {
    // Nothing moved: the session starts again here.
    sessions::start_binding(cx, &m.session, parrot_proto::session::BindingVia::Resume);
    if let Some(id) = m.id.clone() {
        cx.reply(&m.conn, id, Err(e));
    }
}
