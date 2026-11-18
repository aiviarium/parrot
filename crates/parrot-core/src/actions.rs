//! Actions (spec 16), with the runtime's `continue` and `retry` fallbacks (spec 33, Appendix E8).

use std::collections::{BTreeMap, BTreeSet};

use crate::requests::{allows, forbidden, invalid, wrong_state};
use crate::{CallOwner, Cx, Reply, State, directory};
use parrot_proto::action::{
    Action, ActionInvokeParams, ActionInvokeResult, ActionListParams, ActionListResult, ActionScope,
    ActionSource, ActionStatus, ActionWhen, Effect,
};
use parrot_proto::driver::{BindingActionParams, BindingActionResult, DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Map, Value, json};

/// Actions by session, and what the fallbacks need to know about turns.
#[derive(Debug, Default)]
pub struct Actions {
    /// The actions the driver last reported, by session.
    pub by_session: BTreeMap<String, Vec<Action>>,
    /// Sessions with a running turn.
    pub running: BTreeSet<String>,
    /// The inputs of each session's last failed turn, cleared by a turn that doesn't fail.
    pub failed_inputs: BTreeMap<String, Vec<String>>,
    /// Outstanding driver calls by call id.
    pub calls: BTreeMap<u64, PendingCall>,
    /// Per session, the text of a prompt a restart left `uncertain`, which `retry` resends.
    pub uncertain: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct PendingCall {
    pub session: String,
    pub binding: String,
    pub reply_to: Option<(String, RequestId)>,
    /// The user item id of a fallback prompt.
    pub message: Option<String>,
}

/// A session's turn started (`running`) or ended; `failed` holds a failed turn's input texts.
pub fn note_turn(state: &mut State, session: &str, running: bool, failed: Option<Vec<String>>) {
    if running {
        state.actions.running.insert(session.to_string());
        return;
    }
    state.actions.running.remove(session);
    match failed {
        Some(inputs) => state.actions.failed_inputs.insert(session.to_string(), inputs),
        None => state.actions.failed_inputs.remove(session),
    };
}

fn runtime_action(id: &str, label: &str, description: &str, input: Option<Value>) -> Action {
    Action {
        id: id.into(),
        name: None,
        label: label.into(),
        description: Some(description.into()),
        input,
        hint: None,
        scope: ActionScope::Session,
        when: vec![ActionWhen::Idle],
        effects: vec![Effect::Turn],
        requires: None,
        source: Open::Known(ActionSource::Runtime),
        meta: None,
        extra: Map::new(),
    }
}

/// The session's actions, with the runtime's `continue` and `retry` where the harness has none.
pub fn actions_of(state: &State, session: &str) -> Vec<Action> {
    let mut actions = state.actions.by_session.get(session).cloned().unwrap_or_default();
    if !actions.iter().any(|a| a.id == "continue") {
        let input = json!({ "type": "object", "properties": { "note": { "type": "string" } } });
        actions.push(runtime_action(
            "continue",
            "Continue",
            "Carry on where the last turn stopped.",
            Some(input),
        ));
    }
    if !actions.iter().any(|a| a.id == "retry") {
        actions.push(runtime_action(
            "retry",
            "Retry",
            "Run the failed turn's input again. The earlier attempt may already have changed files.",
            None,
        ));
    }
    actions
}

/// The scopes an action's effects and scope need (spec 5.2).
fn scopes_for(action: &Action) -> Vec<&'static str> {
    let mut scopes = Vec::new();
    if action.scope == ActionScope::Agent {
        scopes.push("manage");
    }
    if action.effects.contains(&Effect::Destructive) {
        scopes.push("destructive");
    }
    if action.effects.contains(&Effect::Write) {
        scopes.push("configure");
    }
    if scopes.is_empty() {
        scopes.push("prompt");
    }
    scopes
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "action/list" => Reply::Now(list(cx, conn, params)),
        "action/invoke" => match invoke(cx, conn, id, params) {
            Ok(reply) => reply,
            Err(e) => Reply::Now(Err(e)),
        },
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn list(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    if !allows(cx.state, conn, "read") {
        return Err(forbidden("read"));
    }
    let params = if params.is_null() { json!({}) } else { params };
    let p: ActionListParams = serde_json::from_value(params).map_err(|e| invalid(e.to_string()))?;
    let actions = match &p.session {
        Some(s) if cx.state.sessions.by_id.contains_key(s) => actions_of(cx.state, s),
        Some(s) => return Err(RpcError::new(-32002, format!("no session {s}"))),
        None => Vec::new(),
    };
    Ok(serde_json::to_value(ActionListResult { actions }).unwrap_or_default())
}

fn invoke(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Result<Reply, RpcError> {
    let p: ActionInvokeParams = serde_json::from_value(params).map_err(|e| invalid(e.to_string()))?;
    let Some(session) = p.session.clone() else {
        return Err(RpcError::new(-32001, "only session actions are supported")
            .with_data(json!({ "feature": "action.agent" })));
    };
    if !cx.state.sessions.by_id.contains_key(&session) {
        return Err(RpcError::new(-32002, format!("no session {session}")));
    }
    let Some(action) = actions_of(cx.state, &session)
        .into_iter()
        .find(|a| a.id == p.action)
    else {
        return Err(RpcError::new(-32002, format!("no action {}", p.action)));
    };
    for scope in scopes_for(&action) {
        if !allows(cx.state, conn, scope) {
            return Err(forbidden(scope));
        }
    }
    let active = crate::sessions::active(cx.state, &session);
    if !active {
        // An action starts a stopped session (spec 34), which needs `manage` too.
        if !allows(cx.state, conn, "manage") {
            return Err(forbidden("manage"));
        }
        return Ok(Reply::Now(Ok(start_and_queue(cx, &session, &action, &p)?)));
    }
    crate::queue::release(cx, &session);
    let Some(binding) = directory::binding_of(cx.state, &session) else {
        return Err(wrong_state("stopped", "the session has no running binding"));
    };
    let running = cx.state.actions.running.contains(&session);
    let reply_to = id.map(|id| (conn.to_string(), id.clone()));
    let later = reply_to.is_some();

    if action.source == Open::Known(ActionSource::Runtime) {
        // Queueing behind a running turn belongs to the queue.
        if running {
            return Err(wrong_state("running", "the session is running"));
        }
        let text = fallback_text(cx.state, &session, &action.id, &p)?;
        if action.id == "retry" {
            cx.state.actions.uncertain.remove(&session);
        }
        let message = cx.ids.next("msg");
        crate::queue::inflight_begin(cx, &session, &message, &text);
        let params = json!({
            "binding": binding,
            "message": message,
            "text": text,
            "steer": false,
            "origin": { "kind": "runtime", "action": action.id },
        });
        let call = cx.call_driver(CallOwner::Actions, &binding, "binding/prompt", params);
        cx.state.actions.calls.insert(
            call,
            PendingCall {
                session,
                binding,
                reply_to,
                message: Some(message.clone()),
            },
        );
        return Ok(if later {
            Reply::Later
        } else {
            Reply::Now(Ok(json!({ "status": "started", "message": message })))
        });
    }

    let when = if running {
        ActionWhen::Running
    } else {
        ActionWhen::Idle
    };
    if !action.when.is_empty() && !action.when.contains(&when) {
        let state = if running { "running" } else { "idle" };
        return Err(wrong_state(
            state,
            format!("{} isn't available while {state}", action.id),
        ));
    }
    let message = action.effects.contains(&Effect::Turn).then(|| cx.ids.next("msg"));
    let params = BindingActionParams {
        binding: Some(binding.clone()),
        agent: None,
        action: action.id.clone(),
        input: p.input,
        args: p.args,
        message: message.clone(),
    };
    let call = cx.call_driver(
        CallOwner::Actions,
        &binding,
        "binding/action",
        serde_json::to_value(params).unwrap_or_default(),
    );
    cx.state.actions.calls.insert(
        call,
        PendingCall {
            session,
            binding,
            reply_to,
            message,
        },
    );
    Ok(if later {
        Reply::Later
    } else {
        Reply::Now(Ok(json!({ "status": "started" })))
    })
}

/// The prompt text of the runtime's `continue` and `retry` (spec 33, Appendix E8).
fn fallback_text(
    state: &State,
    session: &str,
    action: &str,
    p: &ActionInvokeParams,
) -> Result<String, RpcError> {
    match action {
        "continue" => {
            let note = p
                .input
                .as_ref()
                .and_then(|i| i.get("note"))
                .and_then(Value::as_str);
            Ok(match note {
                Some(n) if !n.is_empty() => format!("Continue.\n{n}"),
                _ => "Continue.".to_string(),
            })
        }
        // A prompt a restart left `uncertain` comes first: it was the last thing sent.
        "retry" => match (
            state.actions.uncertain.get(session),
            state.actions.failed_inputs.get(session).map(Vec::as_slice),
        ) {
            (Some(text), _) => Ok(text.clone()),
            (None, Some([only])) => Ok(only.clone()),
            _ => Err(wrong_state(
                "idle",
                "retry needs a failed or uncertain turn with exactly one input",
            )),
        },
        other => Err(RpcError::new(-32002, format!("no action {other}"))),
    }
}

/// Start a stopped session's binding and queue the action to run once it is ready.
fn start_and_queue(
    cx: &mut Cx,
    session: &str,
    action: &Action,
    p: &ActionInvokeParams,
) -> Result<Value, RpcError> {
    use parrot_proto::prompt::QueueEntryKind;
    let runtime = action.source == Open::Known(ActionSource::Runtime);
    let entry = if runtime {
        let text = fallback_text(cx.state, session, &action.id, p)?;
        if action.id == "retry" {
            cx.state.actions.uncertain.remove(session);
        }
        let origin = json!({ "kind": "runtime", "action": action.id });
        (
            QueueEntryKind::Prompt,
            text,
            origin.clone(),
            json!({ "origin": origin }),
        )
    } else {
        let mut params = json!({ "action": action.id });
        if let Some(i) = &p.input {
            params["input"] = i.clone();
        }
        if let Some(a) = &p.args {
            params["args"] = json!(a);
        }
        let origin = json!({ "kind": "runtime", "action": action.id });
        (QueueEntryKind::Action, action.id.clone(), origin, params)
    };
    crate::queue::release(cx, session);
    crate::sessions::start_binding(cx, session, parrot_proto::session::BindingVia::New);
    let message = crate::queue::enqueue(cx, session, entry.0, entry.1, entry.2, entry.3);
    let mut out = json!({ "status": "queued" });
    if let Some(m) = message {
        out["message"] = json!(m);
    }
    Ok(out)
}

/// Put back a session's actions read from the store.
pub fn restore(state: &mut State, session: &str, value: &Value) {
    if let Ok(actions) = serde_json::from_value(value.clone()) {
        state.actions.by_session.insert(session.to_string(), actions);
    }
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(pending) = cx.state.actions.calls.remove(&call) else {
        return;
    };
    if let Some(m) = &pending.message {
        crate::queue::inflight_end(cx, m);
    }
    let Some((conn, id)) = pending.reply_to else {
        return;
    };
    let result = result.map(|v| {
        // A fallback prompt answers with a disposition; a driver action with a status.
        let out = match serde_json::from_value::<BindingActionResult>(v.clone()) {
            Ok(r) => ActionInvokeResult {
                status: r.status,
                message: pending.message.clone(),
                request: r.request.map(|key| {
                    crate::requests::derive_id(&pending.session, &pending.binding, "request", &key)
                }),
                result: r.result,
            },
            Err(_) => {
                let handled = v.get("disposition").and_then(Value::as_str) == Some("handled");
                ActionInvokeResult {
                    status: if handled {
                        ActionStatus::Done
                    } else {
                        ActionStatus::Started
                    },
                    message: pending.message.clone(),
                    request: None,
                    result: None,
                }
            }
        };
        serde_json::to_value(out).unwrap_or_default()
    });
    cx.reply(&conn, id, result);
}

/// A driver update of this area.
pub fn on_update(cx: &mut Cx, update: &DriverUpdate) {
    let Open::Known(DriverUpdateKind::Actions { actions }) = &update.update else {
        return;
    };
    let Some(session) = directory::session_of(cx.state, &update.binding) else {
        return;
    };
    cx.state
        .actions
        .by_session
        .insert(session.clone(), actions.clone());
    cx.persist(json!({ "op": "put", "kind": "actions", "id": session, "value": actions }));
    let actions = actions_of(cx.state, &session);
    cx.publish(Some(session), json!({ "type": "actions", "actions": actions }));
}
