//! Connections, the handshake and scopes (spec 4, 5).

use crate::{Cx, Peer, Reply, State};
use parrot_proto::handshake::{InitializeParams, PeerInfo};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Protocol versions this runtime serves.
pub const SUPPORTED: [u32; 1] = [1];

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
        Peer::Session { .. } => Vec::new(),
        Peer::Token { scopes, .. } | Peer::Link { scopes, .. } => ALL_SCOPES
            .iter()
            .copied()
            .filter(|s| scopes.iter().any(|n| *n == scope_name(*s)))
            .collect(),
    }
}

/// The scopes a connection acts with: a session principal has its grant (spec 5.1).
pub fn conn_scopes(state: &State, peer: &Peer) -> Vec<Scope> {
    match peer {
        Peer::Session { session } => state
            .sessions
            .info
            .get(session)
            .map(|s| s.grant.clone())
            .unwrap_or_default(),
        other => scopes_of(other),
    }
}

/// Whether an initialized connection holds `scope`.
pub fn allows(state: &State, conn: &str, scope: Scope) -> bool {
    state
        .principals
        .conns
        .get(conn)
        .is_some_and(|c| c.initialized() && conn_scopes(state, &c.peer).contains(&scope))
}

/// The session a connection acts as, if it is a session principal.
pub fn session_principal<'a>(state: &'a State, conn: &str) -> Option<&'a str> {
    match state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Session { session }) => Some(session.as_str()),
        _ => None,
    }
}

/// Whether a connection may see a session at all (a token's `sessions` filter, spec 3.4).
pub fn sees(state: &State, conn: &str, session: &str) -> bool {
    match state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Token {
            sessions: Some(list), ..
        }) => crate::directory::path_of(state, session)
            .iter()
            .any(|s| list.contains(s)),
        // A session acts only on its own subtree (spec 5.1).
        Some(Peer::Session { session: me }) => {
            crate::directory::path_of(state, session).iter().any(|s| s == me)
        }
        Some(_) => true,
        None => false,
    }
}

/// `-32010` before `initialize`, `-32003` without `scope`.
pub fn check(state: &State, conn: &str, scope: Scope) -> Result<(), RpcError> {
    let Some(c) = state.principals.conns.get(conn).filter(|c| c.initialized()) else {
        return Err(RpcError::new(-32010, "not initialized"));
    };
    if !conn_scopes(state, &c.peer).contains(&scope) {
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
    match state.principals.conns.get(conn).map(|c| &c.peer) {
        Some(Peer::Link { link, .. }) => link.clone(),
        Some(Peer::Session { session }) => session.clone(),
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
    let p: InitializeParams = parse(params)?;
    let Some(protocol) = p.protocol.iter().copied().filter(|v| SUPPORTED.contains(v)).max() else {
        return Err(RpcError::new(-32011, "no shared version").with_data(json!({ "supported": SUPPORTED })));
    };
    // The owner may act as one of its sessions, with that session's grant on its subtree.
    let act_as = p.extra.get("actAs").and_then(Value::as_str).map(str::to_string);
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
    if let Some(session) = act_as {
        c.peer = Peer::Session { session };
    }
    c.info = Some(p.peer);
    c.protocol = Some(protocol);
    let peer = c.peer.clone();
    let mut connection = json!({
        "id": conn,
        "principal": principal_of(cx.state, conn),
        "scopes": conn_scopes(cx.state, &peer),
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
        "features": { "profile.core": {}, "profile.tree": {}, "profile.seed": {} },
        "connection": connection,
    }))
}

/// A connection opened.
pub fn connected(cx: &mut Cx, conn: &str, peer: Peer) {
    cx.state.principals.conns.insert(
        conn.to_string(),
        Connection {
            peer,
            info: None,
            protocol: None,
        },
    );
}

/// A connection closed.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.principals.conns.remove(conn);
}
