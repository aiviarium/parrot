//! Functions (spec 25–29, E5): the catalog, calls between clients, harnesses and providers,
//! the session functions, and the per-binding credentials of the injected MCP server.
//!
//! Functions come from three providers: clients (`function/provide` or `initialize.provides`),
//! the runtime's own session functions, and runtime providers such as the desktop, which run in
//! `parrot-serve` as `Effect::Workspace` operations named `function.<name>`.

use crate::{CallOwner, Cx, Effect, Reply, principals};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct Functions {
    /// Client-provided functions by `(name, env)`: the providing connection and the spec.
    pub provided: BTreeMap<(String, String), (String, Value)>,
    /// Functions the runtime itself provides through the backend, such as `parrot.desktop.*`.
    pub runtime: BTreeMap<String, Value>,
    /// Calls waiting for a provider.
    pub pending: BTreeMap<u64, PendingCall>,
    /// Results of keyed calls, by caller and key (spec 27.1 keys).
    pub keys: BTreeMap<(String, String), Value>,
    /// Per-binding credentials of the injected MCP server, by token.
    pub tokens: BTreeMap<String, String>,
    /// The `parrot` program for `parrot mcp`; `None` turns the injected server off.
    pub mcp: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingCall {
    pub conn: String,
    pub id: Option<RequestId>,
    pub key: Option<(String, String)>,
}

/// The session functions (spec 28) and the protocol method each projects.
pub const SESSION_FUNCTIONS: [(&str, &str, &str); 16] = [
    ("parrot.sessions.spawn", "session/new", "delegate"),
    ("parrot.sessions.send", "session/prompt", "prompt"),
    ("parrot.sessions.act", "action/invoke", "prompt"),
    ("parrot.sessions.cancel", "session/cancel", "prompt"),
    ("parrot.sessions.stop", "session/stop", "manage"),
    ("parrot.sessions.read", "session/get", "read"),
    ("parrot.sessions.tree", "session/tree", "read"),
    ("parrot.sessions.handoff", "session/rebind", "delegate"),
    ("parrot.tasks.assign", "task/assign", "delegate"),
    ("parrot.tasks.wait", "task/wait", "read"),
    ("parrot.task.finish", "task/finish", "read"),
    ("parrot.task.fail", "task/fail", "read"),
    ("parrot.requests.list", "request/list", "read"),
    ("parrot.workspaces.status", "workspace/status", "read"),
    ("parrot.agents.list", "agent/list", "read"),
    ("parrot.envs.list", "env/list", "read"),
];

/// Checks a name against spec 27.1: dot-separated `[a-z][a-z0-9_]*` segments, none with `__`,
/// at most four, and an MCP name of at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    segments.len() <= 4
        && name.replace('.', "__").len() <= 64
        && segments.iter().all(|s| {
            let mut chars = s.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !s.contains("__")
        })
}

const LEVELS: [&str; 5] = ["read", "write", "network", "full", "host"];

fn check_spec(spec: &Value) -> Result<(String, String), RpcError> {
    let name = spec["name"].as_str().unwrap_or_default();
    if !valid_name(name) || name.starts_with("parrot.") {
        return Err(RpcError::new(-32602, format!("invalid function name: {name}")));
    }
    if !spec["input"].is_object() || spec["description"].as_str().is_none() {
        return Err(RpcError::new(
            -32602,
            format!("{name}: description and input are required"),
        ));
    }
    // Levels are required (E5).
    if !spec["level"].as_str().is_some_and(|l| LEVELS.contains(&l)) {
        return Err(RpcError::new(-32602, format!("{name}: level is required")));
    }
    Ok((
        name.to_string(),
        spec["env"].as_str().unwrap_or_default().to_string(),
    ))
}

/// Replace a connection's provided functions (spec 25.2).
pub fn provide(cx: &mut Cx, conn: &str, specs: &[Value]) -> Result<(), RpcError> {
    let mut keys = Vec::new();
    for spec in specs {
        let key = check_spec(spec)?;
        if let Some((owner, _)) = cx.state.functions.provided.get(&key)
            && owner != conn
        {
            return Err(RpcError::new(-32004, format!("{} has another provider", key.0))
                .with_data(json!({ "state": "provided" })));
        }
        keys.push((key, spec.clone()));
    }
    cx.state.functions.provided.retain(|_, (owner, _)| owner != conn);
    for (key, spec) in keys {
        cx.state.functions.provided.insert(key, (conn.to_string(), spec));
    }
    Ok(())
}

fn session_function_specs() -> Vec<Value> {
    SESSION_FUNCTIONS
        .iter()
        .map(|(name, method, scope)| {
            json!({
                "name": name,
                "description": format!("Calls {method} as the calling session."),
                "input": { "type": "object" },
                "output": { "type": "object" },
                "effects": if *scope == "read" { json!(["read"]) } else { json!(["write"]) },
                "level": "read",
                "provider": "runtime",
            })
        })
        .collect()
}

/// Every function visible to a connection, sorted by name.
fn catalog(cx: &Cx, conn: &str) -> Vec<Value> {
    let mut all = session_function_specs();
    for spec in cx.state.functions.runtime.values() {
        let mut s = spec.clone();
        s["provider"] = json!("runtime");
        all.push(s);
    }
    for (provider, spec) in cx.state.functions.provided.values() {
        if let Some(pattern) = provides_filter(cx, provider) {
            let name = spec["name"].as_str().unwrap_or_default();
            if !pattern.iter().any(|p| matches(p, name)) {
                continue;
            }
        }
        let mut s = spec.clone();
        s["provider"] = json!(provider);
        all.push(s);
    }
    let _ = conn;
    all.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    all
}

/// A token client's `provides` patterns, if it has them (E8).
fn provides_filter(_cx: &Cx, _provider: &str) -> Option<Vec<String>> {
    None
}

/// Glob match with `*` as any run of characters.
pub fn matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && {
                let rest = &name[head.len()..];
                tail.is_empty()
                    || (0..=rest.len()).any(|i| rest.is_char_boundary(i) && matches(tail, &rest[i..]))
            }
        }
    }
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "function/list" => Reply::Now(
            principals::check(cx.state, conn, Scope::Read)
                .map(|()| json!({ "functions": catalog(cx, conn) })),
        ),
        "function/search" => Reply::Now(principals::check(cx.state, conn, Scope::Read).map(|()| {
            let query = params["query"].as_str().unwrap_or_default().to_lowercase();
            let limit = params["limit"].as_u64().unwrap_or(20) as usize;
            let words: Vec<&str> = query.split_whitespace().collect();
            let mut ranked: Vec<(usize, Value)> = catalog(cx, conn)
                .into_iter()
                .filter_map(|f| {
                    let name = f["name"].as_str().unwrap_or_default().to_lowercase();
                    let text = f["description"].as_str().unwrap_or_default().to_lowercase();
                    let score: usize = words
                        .iter()
                        .map(|w| 3 * usize::from(name.contains(w)) + usize::from(text.contains(w)))
                        .sum();
                    (score > 0).then_some((score, f))
                })
                .collect();
            ranked.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(a.1["name"].as_str().cmp(&b.1["name"].as_str()))
            });
            json!({ "functions": ranked.into_iter().take(limit).map(|(_, f)| f).collect::<Vec<_>>() })
        })),
        "function/describe" => Reply::Now(principals::check(cx.state, conn, Scope::Read).map(|()| {
            let names: Vec<String> = params["names"]
                .as_array()
                .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let functions: Vec<Value> = catalog(cx, conn)
                .into_iter()
                .filter(|f| names.iter().any(|n| f["name"].as_str() == Some(n)))
                .collect();
            let declarations = functions
                .iter()
                .map(|f| {
                    format!(
                        "/** {} */\ndeclare function {}(input: object): object",
                        f["description"].as_str().unwrap_or_default(),
                        f["name"].as_str().unwrap_or_default().replace('.', "__")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            json!({ "functions": functions, "declarations": declarations })
        })),
        "function/provide" => Reply::Now(principals::check(cx.state, conn, Scope::Functions).and_then(
            |()| {
                let specs = params["functions"].as_array().cloned().unwrap_or_default();
                provide(cx, conn, &specs).map(|()| json!({}))
            },
        )),
        "function/call" => function_call(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn function_call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    if let Err(e) = principals::check(cx.state, conn, Scope::Read) {
        return Reply::Now(Err(e));
    }
    let name = params["function"].as_str().unwrap_or_default().to_string();
    let env = params["env"].as_str().unwrap_or_default().to_string();
    let input = params.get("input").cloned().unwrap_or_else(|| json!({}));
    let principal = principals::principal_of(cx.state, conn);
    let key = params["key"].as_str().map(|k| (principal.clone(), k.to_string()));
    if let Some(done) = key.as_ref().and_then(|k| cx.state.functions.keys.get(k)) {
        return Reply::Now(Ok(done.clone()));
    }
    // The session functions are projections of protocol methods, called as the caller.
    if let Some((_, method, _)) = SESSION_FUNCTIONS.iter().find(|(n, _, _)| *n == name) {
        let mut p = input;
        if *method == "session/new" && p.get("parent").is_none() && principal.starts_with("ses_") {
            p["parent"] = json!(principal);
        }
        return match crate::dispatch::call(cx, conn, id, method, p) {
            Reply::Now(r) => {
                let result = match r {
                    Ok(output) => json!({ "output": output }),
                    Err(e) => json!({ "error": { "code": e.code.to_string(), "message": e.message } }),
                };
                if let Some(k) = key {
                    cx.state.functions.keys.insert(k, result.clone());
                }
                Reply::Now(Ok(result))
            }
            later => later,
        };
    }
    if cx.state.functions.runtime.contains_key(&name) {
        if let Err(e) = principals::check(cx.state, conn, Scope::Functions) {
            return Reply::Now(Err(e));
        }
        let call = cx.call_workspace(CallOwner::Functions, &format!("function.{name}"), input);
        cx.state.functions.pending.insert(
            call,
            PendingCall {
                conn: conn.to_string(),
                id: id.cloned(),
                key,
            },
        );
        return Reply::Later;
    }
    let Some((provider, _)) = cx
        .state
        .functions
        .provided
        .get(&(name.clone(), env.clone()))
        .cloned()
    else {
        return Reply::Now(Err(RpcError::new(-32002, format!("function not found: {name}"))));
    };
    if let Err(e) = principals::check(cx.state, conn, Scope::Functions) {
        return Reply::Now(Err(e));
    }
    let call = cx.next_call();
    cx.state.pending_calls.insert(call, CallOwner::Functions);
    let mut forwarded = json!({ "function": name, "input": input });
    if !env.is_empty() {
        forwarded["env"] = json!(env);
    }
    if let Some(s) = params.get("session") {
        forwarded["session"] = s.clone();
    } else if principal.starts_with("ses_") {
        forwarded["session"] = json!(principal);
    }
    cx.emit(Effect::ClientCall {
        conn: provider,
        call,
        method: "function/call".into(),
        params: forwarded,
    });
    cx.state.functions.pending.insert(
        call,
        PendingCall {
            conn: conn.to_string(),
            id: id.cloned(),
            key,
        },
    );
    Reply::Later
}

/// A driver, workspace or client reply to a call this module made.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(p) = cx.state.functions.pending.remove(&call) else {
        return;
    };
    // A provider's error is the call's error result, not a protocol failure (spec 25).
    let result = match result {
        Ok(v) => v,
        Err(e) => json!({ "error": { "code": e.code.to_string(), "message": e.message } }),
    };
    if let Some(k) = p.key {
        cx.state.functions.keys.insert(k, result.clone());
    }
    if let Some(id) = p.id {
        cx.reply(&p.conn, id, Ok(result));
    }
}

/// The `mcp` entry of a binding's `BindingStart` (spec 26, 30.1), or `None` without functions.
/// The token is for this binding only; drivers pass it to `parrot mcp` in `PARROT_MCP_TOKEN`.
pub fn mcp_for(cx: &mut Cx, _session: &str, binding: &str) -> Option<Value> {
    let program = cx.state.functions.mcp.clone()?;
    let token = format!("{}{}", cx.ids.next("mcp"), cx.ids.next("k"));
    cx.state.functions.tokens.retain(|_, b| b != binding);
    cx.state
        .functions
        .tokens
        .insert(token.clone(), binding.to_string());
    Some(json!({ "command": [program, "mcp", "--binding", binding], "token": token }))
}

/// The session a binding's MCP credential stands for, if `token` is that binding's.
pub fn binding_session(cx: &Cx, binding: &str, token: &str) -> Option<String> {
    (cx.state.functions.tokens.get(token)? == binding)
        .then(|| crate::directory::session_of(cx.state, binding))
        .flatten()
}

/// A client's answer to an `Effect::ClientCall`.
pub fn on_client_reply(cx: &mut Cx, _conn: &str, call: u64, result: Result<Value, RpcError>) {
    match cx.state.pending_calls.remove(&call) {
        Some(CallOwner::Links) => crate::links::on_reply(cx, call, result),
        Some(CallOwner::Functions) => on_reply(cx, call, result),
        Some(other) => {
            cx.state.pending_calls.insert(call, other);
        }
        None => {}
    }
}

/// A connection closed: its functions go away, and calls waiting on it fail.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    cx.state.functions.provided.retain(|_, (owner, _)| owner != conn);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_rules() {
        assert!(valid_name("acme.files.read"));
        assert!(!valid_name("acme.Files"));
        assert!(!valid_name("acme.a__b"));
        assert!(!valid_name("a.b.c.d.e"));
        assert!(matches("acme.*", "acme.files.read"));
        assert!(!matches("acme.*", "other.files"));
        assert!(matches("*.read", "acme.files.read"));
    }
}
