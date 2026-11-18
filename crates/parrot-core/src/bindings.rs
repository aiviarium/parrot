//! Bindings and the driver updates they report (spec 9, 30.2, 30.4, Appendix E4).

use crate::{Cx, State, queue, sessions};
use parrot_proto::agent::Control;
use parrot_proto::driver::{DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::Isolation;
use parrot_proto::rpc::RpcError;
use parrot_proto::session::{BindingStatus, BindingVia, Ref, RefStrength};
use parrot_proto::state::State as SessionState;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Bindings by id.
#[derive(Debug, Default)]
pub struct Bindings {
    pub by_id: BTreeMap<String, BindingRecord>,
}

/// What the core keeps per binding.
#[derive(Debug, Clone)]
pub struct BindingRecord {
    pub id: String,
    pub session: String,
    pub agent: String,
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

impl Default for BindingRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            session: String::new(),
            agent: String::new(),
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
    let b = state.bindings.by_id.get(id)?;
    let mut v = json!({
        "id": b.id,
        "session": b.session,
        "agent": b.agent,
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
    let Some(b) = cx.state.bindings.by_id.get_mut(binding) else {
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
    let session = b.session.clone();
    persist(cx, binding);
    sessions::changed(cx, &session);
}

/// A driver answered a call this module made with `Cx::call_driver` (`binding/stop`). The
/// `binding/start` answer normally never comes here: the backend sends that call itself, so the
/// start is recorded from the first `state` update instead.
pub fn on_reply(cx: &mut Cx, _call: u64, result: Result<Value, RpcError>) {
    if let Err(e) = result {
        cx.publish(
            None,
            json!({ "type": "notice", "level": "warning", "text": format!("binding/stop failed: {}", e.message) }),
        );
    }
}

/// The Parrot id for a driver key, derived once and remembered (spec 30.4).
fn map_key(cx: &mut Cx, binding: &str, kind: &str, key: &str, harness_ref: Option<&Ref>) -> String {
    let Some(b) = cx.state.bindings.by_id.get_mut(binding) else {
        return key.to_string();
    };
    let map = if kind == "trn" { &mut b.turns } else { &mut b.items };
    if let Some(id) = map.get(key) {
        return id.clone();
    }
    let id = match harness_ref {
        Some(r) if r.strength == RefStrength::Strong => derive_id(kind, &[&b.session, kind, &r.id]),
        _ => derive_id(kind, &[&b.session, binding, kind, key]),
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
            if cx.state.bindings.by_id.get(binding).map(|b| b.status) == Some(BindingStatus::Starting) {
                started(cx, binding, &Value::Null);
            }
            let changed = {
                let Some(s) = cx.state.sessions.by_id.get_mut(&session) else {
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
            cx.publish(sid, u);
            if changed {
                sessions::changed(cx, &session);
            }
            if *state == SessionState::Idle {
                queue::on_idle(cx, &session);
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
                .by_id
                .get(binding)
                .and_then(|b| b.items.get(&patch.id))
                .cloned();
            patch.id = match (known, is_user, message) {
                (Some(id), _, _) => id,
                // The echo of a runtime prompt takes the message id (Appendix E4).
                (None, true, Some(m)) => {
                    if let Some(b) = cx.state.bindings.by_id.get_mut(binding) {
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
            turn.id = map_key(cx, binding, "trn", &turn.id, None);
            turn.binding = binding.to_string();
            cx.publish(sid, json!({ "type": "turn", "turn": turn }));
        }
        DriverUpdateKind::Ref { reference } => {
            if let Some(b) = cx.state.bindings.by_id.get_mut(binding) {
                b.reference = Some(reference.clone());
            }
            persist(cx, binding);
            sessions::changed(cx, &session);
        }
        DriverUpdateKind::Mcp { servers } => {
            if let Some(s) = cx.state.sessions.by_id.get_mut(&session) {
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
            let Some(b) = cx.state.bindings.by_id.get_mut(binding) else {
                return;
            };
            let idx = b.frames;
            b.frames += 1;
            let version = cx
                .state
                .sessions
                .agents
                .get(&b.agent)
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
pub fn exited(cx: &mut Cx, binding: &str, detail: &str) {
    let Some(b) = cx.state.bindings.by_id.get_mut(binding) else {
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
    let session = b.session.clone();
    persist(cx, binding);
    queue::on_exit(cx, &session);
    let current = crate::directory::binding_of(cx.state, &session).as_deref() == Some(binding);
    if !current {
        return;
    }
    if let Some(s) = cx.state.sessions.by_id.get_mut(&session) {
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
