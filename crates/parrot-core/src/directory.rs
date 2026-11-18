//! Lookups every module needs, over the sessions and bindings state.

use crate::State;

/// The binding currently attached to a session.
pub fn binding_of(state: &State, session: &str) -> Option<String> {
    state.sessions.by_id.get(session).and_then(|s| s.binding.clone())
}

/// The session a binding belongs to.
pub fn session_of(state: &State, binding: &str) -> Option<String> {
    state.bindings.by_id.get(binding).map(|b| b.session.clone())
}

/// The session ids from the root down to `session` (spec 15 `path`).
pub fn path_of(state: &State, session: &str) -> Vec<String> {
    let mut path = vec![session.to_string()];
    let mut cur = session.to_string();
    while let Some(parent) = state.sessions.by_id.get(&cur).and_then(|s| s.parent.clone()) {
        if path.contains(&parent) {
            break;
        }
        path.push(parent.clone());
        cur = parent;
    }
    path.reverse();
    path
}
