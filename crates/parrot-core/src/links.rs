//! Links to other runtimes in the core (spec 24, E1, E2, E9): link methods, link connections,
//! calls forwarded to sessions another runtime owns, moving sessions with `session/offer` and
//! `session/commit`, blob transfers and the per-link outbox.
//!
//! Link records, keys and carriers are I/O, so they live in `parrot-serve`'s link hub. The core
//! asks the hub for them with `Effect::Workspace` operations named `link.*` and answers the
//! client when the hub has.

use crate::{CallOwner, Cx, Effect, LinkOffer, OfferedSessions, Peer, Reply, principals, sessions};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// How long an offer waits for its commit before it expires and nothing moved (E1).
pub const OFFER_TTL_MS: u64 = 60_000;
/// How long a move waits for a running turn to end before it interrupts it (E9).
pub const TURN_WAIT_MS: u64 = 30_000;
/// How long a move waits for an interrupted turn to end before it stops the binding anyway.
pub const INTERRUPT_GRACE_MS: u64 = 5_000;
/// The largest `blob/put` chunk (spec 24.2).
pub const CHUNK: usize = 1024 * 1024;
/// The most disk a link may fill with its unfinished transfers. Chunks are written to disk as they
/// arrive, so an authenticated peer must not be able to grow them unbounded.
pub const LINK_DISK_MAX: u64 = 4096 * CHUNK as u64;
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
    /// The link each moved session went over.
    pub moved_via: BTreeMap<String, String>,
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
    /// The stored ids of each link's outbox entries, deleted once delivered.
    pub outbox_ids: BTreeMap<String, Vec<String>>,
    /// Sessions that moved here, with the link they came over.
    pub arrived: BTreeMap<String, String>,
    /// Moves waiting for the session's running turn to end, by session (E9).
    pub waiting: BTreeMap<String, Waiting>,
    /// The disk cap of each link's transfers; 0 means [`LINK_DISK_MAX`].
    pub disk_max: u64,
    /// Subscriptions of this runtime's clients to sessions a peer owns, by local subscription id.
    pub forwarded: BTreeMap<String, Forwarded>,
}

/// A client's subscription to a session a peer owns (spec 10.2, 24.5). The peer's updates are
/// passed on under the local id; after a drop it is made again with `replayFrom`.
#[derive(Debug, Clone)]
pub struct Forwarded {
    /// The client's connection.
    pub conn: String,
    pub link: String,
    /// The subscription id on the peer, for the current link connection.
    pub remote: String,
    /// The `subscribe` params, without `replayFrom`.
    pub params: Value,
    /// The peer's `seq` of the last update passed on.
    pub last: String,
}

impl Links {
    fn disk_cap(&self) -> u64 {
        if self.disk_max == 0 {
            LINK_DISK_MAX
        } else {
            self.disk_max
        }
    }
}

/// A move whose session was running a turn when it was asked for. It goes on once the session is
/// idle; after [`TURN_WAIT_MS`] the turn is interrupted (E9).
#[derive(Debug, Clone)]
pub struct Waiting {
    pub conn: String,
    pub id: Option<RequestId>,
    pub link: String,
    pub params: Value,
    pub deadline_ms: u64,
    pub interrupted: bool,
    /// Whether the queue was paused before the move paused it.
    pub paused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PeerInfo {
    pub runtime: String,
    pub env: String,
    /// The link's name, shown as the environment's.
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum Pending {
    /// A hub operation answering a client call.
    Hub { conn: String, id: Option<RequestId> },
    /// `link/update`: the link's live connection takes the new grant and offer once stored.
    LinkUpdate { conn: String, id: Option<RequestId> },
    /// A call forwarded to the session's owner.
    Forward { conn: String, id: Option<RequestId> },
    /// `binding/cancel` of a turn a move waited for; the move goes on once it is answered, or
    /// when the session is idle if that comes first. Stopping the old binding ends what is left.
    Cancel { session: String },
    /// `session/new` in a peer's environment: the session is the peer's from then on.
    NewRemote {
        conn: String,
        id: Option<RequestId>,
        link: String,
    },
    /// A client's `subscribe` to a session a peer owns.
    Subscribe {
        conn: String,
        id: Option<RequestId>,
        link: String,
        params: Value,
    },
    /// A forwarded subscription made again after the link reconnected.
    Resubscribe { local: String, replay: bool },
    /// A chunk written to disk; a failed write spoils its transfer.
    BlobWrite { transfer: String },
    /// Reading an offer's log from disk; the offer is answered after.
    Log {
        conn: String,
        id: Option<RequestId>,
        link: String,
        params: Value,
        unpacked: Option<Value>,
    },
    /// A chunk of a move's log.
    Chunk { session: String },
    /// The offer of a move; the commit follows its answer.
    Offer(Move),
    /// The commit of a move.
    Commit(Move),
    /// Packing the workspace of a move; the offer follows.
    Pack(Move),
    /// Unpacking the workspace of an offer; the offer is answered after.
    Unpack {
        conn: String,
        id: Option<RequestId>,
        link: String,
        params: Value,
    },
    /// Packing a workspace for `workspace/transfer`.
    TransferPack {
        conn: String,
        id: Option<RequestId>,
        link: String,
        workspace: String,
        key: String,
    },
    /// Unpacking a workspace that `workspace/receive` brought.
    Receive {
        conn: String,
        id: Option<RequestId>,
        link: String,
        workspace: String,
    },
    /// `token/revoke`: the token's connections close once the hub has revoked it.
    Revoke {
        conn: String,
        id: Option<RequestId>,
        token: String,
    },
}

#[derive(Debug, Clone)]
pub struct Move {
    pub session: String,
    pub link: String,
    pub key: String,
    pub conn: String,
    pub id: Option<RequestId>,
    pub binding: Option<Value>,
    /// The `session/offer` params, without the workspace transfer.
    pub offer: Value,
    /// Whether the queue was paused before the move paused it.
    pub paused: bool,
    /// Whether the workspace must arrive for the move to happen (a worktree).
    pub needs_workspace: bool,
}

/// A transfer arriving from a peer. Its chunks are on disk; the core keeps their sizes.
#[derive(Debug, Clone, Default)]
pub struct Blob {
    pub link: String,
    pub parts: BTreeMap<u64, u64>,
    pub last: Option<u64>,
    pub expires_ms: u64,
    /// A chunk couldn't be written, so the transfer can't be used.
    pub failed: bool,
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub link: String,
    pub expires_ms: u64,
    pub binding: Option<String>,
    /// Whether the queue was paused on the old owner; it is held until the commit.
    pub paused: bool,
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
            let (conn, id) = (conn.to_string(), id.cloned());
            let pending = match method {
                "link/update" => Pending::LinkUpdate { conn, id },
                _ => Pending::Hub { conn, id },
            };
            cx.state.links.pending.insert(call, pending);
            Reply::Later
        }
        "token/create" | "token/list" | "token/revoke" => {
            let mut params = params;
            if let Some(o) = params.as_object_mut() {
                // Only the core says who minted a token.
                o.remove("by");
            }
            if let Some(link) = link_of(cx, conn) {
                if let Err(e) = link_token(cx, conn, &link, method, &params) {
                    return Reply::Now(Err(e));
                }
                params["by"] = json!(link);
            } else if let Err(e) = owner_only(cx, conn) {
                return Reply::Now(Err(e));
            }
            let token = params.get("token").and_then(Value::as_str).map(str::to_string);
            let op = method.replacen('/', ".", 1);
            let call = cx.call_workspace(CallOwner::Links, &op, params);
            let (conn, id) = (conn.to_string(), id.cloned());
            let pending = match token.filter(|_| method == "token/revoke") {
                Some(token) => Pending::Revoke { conn, id, token },
                None => Pending::Hub { conn, id },
            };
            cx.state.links.pending.insert(call, pending);
            Reply::Later
        }
        "update" => {
            peer_update(cx, conn, &params);
            Reply::Now(Ok(json!({})))
        }
        "blob/put" => Reply::Now(blob_put(cx, conn, params)),
        "session/offer" => offer(cx, conn, id, params),
        "workspace/receive" => receive(cx, conn, id, params),
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
    // A peer's updates name the session they are about; they are passed on, not forwarded.
    if method == "update" {
        return None;
    }
    // A child that moved to the peer reports its task; the session itself redirects (E2).
    if method == "task/changed" {
        return Some(Reply::Now(task_changed(cx, conn, params)));
    }
    // A token limited to some sessions can't make roots (E8).
    if method == "session/new"
        && params.get("parent").is_none_or(Value::is_null)
        && matches!(
            cx.state.principals.conns.get(conn).map(|c| &c.peer),
            Some(Peer::Token {
                sessions: Some(_),
                ..
            })
        )
    {
        return Some(Reply::Now(Err(
            RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "sessions" }))
        )));
    }
    if let Some((_, offer)) = principals::link_offer(cx.state, conn) {
        let offer = offer.clone();
        if let Err(e) = offered(&offer, method) {
            return Some(Reply::Now(Err(e)));
        }
    }
    if method == "subscribe"
        && let Some(target) = params.get("target")
        && let Some(s) = target
            .get("session")
            .or(target.get("tree"))
            .and_then(Value::as_str)
        && let Some(link) = cx.state.links.remote.get(s).cloned()
    {
        if link_of(cx, conn).is_some() {
            return Some(Reply::Now(Err(RpcError::new(-32002, "session not found"))));
        }
        return Some(forward_subscribe(cx, conn, id, &link, params.clone()));
    }
    if method == "unsubscribe"
        && let Some(sub) = params.get("subscription").and_then(Value::as_str)
        && cx.state.links.forwarded.get(sub).is_some_and(|f| f.conn == conn)
    {
        if let Some(f) = cx.state.links.forwarded.remove(sub) {
            unsubscribe_peer(cx, &f);
        }
        return Some(Reply::Now(Ok(json!({}))));
    }
    if method == "session/new"
        && let Some(env) = params.get("env").and_then(Value::as_str)
        && env != cx.state.principals.env
    {
        return Some(new_remote(cx, conn, id, env, params.clone()));
    }
    if method == "workspace/transfer" {
        return Some(match transfer(cx, conn, id, params) {
            Ok(()) => Reply::Later,
            Err(e) => Reply::Now(Err(e)),
        });
    }
    let session = params.get("session").and_then(Value::as_str)?.to_string();
    if let Some(runtime) = cx.state.links.moved.get(&session) {
        if link_of(cx, conn).is_some() && !principals::sees(cx.state, conn, &session) {
            return Some(Reply::Now(Err(RpcError::new(-32002, "session not found"))));
        }
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
        return Some(match start_move(cx, conn, id, &session, &link, params) {
            Ok(Value::Null) => Reply::Later,
            other => Reply::Now(other),
        });
    }
    None
}

/// What a link's offer allows before any session is looked at (spec 24.1). Bringing something
/// into this environment needs the environment offered; making or receiving sessions also needs
/// sessions offered, or the peer could never reach what it made; tokens are only for offered
/// sessions.
fn offered(offer: &LinkOffer, method: &str) -> Result<(), RpcError> {
    let refuse = || Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "offer" })));
    let into_env = matches!(
        method,
        "session/new"
            | "session/offer"
            | "session/commit"
            | "workspace/receive"
            | "workspace/create"
            | "blob/put"
            | "agent/refresh"
    );
    if into_env && !offer.env {
        return refuse();
    }
    let needs_sessions =
        matches!(method, "session/new" | "session/offer" | "session/commit") || method.starts_with("token/");
    if needs_sessions && offer.sessions == OfferedSessions::None {
        return refuse();
    }
    Ok(())
}

/// `session/new` in a linked environment: the peer creates the session (spec 24.5).
fn new_remote(cx: &mut Cx, conn: &str, id: Option<&RequestId>, env: &str, params: Value) -> Reply {
    if let Err(e) = principals::check(cx.state, conn, Scope::Manage) {
        return Reply::Now(Err(e));
    }
    // No transitivity: a peer creates sessions only in this runtime's own environment.
    let link = (link_of(cx, conn).is_none())
        .then(|| {
            cx.state
                .links
                .peers
                .iter()
                .find(|(_, p)| p.env == env)
                .map(|(l, _)| l.clone())
        })
        .flatten();
    let Some(link) = link else {
        return Reply::Now(Err(RpcError::new(
            -32002,
            format!("environment not found: {env}"),
        )));
    };
    // A child on another runtime needs a tree across runtimes, which this runtime doesn't make.
    if params.get("parent").is_some_and(|p| !p.is_null()) {
        return Reply::Now(Err(RpcError::new(-32001, "a child in a linked environment")
            .with_data(json!({ "feature": "tree.remote" }))));
    }
    let Some(link_conn) = cx.state.links.conns.get(&link).cloned() else {
        return Reply::Now(Err(RpcError::new(-32030, "environment offline")));
    };
    let call = client_call(cx, &link_conn, "session/new", params);
    cx.state.links.pending.insert(
        call,
        Pending::NewRemote {
            conn: conn.to_string(),
            id: id.cloned(),
            link,
        },
    );
    Reply::Later
}

fn forward_subscribe(cx: &mut Cx, conn: &str, id: Option<&RequestId>, link: &str, params: Value) -> Reply {
    if let Err(e) = principals::check(cx.state, conn, Scope::Read) {
        return Reply::Now(Err(e));
    }
    let Some(link_conn) = cx.state.links.conns.get(link).cloned() else {
        return Reply::Now(Err(RpcError::new(-32030, "environment offline")));
    };
    let call = client_call(cx, &link_conn, "subscribe", params.clone());
    cx.state.links.pending.insert(
        call,
        Pending::Subscribe {
            conn: conn.to_string(),
            id: id.cloned(),
            link: link.to_string(),
            params,
        },
    );
    Reply::Later
}

/// Keeps a subscription the peer made for this runtime, and gives the client a local id for it.
fn adopt_subscription(cx: &mut Cx, conn: &str, link: &str, params: Value, result: &mut Value) {
    let Some(remote) = result["subscription"].as_str().map(str::to_string) else {
        return;
    };
    let local = cx.ids.next("sub");
    let mut params = params;
    if let Some(o) = params.as_object_mut() {
        o.remove("replayFrom");
    }
    let last = match &result["head"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "0".into(),
    };
    cx.state.links.forwarded.insert(
        local.clone(),
        Forwarded {
            conn: conn.to_string(),
            link: link.to_string(),
            remote,
            params,
            last,
        },
    );
    result["subscription"] = json!(local);
}

fn unsubscribe_peer(cx: &mut Cx, f: &Forwarded) {
    if let Some(link_conn) = cx.state.links.conns.get(&f.link).cloned() {
        client_call(cx, &link_conn, "unsubscribe", json!({ "subscription": f.remote }));
    }
}

/// An `update` from a peer: pass it on to the client whose forwarded subscription it is for.
fn peer_update(cx: &mut Cx, conn: &str, params: &Value) {
    let Some(link) = link_of(cx, conn) else { return };
    let Some(remote) = params["subscription"].as_str() else {
        return;
    };
    let Some((local, f)) = cx
        .state
        .links
        .forwarded
        .iter_mut()
        .find(|(_, f)| f.link == link && f.remote == remote)
    else {
        return;
    };
    let seq = match &params["seq"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if !seq.is_empty() {
        f.last = seq;
    }
    let mut out = params.clone();
    out["subscription"] = json!(local);
    let to = f.conn.clone();
    cx.emit(Effect::Notify {
        conn: to,
        method: "update".into(),
        params: out,
    });
}

/// The link reconnected: make each forwarded subscription again from where it left off.
fn resubscribe(cx: &mut Cx, link: &str, link_conn: &str) {
    let again: Vec<(String, Value)> = cx
        .state
        .links
        .forwarded
        .iter()
        .filter(|(_, f)| f.link == link)
        .map(|(l, f)| {
            let mut p = f.params.clone();
            p["replayFrom"] = json!(f.last);
            (l.clone(), p)
        })
        .collect();
    for (local, params) in again {
        let call = client_call(cx, link_conn, "subscribe", params);
        cx.state
            .links
            .pending
            .insert(call, Pending::Resubscribe { local, replay: true });
    }
}

/// The JSON of a linked environment, online while its link is connected (spec 7, 24.5).
fn peer_env(cx: &Cx, link: &str, p: &PeerInfo) -> Value {
    let online = cx.state.links.conns.contains_key(link);
    json!({
        "id": p.env,
        "name": if p.name.is_empty() { link } else { p.name.as_str() },
        "runtime": p.runtime,
        "status": if online { "online" } else { "offline" },
    })
}

/// The linked environments a connection sees: none for a link's peer (no transitivity).
pub fn peer_envs(cx: &Cx, conn: &str) -> Vec<Value> {
    if link_of(cx, conn).is_some() {
        return Vec::new();
    }
    cx.state
        .links
        .peers
        .iter()
        .filter(|(_, p)| !p.env.is_empty())
        .map(|(l, p)| peer_env(cx, l, p))
        .collect()
}

fn publish_env(cx: &mut Cx, link: &str) {
    if let Some(p) = cx.state.links.peers.get(link).cloned()
        && !p.env.is_empty()
    {
        let env = peer_env(cx, link, &p);
        cx.publish(None, json!({ "type": "env", "env": env }));
    }
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
    if !cx.state.links.conns.contains_key(link) {
        return Err(RpcError::new(-32030, "environment offline"));
    }
    if cx.state.links.waiting.contains_key(session) || crate::seeds::rebinding(cx.state, session) {
        return Err(
            RpcError::new(-32004, "a rebind is in progress").with_data(json!({ "state": "rebinding" }))
        );
    }
    if session_value(cx, session).is_none() {
        return Err(RpcError::new(-32002, "session not found"));
    }
    // Nothing is dispatched here from now on (E9), even a prompt queued while the move waits; a
    // failed move puts this back.
    let q = cx.state.queue.by_session.entry(session.to_string()).or_default();
    let paused = std::mem::replace(&mut q.paused, true);
    let interrupt = params.get("interrupt").and_then(Value::as_bool) == Some(true);
    let w = Waiting {
        conn: conn.to_string(),
        id: id.cloned(),
        link: link.to_string(),
        params: params.clone(),
        deadline_ms: cx.now_ms + TURN_WAIT_MS,
        interrupted: interrupt,
        paused,
    };
    if !turn_running(cx, session) {
        continue_move(cx, session, w);
        return Ok(Value::Null);
    }
    // The running turn ends first: the move waits up to 30 seconds, then interrupts it (E9).
    let mut w = w;
    if interrupt {
        w.deadline_ms = cx.now_ms + INTERRUPT_GRACE_MS;
        cancel_turn(cx, session);
    }
    cx.emit(Effect::TimerAt { at_ms: w.deadline_ms });
    cx.state.links.waiting.insert(session.to_string(), w);
    Ok(Value::Null)
}

/// Whether the session's turn is at work. A turn that waits on a request isn't: its open
/// requests travel with the session and are answered on the new owner (E1).
fn turn_running(cx: &Cx, session: &str) -> bool {
    sessions::active(cx.state, session)
        && cx
            .state
            .sessions
            .info
            .get(session)
            .is_some_and(|i| i.state == parrot_proto::state::State::Running)
}

fn cancel_turn(cx: &mut Cx, session: &str) {
    if let Some(b) = crate::directory::binding_of(cx.state, session) {
        let call = cx.call_driver(CallOwner::Links, &b, "binding/cancel", json!({ "binding": b }));
        cx.state.links.pending.insert(
            call,
            Pending::Cancel {
                session: session.to_string(),
            },
        );
    }
}

/// A session went idle: a move that waited for its turn goes on.
pub fn on_idle(cx: &mut Cx, session: &str) {
    if let Some(w) = cx.state.links.waiting.remove(session) {
        continue_move(cx, session, w);
    }
}

/// The session's turn has ended, or none ran: snapshot it and offer it to the target (E1).
fn continue_move(cx: &mut Cx, session: &str, w: Waiting) {
    let fail = |cx: &mut Cx, e: RpcError| {
        if let Some(q) = cx.state.queue.by_session.get_mut(session) {
            q.paused = w.paused;
        }
        if let Some(id) = w.id.clone() {
            cx.reply(&w.conn, id, Err(e));
        }
    };
    if !cx.state.links.conns.contains_key(&w.link) {
        return fail(cx, RpcError::new(-32030, "environment offline"));
    }
    let Some(value) = session_value(cx, session) else {
        return fail(cx, RpcError::new(-32002, "session not found"));
    };
    let (conn, id, link, params, paused) = (
        w.conn.as_str(),
        w.id.as_ref(),
        w.link.as_str(),
        &w.params,
        w.paused,
    );
    let wid = cx
        .state
        .sessions
        .info
        .get(session)
        .map(|i| i.workspace.clone())
        .unwrap_or_default();
    let workspace = crate::tree::workspace_json(cx.state, &wid);
    let log = serde_json::to_string(&crate::items::newest(cx.state, session, usize::MAX))
        .unwrap_or_else(|_| "[]".into());
    let key = params
        .get("key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| cx.ids.next("mov"));
    // What runs next travels with the session (E1): its queue, open requests, turn keys and task.
    let mut queue = stored_queue(cx, session);
    queue["paused"] = json!(paused);
    let requests: Vec<Value> = cx
        .state
        .requests
        .by_id
        .values()
        .filter(|r| r.request.session == session)
        .map(|r| json!({ "request": r.request, "binding": r.binding, "key": r.key }))
        .collect();
    let turn_keys = crate::directory::binding_of(cx.state, session)
        .and_then(|b| cx.state.bindings.info.get(&b))
        .map(|b| json!(b.turns))
        .unwrap_or_else(|| json!({}));
    let tasks: Vec<Value> = cx
        .state
        .tree
        .nodes
        .get(session)
        .map(|n| json!({ "session": session, "node": n }))
        .into_iter()
        .collect();
    // The harness session resumes on the other side, so the old binding stops first (E9).
    sessions::stop_session(cx, session);
    let offer = json!({
        "session": session,
        "record": value,
        "workspace": workspace,
        "queue": queue,
        "requests": requests,
        "turnKeys": turn_keys,
        "tasks": tasks,
    });
    let needs_workspace = crate::tree::is_worktree(cx.state, &wid);
    let m = Move {
        session: session.to_string(),
        link: link.to_string(),
        key,
        conn: conn.to_string(),
        id: id.cloned(),
        binding: None,
        offer,
        paused,
        needs_workspace,
    };
    let path = workspace
        .as_ref()
        .and_then(|w| w["path"].as_str().map(str::to_string));
    match path {
        Some(path) => {
            let branch = workspace.as_ref().and_then(|w| w.get("branch").cloned());
            let call = cx.call_workspace(
                CallOwner::Links,
                "transfer.pack",
                json!({ "path": path, "branch": branch, "include": params.get("include") }),
            );
            cx.state.links.pending.insert(call, Pending::Pack(m));
        }
        None => send_offer(cx, m, None, log),
    }
}

/// The stored form of a session's queue, with each entry's driver params.
fn stored_queue(cx: &Cx, session: &str) -> Value {
    let mut q = crate::queue::queue_state(cx.state, session);
    if let (Some(sq), Some(entries)) = (
        cx.state.queue.by_session.get(session),
        q.get_mut("entries").and_then(Value::as_array_mut),
    ) {
        for (v, e) in entries.iter_mut().zip(&sq.entries) {
            v["params"] = e.params.clone();
        }
    }
    q
}

/// Sends `text` as `blob/put` chunks of at most 1 MiB (spec 24.2, E9).
fn send_blob(cx: &mut Cx, link_conn: &str, transfer: &str, text: &str, session: &str) {
    let chunks = split(text, CHUNK);
    let n = chunks.len() as u64;
    for (index, data) in chunks.into_iter().enumerate() {
        let call = client_call(
            cx,
            link_conn,
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
}

/// Sends the offer of a move, after its workspace was packed when it has one.
fn send_offer(cx: &mut Cx, m: Move, packed: Option<Result<Value, RpcError>>, log: String) {
    let Some(link_conn) = cx.state.links.conns.get(&m.link).cloned() else {
        fail_move(cx, &m, RpcError::new(-32030, "environment offline"));
        return;
    };
    let mut offer = m.offer.clone();
    match packed {
        Some(Ok(p)) => {
            let transfer = format!("{}.ws", m.key);
            send_blob(
                cx,
                &link_conn,
                &transfer,
                p["data"].as_str().unwrap_or_default(),
                &m.session,
            );
            offer["workspace"]["transfer"] = json!(transfer);
            offer["workspace"]["branch"] = p["branch"].clone();
        }
        // A worktree has to arrive; a plain directory that isn't a repository stays where it is,
        // for runtimes that share it.
        Some(Err(e)) if m.needs_workspace => {
            fail_move(cx, &m, e);
            return;
        }
        Some(Err(_)) | None => {}
    }
    let transfer = format!("{}.log", m.key);
    send_blob(cx, &link_conn, &transfer, &log, &m.session);
    offer["log"] = json!(transfer);
    let call = client_call(cx, &link_conn, "session/offer", offer);
    cx.state.links.pending.insert(call, Pending::Offer(m));
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
        .filter(|t| !t.is_empty() && t.len() <= 128)
        .ok_or_else(|| RpcError::new(-32602, "transfer is required, at most 128 bytes"))?
        .to_string();
    let index = params["index"]
        .as_u64()
        .ok_or_else(|| RpcError::new(-32602, "index is required"))?;
    let data = params["data"].as_str().unwrap_or_default();
    if data.len() > CHUNK {
        return Err(RpcError::new(-32602, "a chunk is at most 1 MiB"));
    }
    if let Some(b) = cx.state.links.blobs.get(&transfer)
        && b.link != link
    {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "transfer" })));
    }
    // A resent chunk replaces itself, so it doesn't count twice.
    let held: u64 = cx
        .state
        .links
        .blobs
        .iter()
        .filter(|(_, b)| b.link == link)
        .flat_map(|(t, b)| b.parts.iter().map(move |(i, n)| (t, *i, *n)))
        .filter(|(t, i, _)| !(**t == transfer && *i == index))
        .map(|(_, _, n)| n)
        .sum();
    let cap = cx.state.links.disk_cap();
    if held + data.len() as u64 > cap {
        return Err(
            RpcError::new(-32004, "transfer too large").with_data(json!({ "reason": "size", "limit": cap }))
        );
    }
    let expires_ms = cx.now_ms + OFFER_TTL_MS;
    let blob = cx
        .state
        .links
        .blobs
        .entry(transfer.clone())
        .or_insert_with(|| Blob {
            link: link.clone(),
            expires_ms,
            ..Blob::default()
        });
    // A transfer lives as long as chunks keep arriving, so a slow link can finish a large one.
    blob.expires_ms = expires_ms;
    blob.parts.insert(index, data.len() as u64);
    if params["last"].as_bool() == Some(true) {
        blob.last = Some(index);
    }
    // The chunk goes to disk; the host writes chunks and reads transfers in the order sent.
    let call = cx.call_workspace(
        CallOwner::Links,
        "blob.put",
        json!({ "link": link, "transfer": transfer, "index": index, "data": data }),
    );
    cx.state
        .links
        .pending
        .insert(call, Pending::BlobWrite { transfer });
    Ok(json!({}))
}

/// A complete transfer of `link`, handed over for reading: `{ link, transfer, parts }`, which
/// `blob.take` and `transfer.unpack` read from disk.
fn blob_ref(cx: &mut Cx, transfer: &str, link: &str) -> Option<Value> {
    let blob = cx.state.links.blobs.get(transfer)?;
    // A transfer is read only by the link that sent it.
    if blob.link != link || blob.failed {
        return None;
    }
    let last = blob.last?;
    if (0..=last).any(|i| !blob.parts.contains_key(&i)) {
        return None;
    }
    cx.state.links.blobs.remove(transfer);
    Some(json!({ "link": link, "transfer": transfer, "parts": last + 1 }))
}

/// The target of a move: take the session and start its binding idle (E1). A workspace that
/// travels is unpacked first, and the offer is answered once it is in place.
fn offer(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    match check_offer(cx, conn, &params) {
        Err(e) => Reply::Now(Err(e)),
        Ok(Some(done)) => Reply::Now(Ok(done)),
        Ok(None) => {
            let link = link_of(cx, conn).unwrap_or_default();
            let ws = params.get("workspace").filter(|w| w.is_object());
            let transfer = ws.and_then(|w| w["transfer"].as_str()).map(str::to_string);
            let Some(transfer) = transfer else {
                return read_log(cx, conn, id, &link, params, None);
            };
            let wid = ws.and_then(|w| w["id"].as_str()).unwrap_or_default().to_string();
            if let Err(e) = principals::check(cx.state, conn, Scope::Workspace) {
                return Reply::Now(Err(e));
            }
            if cx.state.sessions.workspaces.contains_key(&wid) {
                return Reply::Now(Err(
                    RpcError::new(-32004, "wrong state").with_data(json!({ "state": "workspace exists" }))
                ));
            }
            let Some(blob) = blob_ref(cx, &transfer, &link) else {
                return Reply::Now(Err(
                    RpcError::new(-32004, "wrong state").with_data(json!({ "state": "transfer incomplete" }))
                ));
            };
            let call = cx.call_workspace(
                CallOwner::Links,
                "transfer.unpack",
                json!({ "workspace": wid, "blob": blob }),
            );
            cx.state.links.pending.insert(
                call,
                Pending::Unpack {
                    conn: conn.to_string(),
                    id: id.cloned(),
                    link,
                    params,
                },
            );
            Reply::Later
        }
    }
}

/// Reads the offer's log from disk, then puts the session in place. `unpacked` is where its
/// workspace arrived, when it travelled.
fn read_log(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    link: &str,
    params: Value,
    unpacked: Option<Value>,
) -> Reply {
    let blob = params["log"]
        .as_str()
        .map(str::to_string)
        .and_then(|t| blob_ref(cx, &t, link));
    let Some(blob) = blob else {
        return Reply::Now(finish_offer(cx, link, &params, unpacked.as_ref(), None));
    };
    let call = cx.call_workspace(CallOwner::Links, "blob.take", json!({ "blob": blob }));
    cx.state.links.pending.insert(
        call,
        Pending::Log {
            conn: conn.to_string(),
            id: id.cloned(),
            link: link.to_string(),
            params,
            unpacked,
        },
    );
    Reply::Later
}

/// Checks an offer; `Some` answers a repeated one with what it already did.
fn check_offer(cx: &mut Cx, conn: &str, params: &Value) -> Result<Option<Value>, RpcError> {
    if link_of(cx, conn).is_none() {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" })));
    }
    principals::check(cx.state, conn, Scope::Delegate)?;
    let session = params["session"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "session is required"))?;
    if let Some(o) = cx.state.links.offers.get(session) {
        // A repeated offer answers with the binding it already started.
        return Ok(Some(json!({ "binding": o.binding })));
    }
    if cx.state.sessions.by_id.contains_key(session) {
        return Err(RpcError::new(-32004, "wrong state").with_data(json!({ "state": "exists" })));
    }
    if params.get("record").and_then(|r| r.get("session")).is_none() {
        return Err(RpcError::new(-32602, "record is required"));
    }
    Ok(None)
}

/// Puts an offered session in place and starts its binding idle. `unpacked` is where its
/// workspace arrived, when it travelled.
fn finish_offer(
    cx: &mut Cx,
    link: &str,
    params: &Value,
    unpacked: Option<&Value>,
    log: Option<&str>,
) -> Result<Value, RpcError> {
    let session = params["session"].as_str().unwrap_or_default().to_string();
    // Checked again: another offer may have taken the id while the workspace was unpacked.
    if cx.state.sessions.by_id.contains_key(&session) || cx.state.links.offers.contains_key(&session) {
        return Err(RpcError::new(-32004, "wrong state").with_data(json!({ "state": "exists" })));
    }
    let mut record = params["record"].clone();
    if let Some(w) = params.get("workspace").filter(|w| w.is_object())
        && let Some(wid) = w["id"].as_str()
    {
        let mut w = w.clone();
        if let Some(u) = unpacked {
            let old = w["path"].as_str().unwrap_or_default().to_string();
            let new = u["path"].as_str().unwrap_or_default().to_string();
            // The cwd keeps its place inside the workspace.
            let cwd = record["session"]["cwd"].as_str().unwrap_or_default();
            let rest = cwd
                .strip_prefix(old.as_str())
                .filter(|r| r.is_empty() || r.starts_with('/'));
            record["session"]["cwd"] = json!(format!("{new}{}", rest.unwrap_or("")));
            // It arrives as a repository of its own: no worktree of a clone here.
            let mut arrived = json!({ "id": wid, "env": cx.state.principals.env, "kind": "dir",
                "path": new, "sessions": [], "created": w["created"] });
            if let Some(b) = u["branch"].as_str() {
                arrived["branch"] = json!(b);
            }
            w = arrived;
        }
        sessions::restore_workspace(cx.state, wid, &w);
        crate::tree::restore_workspace(cx.state, wid, &w);
        cx.persist(json!({ "op": "put", "kind": "workspace", "id": wid, "value": w }));
    }
    sessions::restore(cx.state, &session, &record);
    // Only clients of this runtime reach it now; the session is no longer the peer's.
    cx.state.links.remote.remove(&session);
    if let Some(text) = log
        && let Ok(items) = serde_json::from_str::<Vec<Value>>(text)
    {
        for item in items {
            if let Value::Object(map) = item {
                let seq = cx.next_seq();
                crate::items::restore(cx.state, &session, map, seq);
            }
        }
    }
    for t in params["tasks"].as_array().into_iter().flatten() {
        if t["session"] == session.as_str() {
            crate::tree::restore(cx.state, &session, &t["node"]);
        }
    }
    crate::tree::ensure(cx.state, &session);
    crate::tree::persist(cx, &session);
    let queue = params.get("queue").cloned().unwrap_or_else(|| json!({}));
    let paused = queue["paused"].as_bool().unwrap_or(false);
    crate::queue::restore(cx.state, &session, &queue);
    // The old owner owns the session until the commit, so nothing is dispatched before it.
    if let Some(q) = cx.state.queue.by_session.get_mut(&session) {
        q.paused = true;
        q.held = true;
    }
    crate::queue::publish_queue(cx, &session);
    for r in params["requests"].as_array().into_iter().flatten() {
        let Some(rid) = r.pointer("/request/id").and_then(Value::as_str) else {
            continue;
        };
        // A request is the session's own, never another one's that happens to share the id.
        if r.pointer("/request/session").and_then(Value::as_str) != Some(session.as_str())
            || cx.state.requests.by_id.contains_key(rid)
        {
            continue;
        }
        crate::requests::restore(cx, rid, r);
        crate::requests::publish_request(cx, rid);
    }
    let binding = sessions::start_binding(cx, &session, parrot_proto::session::BindingVia::New);
    if let (Some(b), Some(keys)) = (&binding, params["turnKeys"].as_object())
        && let Some(info) = cx.state.bindings.info.get_mut(b)
    {
        for (k, v) in keys {
            if let Some(v) = v.as_str() {
                info.turns.insert(k.clone(), v.to_string());
            }
        }
    }
    cx.state.links.offers.insert(
        session.clone(),
        Offer {
            link: link.to_string(),
            expires_ms: cx.now_ms + OFFER_TTL_MS,
            binding: binding.clone(),
            paused,
        },
    );
    cx.emit(Effect::TimerAt {
        at_ms: cx.now_ms + OFFER_TTL_MS,
    });
    Ok(json!({ "binding": binding }))
}

/// `workspace/transfer` (spec 8): pack a workspace of this runtime and send it to a linked
/// environment, which makes it a workspace of its own.
fn transfer(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: &Value) -> Result<(), RpcError> {
    principals::check(cx.state, conn, Scope::Workspace)?;
    let workspace = params["workspace"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "workspace is required"))?
        .to_string();
    let to = params["to"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "to is required"))?;
    if link_of(cx, conn).is_some() {
        // No transitivity: a peer can't send this runtime's workspaces on to a third.
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" })));
    }
    let Some(w) = crate::tree::workspace_json(cx.state, &workspace)
        .filter(|_| crate::tree::visible_workspace(cx.state, conn, &workspace))
    else {
        return Err(RpcError::new(-32002, format!("workspace not found: {workspace}")));
    };
    let link = cx
        .state
        .links
        .peers
        .iter()
        .find(|(_, p)| p.env == to)
        .map(|(l, _)| l.clone())
        .ok_or_else(|| RpcError::new(-32002, format!("environment not found: {to}")))?;
    if !cx.state.links.conns.contains_key(&link) {
        return Err(RpcError::new(-32030, "environment offline"));
    }
    let key = params["key"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| cx.ids.next("xfr"));
    let call = cx.call_workspace(
        CallOwner::Links,
        "transfer.pack",
        json!({ "path": w["path"], "branch": w.get("branch"), "include": params.get("include") }),
    );
    cx.state.links.pending.insert(
        call,
        Pending::TransferPack {
            conn: conn.to_string(),
            id: id.cloned(),
            link,
            workspace,
            key,
        },
    );
    Ok(())
}

/// `workspace/receive`, from a peer's `workspace/transfer`: unpack what its blob carried.
fn receive(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let Some(link) = link_of(cx, conn) else {
        return Reply::Now(Err(
            RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" }))
        ));
    };
    if let Err(e) = principals::check(cx.state, conn, Scope::Workspace) {
        return Reply::Now(Err(e));
    }
    let (Some(workspace), Some(transfer)) = (params["workspace"].as_str(), params["transfer"].as_str())
    else {
        return Reply::Now(Err(RpcError::new(-32602, "workspace and transfer are required")));
    };
    if cx.state.sessions.workspaces.contains_key(workspace) {
        return Reply::Now(Err(
            RpcError::new(-32004, "wrong state").with_data(json!({ "state": "workspace exists" }))
        ));
    }
    let Some(blob) = blob_ref(cx, transfer, &link) else {
        return Reply::Now(Err(
            RpcError::new(-32004, "wrong state").with_data(json!({ "state": "transfer incomplete" }))
        ));
    };
    let call = cx.call_workspace(
        CallOwner::Links,
        "transfer.unpack",
        json!({ "workspace": workspace, "blob": blob }),
    );
    cx.state.links.pending.insert(
        call,
        Pending::Receive {
            conn: conn.to_string(),
            id: id.cloned(),
            link,
            workspace: workspace.to_string(),
        },
    );
    Reply::Later
}

/// Token methods over a link (E8): a peer mints only tokens within what it holds itself, for
/// sessions it created here or moved here, and sees and revokes only those tokens.
fn link_token(cx: &Cx, conn: &str, link: &str, method: &str, params: &Value) -> Result<(), RpcError> {
    principals::check(cx.state, conn, Scope::Read)?;
    if method != "token/create" {
        return Ok(());
    }
    let forbidden = |reason: &str| RpcError::new(-32003, "forbidden").with_data(json!({ "reason": reason }));
    let held = match cx.state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(peer @ Peer::Link { .. }) => principals::scopes_of(peer),
        _ => Vec::new(),
    };
    let asked: Vec<Scope> = params["scopes"]
        .as_array()
        .ok_or_else(|| RpcError::new(-32602, "scopes is required"))?
        .iter()
        .map(|s| serde_json::from_value::<Scope>(s.clone()))
        .collect::<Result<_, _>>()
        .map_err(|_| RpcError::new(-32602, "unknown scope"))?;
    if asked.iter().any(|s| !held.contains(s)) {
        return Err(forbidden("scope"));
    }
    if !params["provides"].is_null() && !held.contains(&Scope::Functions) {
        return Err(forbidden("scope"));
    }
    let sessions = params["sessions"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| forbidden("sessions"))?;
    for s in sessions {
        let s = s.as_str().ok_or_else(|| RpcError::new(-32602, "sessions"))?;
        // Only sessions the link made or moved here, and only while the link offers them.
        if !principals::linked_to(cx.state, s, link) || !principals::sees(cx.state, conn, s) {
            return Err(forbidden("sessions"));
        }
    }
    Ok(())
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
    let paused = o.paused;
    cx.state.links.offers.remove(&session);
    if !key.is_empty() {
        cx.state.links.commits.insert(key, result.clone());
    }
    cx.state.links.arrived.insert(session.clone(), link.clone());
    cx.persist(json!({ "op": "put", "kind": "arrived", "id": session, "value": { "link": link } }));
    sessions::changed(cx, &session);
    // This runtime owns the session now: its queue runs as it did on the old owner.
    if let Some(q) = cx.state.queue.by_session.get_mut(&session) {
        q.held = false;
        q.paused = paused;
    }
    crate::queue::publish_queue(cx, &session);
    crate::queue::dispatch(cx, &session);
    Ok(result)
}

/// Offers and blobs that outlived their 60 seconds: the session didn't move (E1). Moves that
/// waited 30 seconds for a turn interrupt it (E9).
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let due: Vec<(String, bool)> = cx
        .state
        .links
        .waiting
        .iter()
        .filter(|(_, w)| w.deadline_ms <= now)
        .map(|(s, w)| (s.clone(), w.interrupted))
        .collect();
    for (session, interrupted) in due {
        if interrupted {
            // The interrupt didn't end the turn in time: stopping the old binding will.
            on_idle(cx, &session);
            continue;
        }
        if let Some(w) = cx.state.links.waiting.get_mut(&session) {
            w.interrupted = true;
            w.deadline_ms = now + INTERRUPT_GRACE_MS;
        }
        cx.emit(Effect::TimerAt {
            at_ms: now + INTERRUPT_GRACE_MS,
        });
        cancel_turn(cx, &session);
    }
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
        cx.state.queue.by_session.remove(&session);
        cx.persist(json!({ "op": "delete", "kind": "queue", "id": session }));
        let requests: Vec<String> = cx
            .state
            .requests
            .by_id
            .iter()
            .filter(|(_, r)| r.request.session == session)
            .map(|(id, _)| id.clone())
            .collect();
        for id in requests {
            cx.state.requests.by_id.remove(&id);
            cx.persist(json!({ "op": "delete", "kind": "request", "id": id }));
        }
        sessions::stop_session(cx, &session);
        cx.state.sessions.by_id.remove(&session);
        cx.state.sessions.info.remove(&session);
        cx.persist(json!({ "op": "delete", "kind": "session", "id": session }));
    }
    let expired: Vec<(String, String)> = cx
        .state
        .links
        .blobs
        .iter()
        .filter(|(_, b)| b.expires_ms <= now)
        .map(|(t, b)| (t.clone(), b.link.clone()))
        .collect();
    for (transfer, link) in expired {
        cx.state.links.blobs.remove(&transfer);
        cx.call_workspace(
            CallOwner::Links,
            "blob.drop",
            json!({ "link": link, "transfer": transfer }),
        );
    }
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
    for id in cx.state.links.outbox_ids.remove(link).unwrap_or_default() {
        cx.persist(json!({ "op": "delete", "kind": "outbox", "id": id }));
    }
    resubscribe(cx, link, conn);
    publish_env(cx, link);
}

/// Puts back what links persisted, after a restart (spec 34): redirects of moved sessions and
/// undelivered outbox entries.
pub fn restore(cx: &mut Cx, kind: &str, id: &str, value: &Value) {
    match kind {
        "moved" => {
            if let Some(runtime) = value["runtime"].as_str() {
                cx.state.links.moved.insert(id.to_string(), runtime.to_string());
            }
            if let Some(link) = value["link"].as_str() {
                cx.state.links.moved_via.insert(id.to_string(), link.to_string());
            }
        }
        "arrived" => {
            if let Some(link) = value["link"].as_str() {
                cx.state.links.arrived.insert(id.to_string(), link.to_string());
            }
        }
        "remote" => {
            if let Some(link) = value["link"].as_str() {
                cx.state.links.remote.insert(id.to_string(), link.to_string());
            }
        }
        "peer" => {
            let info = PeerInfo {
                runtime: value["runtime"].as_str().unwrap_or_default().to_string(),
                env: value["env"].as_str().unwrap_or_default().to_string(),
                name: value["name"].as_str().unwrap_or_default().to_string(),
            };
            cx.state.links.peers.insert(id.to_string(), info);
        }
        "outbox" => {
            let (Some(link), Some(method)) = (value["link"].as_str(), value["method"].as_str()) else {
                return;
            };
            let entry = (method.to_string(), value["params"].clone());
            cx.state
                .links
                .outbox
                .entry(link.to_string())
                .or_default()
                .push(entry);
            cx.state
                .links
                .outbox_ids
                .entry(link.to_string())
                .or_default()
                .push(id.to_string());
        }
        _ => {}
    }
}

/// A link connection closed.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    let dropped: Vec<String> = cx
        .state
        .links
        .conns
        .iter()
        .filter(|(_, c)| *c == conn)
        .map(|(l, _)| l.clone())
        .collect();
    cx.state.links.conns.retain(|_, c| c != conn);
    // The environment shows offline while its link is down; forwarded subscriptions wait for it.
    for link in dropped {
        publish_env(cx, &link);
    }
    let gone: Vec<String> = cx
        .state
        .links
        .forwarded
        .iter()
        .filter(|(_, f)| f.conn == conn)
        .map(|(l, _)| l.clone())
        .collect();
    for local in gone {
        if let Some(f) = cx.state.links.forwarded.remove(&local) {
            unsubscribe_peer(cx, &f);
        }
    }
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
            let n = cx.state.links.outbox.get(link).map_or(0, Vec::len);
            // Zero-padded so stored entries list in the order they were sent.
            let id = format!("{link}/{:016}-{n:06}", cx.now_ms);
            cx.persist(json!({ "op": "put", "kind": "outbox", "id": id,
                "value": { "link": link, "method": entry.0, "params": entry.1 } }));
            cx.state
                .links
                .outbox_ids
                .entry(link.to_string())
                .or_default()
                .push(id);
            cx.state
                .links
                .outbox
                .entry(link.to_string())
                .or_default()
                .push(entry);
        }
    }
}

/// A session that moved here, whose parent stayed on the runtime it came from, sends each change
/// of its task there. While the link is down the changes wait in the link's outbox (E2).
pub fn child_changed(cx: &mut Cx, session: &str, status: &str, task: &Value) {
    let Some(link) = cx.state.links.arrived.get(session).cloned() else {
        return;
    };
    let Some(parent) = cx
        .state
        .sessions
        .by_id
        .get(session)
        .and_then(|r| r.parent.clone())
    else {
        return;
    };
    if cx.state.sessions.by_id.contains_key(&parent) {
        return;
    }
    notify_peer(
        cx,
        &link,
        "task/changed",
        json!({ "session": session, "parent": parent, "status": status, "task": task }),
    );
}

/// `task/changed` from a peer: a child this runtime moved to it changed its task. It counts as
/// the child's own change here, so waits, wakes and budgets of the parent see it.
fn task_changed(cx: &mut Cx, conn: &str, params: &Value) -> Result<Value, RpcError> {
    let Some(link) = link_of(cx, conn) else {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "link" })));
    };
    let session = params["session"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "session is required"))?;
    let status = params["status"]
        .as_str()
        .ok_or_else(|| RpcError::new(-32602, "status is required"))?;
    // Only the runtime the child moved to speaks for it.
    if cx.state.links.moved_via.get(session) != Some(&link) {
        return Err(RpcError::new(-32002, format!("session not found: {session}")));
    }
    let known = [
        "running",
        "awaiting",
        "done",
        "failed",
        "cancelled",
        "interrupted",
    ];
    if !known.contains(&status) {
        return Err(RpcError::new(-32602, "unknown task status"));
    }
    let output = params["task"].get("output").filter(|v| !v.is_null()).cloned();
    let error = params["task"]["error"].as_str().map(str::to_string);
    crate::tasks::set_status(cx, session, status, output, error);
    Ok(json!({}))
}

fn peer_said(cx: &mut Cx, conn: &str, params: &Value) {
    let Some(link) = link_of(cx, conn) else {
        return;
    };
    let info = PeerInfo {
        runtime: params["runtime"].as_str().unwrap_or_default().to_string(),
        env: params["env"].as_str().unwrap_or_default().to_string(),
        name: params["name"].as_str().unwrap_or_default().to_string(),
    };
    // Kept, so the environment shows offline after a restart until the link is back.
    cx.persist(json!({ "op": "put", "kind": "peer", "id": link,
        "value": { "runtime": info.runtime, "env": info.env, "name": info.name } }));
    cx.state.links.peers.insert(link.clone(), info);
    publish_env(cx, &link);
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
        Pending::NewRemote { conn, id, link } => {
            let result = result.map(|mut v| {
                if let Some(sid) = v
                    .pointer("/session/id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    cx.state.links.remote.insert(sid.clone(), link.clone());
                    cx.persist(
                        json!({ "op": "put", "kind": "remote", "id": sid, "value": { "link": link } }),
                    );
                    let session = sid.clone();
                    adopt_subscription(
                        cx,
                        &conn,
                        &link,
                        json!({ "target": { "session": session } }),
                        &mut v,
                    );
                }
                v
            });
            if let Some(id) = id {
                cx.reply(&conn, id, result);
            }
        }
        Pending::Subscribe {
            conn,
            id,
            link,
            params,
        } => {
            let result = result.map(|mut v| {
                adopt_subscription(cx, &conn, &link, params, &mut v);
                v
            });
            if let Some(id) = id {
                cx.reply(&conn, id, result);
            }
        }
        Pending::Resubscribe { local, replay } => match result {
            Ok(v) => {
                let Some(f) = cx.state.links.forwarded.get_mut(&local) else {
                    return;
                };
                if let Some(remote) = v["subscription"].as_str() {
                    f.remote = remote.to_string();
                }
                if !replay {
                    // The gap couldn't be replayed: the client reads the session again (E1).
                    if let Some(head) = v["head"].as_str() {
                        f.last = head.to_string();
                    }
                    let (to, session) = (f.conn.clone(), f.params["target"]["session"].clone());
                    cx.emit(Effect::Notify {
                        conn: to,
                        method: "update".into(),
                        params: json!({ "subscription": local, "session": session, "update": {
                            "type": "notice", "level": "warning", "code": "resync",
                            "text": "updates were lost while the link was down; read the session again",
                        } }),
                    });
                }
            }
            Err(e) if replay && e.code == -32012 => {
                let Some(f) = cx.state.links.forwarded.get(&local).cloned() else {
                    return;
                };
                if let Some(link_conn) = cx.state.links.conns.get(&f.link).cloned() {
                    let call = client_call(cx, &link_conn, "subscribe", f.params.clone());
                    cx.state
                        .links
                        .pending
                        .insert(call, Pending::Resubscribe { local, replay: false });
                }
            }
            Err(_) => {
                cx.state.links.forwarded.remove(&local);
            }
        },
        Pending::LinkUpdate { conn, id } => {
            if let Ok(v) = &result {
                regrant(cx, &v["link"]);
            }
            if let Some(id) = id {
                cx.reply(&conn, id, result);
            }
        }
        Pending::Revoke { conn, id, token } => {
            if result.is_ok() {
                let closing: Vec<String> = cx
                    .state
                    .principals
                    .conns
                    .iter()
                    .filter(|(_, c)| matches!(&c.peer, Peer::Token { token: t, .. } if *t == token))
                    .map(|(k, _)| k.clone())
                    .collect();
                for c in closing {
                    cx.emit(Effect::Close { conn: c });
                }
            }
            if let Some(id) = id {
                cx.reply(&conn, id, result);
            }
        }
        Pending::Chunk { .. } => {}
        Pending::Cancel { session } => on_idle(cx, &session),
        Pending::Pack(m) => {
            let log = serde_json::to_string(&crate::items::newest(cx.state, &m.session, usize::MAX))
                .unwrap_or_else(|_| "[]".into());
            send_offer(cx, m, Some(result), log);
        }
        Pending::Unpack {
            conn,
            id,
            link,
            params,
        } => match result {
            Ok(u) => {
                if let Reply::Now(answer) = read_log(cx, &conn, id.as_ref(), &link, params, Some(u))
                    && let Some(id) = id
                {
                    cx.reply(&conn, id, answer);
                }
            }
            Err(e) => {
                if let Some(id) = id {
                    cx.reply(&conn, id, Err(e));
                }
            }
        },
        Pending::Log {
            conn,
            id,
            link,
            params,
            unpacked,
        } => {
            let answer = result.and_then(|v| {
                let log = v["data"].as_str().unwrap_or("[]");
                finish_offer(cx, &link, &params, unpacked.as_ref(), Some(log))
            });
            if let Some(id) = id {
                cx.reply(&conn, id, answer);
            }
        }
        Pending::BlobWrite { transfer } => {
            if result.is_err()
                && let Some(b) = cx.state.links.blobs.get_mut(&transfer)
            {
                b.failed = true;
            }
        }
        Pending::TransferPack {
            conn,
            id,
            link,
            workspace,
            key,
        } => {
            let packed = match result {
                Ok(p) => p,
                Err(e) => {
                    if let Some(id) = id {
                        cx.reply(&conn, id, Err(e));
                    }
                    return;
                }
            };
            let Some(link_conn) = cx.state.links.conns.get(&link).cloned() else {
                if let Some(id) = id {
                    cx.reply(&conn, id, Err(RpcError::new(-32030, "environment offline")));
                }
                return;
            };
            let transfer = format!("{key}.ws");
            send_blob(
                cx,
                &link_conn,
                &transfer,
                packed["data"].as_str().unwrap_or_default(),
                "",
            );
            let call = client_call(
                cx,
                &link_conn,
                "workspace/receive",
                json!({ "workspace": workspace, "transfer": transfer, "branch": packed["branch"] }),
            );
            cx.state.links.pending.insert(call, Pending::Forward { conn, id });
        }
        Pending::Receive {
            conn,
            id,
            link: _,
            workspace,
        } => {
            let answer = result.map(|u| {
                let meta = crate::tree::WorkspaceMeta {
                    kind: "dir".into(),
                    branch: u["branch"].as_str().map(str::to_string),
                    ..Default::default()
                };
                let path = u["path"].as_str().unwrap_or_default().to_string();
                crate::tree::add_workspace(cx, &workspace, &path, meta);
                json!({ "workspace": crate::tree::workspace_json(cx.state, &workspace) })
            });
            if let Some(id) = id {
                cx.reply(&conn, id, answer);
            }
        }
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
                cx.state.links.moved_via.insert(m.session.clone(), m.link.clone());
                // The new owner has the queue and the requests; answers here would go nowhere.
                cx.state.queue.by_session.remove(&m.session);
                cx.persist(json!({ "op": "delete", "kind": "queue", "id": m.session }));
                let requests: Vec<String> = cx
                    .state
                    .requests
                    .by_id
                    .iter()
                    .filter(|(_, r)| r.request.session == m.session)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in requests {
                    cx.state.requests.by_id.remove(&id);
                    cx.persist(json!({ "op": "delete", "kind": "request", "id": id }));
                }
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

/// A link record changed: its connected peer acts with the new grant and offer from its next call
/// on, and loses subscriptions to what it no longer sees.
fn regrant(cx: &mut Cx, record: &Value) {
    let Some(link) = record["id"].as_str() else { return };
    let new_scopes: Vec<String> = record["grant"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let new_offer = LinkOffer::from_json(&record["offer"]);
    let mut changed = Vec::new();
    for (id, c) in cx.state.principals.conns.iter_mut() {
        if let Peer::Link {
            link: l,
            scopes,
            offer,
            ..
        } = &mut c.peer
            && l == link
        {
            *scopes = new_scopes.clone();
            *offer = new_offer.clone();
            changed.push(id.clone());
        }
    }
    for conn in changed {
        crate::subscriptions::drop_unseen(cx, &conn);
    }
}

fn fail_move(cx: &mut Cx, m: &Move, e: RpcError) {
    if let Some(q) = cx.state.queue.by_session.get_mut(&m.session) {
        q.paused = m.paused;
    }
    // Nothing moved: the session starts again here.
    sessions::start_binding(cx, &m.session, parrot_proto::session::BindingVia::Resume);
    if let Some(id) = m.id.clone() {
        cx.reply(&m.conn, id, Err(e));
    }
}
