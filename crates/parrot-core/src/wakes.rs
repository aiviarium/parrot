//! Waking a parent on its children's changes (spec 20.2, Appendix E7).
//!
//! Changes a parent wakes on are recorded in `tasks.undelivered`. When the parent is idle with an
//! unpaused queue, one wake entry lists them all. Wake entries never unpause a queue and only
//! start a stopped parent when its `Wake.start` says so.

use crate::{Cx, State};
use parrot_proto::prompt::QueueEntryKind;
use parrot_proto::session::BindingVia;
use parrot_proto::state::State as SessionState;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `Wake` (spec 20.2, E7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wake {
    #[serde(default)]
    pub on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<bool>,
}

fn idle(state: &State, session: &str) -> bool {
    state.sessions.info.get(session).map(|s| s.state) == Some(SessionState::Idle)
        && crate::sessions::active(state, session)
        && !crate::queue::is_busy(state, session)
}

/// Send a wake entry to `parent` if it has undelivered changes and can take one now.
pub fn pump(cx: &mut Cx, parent: &str) {
    let pending = cx
        .state
        .tasks
        .undelivered
        .get(parent)
        .is_some_and(|l| !l.is_empty());
    if !pending {
        return;
    }
    let q = cx.state.queue.by_session.get(parent);
    if q.is_some_and(|q| q.paused || q.entries.iter().any(|e| e.kind == QueueEntryKind::Wake)) {
        return;
    }
    if !idle(cx.state, parent) {
        let start = cx
            .state
            .tree
            .nodes
            .get(parent)
            .and_then(|n| n.wake.as_ref())
            .and_then(|w| w.start)
            .unwrap_or(false);
        if start && !crate::sessions::active(cx.state, parent) {
            crate::sessions::start_binding(cx, parent, BindingVia::New);
        }
        return;
    }
    let cursors = cx.state.tasks.undelivered.remove(parent).unwrap_or_default();
    let mut lines = Vec::new();
    let mut cause = None;
    let mut seen = Vec::new();
    for c in cx.state.tasks.changes.iter().rev() {
        if !cursors.contains(&c.cursor) || seen.contains(&c.session) {
            continue;
        }
        seen.push(c.session.clone());
        cause.get_or_insert_with(|| c.session.clone());
        let title = cx
            .state
            .sessions
            .info
            .get(&c.session)
            .and_then(|i| i.title.clone())
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        lines.push(format!("- {}{title}: {}", c.session, c.status));
    }
    lines.reverse();
    let text = format!(
        "Child sessions changed:\n{}\nRead more with parrot.tasks.wait or parrot.sessions.read.",
        lines.join("\n")
    );
    let origin = json!({ "kind": "wake", "cause": { "session": cause } });
    crate::queue::enqueue(cx, parent, QueueEntryKind::Wake, text, origin, json!({}));
    crate::queue::dispatch(cx, parent);
}
