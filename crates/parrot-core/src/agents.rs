//! The agents the runtime knows (spec 6): which exist, how each is driven, and what discovery
//! found. Agents come from installed driver eggs, fed in with [`load`]. With no egg installed the
//! runtime has no agents, and `agent/list` says how to install one. The built-in list is only the
//! default of a core that never loads eggs, as in tests.
//!
//! When a driver reports the harness version it found, the agent's `harness.verified` says
//! whether its egg's `cli versions` range covers it (spec 31). Sessions of an unverified agent
//! carry the same `harness` object, so clients can show that the data may be out of date.

use crate::principals::{check, parse};
use crate::{Cx, Effect, Reply, State};
use parrot_proto::agent::{Control, DriverKind};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What `agent/list` adds when no driver egg is installed.
pub const NO_EGGS_HINT: &str = "No driver eggs are installed. Install one with `parrot eggs install aiviarium/claude` (or codex, pi, opencode, t3, acp), then restart `parrot serve`.";

/// What a driver egg says about its agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEgg {
    pub id: String,
    pub name: String,
    pub driver_name: String,
    pub driver_version: String,
    pub kind: DriverKind,
    /// The driver data version.
    pub data: String,
    pub controls: Vec<Control>,
    /// The semver range of harness versions the egg's data covers.
    pub cli_versions: Option<String>,
    /// Why the egg can't be used, such as a `requires parrot` this runtime doesn't meet.
    pub refused: Option<String>,
}

/// The agents known before any egg is loaded.
pub fn builtin() -> BTreeMap<String, Value> {
    let mut agents = BTreeMap::new();
    agents.insert("claude".to_string(), builtin_agent("claude", "Claude Code"));
    agents.insert("codex".to_string(), builtin_agent("codex", "Codex"));
    agents.insert("pi".to_string(), builtin_agent("pi", "Pi"));
    agents.insert("opencode".to_string(), builtin_agent("opencode", "opencode"));
    agents
}

/// A built-in agent before its driver has answered `agent/list`.
fn builtin_agent(id: &str, name: &str) -> Value {
    record(&AgentEgg {
        id: id.into(),
        name: name.into(),
        driver_name: id.into(),
        driver_version: env!("CARGO_PKG_VERSION").into(),
        kind: DriverKind::Builtin,
        data: "1".into(),
        controls: vec![Control::Native],
        cli_versions: None,
        refused: None,
    })
}

/// The `Agent` JSON for an egg before its driver has answered `agent/list`.
pub fn record(egg: &AgentEgg) -> Value {
    let mut a = json!({
        "id": egg.id,
        "name": egg.name,
        "driver": { "name": egg.driver_name, "version": egg.driver_version, "kind": egg.kind, "data": egg.data },
        "installed": true,
        "auth": "unknown",
        "usable": egg.refused.is_none(),
        "controls": egg.controls,
        "features": {},
        "levels": [],
        "isolation": ["none"],
        "options": [],
        "actions": [],
    });
    if let Some(r) = &egg.refused {
        a["detail"] = json!(r);
    }
    a
}

/// Replace the known agents with those of `eggs`. Called once at startup, before the store
/// is restored.
pub fn load(state: &mut State, eggs: Vec<AgentEgg>) {
    state.sessions.agents.clear();
    state.sessions.eggs.clear();
    state.sessions.eggs_loaded = true;
    for egg in eggs {
        state.sessions.agents.insert(egg.id.clone(), record(&egg));
        state.sessions.eggs.insert(egg.id.clone(), egg);
    }
}

/// Put back an agent read from the store. What its egg says wins over the stored copy, since
/// the egg may have changed since.
pub fn restore(state: &mut State, id: String, mut value: Value) {
    if state.sessions.eggs_loaded && !state.sessions.eggs.contains_key(&id) {
        // Its egg is gone: the agent is no longer known.
        return;
    }
    apply_egg(state, &id, &mut value);
    state.sessions.agents.insert(id, value);
}

/// Overlay what the egg says (driver kind and data version, refusal, harness coverage).
fn apply_egg(state: &State, id: &str, a: &mut Value) {
    let Some(egg) = state.sessions.eggs.get(id) else {
        return;
    };
    if !a.get("driver").is_some_and(Value::is_object) {
        a["driver"] = json!({ "name": egg.driver_name, "version": egg.driver_version });
    }
    a["driver"]["kind"] = json!(egg.kind);
    a["driver"]["data"] = json!(egg.data);
    // The egg's `controls` add what the runtime drives itself, such as TUI control (E4).
    if let Some(list) = a.get_mut("controls").and_then(Value::as_array_mut) {
        for c in &egg.controls {
            let c = json!(c);
            if !list.contains(&c) {
                list.push(c);
            }
        }
    }
    if let Some(r) = &egg.refused {
        a["usable"] = json!(false);
        a["detail"] = json!(r);
        return;
    }
    let (Some(version), Some(range)) = (
        a.pointer("/harness/version")
            .and_then(Value::as_str)
            .map(str::to_string),
        egg.cli_versions.as_deref(),
    ) else {
        return;
    };
    let verified = covers(range, &version);
    a["harness"]["verified"] = json!(verified);
    if !verified && a.get("detail").is_none() {
        a["detail"] = json!(format!(
            "{} {version} is outside the versions its driver data covers ({range}); it runs unverified",
            egg.name
        ));
    }
}

/// Whether a harness version, as a CLI prints it (`2.1.3 (Claude Code)`, `codex-cli 0.9`), is
/// in a semver range. A version that can't be read is not covered.
pub fn covers(range: &str, version: &str) -> bool {
    let Ok(req) = semver::VersionReq::parse(range) else {
        return false;
    };
    let token = version
        .split(|c: char| c.is_whitespace() || c == '(' || c == ',')
        .map(|t| t.trim_start_matches('v'))
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()));
    let Some(token) = token else {
        return false;
    };
    let (core, rest) = match token.find(['-', '+']) {
        Some(i) => token.split_at(i),
        None => (token, ""),
    };
    let mut parts: Vec<&str> = core.split('.').collect();
    while parts.len() < 3 {
        parts.push("0");
    }
    semver::Version::parse(&format!("{}{rest}", parts.join("."))).is_ok_and(|v| req.matches(&v))
}

/// The `harness` object sessions of `agent` carry when its harness is unverified.
pub fn session_harness(state: &State, agent: &str) -> Option<Value> {
    let h = state.sessions.agents.get(agent)?.get("harness")?;
    (h.get("verified") == Some(&json!(false))).then(|| h.clone())
}

/// `agent/*` methods.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "agent/list" => Reply::Now(check(cx.state, conn, Scope::Read).map(|()| {
            let agents = agents(cx.state);
            if agents.is_empty() && cx.state.sessions.eggs_loaded {
                json!({ "agents": agents, "hint": NO_EGGS_HINT })
            } else {
                json!({ "agents": agents })
            }
        })),
        "agent/refresh" => refresh(cx, conn, id, params),
        _ => crate::sessions::call(cx, conn, id, method, params),
    }
}

/// Agents with `env` set.
pub fn agents(state: &State) -> Vec<Value> {
    state
        .sessions
        .agents
        .values()
        .map(|a| {
            let mut a = a.clone();
            a["env"] = json!(state.principals.env);
            a
        })
        .collect()
}

fn refresh(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let agent = match check(cx.state, conn, Scope::Configure)
        .and_then(|()| parse::<parrot_proto::agent::AgentRefreshParams>(params))
    {
        Ok(p) => p.agent,
        Err(e) => return Reply::Now(Err(e)),
    };
    if !cx.state.sessions.agents.contains_key(&agent) {
        return Reply::Now(Err(not_found(&agent)));
    }
    discover(cx, &agent, id.map(|id| (conn.to_string(), id.clone())));
    Reply::Later
}

fn not_found(id: &str) -> RpcError {
    RpcError::new(-32002, format!("agent not found: {id}"))
}

/// Ask an agent's driver for its agents, models and options (spec 6, 30.1). `reply_to` is an
/// `agent/refresh` call to answer once the driver has.
pub fn discover(cx: &mut Cx, agent: &str, reply_to: Option<(String, RequestId)>) {
    let call = cx.next_call();
    cx.state
        .sessions
        .discovery
        .insert(call, (agent.to_string(), reply_to));
    cx.emit(Effect::AgentCall {
        agent: agent.to_string(),
        call,
        method: "agent/list".into(),
        params: json!({}),
    });
}

/// A driver answered `agent/list`: its agents replace what the runtime knew, and go to the index.
pub fn on_agent_reply(cx: &mut Cx, agent: &str, call: u64, result: Result<Value, RpcError>) {
    let Some((asked, reply_to)) = cx.state.sessions.discovery.remove(&call) else {
        return;
    };
    let asked = if asked.is_empty() {
        agent.to_string()
    } else {
        asked
    };
    let mut found: Vec<Value> = match &result {
        Ok(v) => v
            .get("agents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.get("id").and_then(Value::as_str).is_some())
            .collect(),
        Err(_) => Vec::new(),
    };
    if found.is_empty() {
        // The driver can't be asked: keep what is known, marked unusable with the reason.
        let detail = match &result {
            Err(e) => e.message.clone(),
            Ok(_) => "the driver reported no agents".into(),
        };
        let mut a = cx
            .state
            .sessions
            .agents
            .get(&asked)
            .cloned()
            .unwrap_or_else(|| json!({ "id": asked }));
        a["usable"] = json!(false);
        a["detail"] = json!(detail);
        found.push(a);
    }
    for mut a in found {
        a["env"] = json!(cx.state.principals.env);
        let id = a["id"].as_str().unwrap_or_default().to_string();
        // A variant such as `claude-work` shares the egg of the agent that was asked.
        let egg = if cx.state.sessions.eggs.contains_key(&id) {
            id.clone()
        } else {
            asked.clone()
        };
        apply_egg(cx.state, &egg, &mut a);
        let before = session_harness(cx.state, &id);
        cx.persist(json!({ "op": "put", "kind": "agent", "id": id, "value": a }));
        cx.publish(None, json!({ "type": "agent", "agent": a }));
        cx.state.sessions.agents.insert(id.clone(), a);
        if session_harness(cx.state, &id) != before {
            let sessions: Vec<String> = cx
                .state
                .sessions
                .by_id
                .values()
                .filter(|s| s.agent == id)
                .map(|s| s.id.clone())
                .collect();
            for s in sessions {
                crate::sessions::changed(cx, &s);
            }
        }
    }
    if let Some((conn, id)) = reply_to {
        let agent = cx.state.sessions.agents.get(&asked).cloned();
        let result = match (agent, result) {
            (_, Err(e)) => Err(e),
            (Some(a), Ok(_)) => Ok(json!({ "agent": a })),
            (None, Ok(_)) => Err(not_found(&asked)),
        };
        cx.reply(&conn, id, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_as_clis_print_them() {
        assert!(covers(">=2.0", "2.1.327 (Claude Code)"));
        assert!(covers(">=0.100", "codex-cli 0.167.1"));
        assert!(covers("^1", "v1.5"));
        assert!(!covers(">=2.0", "1.9.9"));
        assert!(!covers(">=2.0", "unknown"));
        assert!(!covers("not a range", "2.0.0"));
    }
}
