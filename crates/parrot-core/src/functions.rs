//! Functions (spec 25–29, E5): the catalog, calls between clients, harnesses and providers,
//! the session functions, and the per-binding credentials of the injected MCP server.
//!
//! Functions come from three providers: clients (`function/provide` or `initialize.provides`),
//! the runtime's own session functions, and runtime providers such as the desktop, which run in
//! `parrot-serve` as `Effect::Workspace` operations named `function.<name>`.

use crate::{CallOwner, Cx, Effect, Reply, principals};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
pub struct Functions {
    /// Client-provided functions by `(name, env)`: the providing connection and the spec.
    pub provided: BTreeMap<(String, String), (String, Value)>,
    /// Functions the runtime itself provides through the backend, such as `parrot.desktop.*`.
    pub runtime: BTreeMap<String, Value>,
    /// Calls waiting for a provider.
    pub pending: BTreeMap<u64, PendingCall>,
    /// Results of keyed calls, by caller and key (spec 27.1 keys).
    pub keys: BTreeMap<(String, String), Value>,
    /// Per-binding credentials of the injected MCP server, by token.
    pub tokens: BTreeMap<String, String>,
    /// The `parrot` program for `parrot mcp`; `None` turns the injected server off.
    pub mcp: Option<String>,
    /// Calls waiting for an approval, by request id (E5, 27.2).
    pub approvals: BTreeMap<String, Approval>,
    /// The approval of each keyed call, by caller and key.
    pub approval_keys: BTreeMap<(String, String), String>,
    /// How long a call waits for its approval before it returns `pending_approval`; `None` is
    /// [`MAX_WAIT_MS`].
    pub wait_ms: Option<u64>,
}

/// No call blocks longer than this (spec 27.2: 3 minutes, less a margin).
pub const MAX_WAIT_MS: u64 = 170_000;

#[derive(Debug, Clone)]
pub struct Approval {
    pub session: String,
    /// The caller waiting for the result; `None` once it got `pending_approval`.
    pub waiting: Option<(String, RequestId)>,
    /// The connection the call came from, which the approved call runs as.
    pub conn: String,
    pub key: Option<(String, String)>,
    pub params: Value,
    pub deadline_ms: u64,
    pub status: ApprovalStatus,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalStatus {
    Open,
    /// Approved while its caller was gone, such as after a restart; a repeat of the keyed call
    /// runs it (27.2).
    Approved,
    /// Approved and running.
    Running,
    Denied,
}

#[derive(Debug, Clone)]
pub struct PendingCall {
    pub conn: String,
    pub id: Option<RequestId>,
    pub key: Option<(String, String)>,
}

/// The session functions (spec 28) and the protocol method each projects.
pub const SESSION_FUNCTIONS: [(&str, &str, &str); 16] = [
    ("parrot.sessions.spawn", "session/new", "delegate"),
    ("parrot.sessions.send", "session/prompt", "prompt"),
    ("parrot.sessions.act", "action/invoke", "prompt"),
    ("parrot.sessions.cancel", "session/cancel", "prompt"),
    ("parrot.sessions.stop", "session/stop", "manage"),
    ("parrot.sessions.read", "session/get", "read"),
    ("parrot.sessions.tree", "session/tree", "read"),
    ("parrot.sessions.handoff", "session/rebind", "delegate"),
    ("parrot.tasks.assign", "task/assign", "delegate"),
    ("parrot.tasks.wait", "task/wait", "read"),
    ("parrot.task.finish", "task/finish", "read"),
    ("parrot.task.fail", "task/fail", "read"),
    ("parrot.requests.list", "request/list", "read"),
    ("parrot.workspaces.status", "workspace/status", "read"),
    ("parrot.agents.list", "agent/list", "read"),
    ("parrot.envs.list", "env/list", "read"),
];

/// The function a harness's tool call reached, when the tool is one the injected `parrot` server
/// lists (spec 25.1, 26). Harnesses name such tools by the MCP name (dots as `__`), with or
/// without a prefix of their own, so a tool matches when its name ends with an MCP name. A call of
/// `parrot.functions.call` is reported as the function it called.
pub fn function_of_tool(state: &crate::State, tool: &str, input: Option<&Value>) -> Option<String> {
    let session = SESSION_FUNCTIONS.iter().map(|(n, _, _)| n.to_string());
    // The search tools, and `parrot.workspaces.merge`, which `parrot mcp` maps to its method.
    let meta = [
        "parrot.functions.search",
        "parrot.functions.describe",
        "parrot.functions.call",
        "parrot.workspaces.merge",
    ]
    .into_iter()
    .map(String::from);
    let provided = state.functions.provided.keys().map(|(n, _)| n.clone());
    let runtime = state.functions.runtime.keys().cloned();
    let found = session
        .chain(meta)
        .chain(provided)
        .chain(runtime)
        .filter(|name| {
            let mcp = name.replace('.', "__");
            tool == mcp || tool.ends_with(&format!("__{mcp}"))
        })
        .max_by_key(String::len)?;
    if found == "parrot.functions.call"
        && let Some(called) = input.and_then(|i| i.get("function")).and_then(Value::as_str)
    {
        return Some(called.to_string());
    }
    Some(found)
}

/// Checks a name against spec 27.1: dot-separated `[a-z][a-z0-9_]*` segments, none with `__`,
/// at most four, and an MCP name of at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    segments.len() <= 4
        && name.replace('.', "__").len() <= 64
        && segments.iter().all(|s| {
            let mut chars = s.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !s.contains("__")
        })
}

const LEVELS: [&str; 5] = ["read", "write", "network", "full", "host"];

fn check_spec(spec: &Value) -> Result<(String, String), RpcError> {
    let name = spec["name"].as_str().unwrap_or_default();
    if !valid_name(name) || name.starts_with("parrot.") {
        return Err(RpcError::new(-32602, format!("invalid function name: {name}")));
    }
    if !spec["input"].is_object() || spec["description"].as_str().is_none() {
        return Err(RpcError::new(
            -32602,
            format!("{name}: description and input are required"),
        ));
    }
    // Levels are required (E5).
    if !spec["level"].as_str().is_some_and(|l| LEVELS.contains(&l)) {
        return Err(RpcError::new(-32602, format!("{name}: level is required")));
    }
    Ok((
        name.to_string(),
        spec["env"].as_str().unwrap_or_default().to_string(),
    ))
}

/// The pinned provided functions, by name, with their specs as text to compare.
fn pinned(state: &crate::State) -> BTreeMap<String, String> {
    state
        .functions
        .provided
        .iter()
        .filter(|(_, (_, spec))| spec["pinned"] == true)
        .map(|((name, env), (_, spec))| (format!("{name}@{env}"), spec.to_string()))
        .collect()
}

/// Tells each session with an MCP server that the pinned functions changed (spec 26), so the
/// server lists them again.
fn pinned_changed(cx: &mut Cx, before: &BTreeMap<String, String>) {
    let after = pinned(cx.state);
    if &after == before {
        return;
    }
    let names: BTreeSet<String> = after
        .keys()
        .filter_map(|k| k.split('@').next().map(str::to_string))
        .collect();
    let sessions: BTreeSet<String> = cx
        .state
        .functions
        .tokens
        .values()
        .filter_map(|b| crate::directory::session_of(cx.state, b))
        .collect();
    for session in sessions {
        cx.publish(Some(session), json!({ "type": "functions", "pinned": names }));
    }
}

/// Replace a connection's provided functions (spec 25.2).
pub fn provide(cx: &mut Cx, conn: &str, specs: &[Value]) -> Result<(), RpcError> {
    let mut keys = Vec::new();
    for spec in specs {
        let key = check_spec(spec)?;
        if let Some(patterns) = provides_filter(cx, conn)
            && !patterns.iter().any(|p| matches(p, &key.0))
        {
            return Err(
                RpcError::new(-32003, format!("this token may not provide {}", key.0))
                    .with_data(json!({ "reason": "provides" })),
            );
        }
        if let Some((owner, _)) = cx.state.functions.provided.get(&key)
            && owner != conn
        {
            return Err(RpcError::new(-32004, format!("{} has another provider", key.0))
                .with_data(json!({ "state": "provided" })));
        }
        keys.push((key, spec.clone()));
    }
    let before = pinned(cx.state);
    cx.state.functions.provided.retain(|_, (owner, _)| owner != conn);
    for (key, spec) in keys {
        cx.state.functions.provided.insert(key, (conn.to_string(), spec));
    }
    pinned_changed(cx, &before);
    Ok(())
}

fn session_function_specs() -> Vec<Value> {
    SESSION_FUNCTIONS
        .iter()
        .map(|(name, method, scope)| {
            json!({
                "name": name,
                "description": format!("Calls {method} as the calling session."),
                "input": { "type": "object" },
                "output": { "type": "object" },
                "effects": if *scope == "read" { json!(["read"]) } else { json!(["write"]) },
                "level": "read",
                "provider": "runtime",
            })
        })
        .collect()
}

/// Every function visible to a connection, sorted by name.
fn catalog(cx: &Cx, conn: &str) -> Vec<Value> {
    let mut all = session_function_specs();
    for spec in cx.state.functions.runtime.values() {
        let mut s = spec.clone();
        s["provider"] = json!("runtime");
        all.push(s);
    }
    for (provider, spec) in cx.state.functions.provided.values() {
        if let Some(pattern) = provides_filter(cx, provider) {
            let name = spec["name"].as_str().unwrap_or_default();
            if !pattern.iter().any(|p| matches(p, name)) {
                continue;
            }
        }
        let mut s = spec.clone();
        s["provider"] = json!(provider);
        all.push(s);
    }
    let _ = conn;
    all.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    all
}

/// A token client's `provides` patterns, if it has them (E8).
fn provides_filter(cx: &Cx, provider: &str) -> Option<Vec<String>> {
    match cx.state.principals.conns.get(provider).map(|c| &c.peer) {
        Some(crate::Peer::Token { provides, .. }) => provides.clone(),
        _ => None,
    }
}

/// Glob match with `*` as any run of characters.
pub fn matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && {
                let rest = &name[head.len()..];
                tail.is_empty()
                    || (0..=rest.len()).any(|i| rest.is_char_boundary(i) && matches(tail, &rest[i..]))
            }
        }
    }
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "function/list" => Reply::Now(
            principals::check(cx.state, conn, Scope::Read)
                .map(|()| json!({ "functions": catalog(cx, conn) })),
        ),
        "function/search" => Reply::Now(principals::check(cx.state, conn, Scope::Read).map(|()| {
            let query = params["query"].as_str().unwrap_or_default().to_lowercase();
            let limit = params["limit"].as_u64().unwrap_or(20) as usize;
            let words: Vec<&str> = query.split_whitespace().collect();
            let mut ranked: Vec<(usize, Value)> = catalog(cx, conn)
                .into_iter()
                .filter_map(|f| {
                    let name = f["name"].as_str().unwrap_or_default().to_lowercase();
                    let text = f["description"].as_str().unwrap_or_default().to_lowercase();
                    let score: usize = words
                        .iter()
                        .map(|w| 3 * usize::from(name.contains(w)) + usize::from(text.contains(w)))
                        .sum();
                    (score > 0).then_some((score, f))
                })
                .collect();
            ranked.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(a.1["name"].as_str().cmp(&b.1["name"].as_str()))
            });
            json!({ "functions": ranked.into_iter().take(limit).map(|(_, f)| f).collect::<Vec<_>>() })
        })),
        "function/describe" => Reply::Now(principals::check(cx.state, conn, Scope::Read).map(|()| {
            let names: Vec<String> = params["names"]
                .as_array()
                .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let functions: Vec<Value> = catalog(cx, conn)
                .into_iter()
                .filter(|f| names.iter().any(|n| f["name"].as_str() == Some(n)))
                .collect();
            let declarations = functions
                .iter()
                .map(|f| {
                    format!(
                        "/** {} */\ndeclare function {}(input: object): object",
                        f["description"].as_str().unwrap_or_default(),
                        f["name"].as_str().unwrap_or_default().replace('.', "__")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            json!({ "functions": functions, "declarations": declarations })
        })),
        "function/provide" => Reply::Now(principals::check(cx.state, conn, Scope::Functions).and_then(
            |()| {
                let specs = params["functions"].as_array().cloned().unwrap_or_default();
                provide(cx, conn, &specs).map(|()| json!({}))
            },
        )),
        "function/call" => function_call(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

/// How permission levels and function levels order, lowest first; `network` and `sandboxed`
/// are the same rung.
fn level_rank(level: &str) -> u8 {
    match level {
        "read" => 0,
        "write" => 1,
        "sandboxed" | "network" => 2,
        "full" => 3,
        _ => 4,
    }
}

/// The ceiling over a session, as `Ceiling` JSON; `{}` is the empty ceiling.
pub fn ceiling_of(state: &crate::State, session: &str) -> Value {
    crate::ceilings::json_of(&crate::ceilings::of(state, session))
}

/// The level a session acts at: its binding's, under its ceiling's.
fn session_level(state: &crate::State, session: &str, ceiling: &Value) -> u8 {
    let binding = crate::directory::binding_of(state, session)
        .and_then(|b| state.bindings.info.get(&b))
        .and_then(|b| b.level)
        .and_then(|l| serde_json::to_value(l).ok())
        .and_then(|v| v.as_str().map(level_rank))
        .unwrap_or(0);
    match ceiling["level"].as_str() {
        Some(c) => binding.min(level_rank(c)),
        None => binding,
    }
}

fn error_result(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

fn function_call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    if let Err(e) = principals::check(cx.state, conn, Scope::Read) {
        return Reply::Now(Err(e));
    }
    let name = params["function"].as_str().unwrap_or_default().to_string();
    // A call for a session runs as the session (spec 25.1): its agent's, or a client's naming it.
    let acting = match cx
        .state
        .principals
        .conns
        .get(conn)
        .and_then(|c| c.session.clone())
    {
        Some(s) => Some(s),
        None => match params["session"].as_str() {
            Some(s) => {
                if let Err(e) = principals::check_session(cx.state, conn, Scope::Read, s) {
                    return Reply::Now(Err(e));
                }
                Some(s.to_string())
            }
            None => None,
        },
    };
    let principal = acting
        .clone()
        .unwrap_or_else(|| principals::principal_of(cx.state, conn));
    let key = params["key"].as_str().map(|k| (principal.clone(), k.to_string()));
    if let Some(done) = key.as_ref().and_then(|k| cx.state.functions.keys.get(k)) {
        return Reply::Now(Ok(done.clone()));
    }
    let is_session_function = SESSION_FUNCTIONS.iter().any(|(n, _, _)| *n == name);
    if let (Some(session), false) = (&acting, is_session_function) {
        if let Some(rid) = key
            .as_ref()
            .and_then(|k| cx.state.functions.approval_keys.get(k))
            .cloned()
        {
            let approved = cx
                .state
                .functions
                .approvals
                .get(&rid)
                .is_some_and(|a| a.status == ApprovalStatus::Approved);
            if !approved {
                // Asked again while the approval is open or the approved call runs (27.2).
                return Reply::Now(Ok(pending(&rid)));
            }
            // Approved while nobody waited: this repeat runs the call.
            if let Some(a) = cx.state.functions.approvals.get_mut(&rid) {
                a.status = ApprovalStatus::Running;
            }
            persist_approval(cx, &rid);
            let stored = cx.state.functions.approvals.get(&rid).map(|a| a.params.clone());
            let params = stored.unwrap_or(params);
            return match run_call(cx, conn, id, params, key.clone(), session) {
                Reply::Now(result) => {
                    finish_approved(cx, &rid, key, conn.to_string(), id.cloned(), result);
                    Reply::Later
                }
                later => later,
            };
        }
        let env = params["env"].as_str().unwrap_or_default().to_string();
        let spec = cx.state.functions.runtime.get(&name).cloned().or_else(|| {
            cx.state
                .functions
                .provided
                .get(&(name.clone(), env))
                .map(|(_, s)| s.clone())
        });
        if let Some(spec) = spec {
            let ceiling = ceiling_of(cx.state, session);
            if let Some(patterns) = ceiling["functions"].as_array() {
                let allowed = patterns
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|p| matches(p, &name));
                if !allowed {
                    return Reply::Now(Err(RpcError::new(
                        -32003,
                        format!("{name} is outside the session's ceiling"),
                    )
                    .with_data(json!({ "reason": "ceiling", "function": name }))));
                }
            }
            let level = spec["level"].as_str().unwrap_or("host");
            let above = level_rank(level) > session_level(cx.state, session, &ceiling);
            let unlisted = ceiling["effects"].as_array().is_some_and(|listed| {
                spec["effects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|e| *e != "read")
                    .any(|e| !listed.contains(e))
            });
            if above || unlisted {
                return open_approval(cx, conn, id, session, &spec, params, key);
            }
        }
    }
    run_call(cx, conn, id, params, key, &principal)
}

/// The `pending_approval` error result for an approval that is still open (27.2).
fn pending(request: &str) -> Value {
    json!({ "error": { "code": "pending_approval", "message": "the call waits for an approval",
        "request": request } })
}

/// How long a session's call waits for its approval: within its harness's tool-call timeout
/// from `limits.kdl`, less a margin, and never over [`MAX_WAIT_MS`] (27.2).
pub fn approval_wait_ms(state: &crate::State, session: &str) -> u64 {
    let timeout = crate::directory::binding_of(state, session)
        .and_then(|b| state.bindings.by_id.get(&b).map(|r| r.agent.clone()))
        .and_then(|agent| crate::agents::tool_timeout(state, &agent));
    let limit = parrot_proto::driver_data::call_limit_ms(timeout, MAX_WAIT_MS);
    state.functions.wait_ms.map_or(limit, |w| w.min(limit))
}

/// Opens a request for a call that needs one, and waits within the time limit (E5, 27.2).
fn open_approval(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    session: &str,
    spec: &Value,
    params: Value,
    key: Option<(String, String)>,
) -> Reply {
    let rid = cx.ids.next("req");
    let name = spec["name"].as_str().unwrap_or_default();
    let level = match spec["level"].as_str() {
        Some("read") => "read",
        Some("write") => "write",
        Some("network" | "sandboxed") => "sandboxed",
        _ => "host",
    };
    let request = json!({
        "id": rid,
        "session": session,
        "path": crate::directory::path_of(cx.state, session),
        "kind": "approval",
        "title": format!("Call {name}?"),
        "description": spec["description"],
        "subject": { "type": "function", "function": name, "input": params.get("input").cloned().unwrap_or_else(|| json!({})) },
        "level": level,
        "choices": [
            { "id": "allow", "label": "Allow", "intent": "allow", "scope": "once" },
            { "id": "deny", "label": "Deny", "intent": "deny", "scope": "once" },
        ],
        "blocking": false,
        "answerable": "message",
        "notify": "normal",
        "status": "open",
    });
    let request: parrot_proto::request::Request = match serde_json::from_value(request) {
        Ok(r) => r,
        Err(e) => {
            return Reply::Now(Err(RpcError::new(
                -32603,
                format!("cannot open the request: {e}"),
            )));
        }
    };
    cx.state.requests.by_id.insert(
        rid.clone(),
        crate::requests::RequestRecord {
            request,
            binding: String::new(),
            key: rid.clone(),
            conn: None,
            expires_ms: None,
            pending: None,
            answer_key: None,
        },
    );
    crate::requests::publish_request(cx, &rid);
    let wait = params["timeout"]
        .as_u64()
        .map(|s| s.saturating_mul(1000))
        .unwrap_or(u64::MAX)
        .min(approval_wait_ms(cx.state, session));
    let deadline_ms = cx.now_ms + wait;
    cx.emit(Effect::TimerAt { at_ms: deadline_ms });
    if let Some(k) = &key {
        cx.state.functions.approval_keys.insert(k.clone(), rid.clone());
    }
    cx.state.functions.approvals.insert(
        rid.clone(),
        Approval {
            session: session.to_string(),
            waiting: id.map(|id| (conn.to_string(), id.clone())),
            conn: conn.to_string(),
            key,
            params,
            deadline_ms,
            status: ApprovalStatus::Open,
        },
    );
    persist_approval(cx, &rid);
    if id.is_some() {
        Reply::Later
    } else {
        Reply::Now(Ok(pending(&rid)))
    }
}

/// Answers to the requests of function approvals, before the requests module sees them: they
/// have no binding to answer.
pub fn intercept(
    cx: &mut Cx,
    conn: &str,
    _id: Option<&RequestId>,
    method: &str,
    params: &Value,
) -> Option<Reply> {
    if method != "request/answer" {
        return None;
    }
    let rid = params["request"].as_str()?.to_string();
    let approval = cx.state.functions.approvals.get(&rid)?.clone();
    if let Err(e) = principals::check_session(cx.state, conn, Scope::Answer, &approval.session) {
        return Some(Reply::Now(Err(e)));
    }
    if approval.status != ApprovalStatus::Open {
        return Some(Reply::Now(Err(RpcError::new(-32013, "already answered"))));
    }
    let answer: parrot_proto::request::Answer = match serde_json::from_value(params["answer"].clone()) {
        Ok(a) => a,
        Err(e) => {
            return Some(Reply::Now(Err(RpcError::new(
                -32602,
                format!("invalid params: {e}"),
            ))));
        }
    };
    let allow = answer.choice.as_deref() == Some("allow");
    let by = principals::principal_of(cx.state, conn);
    if let Some(rec) = cx.state.requests.by_id.get_mut(&rid) {
        rec.request.status = parrot_proto::request::RequestStatus::Answered;
        rec.request.answer = Some(answer);
        rec.request.answered_by = Some(by);
    }
    crate::requests::publish_request(cx, &rid);
    let caller_gone = !cx.state.principals.conns.contains_key(&approval.conn);
    let a = cx.state.functions.approvals.get_mut(&rid)?;
    if allow && a.waiting.is_none() && caller_gone {
        // Nobody can take the result now; the caller's repeat with its key runs the call.
        a.status = ApprovalStatus::Approved;
        if a.key.is_none() {
            cx.state.functions.approvals.remove(&rid);
            cx.persist(json!({ "op": "delete", "kind": "approval", "id": rid }));
        } else {
            persist_approval(cx, &rid);
        }
    } else if allow {
        a.status = ApprovalStatus::Running;
        let (waiting, key, params, caller) =
            (a.waiting.take(), a.key.clone(), a.params.clone(), a.conn.clone());
        let (wconn, wid) = match waiting {
            Some((c, i)) => (c, Some(i)),
            None => (caller, None),
        };
        let principal = approval.session.clone();
        match run_call(cx, &wconn, wid.as_ref(), params, key.clone(), &principal) {
            Reply::Now(result) => finish_approved(cx, &rid, key, wconn, wid, result),
            Reply::Later => {}
        }
    } else {
        a.status = ApprovalStatus::Denied;
        let denied = error_result("denied", "the call was denied");
        if let Some(k) = a.key.clone() {
            cx.state.functions.keys.insert(k.clone(), denied.clone());
            cx.state.functions.approval_keys.remove(&k);
        }
        if let Some((c, i)) = a.waiting.take() {
            cx.reply(&c, i, Ok(denied));
        }
        cx.state.functions.approvals.remove(&rid);
        cx.persist(json!({ "op": "delete", "kind": "approval", "id": rid }));
    }
    Some(Reply::Now(Ok(json!({}))))
}

/// An approved call that answered at once.
fn finish_approved(
    cx: &mut Cx,
    rid: &str,
    key: Option<(String, String)>,
    conn: String,
    id: Option<RequestId>,
    result: Result<Value, RpcError>,
) {
    let result = result.unwrap_or_else(|e| error_result(&e.code.to_string(), &e.message));
    if let Some(k) = key {
        cx.state.functions.keys.insert(k.clone(), result.clone());
        cx.state.functions.approval_keys.remove(&k);
    }
    cx.state.functions.approvals.remove(rid);
    cx.persist(json!({ "op": "delete", "kind": "approval", "id": rid }));
    if let Some(id) = id {
        cx.reply(&conn, id, Ok(result));
    }
}

/// Keep an approval in the store, so it can still be answered after a restart.
fn persist_approval(cx: &mut Cx, rid: &str) {
    let Some(a) = cx.state.functions.approvals.get(rid) else {
        return;
    };
    let status = match a.status {
        ApprovalStatus::Open => "open",
        ApprovalStatus::Approved | ApprovalStatus::Running => "approved",
        ApprovalStatus::Denied => "denied",
    };
    let value = json!({
        "session": a.session,
        "key": a.key.as_ref().map(|(p, k)| json!([p, k])),
        "params": a.params,
        "status": status,
    });
    cx.persist(json!({ "op": "put", "kind": "approval", "id": rid, "value": value }));
}

/// Put back an approval read from the store. Its caller is gone; an approved one waits for the
/// caller's repeat with its key.
pub fn restore_approval(cx: &mut Cx, rid: &str, value: &Value) {
    let Some(session) = value["session"].as_str() else {
        return;
    };
    let key = value["key"]
        .as_array()
        .and_then(|k| Some((k.first()?.as_str()?.to_string(), k.get(1)?.as_str()?.to_string())));
    let status = match value["status"].as_str() {
        Some("approved") => ApprovalStatus::Approved,
        Some("denied") => return,
        _ => ApprovalStatus::Open,
    };
    if let Some(k) = &key {
        cx.state
            .functions
            .approval_keys
            .insert(k.clone(), rid.to_string());
    }
    cx.state.functions.approvals.insert(
        rid.to_string(),
        Approval {
            session: session.to_string(),
            waiting: None,
            conn: String::new(),
            key,
            params: value["params"].clone(),
            deadline_ms: cx.now_ms,
            status,
        },
    );
}

/// Time passed: callers whose approval is still open get `pending_approval` (27.2).
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let due: Vec<(String, (String, RequestId))> = cx
        .state
        .functions
        .approvals
        .iter_mut()
        .filter(|(_, a)| a.deadline_ms <= now && a.status == ApprovalStatus::Open)
        .filter_map(|(rid, a)| a.waiting.take().map(|w| (rid.clone(), w)))
        .collect();
    for (rid, (conn, id)) in due {
        cx.reply(&conn, id, Ok(pending(&rid)));
    }
}

/// Runs a call that may run: a session function, a runtime function or a client's.
fn run_call(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    params: Value,
    key: Option<(String, String)>,
    principal: &str,
) -> Reply {
    let name = params["function"].as_str().unwrap_or_default().to_string();
    let env = params["env"].as_str().unwrap_or_default().to_string();
    let input = params.get("input").cloned().unwrap_or_else(|| json!({}));
    // The session functions are projections of protocol methods, called as the caller.
    if let Some((_, method, _)) = SESSION_FUNCTIONS.iter().find(|(n, _, _)| *n == name) {
        let mut p = input;
        if *method == "session/new" && p.get("parent").is_none() && principal.starts_with("ses_") {
            p["parent"] = json!(principal);
        }
        return match crate::dispatch::call(cx, conn, id, method, p) {
            Reply::Now(r) => {
                let result = match r {
                    Ok(output) => json!({ "output": output }),
                    Err(e) => error_result(&e.code.to_string(), &e.message),
                };
                if let Some(k) = key {
                    cx.state.functions.keys.insert(k, result.clone());
                }
                Reply::Now(Ok(result))
            }
            later => later,
        };
    }
    if cx.state.functions.runtime.contains_key(&name) {
        if let Err(e) = principals::check(cx.state, conn, Scope::Functions) {
            return Reply::Now(Err(e));
        }
        let call = cx.call_workspace(CallOwner::Functions, &format!("function.{name}"), input);
        cx.state.functions.pending.insert(
            call,
            PendingCall {
                conn: conn.to_string(),
                id: id.cloned(),
                key,
            },
        );
        return Reply::Later;
    }
    let Some((provider, _)) = cx
        .state
        .functions
        .provided
        .get(&(name.clone(), env.clone()))
        .cloned()
    else {
        return Reply::Now(Err(RpcError::new(-32002, format!("function not found: {name}"))));
    };
    if let Err(e) = principals::check(cx.state, conn, Scope::Functions) {
        return Reply::Now(Err(e));
    }
    let call = cx.next_call();
    cx.state.pending_calls.insert(call, CallOwner::Functions);
    let mut forwarded = json!({ "function": name, "input": input });
    if !env.is_empty() {
        forwarded["env"] = json!(env);
    }
    if principal.starts_with("ses_") {
        forwarded["session"] = json!(principal);
    }
    cx.emit(Effect::ClientCall {
        conn: provider,
        call,
        method: "function/call".into(),
        params: forwarded,
    });
    cx.state.functions.pending.insert(
        call,
        PendingCall {
            conn: conn.to_string(),
            id: id.cloned(),
            key,
        },
    );
    Reply::Later
}

/// A driver, workspace or client reply to a call this module made.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(p) = cx.state.functions.pending.remove(&call) else {
        return;
    };
    // A provider's error is the call's error result, not a protocol failure (spec 25).
    let result = match result {
        Ok(v) => v,
        Err(e) => json!({ "error": { "code": e.code.to_string(), "message": e.message } }),
    };
    if let Some(k) = p.key {
        cx.state.functions.keys.insert(k.clone(), result.clone());
        if let Some(rid) = cx.state.functions.approval_keys.remove(&k) {
            cx.state.functions.approvals.remove(&rid);
            cx.persist(json!({ "op": "delete", "kind": "approval", "id": rid }));
        }
    }
    if let Some(id) = p.id {
        cx.reply(&p.conn, id, Ok(result));
    }
}

/// The `mcp` entry of a binding's `BindingStart` (spec 26, 30.1), or `None` without functions.
/// The token is for this binding only; drivers pass it to `parrot mcp` in `PARROT_MCP_TOKEN`.
pub fn mcp_for(cx: &mut Cx, session: &str, binding: &str) -> Option<Value> {
    let program = cx.state.functions.mcp.clone()?;
    let agent = cx.state.sessions.by_id.get(session).map(|s| s.agent.clone());
    let timeout = agent.and_then(|a| crate::agents::tool_timeout(cx.state, &a));
    let token = format!("{}{}", cx.ids.next("mcp"), cx.ids.next("k"));
    cx.state.functions.tokens.retain(|_, b| b != binding);
    cx.state
        .functions
        .tokens
        .insert(token.clone(), binding.to_string());
    let mut command = vec![program, "mcp".into(), "--binding".into(), binding.to_string()];
    if let Some(t) = timeout {
        command.extend(["--tool-timeout".to_string(), t.to_string()]);
    }
    Some(json!({ "command": command, "token": token }))
}

/// The session a binding's MCP credential stands for, if `token` is that binding's.
pub fn binding_session(cx: &Cx, binding: &str, token: &str) -> Option<String> {
    (cx.state.functions.tokens.get(token)? == binding)
        .then(|| crate::directory::session_of(cx.state, binding))
        .flatten()
}

/// A client's answer to an `Effect::ClientCall`.
pub fn on_client_reply(cx: &mut Cx, _conn: &str, call: u64, result: Result<Value, RpcError>) {
    match cx.state.pending_calls.remove(&call) {
        Some(CallOwner::Links) => crate::links::on_reply(cx, call, result),
        Some(CallOwner::Functions) => on_reply(cx, call, result),
        Some(other) => {
            cx.state.pending_calls.insert(call, other);
        }
        None => {}
    }
}

/// A connection closed: its functions go away, and calls waiting on it fail.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    let before = pinned(cx.state);
    cx.state.functions.provided.retain(|_, (owner, _)| owner != conn);
    pinned_changed(cx, &before);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_rules() {
        assert!(valid_name("acme.files.read"));
        assert!(!valid_name("acme.Files"));
        assert!(!valid_name("acme.a__b"));
        assert!(!valid_name("a.b.c.d.e"));
        assert!(matches("acme.*", "acme.files.read"));
        assert!(!matches("acme.*", "other.files"));
        assert!(matches("*.read", "acme.files.read"));
    }

    #[test]
    fn tool_names_map_to_the_functions_they_reach() {
        let mut state = crate::State::default();
        state
            .functions
            .provided
            .insert(("acme.deploy".into(), String::new()), ("con_p".into(), json!({})));
        let f = |tool: &str, input: Value| function_of_tool(&state, tool, Some(&input));
        assert_eq!(
            f("mcp__parrot__parrot__sessions__spawn", json!({})).as_deref(),
            Some("parrot.sessions.spawn")
        );
        assert_eq!(
            f("parrot__tasks__wait", json!({})).as_deref(),
            Some("parrot.tasks.wait")
        );
        assert_eq!(
            f("mcp__parrot__acme__deploy", json!({})).as_deref(),
            Some("acme.deploy")
        );
        assert_eq!(
            f(
                "mcp__parrot__parrot__functions__call",
                json!({ "function": "acme.deploy" })
            )
            .as_deref(),
            Some("acme.deploy")
        );
        assert_eq!(f("Bash", json!({})), None);
        assert_eq!(f("mcp__other__sessions__spawn", json!({})), None);
    }
}
