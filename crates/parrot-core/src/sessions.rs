//! Sessions, workspaces and agents (spec 6, 8, 9.1, 9.2, 9.4).

use crate::bindings::{self, BindingInfo, BindingRecord};
use crate::principals::{check, check_session, parse, principal_of};
use crate::{Cx, Effect, Reply, State, queue, subscriptions};
use parrot_proto::agent::Control;
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::session::{
    Background, BindingStatus, BindingVia, McpServer, SessionArchiveParams, SessionListParams,
    SessionNewParams, SessionRef, SessionRenameParams, SessionStartParams, SessionStopParams,
};
use parrot_proto::state::State as SessionState;
use parrot_proto::subscribe::Target;
use parrot_proto::workspace::WorkspaceRequest;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Sessions by id, plus the workspaces and agents they use.
#[derive(Debug)]
pub struct Sessions {
    /// The records every module reads through `directory`.
    pub by_id: BTreeMap<String, SessionRecord>,
    /// What this module keeps beside them, by session id.
    pub info: BTreeMap<String, SessionInfo>,
    pub workspaces: BTreeMap<String, WorkspaceRecord>,
    /// Agents as `Agent` JSON (spec 6), by id.
    pub agents: BTreeMap<String, Value>,
    /// Outstanding `agent/list` calls to drivers: the agent asked, and the `agent/refresh` call
    /// waiting on it, if any.
    pub discovery: BTreeMap<u64, (String, Option<(String, RequestId)>)>,
    /// Outstanding `binding/screen` and `binding/keys` calls, to the client call they answer.
    pub calls: BTreeMap<u64, Option<(String, RequestId)>>,
}

impl Default for Sessions {
    fn default() -> Self {
        let agents = crate::agents::builtin();
        Self {
            by_id: BTreeMap::new(),
            info: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            agents,
            discovery: BTreeMap::new(),
            calls: BTreeMap::new(),
        }
    }
}

/// What the core keeps per session; other modules read it through `directory`.
#[derive(Debug, Clone, Default)]
pub struct SessionRecord {
    pub id: String,
    pub parent: Option<String>,
    pub binding: Option<String>,
    pub agent: String,
}

/// The rest of a session, kept by this module.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub title: Option<String>,
    pub workspace: String,
    pub cwd: String,
    pub state: SessionState,
    pub background: Vec<Background>,
    /// Every binding the session has had, oldest first.
    pub bindings: Vec<String>,
    pub origin: Value,
    pub grant: Vec<Scope>,
    pub options: Map<String, Value>,
    pub control: Control,
    pub mcp: Option<Vec<McpServer>>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub archived: bool,
}

impl Default for SessionInfo {
    fn default() -> Self {
        Self {
            title: None,
            workspace: String::new(),
            cwd: String::new(),
            state: SessionState::Stopped,
            background: Vec::new(),
            bindings: Vec::new(),
            origin: json!({ "kind": "runtime" }),
            grant: Vec::new(),
            options: Map::new(),
            control: Control::Native,
            mcp: None,
            created_ms: 0,
            updated_ms: 0,
            archived: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceRecord {
    pub id: String,
    pub path: String,
    pub created_ms: u64,
}

/// RFC 3339 in UTC with milliseconds.
pub fn timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

/// The `Session` JSON (spec 9).
pub fn session_json(state: &State, id: &str) -> Option<Value> {
    let r = state.sessions.by_id.get(id)?;
    let s = state.sessions.info.get(id).cloned().unwrap_or_default();
    let binding = r
        .binding
        .as_deref()
        .and_then(|b| bindings::binding_json(state, b));
    let advisory = r
        .binding
        .as_deref()
        .and_then(|b| state.bindings.info.get(b))
        .is_none_or(|b| b.isolation < parrot_proto::principal::Isolation::Sandbox);
    let mut v = json!({
        "id": r.id,
        "runtime": state.principals.runtime,
        "workspace": s.workspace,
        "cwd": s.cwd,
        "state": s.state,
        "background": s.background,
        "bindings": s.bindings,
        "kind": "root",
        "origin": s.origin,
        "grant": s.grant,
        "ceiling": {},
        "advisory": advisory,
        "created": timestamp(s.created_ms),
        "updated": timestamp(s.updated_ms),
        "archived": s.archived,
    });
    if let Some(t) = &s.title {
        v["title"] = json!(t);
    }
    if let Some(b) = binding {
        v["binding"] = b;
    }
    if let Some(m) = &s.mcp {
        v["mcp"] = json!(m);
    }
    if let Some(p) = &r.parent {
        v["parent"] = json!({ "session": p });
        v["kind"] = json!("child");
    }
    Some(v)
}

/// The `SessionSummary` JSON (spec 9).
pub fn summary_json(state: &State, id: &str) -> Option<Value> {
    let r = state.sessions.by_id.get(id)?;
    let s = state.sessions.info.get(id).cloned().unwrap_or_default();
    let mut v = json!({
        "id": r.id,
        "runtime": state.principals.runtime,
        "state": s.state,
        "kind": if r.parent.is_some() { "child" } else { "root" },
        "agent": r.agent,
        "env": state.principals.env,
        "archived": s.archived,
        "updated": timestamp(s.updated_ms),
    });
    if let Some(t) = &s.title {
        v["title"] = json!(t);
    }
    if let Some(p) = &r.parent {
        v["parent"] = json!(p);
    }
    Some(v)
}

/// Persist a session and publish it to its subscribers and the index.
pub fn changed(cx: &mut Cx, id: &str) {
    if let Some(s) = cx.state.sessions.info.get_mut(id) {
        s.updated_ms = s.updated_ms.max(cx.now_ms);
    }
    let (Some(session), Some(summary)) = (session_json(cx.state, id), summary_json(cx.state, id)) else {
        return;
    };
    // The stored form keeps what the `Session` JSON doesn't carry, for a restart.
    let core = cx.state.sessions.info.get(id).map(|s| {
        json!({ "options": s.options, "control": s.control, "createdMs": s.created_ms, "updatedMs": s.updated_ms })
    });
    let agent = cx.state.sessions.by_id.get(id).map(|r| r.agent.clone());
    cx.persist(json!({ "op": "put", "kind": "session", "id": id,
        "value": { "session": session, "agent": agent, "core": core } }));
    cx.publish(
        Some(id.to_string()),
        json!({ "type": "session", "session": session }),
    );
    cx.publish(
        Some(id.to_string()),
        json!({ "type": "summary", "session": summary }),
    );
}

/// Put back a session read from the store (the shape `changed` persists). Its state is
/// `stopped`: nothing runs after a restart until something starts it (spec 34).
pub fn restore(state: &mut State, id: &str, value: &Value) {
    let Some(s) = value.get("session") else {
        return;
    };
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let core = value.get("core").cloned().unwrap_or(Value::Null);
    let state_now: SessionState = s
        .get("state")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or(SessionState::Stopped);
    let state_now = match state_now {
        SessionState::Lost => SessionState::Lost,
        _ => SessionState::Stopped,
    };
    let ms = |k: &str, field: &str| {
        core.get(k)
            .and_then(Value::as_u64)
            .or_else(|| {
                s.get(field)
                    .and_then(Value::as_str)
                    .and_then(crate::requests::parse_rfc3339_ms)
            })
            .unwrap_or(0)
    };
    state.sessions.by_id.insert(
        id.to_string(),
        SessionRecord {
            id: id.to_string(),
            parent: s
                .pointer("/parent/session")
                .and_then(Value::as_str)
                .map(str::to_string),
            binding: s
                .pointer("/binding/id")
                .and_then(Value::as_str)
                .map(str::to_string),
            agent: str_of(value, "agent")
                .or_else(|| {
                    s.pointer("/binding/agent")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default(),
        },
    );
    state.sessions.info.insert(
        id.to_string(),
        SessionInfo {
            title: str_of(s, "title"),
            workspace: str_of(s, "workspace").unwrap_or_default(),
            cwd: str_of(s, "cwd").unwrap_or_default(),
            state: state_now,
            background: Vec::new(),
            bindings: s
                .get("bindings")
                .and_then(|b| serde_json::from_value(b.clone()).ok())
                .unwrap_or_default(),
            origin: s
                .get("origin")
                .cloned()
                .unwrap_or_else(|| json!({ "kind": "runtime" })),
            grant: s
                .get("grant")
                .and_then(|g| serde_json::from_value(g.clone()).ok())
                .unwrap_or_default(),
            options: core
                .get("options")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            control: core
                .get("control")
                .and_then(|c| serde_json::from_value(c.clone()).ok())
                .unwrap_or(Control::Native),
            mcp: s.get("mcp").and_then(|m| serde_json::from_value(m.clone()).ok()),
            created_ms: ms("createdMs", "created"),
            updated_ms: ms("updatedMs", "updated"),
            archived: s.get("archived").and_then(Value::as_bool).unwrap_or(false),
        },
    );
}

/// Put back a workspace read from the store.
pub fn restore_workspace(state: &mut State, id: &str, value: &Value) {
    let Some(path) = value.get("path").and_then(Value::as_str) else {
        return;
    };
    let created_ms = value
        .get("created")
        .and_then(Value::as_str)
        .and_then(crate::requests::parse_rfc3339_ms)
        .unwrap_or(0);
    state.sessions.workspaces.insert(
        id.to_string(),
        WorkspaceRecord {
            id: id.to_string(),
            path: path.to_string(),
            created_ms,
        },
    );
}

/// Ask an agent's driver for its agents, models and options (spec 6, 30.1). `reply_to` is an
/// `agent/refresh` call to answer once the driver has.
pub fn discover(cx: &mut Cx, agent: &str, reply_to: Option<(String, RequestId)>) {
    let call = cx.next_call();
    cx.state
        .sessions
        .discovery
        .insert(call, (agent.to_string(), reply_to));
    cx.emit(Effect::AgentCall {
        agent: agent.to_string(),
        call,
        method: "agent/list".into(),
        params: json!({}),
    });
}

/// A driver answered `agent/list`: its agents replace what the runtime knew, and go to the index.
pub fn on_agent_reply(cx: &mut Cx, agent: &str, call: u64, result: Result<Value, RpcError>) {
    let Some((asked, reply_to)) = cx.state.sessions.discovery.remove(&call) else {
        return;
    };
    let asked = if asked.is_empty() {
        agent.to_string()
    } else {
        asked
    };
    let mut found: Vec<Value> = match &result {
        Ok(v) => v
            .get("agents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.get("id").and_then(Value::as_str).is_some())
            .collect(),
        Err(_) => Vec::new(),
    };
    if found.is_empty() {
        // The driver can't be asked: keep what is known, marked unusable with the reason.
        let detail = match &result {
            Err(e) => e.message.clone(),
            Ok(_) => "the driver reported no agents".into(),
        };
        let mut a = cx
            .state
            .sessions
            .agents
            .get(&asked)
            .cloned()
            .unwrap_or_else(|| json!({ "id": asked }));
        a["usable"] = json!(false);
        a["detail"] = json!(detail);
        found.push(a);
    }
    for mut a in found {
        a["env"] = json!(cx.state.principals.env);
        let id = a["id"].as_str().unwrap_or_default().to_string();
        cx.persist(json!({ "op": "put", "kind": "agent", "id": id, "value": a }));
        cx.publish(None, json!({ "type": "agent", "agent": a }));
        cx.state.sessions.agents.insert(id, a);
    }
    if let Some((conn, id)) = reply_to {
        let agent = cx.state.sessions.agents.get(&asked).cloned();
        let result = match (agent, result) {
            (_, Err(e)) => Err(e),
            (Some(a), Ok(_)) => Ok(json!({ "agent": a })),
            (None, Ok(_)) => Err(not_found("agent", &asked)),
        };
        cx.reply(&conn, id, result);
    }
}

/// Start a new binding for a session: emits `StartBinding` with the `BindingStart` of spec 30.1.
pub fn start_binding(cx: &mut Cx, session: &str, via: BindingVia) -> Option<String> {
    let agent = cx.state.sessions.by_id.get(session)?.agent.clone();
    let s = cx.state.sessions.info.get(session).cloned().unwrap_or_default();
    let resume = s
        .bindings
        .last()
        .and_then(|b| cx.state.bindings.info.get(b))
        .and_then(|b| b.reference.clone());
    let via = if resume.is_some() { BindingVia::Resume } else { via };
    let id = cx.ids.next("bnd");
    let workspace = cx
        .state
        .sessions
        .workspaces
        .get(&s.workspace)
        .map(|w| w.path.clone())
        .unwrap_or_else(|| s.cwd.clone());
    let mut start = json!({
        "session": session,
        "binding": id,
        "agent": agent,
        "workspace": workspace,
        "cwd": s.cwd,
        "options": s.options,
        "control": s.control,
        "deny": [],
    });
    if let Some(r) = &resume {
        start["resume"] = json!(r);
    }
    cx.state.bindings.by_id.insert(
        id.clone(),
        BindingRecord {
            id: id.clone(),
            session: session.to_string(),
            agent: agent.clone(),
        },
    );
    cx.state.bindings.info.insert(
        id.clone(),
        BindingInfo {
            control: s.control,
            status: BindingStatus::Starting,
            via,
            started_ms: cx.now_ms,
            ..Default::default()
        },
    );
    if let Some(rec) = cx.state.sessions.by_id.get_mut(session) {
        rec.binding = Some(id.clone());
    }
    if let Some(rec) = cx.state.sessions.info.get_mut(session) {
        rec.bindings.push(id.clone());
        rec.state = SessionState::Starting;
    }
    cx.emit(Effect::StartBinding {
        binding: id.clone(),
        agent,
        start,
    });
    bindings::persist(cx, &id);
    cx.publish(
        Some(session.to_string()),
        json!({ "type": "state", "state": "starting", "background": [] }),
    );
    changed(cx, session);
    Some(id)
}

/// Whether the session's current binding is starting or running.
pub fn active(state: &State, session: &str) -> bool {
    crate::directory::binding_of(state, session)
        .and_then(|b| state.bindings.info.get(&b))
        .is_some_and(|b| matches!(b.status, BindingStatus::Starting | BindingStatus::Running))
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    let result = match method {
        // Prompts belong to the queue, which owns turn dispatch.
        "session/prompt" | "session/cancel" => return queue::call(cx, conn, id, method, params),
        // Moving a session between runtimes belongs to the links (E1).
        "session/offer" | "session/commit" => return crate::links::call(cx, conn, id, method, params),
        "session/new" => new(cx, conn, id, params),
        "session/list" => list(cx, conn, params),
        "session/get" => get(cx, conn, params),
        "session/start" => start(cx, conn, params),
        "session/stop" => stop(cx, conn, params),
        "session/rename" => rename(cx, conn, params),
        "session/archive" => archive(cx, conn, params),
        "session/delete" => delete(cx, conn, params),
        "agent/list" => check(cx.state, conn, Scope::Read).map(|()| json!({ "agents": agents(cx.state) })),
        "agent/refresh" => return refresh(cx, conn, id, params),
        "session/screen" => return tui_call(cx, conn, id, params, "binding/screen"),
        "session/keys" => return tui_call(cx, conn, id, params, "binding/keys"),
        "env/list" => check(cx.state, conn, Scope::Read).map(|()| json!({ "envs": [env_json(cx.state)] })),
        _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
    };
    match result {
        Ok(Value::Null) => Reply::Later,
        other => Reply::Now(other),
    }
}

/// Agents with `env` set.
pub fn agents(state: &State) -> Vec<Value> {
    state
        .sessions
        .agents
        .values()
        .map(|a| {
            let mut a = a.clone();
            a["env"] = json!(state.principals.env);
            a
        })
        .collect()
}

/// The local environment (spec 7).
pub fn env_json(state: &State) -> Value {
    json!({
        "id": state.principals.env,
        "name": "local",
        "runtime": state.principals.runtime,
        "status": "online",
    })
}

fn refresh(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let agent = match check(cx.state, conn, Scope::Configure)
        .and_then(|()| parse::<parrot_proto::agent::AgentRefreshParams>(params))
    {
        Ok(p) => p.agent,
        Err(e) => return Reply::Now(Err(e)),
    };
    if !cx.state.sessions.agents.contains_key(&agent) {
        return Reply::Now(Err(not_found("agent", &agent)));
    }
    discover(cx, &agent, id.map(|id| (conn.to_string(), id.clone())));
    Reply::Later
}

/// `session/screen` and `session/keys` go to the binding as `binding/screen` and `binding/keys`,
/// which the runtime's TUI control answers (Appendix E4).
fn tui_call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value, method: &str) -> Reply {
    let p: SessionRef = match parse(params.clone()) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(e)),
    };
    // Keys need `answer` and `manage` (Appendix E8); the screen only `read` (spec 39).
    let scopes: &[Scope] = if method == "binding/keys" {
        &[Scope::Answer, Scope::Manage]
    } else {
        &[Scope::Read]
    };
    for scope in scopes {
        if let Err(e) = check_session(cx.state, conn, *scope, &p.session) {
            return Reply::Now(Err(e));
        }
    }
    let binding = crate::directory::binding_of(cx.state, &p.session).filter(|_| active(cx.state, &p.session));
    let Some(binding) = binding else {
        return Reply::Now(Err(RpcError::new(-32004, "the session has no running binding")
            .with_data(json!({ "state": "stopped" }))));
    };
    let mut call_params = params;
    if let Some(o) = call_params.as_object_mut() {
        o.remove("session");
        o.insert("binding".into(), json!(binding));
    }
    let call = cx.call_driver(crate::CallOwner::Sessions, &binding, method, call_params);
    cx.state
        .sessions
        .calls
        .insert(call, id.map(|id| (conn.to_string(), id.clone())));
    if id.is_some() {
        Reply::Later
    } else {
        Reply::Now(Ok(json!({})))
    }
}

/// A driver answered `binding/screen` or `binding/keys`. A driver without them answers
/// `-32601`, which the client sees as `-32001`.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(Some((conn, id))) = cx.state.sessions.calls.remove(&call) else {
        return;
    };
    let result = result.map_err(|e| {
        if e.code == -32601 {
            RpcError::new(-32001, "the binding has no screen").with_data(json!({ "feature": "tui" }))
        } else {
            e
        }
    });
    cx.reply(&conn, id, result);
}

/// Read-only access to the state, shaped like `Cx` for code shared with handlers.
struct CxView<'a> {
    state: &'a State,
}

fn not_found(what: &str, id: &str) -> RpcError {
    RpcError::new(-32002, format!("{what} not found: {id}"))
}

/// Returns `Value::Null` once it has replied itself, so the caller's reply precedes the updates.
fn new(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Result<Value, RpcError> {
    let p: SessionNewParams = parse(params)?;
    if p.parent.is_some() || p.seed.is_some() || p.task.is_some() {
        check(cx.state, conn, Scope::Delegate)?;
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "profile.tree" })));
    }
    check(cx.state, conn, Scope::Manage)?;
    if !cx.state.sessions.agents.contains_key(&p.agent) {
        return Err(not_found("agent", &p.agent));
    }
    let (workspace, path) = match &p.workspace {
        WorkspaceRequest::Dir { path } => {
            let existing = cx.state.sessions.workspaces.values().find(|w| &w.path == path);
            match existing {
                Some(w) => (w.id.clone(), w.path.clone()),
                None => {
                    let id = cx.ids.next("wsp");
                    cx.state.sessions.workspaces.insert(
                        id.clone(),
                        WorkspaceRecord {
                            id: id.clone(),
                            path: path.clone(),
                            created_ms: cx.now_ms,
                        },
                    );
                    let w = json!({ "id": id, "env": cx.state.principals.env, "kind": "dir", "path": path,
                        "sessions": [], "created": timestamp(cx.now_ms) });
                    cx.persist(json!({ "op": "put", "kind": "workspace", "id": id, "value": w }));
                    cx.publish(None, json!({ "type": "workspace", "workspace": w }));
                    (id, path.clone())
                }
            }
        }
        WorkspaceRequest::Use { workspace } => {
            let w = cx
                .state
                .sessions
                .workspaces
                .get(workspace)
                .ok_or_else(|| not_found("workspace", workspace))?;
            (w.id.clone(), w.path.clone())
        }
        WorkspaceRequest::Worktree { .. } => {
            return Err(
                RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "workspace.worktree" }))
            );
        }
    };
    let sid = cx.ids.next("ses");
    let origin = json!({ "kind": "client", "principal": principal_of(cx.state, conn) });
    cx.state.sessions.by_id.insert(
        sid.clone(),
        SessionRecord {
            id: sid.clone(),
            agent: p.agent.clone(),
            ..Default::default()
        },
    );
    cx.state.sessions.info.insert(
        sid.clone(),
        SessionInfo {
            title: p.title.clone(),
            workspace,
            cwd: p.cwd.clone().unwrap_or(path),
            origin,
            grant: p.grant.clone().unwrap_or_default(),
            options: p.options.clone().unwrap_or_default(),
            control: p.control.unwrap_or(Control::Native),
            created_ms: cx.now_ms,
            updated_ms: cx.now_ms,
            ..Default::default()
        },
    );
    let (subscription, head) =
        subscriptions::subscribe_conn(cx, conn, Target::Session { session: sid.clone() });
    if p.start.unwrap_or(true) {
        start_binding(cx, &sid, BindingVia::New);
    } else {
        changed(cx, &sid);
    }
    let result = json!({
        "session": session_json(cx.state, &sid),
        "subscription": subscription,
        "head": head,
    });
    if let Some(id) = id {
        cx.reply(conn, id.clone(), Ok(result));
    }
    Ok(Value::Null)
}

fn list(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    check(cx.state, conn, Scope::Read)?;
    let p: SessionListParams = parse(params)?;
    list_page(cx.state, conn, &p)
}

/// One page of the sessions `conn` sees, newest change first.
pub fn list_page(state: &State, conn: &str, p: &SessionListParams) -> Result<Value, RpcError> {
    let cx = &CxView { state };
    let archived = p.archived.unwrap_or(false);
    let matching: Vec<&SessionRecord> = cx
        .state
        .sessions
        .by_id
        .values()
        .filter(|s| {
            let info = cx.state.sessions.info.get(&s.id);
            info.is_none_or(|i| i.archived == archived)
                && p.workspace
                    .as_ref()
                    .is_none_or(|x| info.is_some_and(|i| &i.workspace == x))
        })
        .filter(|s| p.parent.as_ref().is_none_or(|x| s.parent.as_ref() == Some(x)))
        .filter(|s| p.agent.as_ref().is_none_or(|x| &s.agent == x))
        .filter(|s| crate::principals::sees(cx.state, conn, &s.id))
        .filter(|_| p.env.as_ref().is_none_or(|e| e == &cx.state.principals.env))
        .collect();
    let mut matching = matching;
    let updated = |id: &str| cx.state.sessions.info.get(id).map_or(0, |i| i.updated_ms);
    matching.sort_by(|a, b| updated(&b.id).cmp(&updated(&a.id)).then(a.id.cmp(&b.id)));
    let offset: usize = match &p.cursor {
        Some(c) => c.parse().map_err(|_| RpcError::new(-32602, "invalid cursor"))?,
        None => 0,
    };
    let limit = p.limit.unwrap_or(100).clamp(1, 1000) as usize;
    let page: Vec<Value> = matching
        .iter()
        .skip(offset)
        .take(limit)
        .filter_map(|s| summary_json(cx.state, &s.id))
        .collect();
    let mut v = json!({ "sessions": page });
    if offset + limit < matching.len() {
        v["next"] = json!((offset + limit).to_string());
    }
    Ok(v)
}

fn get(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionRef = parse(params)?;
    check_session(cx.state, conn, Scope::Read, &p.session)?;
    Ok(json!({ "session": session_json(cx.state, &p.session) }))
}

fn start(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionStartParams = parse(params)?;
    check_session(cx.state, conn, Scope::Manage, &p.session)?;
    queue::release(cx, &p.session);
    if let Some(options) = p.options
        && let Some(s) = cx.state.sessions.info.get_mut(&p.session)
    {
        s.options.extend(options);
    }
    let binding = if active(cx.state, &p.session) {
        crate::directory::binding_of(cx.state, &p.session)
    } else {
        start_binding(cx, &p.session, BindingVia::New)
    };
    let binding = binding.and_then(|b| bindings::binding_json(cx.state, &b));
    Ok(json!({ "binding": binding }))
}

/// Stop a session's binding, if it has a live one.
pub fn stop_session(cx: &mut Cx, session: &str) {
    if !active(cx.state, session) {
        return;
    }
    let Some(binding) = crate::directory::binding_of(cx.state, session) else {
        return;
    };
    if let Some(b) = cx.state.bindings.info.get_mut(&binding) {
        b.stop_requested = true;
    }
    cx.call_driver(
        crate::CallOwner::Bindings,
        &binding,
        "binding/stop",
        json!({ "binding": binding }),
    );
    cx.emit(Effect::StopBinding { binding });
}

fn stop(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionStopParams = parse(params)?;
    check_session(cx.state, conn, Scope::Manage, &p.session)?;
    stop_session(cx, &p.session);
    Ok(json!({}))
}

fn rename(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionRenameParams = parse(params)?;
    check_session(cx.state, conn, Scope::Manage, &p.session)?;
    if let Some(s) = cx.state.sessions.info.get_mut(&p.session) {
        s.title = Some(p.title);
    }
    changed(cx, &p.session);
    Ok(json!({}))
}

fn archive(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionArchiveParams = parse(params)?;
    check_session(cx.state, conn, Scope::Manage, &p.session)?;
    if let Some(s) = cx.state.sessions.info.get_mut(&p.session) {
        s.archived = p.archived;
    }
    changed(cx, &p.session);
    Ok(json!({}))
}

fn delete(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionRef = parse(params)?;
    check_session(cx.state, conn, Scope::Destructive, &p.session)?;
    stop_session(cx, &p.session);
    cx.publish(
        Some(p.session.clone()),
        json!({ "type": "deleted", "kind": "session", "id": p.session }),
    );
    cx.state.sessions.by_id.remove(&p.session);
    cx.state.sessions.info.remove(&p.session);
    if let Some(items) = cx.state.items.by_session.remove(&p.session) {
        for item in items.order {
            cx.persist(json!({ "op": "delete", "kind": "item", "id": format!("{}/{item}", p.session) }));
        }
    }
    for kind in ["queue", "options", "actions", "usage"] {
        cx.persist(json!({ "op": "delete", "kind": kind, "id": p.session }));
    }
    queue::forget(cx, &p.session);
    cx.persist(json!({ "op": "delete", "kind": "session", "id": p.session }));
    Ok(json!({}))
}
