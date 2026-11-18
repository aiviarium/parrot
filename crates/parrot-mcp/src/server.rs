//! The MCP server over stdio: newline-delimited JSON-RPC with `initialize`, `ping`, `tools/list`,
//! `tools/call` and `notifications/tools/list_changed`.

use std::sync::Arc;
use std::time::Duration;

use parrot_proto::rpc::{Notification, RpcError};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{RwLock, mpsc};

use crate::catalog::{self, Access, Catalog, Entry};
use crate::rules;
use crate::session_functions::{self, Context};
use crate::upstream::Upstream;

/// MCP protocol versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The Parrot protocol versions this client speaks.
pub const PARROT_PROTOCOL: &[u32] = &[1];

/// How the server identifies itself upstream.
#[derive(Debug, Clone)]
pub struct Auth {
    pub binding: String,
    /// The binding's credential, sent in `initialize` (never in argv or logs).
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The binding's tool-call timeout from driver data; calls stop a margin before it.
    pub tool_timeout: Option<Duration>,
    /// Tools per `tools/list` page.
    pub page_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tool_timeout: None,
            page_size: 100,
        }
    }
}

/// The binding's session as the runtime reported it at the handshake.
#[derive(Debug, Clone)]
pub struct Identity {
    pub session: String,
    pub workspace: Option<String>,
    pub access: Access,
}

/// Initializes upstream as the binding's session principal and reads the session.
pub async fn handshake(up: &Upstream, auth: &Auth) -> Result<Identity, String> {
    let mut binding = json!({ "id": auth.binding });
    if let Some(token) = &auth.token {
        binding["token"] = json!(token);
    }
    let params = json!({
        "protocol": PARROT_PROTOCOL,
        "peer": { "kind": "client", "name": "parrot-mcp", "version": env!("CARGO_PKG_VERSION") },
        "features": {},
        "binding": binding,
    });
    let init = up
        .call("initialize", params)
        .await
        .map_err(|e| format!("initialize failed: {e}"))?;
    let principal = init["connection"]["principal"].as_str().unwrap_or_default();
    if !principal.starts_with("ses_") {
        return Err(format!(
            "the runtime did not accept binding {} as a session principal",
            auth.binding
        ));
    }
    let scopes = rules::strings(&init["connection"], "scopes").unwrap_or_default();
    let session = principal.to_string();
    let got = up
        .call("session/get", json!({ "session": session }))
        .await
        .map_err(|e| format!("session/get failed: {e}"))?;
    let s = &got["session"];
    Ok(Identity {
        workspace: s["workspace"].as_str().map(str::to_string),
        access: Access {
            scopes,
            task: s.get("task").is_some_and(|t| t.is_object()),
        },
        session,
    })
}

struct State {
    cx: Context,
    config: Config,
    access: RwLock<Access>,
    catalog: RwLock<Catalog>,
    out: mpsc::UnboundedSender<String>,
}

/// Serves MCP on `input`/`output` for the session of `identity` until `input` ends.
pub async fn serve<R, W>(
    input: R,
    output: W,
    upstream: Upstream,
    mut notes: mpsc::UnboundedReceiver<Notification>,
    identity: Identity,
    config: Config,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(line) = out_rx.recv().await {
            output.write_all(line.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let state = Arc::new(State {
        cx: Context {
            upstream: upstream.clone(),
            session: identity.session.clone(),
            workspace: identity.workspace.clone(),
            limit: rules::call_limit(config.tool_timeout),
        },
        config,
        access: RwLock::new(identity.access.clone()),
        catalog: RwLock::new(Catalog::default()),
        out,
    });
    refresh(&state).await;
    // Updates of the own session tell when scopes or the task change; without them the listing
    // stays as it was at the start.
    let _ = upstream
        .call(
            "subscribe",
            json!({ "target": { "session": identity.session }, "items": 0 }),
        )
        .await;

    let mut lines = BufReader::new(input).lines();
    let mut notes_open = true;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                handle_line(&state, line.trim_end_matches('\r')).await;
            }
            note = notes.recv(), if notes_open => match note {
                Some(note) => on_note(&state, note).await,
                None => notes_open = false,
            },
        }
    }
    drop(state);
    writer.await.map_err(std::io::Error::other)?
}

fn send(state: &State, msg: Value) {
    let _ = state.out.send(msg.to_string());
}

fn reply(state: &State, id: Value, result: Result<Value, RpcError>) {
    let msg = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    };
    send(state, msg);
}

async fn handle_line(state: &Arc<State>, line: &str) {
    if line.trim().is_empty() {
        return;
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return reply(
                state,
                Value::Null,
                Err(RpcError::new(-32700, format!("parse error: {e}"))),
            );
        }
    };
    if msg.is_array() {
        return reply(
            state,
            Value::Null,
            Err(RpcError::new(-32600, "batches are not supported")),
        );
    }
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return; // a response to nothing this server asked
    };
    let Some(id) = msg.get("id").cloned() else {
        return; // notifications: initialized, cancelled, roots changed
    };
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    match method {
        "initialize" => reply(state, id, Ok(initialize(&params))),
        "ping" => reply(state, id, Ok(json!({}))),
        "tools/list" => {
            let result = tools_list(state, &params).await;
            reply(state, id, result);
        }
        "tools/call" => {
            let state = state.clone();
            tokio::spawn(async move {
                let result = tools_call(&state, params).await;
                reply(&state, id, result);
            });
        }
        _ => reply(
            state,
            id,
            Err(RpcError::new(-32601, format!("method not found: {method}"))),
        ),
    }
}

/// Answers with the client's version when this server speaks it, else the newest it speaks.
fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": "parrot", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Parrot session functions act on this session and its children. Other functions are found with parrot__functions__search and run with parrot__functions__call. Every result is JSON text; errors carry error.code.",
    })
}

async fn tools_list(state: &State, params: &Value) -> Result<Value, RpcError> {
    let cursor = params.get("cursor").and_then(Value::as_str);
    let catalog = state.catalog.read().await;
    if let Some(c) = cursor
        && catalog.get(c).is_none()
    {
        return Err(RpcError::new(-32602, "unknown cursor"));
    }
    let (tools, next) = catalog.page(cursor, state.config.page_size);
    let mut result = json!({ "tools": tools });
    if let Some(next) = next {
        result["nextCursor"] = json!(next);
    }
    Ok(result)
}

/// Rebuilds the listing; returns whether it changed.
async fn refresh(state: &State) -> bool {
    let mut functions = Vec::new();
    let mut cursor: Option<Value> = None;
    for _ in 0..50 {
        let mut params = json!({ "session": state.cx.session });
        if let Some(c) = &cursor {
            params["cursor"] = c.clone();
        }
        let Ok(page) = state.cx.upstream.call("function/list", params).await else {
            break;
        };
        if let Some(list) = page["functions"].as_array() {
            functions.extend(list.iter().cloned());
        }
        match page.get("next").filter(|n| !n.is_null()) {
            Some(n) => cursor = Some(n.clone()),
            None => break,
        }
    }
    let access = state.access.read().await.clone();
    let built = Catalog::build(&access, &functions);
    let mut catalog = state.catalog.write().await;
    let changed = catalog.names() != built.names()
        || built
            .names()
            .iter()
            .any(|n| catalog.get(n).map(|t| &t.json) != built.get(n).map(|t| &t.json));
    *catalog = built;
    changed
}

async fn on_note(state: &State, note: Notification) {
    if note.method != "update" {
        return;
    }
    let update = &note.params["update"];
    if update["type"] != "session" || update["session"]["id"] != state.cx.session.as_str() {
        return;
    }
    let session = &update["session"];
    {
        let mut access = state.access.write().await;
        if let Some(grant) = rules::strings(session, "grant") {
            access.scopes = grant;
        }
        access.task = session.get("task").is_some_and(|t| t.is_object());
    }
    if refresh(state).await {
        send(
            state,
            json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }),
        );
    }
}

async fn tools_call(state: &State, params: Value) -> Result<Value, RpcError> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err(RpcError::new(-32602, "arguments must be an object")),
    };
    let entry = state.catalog.read().await.get(name).map(|t| t.entry.clone());
    let Some(entry) = entry else {
        return Err(RpcError::new(-32602, format!("unknown tool: {name}")));
    };
    Ok(match entry {
        Entry::Session(sf) => session_functions::call(sf, &state.cx, args).await,
        Entry::Pinned { function, env } => {
            let mut input = args;
            let key = input
                .as_object_mut()
                .and_then(|o| o.remove("key"))
                .and_then(|k| k.as_str().map(str::to_string));
            call_function(state, &function, env.as_deref(), input, key).await
        }
        Entry::Search => search(state, &args).await,
        Entry::Describe => describe(state, &args).await,
        Entry::Call => {
            let Some(function) = args["function"].as_str() else {
                return Ok(rules::invalid_input("function is required"));
            };
            let mut input = args.get("input").cloned().unwrap_or(json!({}));
            let key = args["key"].as_str().map(str::to_string);
            if let Some(sf) = session_functions::find(function) {
                let available = sf.available(&*state.access.read().await);
                if !available {
                    return Ok(rules::error(
                        "forbidden",
                        format!("{function} is not available to this session"),
                        Map::new(),
                    ));
                }
                if let (Some(key), Some(obj)) = (&key, input.as_object_mut()) {
                    obj.insert("key".into(), json!(key));
                }
                return Ok(session_functions::call(sf, &state.cx, input).await);
            }
            call_function(state, function, args["env"].as_str(), input, key).await
        }
    })
}

/// `function/call` within the call limit (spec 27.2). Every call carries a key, generated when the
/// caller gave none, so a call that runs out of time can be collected by calling again with it.
async fn call_function(
    state: &State,
    function: &str,
    env: Option<&str>,
    input: Value,
    key: Option<String>,
) -> Value {
    let key = key.unwrap_or_else(rules::new_key);
    let mut params = json!({ "function": function, "input": input, "session": state.cx.session, "key": key });
    if let Some(env) = env {
        params["env"] = json!(env);
    }
    match tokio::time::timeout(state.cx.limit, state.cx.upstream.call("function/call", params)).await {
        Err(_) => rules::pending_approval(&key, None),
        Ok(Err(e)) => rules::from_rpc(&e),
        Ok(Ok(result)) => rules::from_function_result(result, Some(&key)),
    }
}

fn compact(info: &Value) -> Value {
    let mut out = json!({});
    for f in ["name", "description", "effects", "env", "provider"] {
        if let Some(v) = info.get(f).filter(|v| !v.is_null()) {
            out[f] = v.clone();
        }
    }
    out
}

async fn search(state: &State, args: &Value) -> Value {
    let Some(query) = args["query"].as_str() else {
        return rules::invalid_input("query is required");
    };
    let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 50);
    let params = json!({ "session": state.cx.session, "query": query, "limit": limit });
    match tokio::time::timeout(state.cx.limit, state.cx.upstream.call("function/search", params)).await {
        Err(_) => rules::error("timeout", "function/search did not answer in time", Map::new()),
        Ok(Err(e)) => rules::from_rpc(&e),
        Ok(Ok(r)) => {
            let mut functions: Vec<Value> = r["functions"]
                .as_array()
                .map(|a| a.iter().map(compact).collect())
                .unwrap_or_default();
            rules::cut(&mut functions, rules::MAX_RESULT);
            rules::ok(json!({ "functions": functions }))
        }
    }
}

async fn describe(state: &State, args: &Value) -> Value {
    let Some(names) = rules::strings(args, "names") else {
        return rules::invalid_input("names is required");
    };
    let mut params = json!({ "names": names });
    if let Some(env) = args["env"].as_str() {
        params["env"] = json!(env);
    }
    // Session functions are described here, since they are this server's projections.
    let mut own: Vec<Value> = names
        .iter()
        .filter_map(|n| session_functions::find(n))
        .map(|sf| {
            json!({
                "name": sf.name, "description": sf.description, "input": sf.input_schema(),
                "output": (sf.output)(), "effects": sf.effects, "provider": "runtime",
                "tool": catalog::mcp_name(sf.name),
            })
        })
        .collect();
    let others: Vec<&String> = names
        .iter()
        .filter(|n| session_functions::find(n).is_none())
        .collect();
    let mut declarations = String::new();
    if !others.is_empty() {
        params["names"] = json!(others);
        match tokio::time::timeout(
            state.cx.limit,
            state.cx.upstream.call("function/describe", params),
        )
        .await
        {
            Err(_) => return rules::error("timeout", "function/describe did not answer in time", Map::new()),
            Ok(Err(e)) => return rules::from_rpc(&e),
            Ok(Ok(r)) => {
                own.extend(r["functions"].as_array().cloned().unwrap_or_default());
                declarations = r["declarations"].as_str().unwrap_or_default().to_string();
            }
        }
    }
    rules::cut(&mut own, rules::MAX_RESULT);
    rules::ok(json!({ "functions": own, "declarations": declarations }))
}
