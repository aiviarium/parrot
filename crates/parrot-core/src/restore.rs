//! Starting from the store after a restart (spec 34, Appendix E7).
//!
//! Every module's state comes back from the projections it persisted. Then the restart rules
//! apply: bindings that were running are `stopped` and nothing starts by itself; queues keep
//! their entries and stay paused until the next prompt, action or `session/start`; a prompt the
//! harness never confirmed is `uncertain` and is not sent again; `connection` requests expire and
//! `message` requests stay open. The sequence continues after the stored head.

use crate::input::Restore;
use crate::subscriptions::{Logged, RECENT};
use crate::{Cx, actions, bindings, items, options, queue, requests, sessions, usage};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Apply what the store held. Meant as the first input of a runtime.
pub fn restore(cx: &mut Cx, r: Restore) {
    cx.state.seq = cx.state.seq.max(r.head);
    let subs = &mut cx.state.subscriptions;
    let skip = r.recent.len().saturating_sub(RECENT);
    for s in r.recent.into_iter().skip(skip) {
        subs.recent.push_back(Logged {
            seq: s.seq,
            at_ms: s.at_ms,
            session: s.session,
            update: s.update,
        });
    }
    // Replay from before what came back is unavailable (-32012), not silently empty.
    subs.dropped = match subs.recent.front() {
        Some(first) => first.seq.saturating_sub(1),
        None => r.head,
    };

    let mut by_kind: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();
    for (kind, id, value) in r.projections {
        by_kind.entry(kind).or_default().push((id, value));
    }
    let mut take = |kind: &str| by_kind.remove(kind).unwrap_or_default();

    for (id, value) in take("agent") {
        cx.state.sessions.agents.insert(id, value);
    }
    for (id, value) in take("workspace") {
        sessions::restore_workspace(cx.state, &id, &value);
        crate::tree::restore_workspace(cx.state, &id, &value);
    }
    for (id, value) in take("session") {
        sessions::restore(cx.state, &id, &value);
    }
    let mut stopped = Vec::new();
    for (id, value) in take("binding") {
        if bindings::restore(cx.state, &value, cx.now_ms) {
            stopped.push(id);
        }
    }
    let mut stored_items: Vec<(u64, String, Map<String, Value>)> = take("item")
        .into_iter()
        .filter_map(|(_, v)| {
            let session = v.get("session")?.as_str()?.to_string();
            let created = v.get("created").and_then(Value::as_u64).unwrap_or(0);
            let item = v.get("item")?.as_object()?.clone();
            Some((created, session, item))
        })
        .collect();
    stored_items.sort_by_key(|(created, _, _)| *created);
    for (created, session, item) in stored_items {
        items::restore(cx.state, &session, item, created);
    }
    for (id, value) in take("tree") {
        crate::tree::restore(cx.state, &id, &value);
    }
    for (id, value) in take("seed") {
        crate::seeds::restore(cx.state, &id, &value);
    }
    let ids: Vec<String> = cx.state.sessions.by_id.keys().cloned().collect();
    for id in ids {
        crate::tree::ensure(cx.state, &id);
    }
    for (id, value) in take("options") {
        options::restore(cx.state, &id, &value);
    }
    for (id, value) in take("actions") {
        actions::restore(cx.state, &id, &value);
    }
    for (id, value) in take("usage") {
        usage::restore(cx.state, &id, &value);
    }
    for (id, value) in take("queue") {
        queue::restore(cx.state, &id, &value);
    }
    for (id, value) in take("request") {
        requests::restore(cx, &id, &value);
    }
    for (message, value) in take("inflight") {
        uncertain(cx, &message, &value);
    }

    for binding in stopped {
        bindings::persist(cx, &binding);
        let Some(session) = crate::directory::session_of(cx.state, &binding) else {
            continue;
        };
        if crate::directory::binding_of(cx.state, &session).as_deref() != Some(binding.as_str()) {
            continue;
        }
        cx.publish(
            Some(session.clone()),
            json!({ "type": "state", "state": "stopped", "background": [] }),
        );
        sessions::changed(cx, &session);
    }
    // Queues a restart holds are published as paused.
    let held: Vec<String> = cx
        .state
        .queue
        .by_session
        .iter()
        .filter(|(_, q)| q.held)
        .map(|(s, _)| s.clone())
        .collect();
    for session in held {
        let q = queue::queue_state(cx.state, &session);
        cx.publish(Some(session), json!({ "type": "queue", "queue": q }));
    }

    // Running tasks were interrupted (E7).
    crate::tasks::after_restore(cx);

    let agents: Vec<String> = cx.state.sessions.agents.keys().cloned().collect();
    for agent in agents {
        sessions::discover(cx, &agent, None);
    }
}

/// A prompt sent before the restart that the harness never confirmed: its user item becomes
/// `uncertain`, and `retry` can send it again (Appendix E8).
fn uncertain(cx: &mut Cx, message: &str, value: &Value) {
    cx.persist(json!({ "op": "delete", "kind": "inflight", "id": message }));
    let Some(session) = value.get("session").and_then(Value::as_str) else {
        return;
    };
    if !cx.state.sessions.by_id.contains_key(session) {
        return;
    }
    let text = value.get("text").and_then(Value::as_str).unwrap_or_default();
    cx.state
        .actions
        .uncertain
        .insert(session.to_string(), text.to_string());
    if items::contains(cx.state, session, message) {
        cx.publish(
            Some(session.to_string()),
            json!({ "type": "item", "item": { "id": message, "set": { "status": "uncertain" } } }),
        );
    }
}
