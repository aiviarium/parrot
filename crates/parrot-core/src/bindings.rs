//! Bindings and the driver updates they report (spec 9, 30.2, 30.4, Appendix E4).

use crate::{Cx, State, queue, sessions};
use parrot_proto::agent::Control;
use parrot_proto::driver::{DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::Isolation;
use parrot_proto::rpc::RpcError;
use parrot_proto::session::{BindingStatus, BindingVia, Ref, RefStrength};
use parrot_proto::state::{State as SessionState, TurnStatus};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Bindings by id.
#[derive(Debug, Default)]
pub struct Bindings {
    /// The records every module reads through `directory`.
    pub by_id: BTreeMap<String, BindingRecord>,
    /// What this module keeps beside them, by binding id.
    pub info: BTreeMap<String, BindingInfo>,
}

/// What the core keeps per binding; other modules read it through `directory`.
#[derive(Debug, Clone, Default)]
pub struct BindingRecord {
    pub id: String,
    pub session: String,
    pub agent: String,
}

/// The rest of a binding, kept by this module.
#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub control: Control,
    pub reference: Option<Ref>,
    pub isolation: Isolation,
    pub level: Option<PermissionLevel>,
    pub network: Option<bool>,
    pub status: BindingStatus,
    pub via: BindingVia,
    pub started_ms: u64,
    pub stopped_ms: Option<u64>,
    pub stop_requested: bool,
    /// Driver item keys to Parrot item ids.
    pub items: BTreeMap<String, String>,
    /// Driver turn keys to Parrot turn ids.
    pub turns: BTreeMap<String, String>,
    /// Frames stored so far, the index of the next one.
    pub frames: u64,
}

impl Default for BindingInfo {
    fn default() -> Self {
        Self {
            control: Control::Native,
            reference: None,
            isolation: Isolation::None,
            level: None,
            network: None,
            status: BindingStatus::Starting,
            via: BindingVia::New,
            started_ms: 0,
            stopped_ms: None,
            stop_requested: false,
            items: BTreeMap::new(),
            turns: BTreeMap::new(),
            frames: 0,
        }
    }
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// An id derived from harness data: the first 128 bits of SHA-256 over `parts`, in Crockford
/// base32 (spec 2, 30.4).
pub fn derive_id(prefix: &str, parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    let digest = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    let v = u128::from_be_bytes(bytes);
    let mut out = String::with_capacity(prefix.len() + 27);
    out.push_str(prefix);
    out.push('_');
    for i in 0..26 {
        let shift = 125i32 - 5 * i;
        let idx = if shift >= 0 {
            (v >> shift) & 31
        } else {
            (v << -shift) & 31
        };
        out.push(CROCKFORD[idx as usize] as char);
    }
    out
}

/// The `Binding` JSON (spec 9).
pub fn binding_json(state: &State, id: &str) -> Option<Value> {
    let r = state.bindings.by_id.get(id)?;
    let b = state.bindings.info.get(id).cloned().unwrap_or_default();
    let mut v = json!({
        "id": r.id,
        "session": r.session,
        "agent": r.agent,
        "env": state.principals.env,
        "control": b.control,
        "isolation": b.isolation,
        "status": b.status,
        "via": b.via,
        "started": sessions::timestamp(b.started_ms),
    });
    if let Some(r) = &b.reference {
        v["ref"] = json!(r);
    }
    if let Some(l) = b.level {
        v["level"] = json!(l);
    }
    if let Some(n) = b.network {
        v["network"] = json!(n);
    }
    if let Some(s) = b.stopped_ms {
        v["stopped"] = json!(sessions::timestamp(s));
    }
    let current = crate::directory::binding_of(state, &r.session).as_deref() == Some(id);
    if current && crate::seeds::pending(state, &r.session) {
        v["pending"] = json!("bundle");
    }
    Some(v)
}

/// Write a binding to the store.
pub fn persist(cx: &mut Cx, id: &str) {
    if let Some(v) = binding_json(cx.state, id) {
        cx.persist(json!({ "op": "put", "kind": "binding", "id": id, "value": v }));
    }
}

/// Apply a `binding/start` result (spec 30.1) to a binding still starting.
pub fn started(cx: &mut Cx, binding: &str, result: &Value) {
    let Some(b) = cx.state.bindings.info.get_mut(binding) else {
        return;
    };
    if b.status == BindingStatus::Starting {
        b.status = BindingStatus::Running;
    }
    if let Some(r) = result
        .get("ref")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
    {
        b.reference = Some(r);
    }
    if let Some(i) = result
        .get("isolation")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
    {
        b.isolation = i;
    }
    if let Some(l) = result
        .get("level")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
    {
        b.level = Some(l);
    }
    if let Some(n) = result.get("network").and_then(Value::as_bool) {
        b.network = Some(n);
    }
    let Some(session) = crate::directory::session_of(cx.state, binding) else {
        return;
    };
    persist(cx, binding);
    sessions::changed(cx, &session);
}

/// The answer to `binding/start`, which the backend sends itself under a call id of its own.
/// It comes when the harness is ready for a prompt or has raised a request (Appendix E4).
pub fn start_reply(cx: &mut Cx, binding: &str, result: Result<Value, RpcError>) {
    let Some(b) = cx.state.bindings.info.get(binding) else {
        return;
    };
    if b.status != BindingStatus::Starting {
        return;
    }
    let Some(session) = crate::directory::session_of(cx.state, binding) else {
        return;
    };
    let current = crate::directory::binding_of(cx.state, &session).as_deref() == Some(binding);
    match result {
        Ok(v) => {
            started(cx, binding, &v);
            if !current {
                crate::seeds::on_started(cx, &session, binding, true);
                return;
            }
            let options: Vec<parrot_proto::option::ConfigOption> = v
                .get("options")
                .and_then(|o| serde_json::from_value(o.clone()).ok())
                .unwrap_or_default();
            if !options.is_empty() {
                let level = v
                    .get("level")
                    .and_then(|l| serde_json::from_value(l.clone()).ok());
                let network = v.get("network").and_then(Value::as_bool);
                crate::options::store_with(cx, &session, options, level, network);
            }
            if let Some(actions) = v
                .get("actions")
                .filter(|a| a.as_array().is_some_and(|a| !a.is_empty()))
            {
                let update = DriverUpdate {
                    binding: binding.to_string(),
                    subagent: None,
                    update: Open::Known(DriverUpdateKind::Actions {
                        actions: serde_json::from_value(actions.clone()).unwrap_or_default(),
                    }),
                    extra: Default::default(),
                };
                crate::actions::on_update(cx, &update);
            }
            let starting =
                cx.state.sessions.info.get(&session).map(|s| s.state) == Some(SessionState::Starting);
            if starting {
                let blocked = crate::requests::open_in(cx.state, &session)
                    .iter()
                    .any(|r| r.blocking);
                let state = if blocked {
                    SessionState::RequiresAction
                } else {
                    SessionState::Idle
                };
                if let Some(s) = cx.state.sessions.info.get_mut(&session) {
                    s.state = state;
                }
                // An idle that a queued prompt ends at once isn't shown; `running` follows.
                let hidden = queue::sends_next(cx.state, &session);
                if !hidden {
                    cx.publish(
                        Some(session.clone()),
                        json!({ "type": "state", "state": state, "background": [] }),
                    );
                    sessions::changed(cx, &session);
                }
                if state == SessionState::Idle {
                    queue::on_idle(cx, &session);
                }
                if hidden {
                    show_idle(
                        cx,
                        &session,
                        json!({ "type": "state", "state": state, "background": [] }),
                    );
                }
            }
            crate::seeds::on_started(cx, &session, binding, true);
            crate::wakes::pump(cx, &session);
        }
        Err(e) => {
            if let Some(b) = cx.state.bindings.info.get_mut(binding) {
                b.status = BindingStatus::Failed;
                b.stopped_ms = Some(cx.now_ms);
            }
            persist(cx, binding);
            crate::requests::binding_gone(cx, binding);
            queue::on_exit(cx, &session);
            crate::seeds::on_started(cx, &session, binding, false);
            if crate::directory::binding_of(cx.state, &session).as_deref() != Some(binding) {
                return;
            }
            if let Some(s) = cx.state.sessions.info.get_mut(&session) {
                s.state = SessionState::Stopped;
                s.background.clear();
            }
            let sid = Some(session.clone());
            cx.publish(
                sid.clone(),
                json!({ "type": "state", "state": "stopped", "background": [] }),
            );
            cx.publish(
                sid,
                json!({ "type": "notice", "level": "error", "text": format!("the agent failed to start: {}", e.message), "code": "start_failed" }),
            );
            sessions::changed(cx, &session);
        }
    }
}

/// Put back a binding read from the store. A binding that was starting or running is `stopped`
/// now: a restart starts nothing by itself (spec 34). Returns whether it was live.
pub fn restore(state: &mut State, value: &Value, now_ms: u64) -> bool {
    let str_of = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
    fn parse<T: serde::de::DeserializeOwned>(value: &Value, k: &str) -> Option<T> {
        value.get(k).and_then(|v| serde_json::from_value(v.clone()).ok())
    }
    let ms = |k: &str| {
        value
            .get(k)
            .and_then(Value::as_str)
            .and_then(crate::requests::parse_rfc3339_ms)
    };
    let (Some(id), Some(session), Some(agent)) = (str_of("id"), str_of("session"), str_of("agent")) else {
        return false;
    };
    let status: BindingStatus = parse(value, "status").unwrap_or(BindingStatus::Stopped);
    let live = matches!(status, BindingStatus::Starting | BindingStatus::Running);
    let info = BindingInfo {
        control: parse(value, "control").unwrap_or(Control::Native),
        reference: parse(value, "ref"),
        isolation: parse(value, "isolation").unwrap_or(Isolation::None),
        level: parse(value, "level"),
        network: value.get("network").and_then(Value::as_bool),
        status: if live { BindingStatus::Stopped } else { status },
        via: parse(value, "via").unwrap_or(BindingVia::New),
        started_ms: ms("started").unwrap_or(0),
        stopped_ms: if live { Some(now_ms) } else { ms("stopped") },
        ..Default::default()
    };
    state.bindings.by_id.insert(
        id.clone(),
        BindingRecord {
            id: id.clone(),
            session,
            agent,
        },
    );
    state.bindings.info.insert(id, info);
    live
}

/// A driver answered a call this module made with `Cx::call_driver` (`binding/stop`).
pub fn on_reply(cx: &mut Cx, _call: u64, result: Result<Value, RpcError>) {
    if let Err(e) = result {
        cx.publish(
            None,
            json!({ "type": "notice", "level": "warning", "text": format!("binding/stop failed: {}", e.message) }),
        );
    }
}

/// The Parrot id for a driver key, derived once and remembered (spec 30.4).
pub(crate) fn map_key(
    cx: &mut Cx,
    binding: &str,
    kind: &str,
    key: &str,
    harness_ref: Option<&Ref>,
) -> String {
    let Some(session) = crate::directory::session_of(cx.state, binding) else {
        return key.to_string();
    };
    let b = cx.state.bindings.info.entry(binding.to_string()).or_default();
    let map = if kind == "trn" { &mut b.turns } else { &mut b.items };
    if let Some(id) = map.get(key) {
        return id.clone();
    }
    let id = match harness_ref {
        Some(r) if r.strength == RefStrength::Strong => derive_id(kind, &[&session, kind, &r.id]),
        _ => derive_id(kind, &[&session, binding, kind, key]),
    };
    map.insert(key.to_string(), id.clone());
    id
}

/// A driver update other than requests, options, actions and usage.
pub fn on_update(cx: &mut Cx, update: &DriverUpdate) {
    let binding = update.binding.as_str();
    let Some(session) = crate::directory::session_of(cx.state, binding) else {
        return;
    };
    // Subagents are outside the core profile; their updates are not projected.
    if update.subagent.is_some() {
        return;
    }
    let Open::Known(kind) = &update.update else {
        return;
    };
    let sid = Some(session.clone());
    match kind {
        DriverUpdateKind::State {
            state,
            background,
            detail,
        } => {
            let changed = {
                let Some(s) = cx.state.sessions.info.get_mut(&session) else {
                    return;
                };
                let changed = s.state != *state;
                s.state = *state;
                s.background = background.clone();
                changed
            };
            let mut u = json!({ "type": "state", "state": state, "background": background });
            if let Some(d) = detail {
                u["detail"] = json!(d);
            }
            // An idle that a queued prompt ends at once isn't shown; `running` follows.
            let hidden = queue::sends_next(cx.state, &session);
            if !hidden {
                cx.publish(sid, u.clone());
                if changed {
                    sessions::changed(cx, &session);
                }
            }
            if *state == SessionState::Idle {
                crate::tasks::on_idle(cx, &session);
                crate::budgets::on_idle(cx, &session);
                crate::seeds::on_idle(cx, &session);
                queue::on_idle(cx, &session);
                crate::wakes::pump(cx, &session);
            }
            if hidden {
                show_idle(cx, &session, u);
            }
        }
        DriverUpdateKind::Item { item } => {
            let mut patch = item.clone();
            let is_user = matches!(patch.kind, Some(Open::Known(parrot_proto::item::ItemKind::User)));
            let message = patch
                .set
                .as_ref()
                .and_then(|s| s.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let known = cx
                .state
                .bindings
                .info
                .get(binding)
                .and_then(|b| b.items.get(&patch.id))
                .cloned();
            patch.id = match (known, is_user, message) {
                (Some(id), _, _) => id,
                // The echo of a runtime prompt takes the message id (Appendix E4).
                (None, true, Some(m)) => {
                    if let Some(b) = cx.state.bindings.info.get_mut(binding) {
                        b.items.insert(patch.id.clone(), m.clone());
                    }
                    m
                }
                (None, _, _) => map_key(cx, binding, "itm", &patch.id, patch.reference.as_ref()),
            };
            if let Some(p) = &patch.parent {
                patch.parent = Some(map_key(cx, binding, "itm", p, None));
            }
            if let Some(t) = &patch.turn {
                patch.turn = Some(map_key(cx, binding, "trn", t, None));
            }
            patch.binding = Some(binding.to_string());
            cx.publish(sid, json!({ "type": "item", "item": patch }));
        }
        DriverUpdateKind::Turn { turn } => {
            let mut turn = turn.clone();
            let first = !cx
                .state
                .bindings
                .info
                .get(binding)
                .is_some_and(|b| b.turns.contains_key(&turn.id));
            turn.id = map_key(cx, binding, "trn", &turn.id, None);
            if first {
                let input = turn.input.as_ref().and_then(|key| {
                    cx.state
                        .bindings
                        .info
                        .get(binding)
                        .and_then(|b| b.items.get(key))
                        .cloned()
                });
                crate::seeds::on_turn(cx, &session, &turn.id, input.as_deref());
                crate::budgets::on_turn(cx, &session);
            }
            crate::budgets::on_turn_status(cx, &session, &turn.id, turn.status == TurnStatus::Running);
            crate::tasks::on_turn(cx, &session, turn.status, first);
            turn.binding = binding.to_string();
            // Retry and continue read whether a turn runs and what a failed one was given.
            let running = turn.status == TurnStatus::Running;
            let failed = (turn.status == TurnStatus::Failed).then(|| {
                let input = turn.input.as_ref().map(|key| {
                    cx.state
                        .bindings
                        .info
                        .get(binding)
                        .and_then(|b| b.items.get(key))
                        .cloned()
                        .unwrap_or_else(|| key.clone())
                });
                input
                    .and_then(|id| crate::items::get(cx.state, &session, &id))
                    .and_then(|item| item.get("text").and_then(Value::as_str).map(String::from))
                    .into_iter()
                    .collect()
            });
            let failed_now = failed.is_some();
            crate::actions::note_turn(cx.state, &session, running, failed);
            crate::actions::persist_failed(cx, &session, failed_now || running);
            // A running turn is kept in the store, so a restart can end it as `interrupted` (E7).
            if running {
                cx.persist(json!({ "op": "put", "kind": "turn", "id": turn.id,
                    "value": { "session": session, "turn": turn } }));
            } else {
                cx.persist(json!({ "op": "delete", "kind": "turn", "id": turn.id }));
            }
            cx.publish(sid, json!({ "type": "turn", "turn": turn }));
        }
        DriverUpdateKind::Ref { reference } => {
            if let Some(b) = cx.state.bindings.info.get_mut(binding) {
                b.reference = Some(reference.clone());
            }
            persist(cx, binding);
            sessions::changed(cx, &session);
        }
        DriverUpdateKind::Mcp { servers } => {
            if let Some(s) = cx.state.sessions.info.get_mut(&session) {
                s.mcp = Some(servers.clone());
            }
            sessions::changed(cx, &session);
        }
        DriverUpdateKind::Notice { level, text, code } => {
            let mut u = json!({ "type": "notice", "level": level, "text": text });
            if let Some(c) = code {
                u["code"] = json!(c);
            }
            cx.publish(sid, u);
        }
        DriverUpdateKind::Frame { source, data } => {
            let agent = cx
                .state
                .bindings
                .by_id
                .get(binding)
                .map(|b| b.agent.clone())
                .unwrap_or_default();
            let b = cx.state.bindings.info.entry(binding.to_string()).or_default();
            let idx = b.frames;
            b.frames += 1;
            let version = cx
                .state
                .sessions
                .agents
                .get(&agent)
                .and_then(|a| a.pointer("/driver/data"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            cx.persist(json!({
                "op": "frame", "binding": binding, "idx": idx, "at": cx.now_ms, "dataVersion": version,
                "frame": { "source": source, "data": data },
            }));
        }
        _ => {}
    }
}

/// The driver process for a binding exited: `stopped` if a stop was asked for, else `lost`.
/// Publishes a hidden idle after all if nothing was sent to end it.
fn show_idle(cx: &mut Cx, session: &str, update: Value) {
    if cx.state.sessions.info.get(session).map(|s| s.state) == Some(SessionState::Idle) {
        cx.publish(Some(session.to_string()), update);
        sessions::changed(cx, session);
    }
}

pub fn exited(cx: &mut Cx, binding: &str, detail: &str) {
    let Some(b) = cx.state.bindings.info.get_mut(binding) else {
        return;
    };
    if matches!(
        b.status,
        BindingStatus::Stopped | BindingStatus::Lost | BindingStatus::Failed
    ) {
        return;
    }
    let (status, state) = if b.stop_requested {
        (BindingStatus::Stopped, SessionState::Stopped)
    } else {
        (BindingStatus::Lost, SessionState::Lost)
    };
    b.status = status;
    b.stopped_ms = Some(cx.now_ms);
    let Some(session) = crate::directory::session_of(cx.state, binding) else {
        return;
    };
    persist(cx, binding);
    crate::requests::binding_gone(cx, binding);
    queue::on_exit(cx, &session);
    let current = crate::directory::binding_of(cx.state, &session).as_deref() == Some(binding);
    if !current {
        return;
    }
    if let Some(s) = cx.state.sessions.info.get_mut(&session) {
        s.state = state;
        s.background.clear();
    }
    let sid = Some(session.clone());
    cx.publish(
        sid.clone(),
        json!({ "type": "state", "state": state, "background": [] }),
    );
    if status == BindingStatus::Lost {
        cx.publish(
            sid,
            json!({ "type": "notice", "level": "error", "text": format!("the agent exited: {detail}"), "code": "lost" }),
        );
    }
    sessions::changed(cx, &session);
}
