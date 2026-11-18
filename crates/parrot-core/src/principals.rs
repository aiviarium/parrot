//! Connections, the handshake and scopes (spec 4, 5).

use crate::{Cx, Peer, Reply, State};
use parrot_proto::handshake::{InitializeParams, PeerInfo};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Protocol versions this runtime serves.
pub const SUPPORTED: [u32; 1] = [1];

/// The `initialize` `_meta` key with which the owner acts as one of its sessions, and the feature
/// that announces it.
pub const ACT_AS: &str = "x.parrot/actAs";

/// Connections and what each may do.
#[derive(Debug)]
pub struct Principals {
    pub conns: BTreeMap<String, Connection>,
    /// This runtime's id (`rt_…`).
    pub runtime: String,
    /// The local environment's id (`env_…`).
    pub env: String,
}

impl Default for Principals {
    fn default() -> Self {
        Self {
            conns: BTreeMap::new(),
            runtime: "rt_local".into(),
            env: "env_local".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Connection {
    pub peer: Peer,
    /// Set by `initialize`.
    pub info: Option<PeerInfo>,
    pub protocol: Option<u32>,
    /// The session this connection acts as, for the MCP server of one of its bindings (spec 26).
    pub session: Option<String>,
}

impl Connection {
    pub fn initialized(&self) -> bool {
        self.info.is_some()
    }
}

/// Every scope, as the owner has them.
pub const ALL_SCOPES: [Scope; 9] = [
    Scope::Read,
    Scope::Prompt,
    Scope::Answer,
    Scope::Configure,
    Scope::Manage,
    Scope::Delegate,
    Scope::Workspace,
    Scope::Destructive,
    Scope::Functions,
];

fn scope_name(scope: Scope) -> String {
    serde_json::to_value(scope)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The scopes a peer holds. A session peer holds nothing here; `conn_scopes` reads its grant.
pub fn scopes_of(peer: &Peer) -> Vec<Scope> {
    match peer {
        Peer::Owner => ALL_SCOPES.to_vec(),
        Peer::Token { scopes, .. } | Peer::Link { scopes, .. } => ALL_SCOPES
            .iter()
            .copied()
            .filter(|s| scopes.iter().any(|n| *n == scope_name(*s)))
            .collect(),
    }
}

/// The scopes a connection acts with: its peer's, or its session's grant (spec 5.1).
pub fn conn_scopes(state: &State, c: &Connection) -> Vec<Scope> {
    let peer = scopes_of(&c.peer);
    match &c.session {
        Some(session) => state
            .sessions
            .info
            .get(session)
            .map(|i| i.grant.iter().copied().filter(|s| peer.contains(s)).collect())
            .unwrap_or_default(),
        None => peer,
    }
}

/// Whether an initialized connection holds `scope`.
pub fn allows(state: &State, conn: &str, scope: Scope) -> bool {
    state
        .principals
        .conns
        .get(conn)
        .is_some_and(|c| c.initialized() && conn_scopes(state, c).contains(&scope))
}

/// The session a connection acts as, if it is a session principal.
pub fn session_principal<'a>(state: &'a State, conn: &str) -> Option<&'a str> {
    state
        .principals
        .conns
        .get(conn)
        .and_then(|c| c.session.as_deref())
}

/// Whether a connection may see a session at all (a token's `sessions` filter, spec 3.4).
pub fn sees(state: &State, conn: &str, session: &str) -> bool {
    // A session acts only on its own subtree.
    if let Some(own) = state.principals.conns.get(conn).and_then(|c| c.session.as_ref()) {
        return crate::directory::path_of(state, session).contains(own);
    }
    match state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Token {
            sessions: Some(list), ..
        }) => crate::directory::path_of(state, session)
            .iter()
            .any(|s| list.contains(s)),
        Some(_) => true,
        None => false,
    }
}

/// `-32010` before `initialize`, `-32003` without `scope`.
pub fn check(state: &State, conn: &str, scope: Scope) -> Result<(), RpcError> {
    let Some(c) = state.principals.conns.get(conn).filter(|c| c.initialized()) else {
        return Err(RpcError::new(-32010, "not initialized"));
    };
    if !conn_scopes(state, c).contains(&scope) {
        return Err(
            RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "scope", "scope": scope }))
        );
    }
    Ok(())
}

/// `check` plus the session filter: a session the caller can't see is not found.
pub fn check_session(state: &State, conn: &str, scope: Scope, session: &str) -> Result<(), RpcError> {
    check(state, conn, scope)?;
    if !state.sessions.by_id.contains_key(session) || !sees(state, conn, session) {
        return Err(RpcError::new(-32002, format!("session not found: {session}")));
    }
    Ok(())
}

/// The principal a connection acts as (spec 5.1).
pub fn principal_of(state: &State, conn: &str) -> String {
    if let Some(session) = state.principals.conns.get(conn).and_then(|c| c.session.clone()) {
        return session;
    }
    match state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Link { link, .. }) => link.clone(),
        _ => conn.to_string(),
    }
}

/// Parse params into a wire type, or `-32602`.
pub fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| RpcError::new(-32602, format!("invalid params: {e}")))
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, _id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "initialize" => Reply::Now(initialize(cx, conn, params)),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn initialize(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    // `parrot mcp` names its binding and that binding's credential (spec 26).
    let acting = match params.get("binding") {
        Some(b) => {
            let binding = b["id"].as_str().unwrap_or_default();
            let token = b["token"].as_str().unwrap_or_default();
            let session = crate::functions::binding_session(cx, binding, token).ok_or_else(|| {
                RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "binding" }))
            })?;
            Some(session)
        }
        None => None,
    };
    let provides = params.get("provides").and_then(Value::as_array).cloned();
    let p: InitializeParams = parse(params)?;
    let Some(protocol) = p.protocol.iter().copied().filter(|v| SUPPORTED.contains(v)).max() else {
        return Err(RpcError::new(-32011, "no shared version").with_data(json!({ "supported": SUPPORTED })));
    };
    // The owner may act as one of its sessions, with that session's grant on its subtree. It is an
    // extension, so it is a namespaced `_meta` key (spec 4, 38).
    let act_as = p
        .meta
        .as_ref()
        .and_then(|m| m.get(ACT_AS))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(session) = &act_as {
        let owner = matches!(
            cx.state.principals.conns.get(conn).map(|c| &c.peer),
            Some(Peer::Owner)
        );
        if !owner {
            return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "actAs" })));
        }
        if !cx.state.sessions.by_id.contains_key(session) {
            return Err(RpcError::new(-32002, format!("session not found: {session}")));
        }
    }
    let Some(c) = cx.state.principals.conns.get_mut(conn) else {
        return Err(RpcError::new(-32010, "unknown connection"));
    };
    c.info = Some(p.peer);
    c.protocol = Some(protocol);
    c.session = acting.or(act_as);
    let peer = c.peer.clone();
    let scopes = conn_scopes(cx.state, &cx.state.principals.conns[conn]);
    if let Some(specs) = provides {
        crate::functions::provide(cx, conn, &specs)?;
    }
    let mut connection = json!({
        "id": conn,
        "principal": principal_of(cx.state, conn),
        "scopes": scopes,
    });
    if let Peer::Token {
        sessions: Some(s), ..
    } = &peer
    {
        connection["sessions"] = json!(s);
    }
    Ok(json!({
        "protocol": protocol,
        "peer": { "kind": "runtime", "name": "parrot", "version": env!("CARGO_PKG_VERSION") },
        "runtime": { "id": cx.state.principals.runtime, "env": cx.state.principals.env },
        "features": {
            "profile.core": {}, "profile.tree": {}, "profile.seed": {}, "profile.link": {}, "profile.remote": {},
            "profile.desktop": {}, "profile.tui": {}, "x.parrot/actAs": {},
        },
        "connection": connection,
    }))
}

/// A connection opened.
pub fn connected(cx: &mut Cx, conn: &str, peer: Peer) {
    let peer_for_links = peer.clone();
    cx.state.principals.conns.insert(
        conn.to_string(),
        Connection {
            peer,
            info: None,
            protocol: None,
            session: None,
        },
    );
    if let Peer::Link { link, .. } = &peer_for_links {
        crate::links::connected(cx, conn, link);
    }
}

/// A connection closed.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.principals.conns.remove(conn);
}
