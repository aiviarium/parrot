//! Tasks (spec 19, 20.1, Appendix E2, E7, E8): assign, finish, fail, wait, and how a child's
//! turns complete its task.

use crate::principals::{check_session, parse, session_principal};
use crate::{Cx, Reply, State};
use parrot_proto::principal::Scope;
use parrot_proto::prompt::QueueEntryKind;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::state::TurnStatus;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `TaskRequest` (spec 19).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRequest {
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_on: Option<String>,
}

/// `Task` (spec 19). `status` is `running`, `awaiting`, `done`, `failed`, `cancelled` or
/// `interrupted` (E7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub complete_on: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub history: Vec<Value>,
}

impl Task {
    /// Still being worked on.
    pub fn open(&self) -> bool {
        matches!(self.status.as_str(), "running" | "awaiting")
    }
}

/// Task changes and the waits on them.
#[derive(Debug, Default)]
pub struct Tasks {
    /// Every change so far, oldest first, with its cursor.
    pub changes: Vec<Change>,
    pub cursor: u64,
    /// `task/wait` calls waiting for a change or their timeout.
    pub waits: BTreeMap<u64, Wait>,
    pub next_wait: u64,
    /// How each session's last turn ended since its task started; `None` before any.
    pub last_turn: BTreeMap<String, TurnStatus>,
    /// Changes recorded for a parent's wake and not yet returned to it, by parent.
    pub undelivered: BTreeMap<String, Vec<u64>>,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub cursor: u64,
    pub session: String,
    pub task: Value,
    /// What woke on it: a task status or `requires_action`.
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct Wait {
    pub conn: String,
    pub id: RequestId,
    pub sessions: Vec<String>,
    pub since: u64,
    pub deadline_ms: u64,
    /// The session principal waiting, whose wake changes the answer delivers.
    pub by: Option<String>,
}

/// The default and longest `task/wait` timeout in seconds, below common tool-call timeouts.
pub const WAIT_MAX: u64 = 50;

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "task/assign" => Reply::Now(assign(cx, conn, params)),
        "task/finish" => Reply::Now(finish(cx, conn, params)),
        "task/fail" => Reply::Now(fail(cx, conn, params)),
        "task/wait" => wait(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

/// A driver reply to a call this module made (`binding/cancel` on budget exhaustion).
pub fn on_reply(_cx: &mut Cx, _call: u64, _result: Result<Value, RpcError>) {}

fn task_of(state: &State, session: &str) -> Option<Task> {
    state.tree.nodes.get(session).and_then(|n| n.task.clone())
}

/// The default of `completeOn`: `finish` when the agent has functions over MCP, else `idle`.
fn default_complete_on(state: &State, session: &str) -> &'static str {
    let mcp = state
        .sessions
        .by_id
        .get(session)
        .and_then(|r| state.sessions.agents.get(&r.agent))
        .and_then(|a| a.get("features"))
        .is_some_and(|f| f.get("functions.mcp").is_some());
    if mcp { "finish" } else { "idle" }
}

/// Start a task on a session: record it and queue its brief as the next prompt. `bundle` is
/// prepended by the queue when a seed left one pending.
pub fn begin(cx: &mut Cx, session: &str, req: TaskRequest) -> Task {
    let history = task_of(cx.state, session)
        .map(|t| {
            let mut h = t.history.clone();
            let mut entry = json!({ "id": t.id, "status": t.status });
            if let Some(o) = t.output {
                entry["output"] = o;
            }
            h.push(entry);
            h
        })
        .unwrap_or_default();
    let complete_on = req
        .complete_on
        .clone()
        .filter(|c| c == "finish" || c == "idle")
        .unwrap_or_else(|| default_complete_on(cx.state, session).to_string());
    let task = Task {
        id: cx.ids.next("tsk"),
        brief: req.brief.clone(),
        title: req.title.clone(),
        result: req.result.clone(),
        complete_on,
        status: "running".into(),
        output: None,
        error: None,
        history,
    };
    cx.state.tasks.last_turn.remove(session);
    if let Some(n) = cx.state.tree.nodes.get_mut(session) {
        n.task = Some(task.clone());
    }
    crate::tree::persist(cx, session);
    record(cx, session, "running");
    let origin = json!({ "kind": "task", "task": task.id });
    crate::queue::enqueue(cx, session, QueueEntryKind::Prompt, req.brief, origin, json!({}));
    crate::queue::dispatch(cx, session);
    crate::sessions::changed(cx, session);
    task
}

/// Change a task's status; records the change, answers waits, wakes the parent, and gives the
/// budget back when the task ends.
pub fn set_status(cx: &mut Cx, session: &str, status: &str, output: Option<Value>, error: Option<String>) {
    let Some(t) = cx.state.tree.nodes.get_mut(session).and_then(|n| n.task.as_mut()) else {
        return;
    };
    if t.status == status && output.is_none() && error.is_none() {
        return;
    }
    t.status = status.to_string();
    if output.is_some() {
        t.output = output;
    }
    if error.is_some() {
        t.error = error;
    }
    cx.state.tasks.last_turn.remove(session);
    crate::tree::persist(cx, session);
    record(cx, session, status);
    if matches!(status, "done" | "failed" | "cancelled") {
        crate::budgets::give_back(cx, session);
    }
    if status == "interrupted" {
        cx.publish(
            Some(session.to_string()),
            json!({ "type": "notice", "level": "warning", "text": "the task was interrupted by a restart", "code": "interrupted" }),
        );
    }
    crate::sessions::changed(cx, session);
}

/// Record a change of a session's task or state for waits and its parent's wake.
pub fn record(cx: &mut Cx, session: &str, status: &str) {
    let task = task_of(cx.state, session)
        .map(|t| json!(t))
        .unwrap_or(Value::Null);
    cx.state.tasks.cursor += 1;
    let cursor = cx.state.tasks.cursor;
    cx.state.tasks.changes.push(Change {
        cursor,
        session: session.to_string(),
        task,
        status: status.to_string(),
    });
    let parent = cx
        .state
        .sessions
        .by_id
        .get(session)
        .and_then(|r| r.parent.clone());
    if let Some(parent) = parent {
        let wake_on = cx
            .state
            .tree
            .nodes
            .get(&parent)
            .and_then(|n| n.wake.clone())
            .map(|w| w.on)
            .unwrap_or_default();
        // Interrupted tasks always wake (E7).
        if status == "interrupted" || wake_on.iter().any(|w| w == status) {
            cx.state
                .tasks
                .undelivered
                .entry(parent.clone())
                .or_default()
                .push(cursor);
            crate::wakes::pump(cx, &parent);
        }
    }
    resolve_waits(cx, false);
}

/// Answer waits that have changes now, or whose time is up when `expired`.
fn resolve_waits(cx: &mut Cx, expired: bool) {
    let now = cx.now_ms;
    let ids: Vec<u64> = cx.state.tasks.waits.keys().copied().collect();
    for wid in ids {
        let Some(w) = cx.state.tasks.waits.get(&wid).cloned() else {
            continue;
        };
        let changes = changes_since(cx.state, &w.sessions, w.since);
        let timed_out = expired && w.deadline_ms <= now;
        if changes.is_empty() && !timed_out {
            continue;
        }
        cx.state.tasks.waits.remove(&wid);
        let result = answer(cx, w.by.as_deref(), changes, timed_out);
        cx.reply(&w.conn, w.id, Ok(result));
    }
}

fn changes_since(state: &State, sessions: &[String], since: u64) -> Vec<Change> {
    // Only the newest change per session is returned; earlier ones are superseded.
    let mut latest: BTreeMap<String, Change> = BTreeMap::new();
    for c in state.tasks.changes.iter().filter(|c| c.cursor > since) {
        if sessions.contains(&c.session) {
            latest.insert(c.session.clone(), c.clone());
        }
    }
    let mut v: Vec<Change> = latest.into_values().collect();
    v.sort_by_key(|c| c.cursor);
    v
}

/// The `task/wait` result; changes returned to the waiting parent are no longer woken on.
fn answer(cx: &mut Cx, by: Option<&str>, changes: Vec<Change>, timed_out: bool) -> Value {
    if let Some(by) = by {
        let tasks = &mut cx.state.tasks;
        let returned: Vec<&str> = changes.iter().map(|c| c.session.as_str()).collect();
        if let Some(list) = tasks.undelivered.get_mut(by) {
            let all = &tasks.changes;
            list.retain(|cur| {
                !all.iter()
                    .any(|c| c.cursor == *cur && returned.contains(&c.session.as_str()))
            });
        }
    }
    let out: Vec<Value> = changes
        .iter()
        .map(|c| json!({ "session": c.session, "task": current_task(cx.state, &c.session) }))
        .collect();
    json!({ "changes": out, "cursor": cx.state.tasks.cursor.to_string(), "timedOut": timed_out })
}

fn current_task(state: &State, session: &str) -> Value {
    task_of(state, session).map(|t| json!(t)).unwrap_or(Value::Null)
}

/// Time passed: answer waits whose timeout is up.
pub fn tick(cx: &mut Cx) {
    resolve_waits(cx, true);
}

/// A connection closed: its waits go with it.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.tasks.waits.retain(|_, w| w.conn != conn);
}

fn parse_cursor(v: &Value) -> Result<Option<u64>, RpcError> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(n.as_u64()),
        Value::String(s) => s
            .parse()
            .map(Some)
            .map_err(|_| RpcError::new(-32602, "invalid cursor")),
        _ => Err(RpcError::new(-32602, "invalid cursor")),
    }
}

fn wait(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let sessions: Vec<String> = match params
        .get("sessions")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
    {
        Some(s) => s,
        None => return Reply::Now(Err(RpcError::new(-32602, "invalid params: sessions"))),
    };
    for s in &sessions {
        if let Err(e) = check_session(cx.state, conn, Scope::Read, s) {
            return Reply::Now(Err(e));
        }
    }
    let since = match parse_cursor(params.get("since").unwrap_or(&Value::Null)) {
        Ok(s) => s,
        Err(e) => return Reply::Now(Err(e)),
    };
    let by = session_principal(cx.state, conn).map(str::to_string);
    let Some(since) = since else {
        // The first call gets the current state.
        let changes: Vec<Value> = sessions
            .iter()
            .map(|s| json!({ "session": s, "task": current_task(cx.state, s) }))
            .collect();
        return Reply::Now(Ok(
            json!({ "changes": changes, "cursor": cx.state.tasks.cursor.to_string(), "timedOut": false }),
        ));
    };
    let changes = changes_since(cx.state, &sessions, since);
    let timeout = params
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(WAIT_MAX)
        .min(WAIT_MAX);
    if !changes.is_empty() || timeout == 0 || id.is_none() {
        let empty = changes.is_empty();
        return Reply::Now(Ok(answer(cx, by.as_deref(), changes, empty)));
    }
    let deadline_ms = cx.now_ms + timeout * 1000;
    cx.state.tasks.next_wait += 1;
    let wid = cx.state.tasks.next_wait;
    cx.state.tasks.waits.insert(
        wid,
        Wait {
            conn: conn.to_string(),
            id: id.cloned().expect("checked above"),
            sessions,
            since,
            deadline_ms,
            by,
        },
    );
    cx.emit(crate::Effect::TimerAt { at_ms: deadline_ms });
    Reply::Later
}

fn assign(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: session"))?
        .to_string();
    check_session(cx.state, conn, Scope::Delegate, &session)?;
    if session_principal(cx.state, conn) == Some(session.as_str()) {
        return Err(crate::ceilings::forbidden(
            "subtree",
            "a session can't assign itself a task",
        ));
    }
    let req: TaskRequest = parse(params.get("task").cloned().unwrap_or(Value::Null))?;
    if task_of(cx.state, &session).is_some_and(|t| t.open()) {
        return Err(
            RpcError::new(-32004, "the session's task hasn't ended").with_data(json!({ "state": "running" }))
        );
    }
    let task = begin(cx, &session, req);
    Ok(json!({ "task": task }))
}

fn own_session(params: &Value) -> Result<String, RpcError> {
    params
        .get("session")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: session"))
}

fn finish(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let session = own_session(&params)?;
    if session_principal(cx.state, conn) != Some(session.as_str()) {
        return Err(crate::ceilings::forbidden(
            "principal",
            "task/finish is for the session's own principal",
        ));
    }
    let task = task_of(cx.state, &session)
        .filter(Task::open)
        .ok_or_else(|| RpcError::new(-32004, "the session has no running task"))?;
    let output = params.get("output").cloned().unwrap_or(Value::Null);
    let output = match &task.result {
        Some(schema) => {
            validate(schema, &output, "output").map_err(|e| RpcError::new(-32602, e))?;
            output
        }
        None => match output {
            Value::String(s) => json!({ "text": s }),
            other => other,
        },
    };
    set_status(cx, &session, "done", Some(output), None);
    Ok(json!({}))
}

fn fail(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let session = own_session(&params)?;
    // By the session itself it needs nothing; by anyone else, `delegate` on its subtree (E8).
    if session_principal(cx.state, conn) != Some(session.as_str()) {
        check_session(cx.state, conn, Scope::Delegate, &session)?;
    }
    if !task_of(cx.state, &session).is_some_and(|t| t.open()) {
        return Err(RpcError::new(-32004, "the session has no running task"));
    }
    let error = params
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("failed")
        .to_string();
    set_status(cx, &session, "failed", None, Some(error));
    Ok(json!({}))
}

/// A small JSON Schema check: `type`, `required`, `properties`, `items` and `enum`.
pub fn validate(schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(v)
    {
        return Err(format!("{at} must be one of {}", Value::Array(options.clone())));
    }
    if let Some(ty) = schema.get("type") {
        let types: Vec<&str> = match ty {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let ok = types.is_empty()
            || types.iter().any(|t| match *t {
                "object" => v.is_object(),
                "array" => v.is_array(),
                "string" => v.is_string(),
                "number" => v.is_number(),
                "integer" => v.as_i64().is_some() || v.as_u64().is_some(),
                "boolean" => v.is_boolean(),
                "null" => v.is_null(),
                _ => true,
            });
        if !ok {
            return Err(format!("{at} must be of type {}", types.join(" or ")));
        }
    }
    if let Some(obj) = v.as_object() {
        if let Some(req) = schema.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    return Err(format!("{at}.{r} is required"));
                }
            }
        }
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (k, s) in props {
                if let Some(x) = obj.get(k) {
                    validate(s, x, &format!("{at}.{k}"))?;
                }
            }
        }
    }
    if let (Some(items), Some(arr)) = (schema.get("items"), v.as_array()) {
        for (i, x) in arr.iter().enumerate() {
            validate(items, x, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

/// A turn of a session ended or started.
pub fn on_turn(cx: &mut Cx, session: &str, status: TurnStatus, first: bool) {
    let Some(task) = task_of(cx.state, session) else {
        return;
    };
    if status == TurnStatus::Running {
        // A prompt or `continue` after the child stopped without a result (spec 19).
        if first && task.status == "awaiting" {
            set_status(cx, session, "running", None, None);
        }
        return;
    }
    if task.open() {
        cx.state.tasks.last_turn.insert(session.to_string(), status);
    }
}

/// A session went idle: complete or park its task (spec 19, E8).
pub fn on_idle(cx: &mut Cx, session: &str) {
    let Some(task) = task_of(cx.state, session).filter(|t| t.status == "running") else {
        return;
    };
    let Some(last) = cx.state.tasks.last_turn.get(session).copied() else {
        return;
    };
    let queued = cx
        .state
        .queue
        .by_session
        .get(session)
        .is_some_and(|q| !q.entries.is_empty() && !q.paused);
    if queued {
        return;
    }
    let blocked = crate::requests::open_in(cx.state, session)
        .iter()
        .any(|r| r.session == session && r.blocking);
    if blocked {
        return;
    }
    if task.complete_on == "idle" && last == TurnStatus::Completed {
        let text = last_assistant(cx.state, session).unwrap_or_default();
        set_status(cx, session, "done", Some(json!({ "text": text })), None);
    } else {
        set_status(cx, session, "awaiting", None, None);
    }
}

/// The text of the newest assistant item.
pub fn last_assistant(state: &State, session: &str) -> Option<String> {
    let items = state.items.by_session.get(session)?;
    items.order.iter().rev().find_map(|id| {
        let item = items.by_id.get(id)?;
        (item.get("kind").and_then(Value::as_str) == Some("assistant"))
            .then(|| item.get("text").and_then(Value::as_str).map(str::to_string))
            .flatten()
    })
}

/// A request opened in a session: its parent may wake on `requires_action`.
pub fn on_request(cx: &mut Cx, session: &str) {
    let parent = cx
        .state
        .sessions
        .by_id
        .get(session)
        .and_then(|r| r.parent.clone());
    if parent.is_some() {
        record(cx, session, "requires_action");
    }
}

/// A session was stopped by a client or its parent: a running task is cancelled (spec 19).
pub fn on_stop(cx: &mut Cx, session: &str) {
    if task_of(cx.state, session).is_some_and(|t| t.open()) {
        set_status(cx, session, "cancelled", None, None);
    }
}

/// After a restart: running tasks become `interrupted`, which wakes and notifies (E7).
pub fn after_restore(cx: &mut Cx) {
    let running: Vec<String> = cx
        .state
        .tree
        .nodes
        .iter()
        .filter(|(_, n)| n.task.as_ref().is_some_and(|t| t.status == "running"))
        .map(|(s, _)| s.clone())
        .collect();
    for s in running {
        set_status(cx, &s, "interrupted", None, None);
    }
}
