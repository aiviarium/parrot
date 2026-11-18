//! Prompts and the queue (spec 13, Appendix E4, E8). The runtime owns the queue; drivers never
//! queue.

use crate::principals::{check, check_session, parse, principal_of};
use crate::{CallOwner, Cx, Reply, State, sessions};
use parrot_proto::principal::Scope;
use parrot_proto::prompt::{
    Delivery, QueueEntryKind, QueueMoveParams, QueueRemoveParams, QueueSessionParams, SessionCancelParams,
    SessionPromptParams,
};
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::session::BindingVia;
use parrot_proto::state::State as SessionState;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

/// Queues by session, and the driver calls the queue waits on.
#[derive(Debug, Default)]
pub struct Queues {
    pub by_session: BTreeMap<String, SessionQueue>,
    pub calls: BTreeMap<u64, PendingCall>,
    /// Prompts waiting for their checkpoint, by the workspace call that records it.
    pub deferred: BTreeMap<u64, Deferred>,
}

/// A prompt held until the workspace is recorded (spec 35).
#[derive(Debug, Clone)]
pub struct Deferred {
    pub session: String,
    pub binding: String,
    pub entry: Entry,
    pub params: Value,
}

#[derive(Debug, Default, Clone)]
pub struct SessionQueue {
    pub paused: bool,
    pub entries: VecDeque<Entry>,
    /// A prompt or action was sent and the driver hasn't reported `idle` since.
    pub busy: bool,
    /// Paused by a restart: the next prompt, action or `session/start` resumes it (spec 34).
    pub held: bool,
}

/// One queued prompt, action or wake.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub message: Option<String>,
    pub text: String,
    pub origin: Value,
    pub kind: QueueEntryKind,
    /// The `binding/prompt` or `binding/action` params, without `binding`.
    pub params: Value,
}

/// What a queue driver call was for.
#[derive(Debug, Clone)]
pub enum PendingCall {
    Prompt {
        session: String,
        entry: Entry,
        steer: bool,
    },
    Action {
        session: String,
    },
    Cancel {
        session: String,
        conn: String,
        id: Option<RequestId>,
        queued: u32,
    },
}

/// The `QueueState` JSON (spec 13.2).
pub fn queue_state(state: &State, session: &str) -> Value {
    let q = state.queue.by_session.get(session).cloned().unwrap_or_default();
    let entries: Vec<Value> = q
        .entries
        .iter()
        .map(|e| {
            let mut v = json!({ "id": e.id, "text": e.text, "origin": e.origin, "kind": e.kind });
            if let Some(m) = &e.message {
                v["message"] = json!(m);
            }
            v
        })
        .collect();
    json!({ "paused": q.paused, "entries": entries })
}

/// Whether the session has a prompt or action in flight.
pub fn is_busy(state: &State, session: &str) -> bool {
    state.queue.by_session.get(session).is_some_and(|q| q.busy)
}

fn publish_queue(cx: &mut Cx, session: &str) {
    let q = queue_state(cx.state, session);
    // The stored form also keeps each entry's driver params, so a restart can send it.
    let mut stored = q.clone();
    if let (Some(sq), Some(entries)) = (
        cx.state.queue.by_session.get(session),
        stored.get_mut("entries").and_then(Value::as_array_mut),
    ) {
        for (v, e) in entries.iter_mut().zip(&sq.entries) {
            v["params"] = e.params.clone();
        }
    }
    cx.persist(json!({ "op": "put", "kind": "queue", "id": session, "value": stored }));
    cx.publish(Some(session.to_string()), json!({ "type": "queue", "queue": q }));
}

/// Put back a session's queue read from the store. After a restart it stays paused until the
/// next prompt, action or `session/start` (spec 34).
pub fn restore(state: &mut State, session: &str, value: &Value) {
    let entries: VecDeque<Entry> = value
        .get("entries")
        .and_then(Value::as_array)
        .map(|es| {
            es.iter()
                .filter_map(|e| {
                    let text = e.get("text")?.as_str()?.to_string();
                    let message = e.get("message").and_then(Value::as_str).map(str::to_string);
                    let params = match e.get("params") {
                        Some(p) if p.is_object() => p.clone(),
                        _ => json!({ "message": message, "text": text }),
                    };
                    Some(Entry {
                        id: e.get("id")?.as_str()?.to_string(),
                        message,
                        text,
                        origin: e
                            .get("origin")
                            .cloned()
                            .unwrap_or_else(|| json!({ "kind": "runtime" })),
                        kind: e
                            .get("kind")
                            .and_then(|k| serde_json::from_value(k.clone()).ok())
                            .unwrap_or(QueueEntryKind::Prompt),
                        params,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let paused = value.get("paused").and_then(Value::as_bool).unwrap_or(false);
    let held = !paused && !entries.is_empty();
    state.queue.by_session.insert(
        session.to_string(),
        SessionQueue {
            paused: paused || held,
            entries,
            busy: false,
            held,
        },
    );
}

/// The next prompt, action or `session/start` after a restart resumes a held queue.
pub fn release(cx: &mut Cx, session: &str) {
    if let Some(q) = cx.state.queue.by_session.get_mut(session)
        && q.held
    {
        q.held = false;
        q.paused = false;
        publish_queue(cx, session);
    }
}

/// Note a prompt sent to a harness that hasn't confirmed it yet; a restart before the
/// confirmation marks its user item `uncertain` (spec 34).
pub fn inflight_begin(cx: &mut Cx, session: &str, message: &str, text: &str) {
    cx.persist(json!({ "op": "put", "kind": "inflight", "id": message,
        "value": { "session": session, "message": message, "text": text } }));
}

/// The harness answered a prompt, either way.
pub fn inflight_end(cx: &mut Cx, message: &str) {
    cx.persist(json!({ "op": "delete", "kind": "inflight", "id": message }));
}

/// Queue a prompt or action the runtime makes for a stopped session, to send once its binding is
/// ready. Returns the user item's id for prompts.
pub fn enqueue(
    cx: &mut Cx,
    session: &str,
    kind: QueueEntryKind,
    text: String,
    origin: Value,
    mut params: Value,
) -> Option<String> {
    let message = (kind != QueueEntryKind::Action).then(|| cx.ids.next("msg"));
    if let Some(m) = &message {
        params["message"] = json!(m);
        params["text"] = json!(text);
    }
    let entry = Entry {
        id: cx.ids.next("q"),
        message: message.clone(),
        text,
        origin,
        kind,
        params,
    };
    user_item(cx, session, &entry, "queued", true);
    push(cx, session, entry, false);
    message
}

fn user_item(cx: &mut Cx, session: &str, entry: &Entry, status: &str, first: bool) {
    let Some(message) = &entry.message else { return };
    let mut item = json!({ "id": message, "set": { "status": status } });
    if first {
        item["kind"] = json!("user");
        item["source"] = json!("runtime");
        item["origin"] = entry.origin.clone();
        item["set"]["text"] = json!(entry.text);
        for k in ["context", "attachments"] {
            if let Some(v) = entry.params.get(k) {
                item["set"][k] = v.clone();
            }
        }
    }
    cx.publish(Some(session.to_string()), json!({ "type": "item", "item": item }));
}

/// Add an entry at the back (or front) of a session's queue and publish the queue. Other modules
/// use this to queue `action` and `wake` entries.
pub fn push(cx: &mut Cx, session: &str, entry: Entry, front: bool) {
    let q = cx.state.queue.by_session.entry(session.to_string()).or_default();
    if front {
        q.entries.push_front(entry);
    } else {
        q.entries.push_back(entry);
    }
    publish_queue(cx, session);
}

/// Whether a prompt can be sent to the session right now.
fn ready(state: &State, session: &str) -> bool {
    let idle = state.sessions.info.get(session).map(|s| s.state) == Some(SessionState::Idle);
    // A rebind holds the queue until the new binding runs (E6).
    idle && sessions::active(state, session)
        && !is_busy(state, session)
        && !crate::seeds::rebinding(state, session)
}

/// Send an entry to the session's binding.
fn send(cx: &mut Cx, session: &str, entry: Entry, steer: bool) {
    let Some(binding) = crate::directory::binding_of(cx.state, session) else {
        return;
    };
    let mut params = entry.params.clone();
    params["binding"] = json!(binding);
    match entry.kind {
        QueueEntryKind::Action => {
            let call = cx.call_driver(CallOwner::Queue, &binding, "binding/action", params);
            cx.state.queue.calls.insert(
                call,
                PendingCall::Action {
                    session: session.to_string(),
                },
            );
        }
        _ => {
            let mut checkpoint = None;
            if !steer {
                checkpoint = crate::seeds::before_prompt(cx, session, entry.message.as_deref(), &mut params);
                // The prompt starts a turn: a client that looks before the driver reports it
                // must not take the idle session for one that has finished.
                set_state(cx, session, SessionState::Idle, SessionState::Running);
            }
            params["steer"] = json!(steer);
            if let Some(m) = &entry.message {
                inflight_begin(cx, session, m, &entry.text);
            }
            match checkpoint {
                Some(call) => {
                    cx.state.queue.deferred.insert(
                        call,
                        Deferred {
                            session: session.to_string(),
                            binding,
                            entry: entry.clone(),
                            params,
                        },
                    );
                }
                None => prompt_driver(cx, session, &binding, entry.clone(), params, steer),
            }
        }
    }
    if !steer {
        cx.state
            .queue
            .by_session
            .entry(session.to_string())
            .or_default()
            .busy = true;
    }
}

fn prompt_driver(cx: &mut Cx, session: &str, binding: &str, entry: Entry, params: Value, steer: bool) {
    let call = cx.call_driver(CallOwner::Queue, binding, "binding/prompt", params);
    cx.state.queue.calls.insert(
        call,
        PendingCall::Prompt {
            session: session.to_string(),
            entry,
            steer,
        },
    );
}

/// The checkpoint a prompt waited for was recorded (or failed): send the prompt.
pub fn checkpointed(cx: &mut Cx, workspace_call: u64) {
    if let Some(d) = cx.state.queue.deferred.remove(&workspace_call) {
        if !cx.state.sessions.by_id.contains_key(&d.session) {
            return;
        }
        prompt_driver(cx, &d.session, &d.binding, d.entry, d.params, false);
    }
}

/// Whether `dispatch` would send a prompt now, which starts a turn.
pub fn sends_next(state: &State, session: &str) -> bool {
    ready(state, session)
        && state.queue.by_session.get(session).is_some_and(|q| {
            !q.paused
                && q.entries
                    .front()
                    .is_some_and(|e| e.kind != QueueEntryKind::Action)
        })
}

/// Send the first entry if the session is idle and the queue isn't paused (spec 13.2).
pub fn dispatch(cx: &mut Cx, session: &str) {
    if !ready(cx.state, session) {
        return;
    }
    let Some(q) = cx.state.queue.by_session.get_mut(session) else {
        return;
    };
    if q.paused {
        return;
    }
    let Some(entry) = q.entries.pop_front() else {
        return;
    };
    user_item(cx, session, &entry, "sent", false);
    send(cx, session, entry, false);
    publish_queue(cx, session);
}

/// The driver reported `idle`.
pub fn on_idle(cx: &mut Cx, session: &str) {
    if let Some(q) = cx.state.queue.by_session.get_mut(session) {
        q.busy = false;
    }
    dispatch(cx, session);
}

/// The binding exited: nothing is in flight any more.
pub fn on_exit(cx: &mut Cx, session: &str) {
    if let Some(q) = cx.state.queue.by_session.get_mut(session) {
        q.busy = false;
    }
}

/// A session was deleted.
pub fn forget(cx: &mut Cx, session: &str) {
    cx.state.queue.by_session.remove(session);
    cx.state.queue.deferred.retain(|_, d| d.session != session);
    cx.state.queue.calls.retain(|_, c| match c {
        PendingCall::Prompt { session: s, .. } | PendingCall::Action { session: s } => s != session,
        PendingCall::Cancel { session: s, .. } => s != session,
    });
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    let result = match method {
        "session/prompt" => prompt(cx, conn, params),
        "session/cancel" => return cancel(cx, conn, id, params),
        "queue/list" => parse::<QueueSessionParams>(params).and_then(|p| {
            check_session(cx.state, conn, Scope::Read, &p.session)?;
            Ok(json!({ "queue": queue_state(cx.state, &p.session) }))
        }),
        "queue/clear" => with_session(cx, conn, params, |cx, s| {
            let entries: Vec<Entry> = cx
                .state
                .queue
                .by_session
                .get_mut(s)
                .map(|q| q.entries.drain(..).collect())
                .unwrap_or_default();
            for e in &entries {
                remove_user_item(cx, s, e);
            }
            publish_queue(cx, s);
            Ok(json!({}))
        }),
        "queue/pause" => with_session(cx, conn, params, |cx, s| {
            cx.state.queue.by_session.entry(s.to_string()).or_default().paused = true;
            publish_queue(cx, s);
            Ok(json!({}))
        }),
        "queue/resume" => with_session(cx, conn, params, |cx, s| {
            cx.state.queue.by_session.entry(s.to_string()).or_default().paused = false;
            publish_queue(cx, s);
            dispatch(cx, s);
            Ok(json!({}))
        }),
        "queue/remove" => remove(cx, conn, params),
        "queue/edit" => edit(cx, conn, params),
        "queue/move" => move_entry(cx, conn, params),
        _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
    };
    Reply::Now(result)
}

fn with_session(
    cx: &mut Cx,
    conn: &str,
    params: Value,
    f: impl FnOnce(&mut Cx, &str) -> Result<Value, RpcError>,
) -> Result<Value, RpcError> {
    let p: QueueSessionParams = parse(params)?;
    check_session(cx.state, conn, Scope::Prompt, &p.session)?;
    f(cx, &p.session)
}

fn remove_user_item(cx: &mut Cx, session: &str, e: &Entry) {
    if let Some(m) = &e.message {
        cx.publish(
            Some(session.to_string()),
            json!({ "type": "item", "item": { "id": m, "removed": true } }),
        );
    }
}

fn remove(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: QueueRemoveParams = parse(params)?;
    check_session(cx.state, conn, Scope::Prompt, &p.session)?;
    let q = cx.state.queue.by_session.entry(p.session.clone()).or_default();
    let pos = q
        .entries
        .iter()
        .position(|e| e.id == p.id)
        .ok_or_else(|| RpcError::new(-32002, format!("queue entry not found: {}", p.id)))?;
    let e = q.entries.remove(pos).expect("position is in range");
    remove_user_item(cx, &p.session, &e);
    publish_queue(cx, &p.session);
    Ok(json!({}))
}

fn edit(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: parrot_proto::prompt::QueueEditParams = parse(params)?;
    check_session(cx.state, conn, Scope::Prompt, &p.session)?;
    let q = cx.state.queue.by_session.entry(p.session.clone()).or_default();
    let e = q
        .entries
        .iter_mut()
        .find(|e| e.id == p.id)
        .ok_or_else(|| RpcError::new(-32002, format!("queue entry not found: {}", p.id)))?;
    e.text = p.text.clone();
    e.params["text"] = json!(p.text);
    let message = e.message.clone();
    if let Some(m) = message {
        cx.publish(
            Some(p.session.clone()),
            json!({ "type": "item", "item": { "id": m, "set": { "text": p.text } } }),
        );
    }
    publish_queue(cx, &p.session);
    Ok(json!({}))
}

fn move_entry(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: QueueMoveParams = parse(params)?;
    check_session(cx.state, conn, Scope::Prompt, &p.session)?;
    let q = cx.state.queue.by_session.entry(p.session.clone()).or_default();
    let pos = q
        .entries
        .iter()
        .position(|e| e.id == p.id)
        .ok_or_else(|| RpcError::new(-32002, format!("queue entry not found: {}", p.id)))?;
    let e = q.entries.remove(pos).expect("position is in range");
    let index = (p.index as usize).min(q.entries.len());
    q.entries.insert(index, e);
    publish_queue(cx, &p.session);
    Ok(json!({}))
}

fn steer_supported(state: &State, session: &str) -> bool {
    state
        .sessions
        .by_id
        .get(session)
        .and_then(|s| state.sessions.agents.get(&s.agent))
        .and_then(|a| a.get("features"))
        .is_some_and(|f| f.get("session.steer").is_some())
}

fn prompt(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionPromptParams = parse(params)?;
    check_session(cx.state, conn, Scope::Prompt, &p.session)?;
    let delivery = p.delivery.unwrap_or_default();
    let session = p.session.clone();
    let starting = !sessions::active(cx.state, &session);
    // Prompting a stopped session in one's own subtree needs only `prompt` (E7).
    if starting && crate::principals::session_principal(cx.state, conn).is_none() {
        check(cx.state, conn, Scope::Manage)?;
    }
    let busy = !ready(cx.state, &session);
    if busy && !starting && delivery == Delivery::Steer && !steer_supported(cx.state, &session) {
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "session.steer" })));
    }
    let message = cx.ids.next("msg");
    let origin = json!({ "kind": "client", "principal": principal_of(cx.state, conn) });
    let mut params = json!({ "message": message, "text": p.text });
    if let Some(c) = &p.context {
        params["context"] = json!(c);
    }
    if let Some(a) = &p.attachments {
        params["attachments"] = json!(a);
    }
    let entry = Entry {
        id: cx.ids.next("q"),
        message: Some(message.clone()),
        text: p.text.clone(),
        origin,
        kind: QueueEntryKind::Prompt,
        params,
    };
    release(cx, &session);
    if delivery != Delivery::Queue
        && let Some(q) = cx.state.queue.by_session.get_mut(&session)
        && q.paused
    {
        q.paused = false;
        publish_queue(cx, &session);
    }
    if starting {
        sessions::start_binding(cx, &session, BindingVia::New);
    }
    let paused = cx.state.queue.by_session.get(&session).is_some_and(|q| q.paused);
    let queued_ahead = cx
        .state
        .queue
        .by_session
        .get(&session)
        .is_some_and(|q| !q.entries.is_empty());
    // Only a running turn can be steered; a binding that is still starting queues the prompt.
    let running = cx.state.sessions.info.get(&session).map(|s| s.state) == Some(SessionState::Running);
    let can_steer = !starting && running && steer_supported(cx.state, &session);
    let disposition = if !busy && !paused && !queued_ahead {
        user_item(cx, &session, &entry, "sent", true);
        send(cx, &session, entry.clone(), false);
        "started"
    } else if busy && can_steer && matches!(delivery, Delivery::Auto | Delivery::Steer) {
        user_item(cx, &session, &entry, "sent", true);
        send(cx, &session, entry.clone(), true);
        "steered"
    } else {
        let front = delivery == Delivery::Interrupt;
        if front
            && !starting
            && let Some(binding) = crate::directory::binding_of(cx.state, &session)
        {
            cx.call_driver(
                CallOwner::Queue,
                &binding,
                "binding/cancel",
                json!({ "binding": binding }),
            );
        }
        user_item(cx, &session, &entry, "queued", true);
        let id = entry.id.clone();
        push(cx, &session, entry, front);
        return Ok(json!({ "message": message, "disposition": "queued", "queued": id }));
    };
    Ok(json!({ "message": message, "disposition": disposition }))
}

fn cancel(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let p: SessionCancelParams = match parse(params) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(e)),
    };
    if let Err(e) = check_session(cx.state, conn, Scope::Prompt, &p.session) {
        return Reply::Now(Err(e));
    }
    let q = cx.state.queue.by_session.entry(p.session.clone()).or_default();
    q.paused = true;
    let queued = q.entries.len() as u32;
    publish_queue(cx, &p.session);
    let running = cx
        .state
        .sessions
        .info
        .get(&p.session)
        .is_some_and(|s| matches!(s.state, SessionState::Running | SessionState::RequiresAction))
        || is_busy(cx.state, &p.session);
    let binding = crate::directory::binding_of(cx.state, &p.session);
    match binding {
        Some(b) if running && sessions::active(cx.state, &p.session) => {
            let call = cx.call_driver(CallOwner::Queue, &b, "binding/cancel", json!({ "binding": b }));
            cx.state.queue.calls.insert(
                call,
                PendingCall::Cancel {
                    session: p.session,
                    conn: conn.to_string(),
                    id: id.cloned(),
                    queued,
                },
            );
            Reply::Later
        }
        _ => Reply::Now(Ok(json!({ "interrupted": false, "queued": queued }))),
    }
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(pending) = cx.state.queue.calls.remove(&call) else {
        return;
    };
    if let PendingCall::Prompt {
        entry: Entry { message: Some(m), .. },
        ..
    } = &pending
    {
        inflight_end(cx, m);
    }
    match pending {
        PendingCall::Cancel { conn, id, queued, .. } => {
            let interrupted = result
                .as_ref()
                .ok()
                .and_then(|r| r.get("interrupted"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(id) = id {
                cx.reply(
                    &conn,
                    id,
                    Ok(json!({ "interrupted": interrupted, "queued": queued })),
                );
            }
        }
        PendingCall::Prompt {
            session,
            entry,
            steer,
        } => match result {
            Ok(_) => {}
            Err(e) if steer => {
                // A refused steer goes to the front of the queue (spec 13.1).
                user_item(cx, &session, &entry, "queued", false);
                push(cx, &session, entry, true);
                cx.publish(
                    Some(session.clone()),
                    json!({ "type": "notice", "level": "info", "text": format!("steer refused, queued: {}", e.message) }),
                );
                dispatch(cx, &session);
            }
            Err(e) => {
                failed(cx, &session, &entry, &e);
            }
        },
        PendingCall::Action { session } => {
            if let Err(e) = result {
                cx.publish(
                    Some(session.clone()),
                    json!({ "type": "notice", "level": "error", "text": format!("action failed: {}", e.message) }),
                );
                if let Some(q) = cx.state.queue.by_session.get_mut(&session) {
                    q.busy = false;
                }
                dispatch(cx, &session);
            }
        }
    }
}

/// Moves a session from `from` to `to` and publishes it; any other state is left alone.
fn set_state(cx: &mut Cx, session: &str, from: SessionState, to: SessionState) {
    let Some(s) = cx
        .state
        .sessions
        .info
        .get_mut(session)
        .filter(|s| s.state == from)
    else {
        return;
    };
    s.state = to;
    let background = s.background.clone();
    cx.publish(
        Some(session.to_string()),
        json!({ "type": "state", "state": to, "background": background }),
    );
    sessions::changed(cx, session);
}

/// A prompt that fails after acceptance ends its turn as `failed` (spec 13.1).
fn failed(cx: &mut Cx, session: &str, entry: &Entry, e: &RpcError) {
    let binding = crate::directory::binding_of(cx.state, session).unwrap_or_default();
    let turn = cx.ids.next("trn");
    let at = sessions::timestamp(cx.now_ms);
    let mut t = json!({
        "id": turn, "binding": binding, "status": "failed", "stop": "error", "continuable": false,
        "started": at, "ended": at,
        "error": { "code": "harness_failed", "message": e.message, "retryable": true },
    });
    if let Some(m) = &entry.message {
        t["input"] = json!(m);
        crate::seeds::on_prompt_failed(cx, session, m);
    }
    cx.publish(Some(session.to_string()), json!({ "type": "turn", "turn": t }));
    if let Some(q) = cx.state.queue.by_session.get_mut(session) {
        q.busy = false;
    }
    set_state(cx, session, SessionState::Running, SessionState::Idle);
    dispatch(cx, session);
}

/// Time passed.
pub fn tick(_cx: &mut Cx) {}
