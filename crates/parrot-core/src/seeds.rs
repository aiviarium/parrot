//! Seeds (spec 9.3 within one machine, 9.4, 23, 35, Appendix E6, E9): fork, handoff, rebind,
//! rewind, bundles and checkpoints.

use crate::principals::{check_session, parse};
use crate::{CallOwner, Cx, Effect, Reply, State};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::session::{BindingStatus, BindingVia, SessionRebindParams, SessionRewindParams};
use parrot_proto::state::State as SessionState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// What seeds keep per session and per call.
#[derive(Debug, Default)]
pub struct Seeds {
    pub by_session: BTreeMap<String, SeedState>,
    /// Rebinds in progress, by session. At most one per session.
    pub rebinding: BTreeMap<String, Rebind>,
    /// What to do once a binding has started, by binding.
    pub after_start: BTreeMap<String, AfterStart>,
    /// Workspace calls this module waits on.
    pub calls: BTreeMap<u64, SeedCall>,
    /// Turn ids of each session in the order they started.
    pub turns: BTreeMap<String, Vec<String>>,
}

/// Stored per session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedState {
    /// A bundle waiting to be prepended to the next prompt (`Binding.pending`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Value>,
    /// A native fork for the next binding: `{ ref, at? }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<Value>,
    /// Prompts sent so far: the index of the next turn's checkpoint.
    #[serde(default)]
    pub sent: u32,
    /// Turn indexes with a checkpoint.
    #[serde(default)]
    pub checkpoints: BTreeSet<u32>,
    #[serde(default)]
    pub turns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Rebind {
    pub old: String,
    pub old_agent: String,
    pub agent: String,
    pub interrupt: bool,
    /// The new binding, once started.
    pub new: Option<String>,
    /// When the running turn is interrupted if it hasn't ended (E9: 30 seconds).
    pub deadline_ms: u64,
}

#[derive(Debug, Clone)]
pub enum AfterStart {
    /// A seed's `source` action once the target started (spec 23.1).
    Source { source: String, action: String },
    /// A rebind's new binding: stop the old one once it runs (E6).
    Rebind { session: String },
}

#[derive(Debug, Clone)]
pub enum SeedCall {
    Checkpoint { session: String, turn: u32 },
    Rewind { conn: String, id: Option<RequestId> },
    Ignore,
}

/// What `session/new { seed }` decided before the session exists.
#[derive(Debug, Clone)]
pub struct SeedPlan {
    pub source: String,
    pub at: Option<String>,
    pub via: &'static str,
    pub bundle: Option<Value>,
    pub fork: Option<Value>,
    pub brief: Option<String>,
    pub source_action: String,
}

/// `SeedRequest` (spec 23).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeedRequest {
    session: String,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    carry: Option<Vec<String>>,
    #[serde(default)]
    brief: Option<String>,
    #[serde(default)]
    require: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

fn has_feature(state: &State, agent: &str, feature: &str) -> bool {
    state
        .sessions
        .agents
        .get(agent)
        .and_then(|a| a.get("features"))
        .is_some_and(|f| f.get(feature).is_some())
}

fn current_ref(state: &State, session: &str) -> Option<Value> {
    let info = state.sessions.info.get(session)?;
    info.bindings
        .iter()
        .rev()
        .find_map(|b| state.bindings.info.get(b).and_then(|i| i.reference.clone()))
        .map(|r| json!(r))
}

/// Plan a seed for `session/new` (spec 23.1).
pub fn plan(cx: &mut Cx, conn: &str, seed: &Value, agent: &str) -> Result<SeedPlan, RpcError> {
    let req: SeedRequest = parse(seed.clone())?;
    check_session(cx.state, conn, Scope::Read, &req.session)?;
    if req.summary.as_deref() == Some("source") {
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "action.summarize" })));
    }
    let source_agent = cx
        .state
        .sessions
        .by_id
        .get(&req.session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let reference = current_ref(cx.state, &req.session);
    let native = source_agent == agent && has_feature(cx.state, agent, "session.fork") && reference.is_some();
    if !native && req.require.as_deref() == Some("exact") {
        return Err(RpcError::new(-32001, "an exact fork isn't possible")
            .with_data(json!({ "feature": "session.fork" })));
    }
    let carry_history = req
        .carry
        .as_ref()
        .is_none_or(|c| c.iter().any(|x| x == "history"));
    let (via, bundle, fork) = if native {
        let mut f = json!({ "ref": reference });
        if let Some(at) = &req.at {
            f["at"] = json!(at);
        }
        ("fork", None, Some(f))
    } else {
        let b = carry_history.then(|| {
            crate::bundle::build(
                cx.state,
                &req.session,
                req.at.as_deref(),
                crate::bundle::DEFAULT_CHARS,
            )
        });
        ("bundle", b, None)
    };
    Ok(SeedPlan {
        source: req.session,
        at: req.at,
        via,
        bundle,
        fork,
        brief: req.brief,
        source_action: req.source.unwrap_or_else(|| "keep".into()),
    })
}

/// Apply a planned seed to the new session before its binding starts.
pub fn apply(cx: &mut Cx, session: &str, plan: &SeedPlan) {
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    st.pending = plan.bundle.clone();
    st.fork = plan.fork.clone();
    let mut seed = json!({ "session": plan.source, "via": plan.via });
    if let Some(at) = &plan.at {
        seed["at"] = json!(at);
    }
    if let Some(n) = cx.state.tree.nodes.get_mut(session) {
        n.seed = Some(seed);
    }
    persist(cx, session);
}

/// Called right after a binding is made, before `StartBinding` goes out: a native fork, a pending
/// bundle and the ceiling's pin go into its `BindingStart`.
pub fn start_extra(cx: &mut Cx, session: &str, binding: &str, start: &mut Value) -> Option<BindingVia> {
    let mut via = None;
    let ceiling = crate::ceilings::of(cx.state, session);
    if let Some(level) = ceiling.level {
        start["pin"] = json!({ "level": level, "network": ceiling.network.unwrap_or(true) });
    }
    if let Some(iso) = ceiling.isolation {
        start["isolation"] = json!(iso);
    }
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    if let Some(f) = st.fork.take() {
        start["fork"] = f;
        if let Some(o) = start.as_object_mut() {
            o.remove("resume");
        }
        via = Some(BindingVia::Fork);
    } else if st.pending.is_some() {
        if let Some(o) = start.as_object_mut() {
            o.remove("resume");
        }
        via = Some(BindingVia::Bundle);
    }
    let _ = binding;
    persist(cx, session);
    via
}

/// Whether a binding has a pending bundle, for `Binding.pending`.
pub fn pending(state: &State, session: &str) -> bool {
    state
        .seeds
        .by_session
        .get(session)
        .is_some_and(|s| s.pending.is_some())
}

/// The queue is about to send a prompt: prepend a pending bundle and record a checkpoint for
/// worktree workspaces (spec 23.2, 35).
pub fn before_prompt(cx: &mut Cx, session: &str, params: &mut Value) {
    let bundle = cx
        .state
        .seeds
        .by_session
        .get_mut(session)
        .and_then(|s| s.pending.take());
    if let Some(b) = bundle {
        let text = params.get("text").and_then(Value::as_str).map(str::to_string);
        params["text"] = json!(crate::bundle::render(&b, text.as_deref()));
    }
    let info = cx.state.sessions.info.get(session).cloned();
    if let Some(info) = info
        && crate::tree::is_worktree(cx.state, &info.workspace)
        && let Some(w) = cx.state.sessions.workspaces.get(&info.workspace).cloned()
    {
        let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
        let turn = st.sent;
        st.sent += 1;
        let call = cx.call_workspace(
            CallOwner::Seeds,
            "checkpoint.record",
            json!({ "path": w.path, "session": session, "turn": turn }),
        );
        cx.state.seeds.calls.insert(
            call,
            SeedCall::Checkpoint {
                session: session.to_string(),
                turn,
            },
        );
    } else {
        cx.state
            .seeds
            .by_session
            .entry(session.to_string())
            .or_default()
            .sent += 1;
    }
    persist(cx, session);
}

/// A turn id was seen for the first time.
pub fn on_turn(cx: &mut Cx, session: &str, turn: &str) {
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    if !st.turns.iter().any(|t| t == turn) {
        st.turns.push(turn.to_string());
        let turns = st.turns.clone();
        cx.state.seeds.turns.insert(session.to_string(), turns);
        persist(cx, session);
    }
}

pub fn persist(cx: &mut Cx, session: &str) {
    if let Some(s) = cx.state.seeds.by_session.get(session) {
        let v = serde_json::to_value(s).unwrap_or_default();
        cx.persist(json!({ "op": "put", "kind": "seed", "id": session, "value": v }));
    }
}

pub fn restore(state: &mut State, session: &str, value: &Value) {
    if let Ok(s) = serde_json::from_value::<SeedState>(value.clone()) {
        state.seeds.turns.insert(session.to_string(), s.turns.clone());
        state.seeds.by_session.insert(session.to_string(), s);
    }
}

/// Whether a rebind holds the session's queue (E6).
pub fn rebinding(state: &State, session: &str) -> bool {
    state.seeds.rebinding.contains_key(session)
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "session/bundle" => Reply::Now(bundle(cx, conn, params)),
        "session/rebind" => Reply::Now(rebind(cx, conn, params)),
        "session/rewind" => rewind(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn bundle(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: session"))?
        .to_string();
    let summary = params.get("summary").and_then(Value::as_str);
    check_session(
        cx.state,
        conn,
        if summary.is_some() {
            Scope::Delegate
        } else {
            Scope::Read
        },
        &session,
    )?;
    if summary == Some("source") {
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "action.summarize" })));
    }
    let at = params.get("at").and_then(Value::as_str);
    let b = crate::bundle::build(cx.state, &session, at, crate::bundle::DEFAULT_CHARS);
    let rendered = crate::bundle::render(&b, None);
    Ok(json!({ "bundle": b, "rendered": rendered }))
}

/// `session/rebind` within one machine (spec 9.3, E6, E9). Returns at once; the outcome arrives
/// as a `handoff` item.
fn rebind(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionRebindParams = parse(params)?;
    check_session(cx.state, conn, Scope::Delegate, &p.session)?;
    if p.env.as_ref().is_some_and(|e| e != &cx.state.principals.env) || p.workspace.is_some() {
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "profile.link" })));
    }
    if rebinding(cx.state, &p.session) {
        return Err(
            RpcError::new(-32004, "a rebind is in progress").with_data(json!({ "state": "rebinding" }))
        );
    }
    let old_agent = cx
        .state
        .sessions
        .by_id
        .get(&p.session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let agent = p.agent.clone().unwrap_or_else(|| old_agent.clone());
    if !cx.state.sessions.agents.contains_key(&agent) {
        return Err(RpcError::new(-32002, format!("agent not found: {agent}")));
    }
    let ceiling = crate::ceilings::of(cx.state, &p.session);
    crate::ceilings::check_agent(&ceiling, &agent, &cx.state.principals.env)?;
    let same = agent == old_agent && current_ref(cx.state, &p.session).is_some();
    if p.require == Some(parrot_proto::session::RebindRequire::Exact) && !same {
        return Err(RpcError::new(-32001, "an exact rebind isn't possible")
            .with_data(json!({ "feature": "session.resume" })));
    }
    let old = crate::directory::binding_of(cx.state, &p.session).unwrap_or_default();
    cx.state.seeds.rebinding.insert(
        p.session.clone(),
        Rebind {
            old,
            old_agent,
            agent,
            interrupt: p.interrupt.unwrap_or(false),
            new: None,
            deadline_ms: cx.now_ms + 30_000,
        },
    );
    let running = cx
        .state
        .sessions
        .info
        .get(&p.session)
        .is_some_and(|i| matches!(i.state, SessionState::Running | SessionState::RequiresAction));
    if running && crate::sessions::active(cx.state, &p.session) {
        if p.interrupt.unwrap_or(false) {
            cancel_turn(cx, &p.session);
        } else {
            cx.emit(Effect::TimerAt {
                at_ms: cx.now_ms + 30_000,
            });
        }
    } else {
        proceed(cx, &p.session);
    }
    let b = crate::directory::binding_of(cx.state, &p.session)
        .and_then(|b| crate::bindings::binding_json(cx.state, &b));
    Ok(json!({ "binding": b }))
}

fn cancel_turn(cx: &mut Cx, session: &str) {
    if let Some(b) = crate::directory::binding_of(cx.state, session) {
        cx.call_driver(CallOwner::Seeds, &b, "binding/cancel", json!({ "binding": b }));
    }
}

/// Stop one binding, whether or not it is the session's current one.
fn stop_binding(cx: &mut Cx, binding: &str) {
    let live = cx
        .state
        .bindings
        .info
        .get(binding)
        .is_some_and(|b| matches!(b.status, BindingStatus::Starting | BindingStatus::Running));
    if !live {
        return;
    }
    if let Some(b) = cx.state.bindings.info.get_mut(binding) {
        b.stop_requested = true;
    }
    cx.call_driver(
        CallOwner::Seeds,
        binding,
        "binding/stop",
        json!({ "binding": binding }),
    );
    cx.emit(Effect::StopBinding {
        binding: binding.to_string(),
    });
}

/// No turn runs any more: start the new binding (E9 order).
fn proceed(cx: &mut Cx, session: &str) {
    let Some(r) = cx.state.seeds.rebinding.get(session).cloned() else {
        return;
    };
    if r.new.is_some() {
        return;
    }
    let same_harness = r.agent == r.old_agent && current_ref(cx.state, session).is_some();
    if !same_harness {
        // A bundle carries the history to the new harness.
        let b = crate::bundle::build(cx.state, session, None, crate::bundle::DEFAULT_CHARS);
        cx.state
            .seeds
            .by_session
            .entry(session.to_string())
            .or_default()
            .pending = Some(b);
    } else {
        // The same harness session resumes: the old binding stops first.
        stop_binding(cx, &r.old);
    }
    if let Some(rec) = cx.state.sessions.by_id.get_mut(session) {
        rec.agent = r.agent.clone();
    }
    let via = if same_harness {
        BindingVia::Resume
    } else {
        BindingVia::Bundle
    };
    let new = crate::sessions::start_binding(cx, session, via);
    if let Some(n) = &new {
        cx.state.seeds.after_start.insert(
            n.clone(),
            AfterStart::Rebind {
                session: session.to_string(),
            },
        );
    }
    if let Some(rb) = cx.state.seeds.rebinding.get_mut(session) {
        rb.new = new;
    }
}

/// A session went idle: a waiting rebind goes on.
pub fn on_idle(cx: &mut Cx, session: &str) {
    if cx
        .state
        .seeds
        .rebinding
        .get(session)
        .is_some_and(|r| r.new.is_none())
    {
        proceed(cx, session);
    }
}

/// Time passed: interrupt turns a rebind has waited 30 seconds for.
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let due: Vec<String> = cx
        .state
        .seeds
        .rebinding
        .iter()
        .filter(|(_, r)| r.new.is_none() && !r.interrupt && r.deadline_ms <= now)
        .map(|(s, _)| s.clone())
        .collect();
    for s in due {
        if let Some(r) = cx.state.seeds.rebinding.get_mut(&s) {
            r.interrupt = true;
        }
        cancel_turn(cx, &s);
    }
}

/// A binding answered `binding/start`. Runs the step that waited for it.
pub fn on_started(cx: &mut Cx, session: &str, binding: &str, ok: bool) {
    let Some(after) = cx.state.seeds.after_start.remove(binding) else {
        return;
    };
    match after {
        AfterStart::Source { source, action } => {
            if !ok {
                return;
            }
            match action.as_str() {
                "stop" => crate::sessions::stop_session(cx, &source),
                "archive" => {
                    crate::sessions::stop_session(cx, &source);
                    if let Some(i) = cx.state.sessions.info.get_mut(&source) {
                        i.archived = true;
                    }
                    crate::sessions::changed(cx, &source);
                }
                _ => {}
            }
        }
        AfterStart::Rebind { session: s } => {
            let Some(r) = cx.state.seeds.rebinding.remove(&s) else {
                return;
            };
            let item = cx.ids.next("itm");
            if ok {
                stop_binding(cx, &r.old);
                let via = cx
                    .state
                    .bindings
                    .info
                    .get(binding)
                    .map(|b| json!(b.via))
                    .unwrap_or(json!("bundle"));
                cx.publish(
                    Some(s.clone()),
                    json!({ "type": "item", "item": { "id": item, "kind": "handoff", "source": "runtime",
                        "set": { "from": r.old, "to": binding, "via": via, "status": "ok" } } }),
                );
            } else {
                // The old binding starts again where it can, and the handoff is reported failed.
                if let Some(rec) = cx.state.sessions.by_id.get_mut(&s) {
                    rec.agent = r.old_agent.clone();
                }
                if let Some(st) = cx.state.seeds.by_session.get_mut(&s) {
                    st.pending = None;
                }
                let live =
                    cx.state.bindings.info.get(&r.old).is_some_and(|b| {
                        matches!(b.status, BindingStatus::Starting | BindingStatus::Running)
                    });
                if live {
                    if let Some(rec) = cx.state.sessions.by_id.get_mut(&s) {
                        rec.binding = Some(r.old.clone());
                    }
                    if let Some(i) = cx.state.sessions.info.get_mut(&s) {
                        i.state = SessionState::Idle;
                    }
                } else {
                    crate::sessions::start_binding(cx, &s, BindingVia::Resume);
                }
                cx.publish(
                    Some(s.clone()),
                    json!({ "type": "item", "item": { "id": item, "kind": "handoff", "source": "runtime",
                        "set": { "from": r.old, "to": binding, "status": "error" } } }),
                );
            }
            crate::sessions::changed(cx, &s);
            crate::queue::dispatch(cx, &s);
        }
    }
    let _ = session;
}

/// Remember a seed's `source` action for when the new binding started.
pub fn after_seed(cx: &mut Cx, binding: &str, plan: &SeedPlan) {
    if plan.source_action != "keep" {
        cx.state.seeds.after_start.insert(
            binding.to_string(),
            AfterStart::Source {
                source: plan.source.clone(),
                action: plan.source_action.clone(),
            },
        );
    }
}

/// `session/rewind` (spec 9.4).
fn rewind(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let p: SessionRewindParams = match parse(params) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(e)),
    };
    if let Err(e) = check_session(cx.state, conn, Scope::Destructive, &p.session) {
        return Reply::Now(Err(e));
    }
    let turns = cx.state.seeds.turns.get(&p.session).cloned().unwrap_or_default();
    let Some(idx) = turns.iter().position(|t| t == &p.turn) else {
        return Reply::Now(Err(RpcError::new(-32002, format!("turn not found: {}", p.turn))));
    };
    let info = cx
        .state
        .sessions
        .info
        .get(&p.session)
        .cloned()
        .unwrap_or_default();
    let checkpoint = (idx + 1) as u32;
    let mut restore = None;
    if p.files {
        if !crate::tree::is_worktree(cx.state, &info.workspace) {
            return Reply::Now(Err(RpcError::new(
                -32004,
                "rewinding files needs a worktree workspace",
            )
            .with_data(json!({ "reason": "dir" }))));
        }
        let others = cx
            .state
            .sessions
            .info
            .iter()
            .any(|(s, i)| s != &p.session && i.workspace == info.workspace);
        if others {
            return Reply::Now(Err(RpcError::new(-32004, "other sessions use the workspace")));
        }
        let has = cx
            .state
            .seeds
            .by_session
            .get(&p.session)
            .is_some_and(|s| s.checkpoints.contains(&checkpoint));
        if !has {
            return Reply::Now(Err(RpcError::new(-32004, "there is no checkpoint for that turn")));
        }
        restore = cx
            .state
            .sessions
            .workspaces
            .get(&info.workspace)
            .map(|w| w.path.clone());
    }
    // Items after the turn are removed.
    let keep: Vec<String> = turns[..=idx].to_vec();
    let removed: Vec<String> = cx
        .state
        .items
        .by_session
        .get(&p.session)
        .map(|items| {
            let last = items
                .order
                .iter()
                .rposition(|i| {
                    items
                        .by_id
                        .get(i)
                        .and_then(|m| m.get("turn"))
                        .and_then(Value::as_str)
                        == Some(p.turn.as_str())
                })
                .unwrap_or(0);
            items
                .order
                .iter()
                .enumerate()
                .filter(|(n, i)| {
                    *n > last
                        || items
                            .by_id
                            .get(*i)
                            .and_then(|m| m.get("turn"))
                            .and_then(Value::as_str)
                            .is_some_and(|t| !keep.iter().any(|k| k == t))
                })
                .map(|(_, i)| i.clone())
                .collect()
        })
        .unwrap_or_default();
    for item in &removed {
        cx.publish(
            Some(p.session.clone()),
            json!({ "type": "item", "item": { "id": item, "removed": true } }),
        );
    }
    let bundle = crate::bundle::build(cx.state, &p.session, Some(&p.turn), crate::bundle::DEFAULT_CHARS);
    let agent = cx
        .state
        .sessions
        .by_id
        .get(&p.session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let reference = current_ref(cx.state, &p.session);
    {
        let st = cx.state.seeds.by_session.entry(p.session.clone()).or_default();
        st.turns = keep.clone();
        st.sent = checkpoint;
        st.checkpoints.retain(|c| *c <= checkpoint);
        if has_feature(cx.state, &agent, "session.fork") && reference.is_some() {
            let st = cx
                .state
                .seeds
                .by_session
                .get_mut(&p.session)
                .expect("inserted above");
            st.fork = Some(json!({ "ref": reference, "at": p.turn }));
        } else {
            let st = cx
                .state
                .seeds
                .by_session
                .get_mut(&p.session)
                .expect("inserted above");
            st.pending = Some(bundle);
        }
    }
    cx.state.seeds.turns.insert(p.session.clone(), keep);
    persist(cx, &p.session);
    let was_active = crate::sessions::active(cx.state, &p.session);
    if was_active {
        if let Some(b) = crate::directory::binding_of(cx.state, &p.session) {
            stop_binding(cx, &b);
        }
        crate::sessions::start_binding(cx, &p.session, BindingVia::Bundle);
    }
    match restore {
        Some(path) => {
            let call = cx.call_workspace(
                CallOwner::Seeds,
                "checkpoint.restore",
                json!({ "path": path, "session": p.session, "turn": checkpoint }),
            );
            cx.state.seeds.calls.insert(
                call,
                SeedCall::Rewind {
                    conn: conn.to_string(),
                    id: id.cloned(),
                },
            );
            Reply::Later
        }
        None => Reply::Now(Ok(json!({}))),
    }
}

/// Prune a deleted session's checkpoints.
pub fn on_delete(cx: &mut Cx, session: &str, workspace: &str) {
    if crate::tree::is_worktree(cx.state, workspace)
        && let Some(w) = cx.state.sessions.workspaces.get(workspace).cloned()
    {
        let call = cx.call_workspace(
            CallOwner::Seeds,
            "checkpoint.prune",
            json!({ "path": w.path, "session": session }),
        );
        cx.state.seeds.calls.insert(call, SeedCall::Ignore);
    }
    cx.state.seeds.by_session.remove(session);
    cx.state.seeds.turns.remove(session);
    cx.state.seeds.rebinding.remove(session);
    cx.persist(json!({ "op": "delete", "kind": "seed", "id": session }));
}

/// A driver or workspace reply to a call this module made.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(c) = cx.state.seeds.calls.remove(&call) else {
        return;
    };
    match c {
        SeedCall::Ignore => {}
        SeedCall::Checkpoint { session, turn } => {
            if result.is_ok()
                && let Some(st) = cx.state.seeds.by_session.get_mut(&session)
            {
                st.checkpoints.insert(turn);
                persist(cx, &session);
            }
        }
        SeedCall::Rewind { conn, id } => {
            if let Some(id) = id {
                cx.reply(&conn, id, result.map(|_| json!({})));
            }
        }
    }
}
