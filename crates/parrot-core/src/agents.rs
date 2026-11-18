//! The agents the runtime knows (spec 6): which exist, how each is driven, and what discovery
//! found. Agents come from installed driver eggs; the built-in list is the fallback.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The agents known before any egg is loaded.
pub fn builtin() -> BTreeMap<String, Value> {
    let mut agents = BTreeMap::new();
    agents.insert("claude".to_string(), builtin_agent("claude", "Claude Code"));
    agents.insert("codex".to_string(), builtin_agent("codex", "Codex"));
    agents
}

/// A built-in agent before its driver has answered `agent/list`.
fn builtin_agent(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "driver": { "name": id, "version": env!("CARGO_PKG_VERSION"), "kind": "builtin", "data": "1" },
        "installed": true,
        "auth": "unknown",
        "usable": true,
        "controls": ["native"],
        "features": {},
        "levels": [],
        "isolation": ["none"],
        "options": [],
        "actions": [],
    })
}

/// `agent/*` methods. Discovery still lives in `sessions` for now.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    crate::sessions::call(cx, conn, id, method, params)
}

/// A driver's answer to an `Effect::AgentCall`.
pub fn on_agent_reply(cx: &mut Cx, agent: &str, call: u64, result: Result<Value, RpcError>) {
    crate::sessions::on_agent_reply(cx, agent, call, result)
}
