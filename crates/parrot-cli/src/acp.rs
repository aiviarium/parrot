//! `parrot acp`: serves a Parrot runtime session as an ACP agent on standard input and output.
//!
//! An ACP client such as Zed or T3's ACP provider calls `initialize`, `session/new` (which creates a
//! Parrot session over the local socket), `session/prompt` (answered when the Parrot turn ends) and
//! `session/cancel`. Parrot items become ACP updates, and Parrot requests become
//! `session/request_permission` calls whose answers go back as `request/answer`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

/// The protocol version this export speaks.
const PROTOCOL_VERSION: u32 = 1;

#[derive(clap::Args)]
pub struct Args {
    /// The runtime's socket instead of the default.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Serve this session instead of creating one on `session/new`.
    #[arg(long)]
    pub session: Option<String>,
    /// The agent `session/new` starts; the first usable agent by default.
    #[arg(long)]
    pub agent: Option<String>,
    /// The folder to work in when the ACP client sends no `cwd`.
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

pub async fn run(args: Args) -> Result<(), String> {
    let socket = match &args.socket {
        Some(path) => path.clone(),
        None => crate::serve::socket_path().map_err(|e| format!("no socket: {e}"))?,
    };
    let rpc = Runtime::connect(&socket).await?;
    let default_dir = match &args.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let mut writer = Box::new(tokio::io::stdout()) as Box<dyn AsyncWrite + Unpin + Send>;
    let (out, mut outgoing) = mpsc::unbounded_channel::<Value>();
    let writer_task = tokio::spawn(async move {
        while let Some(value) = outgoing.recv().await {
            let mut line = value.to_string();
            line.push('\n');
            writer.write_all(line.as_bytes()).await?;
            writer.flush().await?;
        }
        std::io::Result::Ok(())
    });
    let mut export = Export {
        rpc,
        out: out.clone(),
        sessions: HashMap::new(),
        prompts: HashMap::new(),
        next_permission: 0,
        agent: args.agent.clone(),
        default_dir,
        wanted_session: args.session.clone(),
    };
    let mut acp = BufReader::new(tokio::io::stdin()).lines();
    // The runtime's notifications are read in a task so the loop can serve both sides.
    loop {
        tokio::select! {
            line = acp.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        if export.message(msg).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            event = export.rpc.recv() => {
                match event {
                    Some(event) => export.event(event).await,
                    None => break,
                }
            }
        }
    }
    drop(out);
    let _ = writer_task.await;
    Ok(())
}

/// One ACP session served from a Parrot session.
struct Session {
    subscription: String,
    /// Parrot options by id, so ACP mode and config updates keep their ids.
    options: Vec<Value>,
    /// The text already sent per item, so a `set` only sends what the client lacks.
    sent: HashMap<String, String>,
    /// A Parrot request id to the ACP request id that asks about it.
    requests: HashMap<String, String>,
    /// Parrot tool statuses, so a tool call is created once.
    tools: HashMap<String, String>,
}

/// A prompt the ACP client waits on.
struct PendingPrompt {
    acp_id: Value,
}

struct Export {
    rpc: Runtime,
    out: mpsc::UnboundedSender<Value>,
    sessions: HashMap<String, Session>,
    prompts: HashMap<String, PendingPrompt>,
    next_permission: u64,
    agent: Option<String>,
    default_dir: PathBuf,
    wanted_session: Option<String>,
}

impl Export {
    fn send(&self, value: Value) {
        let _ = self.out.send(value);
    }

    fn reply(&self, id: Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn fail(&self, id: Value, code: i64, message: impl Into<String>) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message.into() },
        }));
    }

    fn update(&self, session: &str, update: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": session, "update": update },
        }));
    }

    /// One line from the ACP client.
    async fn message(&mut self, msg: Value) -> Result<(), String> {
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to a permission request: answer the Parrot request.
            if let Some(id) = &id {
                self.answer_permission(id, msg.get("result").cloned().unwrap_or(Value::Null))
                    .await;
            }
            return Ok(());
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let response = match method {
            "initialize" => Some((
                id.clone(),
                Ok(json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "agentCapabilities": {
                        "loadSession": true,
                        "promptCapabilities": { "image": true, "embeddedContext": true },
                    },
                    "agentInfo": {
                        "name": "parrot",
                        "title": "Parrot",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "authMethods": [],
                })),
            )),
            "session/new" => {
                let result = self.new_session(&params).await;
                Some((id.clone(), result))
            }
            "session/load" | "session/resume" => {
                let result = self.load_session(&params).await;
                Some((id.clone(), result))
            }
            "session/prompt" => {
                if let Some(id) = id.clone() {
                    self.prompt(id, &params).await;
                }
                None
            }
            "session/cancel" => {
                let session = string_of(&params, "sessionId");
                let _ = self
                    .rpc
                    .call("session/cancel", json!({ "session": session }))
                    .await;
                None
            }
            "session/set_mode" => {
                let session = string_of(&params, "sessionId");
                let mode = params.get("modeId").cloned().unwrap_or(Value::Null);
                let option = self.mode_option(&session);
                let result = self.set_option(&session, option, mode).await;
                Some((id.clone(), result))
            }
            "session/set_config_option" => {
                let session = string_of(&params, "sessionId");
                let option = string_of(&params, "configId");
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                let result = self.set_option(&session, option, value).await;
                Some((id.clone(), result))
            }
            "session/set_model" => {
                let session = string_of(&params, "sessionId");
                let model = params.get("modelId").cloned().unwrap_or(Value::Null);
                let result = self.set_option(&session, "model".into(), model).await;
                Some((id.clone(), result))
            }
            other => {
                if let Some(id) = id {
                    self.fail(id, -32601, format!("Parrot does not offer {other}"));
                }
                None
            }
        };
        if let Some((Some(id), result)) = response {
            match result {
                Ok(value) => self.reply(id, value),
                Err((code, message)) => self.fail(id, code, message),
            }
        }
        Ok(())
    }

    async fn new_session(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        if let Some(id) = self.wanted_session.clone() {
            let value = self.subscribe(&id).await?;
            return Ok(value);
        }
        let agent = match &self.agent {
            Some(agent) => agent.clone(),
            None => {
                let agents = self.rpc.call("agent/list", json!({})).await?;
                agents["agents"]
                    .as_array()
                    .and_then(|a| {
                        a.iter()
                            .find(|agent| agent["usable"] == true)
                            .or_else(|| a.first())
                    })
                    .and_then(|a| a["id"].as_str())
                    .map(str::to_owned)
                    .ok_or((-32020, "the runtime knows no agent".to_string()))?
            }
        };
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| self.default_dir.to_string_lossy().into_owned());
        let created = self
            .rpc
            .call(
                "session/new",
                json!({
                    "agent": agent,
                    "workspace": { "kind": "dir", "path": cwd },
                    "cwd": cwd,
                }),
            )
            .await?;
        let session = created
            .pointer("/session/id")
            .and_then(Value::as_str)
            .or_else(|| created.get("session").and_then(Value::as_str))
            .ok_or((-32020, format!("the runtime returned no session: {created}")))?
            .to_owned();
        // `session/new` subscribed this connection; a second subscription would deliver every
        // update twice, so the first one is dropped and one fresh subscription carries the
        // snapshot the ACP client needs.
        if let Some(subscription) = created
            .get("subscription")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let _ = self
                .rpc
                .call("unsubscribe", json!({ "subscription": subscription }))
                .await;
        }
        self.subscribe(&session).await?;
        Ok(json!({ "sessionId": session }))
    }

    async fn load_session(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let session = self
            .wanted_session
            .clone()
            .or_else(|| params.get("sessionId").and_then(Value::as_str).map(str::to_owned))
            .ok_or((-32602, "session/load needs a sessionId".to_string()))?;
        let value = self.subscribe(&session).await?;
        Ok(value)
    }

    /// Subscribes, seeds the session from the snapshot and returns the ACP session result.
    async fn subscribe(&mut self, session: &str) -> Result<Value, (i64, String)> {
        let sub = self
            .rpc
            .call("subscribe", json!({ "target": { "session": session } }))
            .await?;
        let subscription = sub
            .get("subscription")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let snapshot = sub.get("snapshot").cloned().unwrap_or(Value::Null);
        let options = snapshot
            .pointer("/sessions/0/options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.sessions.insert(
            session.to_owned(),
            Session {
                subscription,
                options,
                sent: HashMap::new(),
                requests: HashMap::new(),
                tools: HashMap::new(),
            },
        );
        if let Some(items) = snapshot.pointer("/sessions/0/items").and_then(Value::as_array) {
            for item in items.clone() {
                self.item(session, item);
            }
        }
        Ok(self.session_result(session))
    }

    /// The ACP session result: modes and config options from the Parrot options.
    fn session_result(&self, session: &str) -> Value {
        let Some(state) = self.sessions.get(session) else {
            return json!({ "sessionId": session });
        };
        let mut result = json!({ "sessionId": session });
        let role = |option: &Value| option.get("role").and_then(Value::as_str).map(str::to_owned);
        let mode = state.options.iter().find(|o| role(o).as_deref() == Some("mode"));
        if let Some(mode) = mode {
            let modes: Vec<Value> = choices(mode)
                .iter()
                .map(|(value, label, description)| {
                    let mut m = json!({ "id": value, "name": label });
                    if let Some(d) = description {
                        m["description"] = json!(d);
                    }
                    m
                })
                .collect();
            result["modes"] = json!({
                "currentModeId": mode.get("value").cloned().unwrap_or(Value::Null),
                "availableModes": modes,
            });
        }
        let config: Vec<Value> = state
            .options
            .iter()
            .filter(|o| matches!(role(o).as_deref(), Some("model") | Some("effort")))
            .map(|o| {
                let category = match role(o).as_deref() {
                    Some("effort") => "thought_level",
                    _ => "model",
                };
                let choices: Vec<Value> = choices(o)
                    .iter()
                    .map(|(value, label, description)| {
                        let mut c = json!({ "value": value, "name": label });
                        if let Some(d) = description {
                            c["description"] = json!(d);
                        }
                        c
                    })
                    .collect();
                let mut config = json!({
                    "id": o.get("id").cloned().unwrap_or(Value::Null),
                    "name": o.get("label").cloned().unwrap_or(Value::Null),
                    "category": category,
                    "type": "select",
                    "currentValue": o.get("value").cloned().unwrap_or(Value::Null),
                    "options": choices,
                });
                if let Some(d) = o.get("description") {
                    config["description"] = d.clone();
                }
                config
            })
            .collect();
        if !config.is_empty() {
            result["configOptions"] = json!(config);
        }
        result
    }

    fn mode_option(&self, session: &str) -> String {
        self.sessions
            .get(session)
            .and_then(|s| {
                s.options
                    .iter()
                    .find(|o| o.get("role").and_then(Value::as_str) == Some("mode"))
            })
            .and_then(|o| o.get("id").and_then(Value::as_str))
            .unwrap_or("mode")
            .to_owned()
    }

    async fn set_option(
        &mut self,
        session: &str,
        option: String,
        value: Value,
    ) -> Result<Value, (i64, String)> {
        let result = self
            .rpc
            .call(
                "option/set",
                json!({ "session": session, "values": { option: value } }),
            )
            .await?;
        if let Some(state) = self.sessions.get_mut(session)
            && let Some(options) = result.get("options").and_then(Value::as_array)
        {
            state.options = options.clone();
        }
        Ok(self.session_result(session))
    }

    async fn prompt(&mut self, id: Value, params: &Value) {
        let session = string_of(params, "sessionId");
        if !self.sessions.contains_key(&session) {
            let result = self.subscribe(&session).await;
            if let Err((code, message)) = result {
                self.fail(id, code, message);
                return;
            }
        }
        let (text, context, attachments) = prompt_parts(params);
        let sent = self
            .rpc
            .call(
                "session/prompt",
                json!({
                    "session": session,
                    "text": text,
                    "context": context,
                    "attachments": attachments,
                }),
            )
            .await;
        if let Err((code, message)) = sent {
            self.fail(id, code, message);
            return;
        }
        self.prompts.insert(session, PendingPrompt { acp_id: id });
    }

    async fn answer_permission(&mut self, acp_id: &Value, result: Value) {
        let key = acp_id.to_string();
        let Some((session, request)) = self.sessions.iter().find_map(|(session, state)| {
            state
                .requests
                .iter()
                .find(|(_, acp)| **acp == key)
                .map(|(request, _)| (session.clone(), request.clone()))
        }) else {
            return;
        };
        let answer = match result.pointer("/outcome/outcome").and_then(Value::as_str) {
            Some("selected") => {
                let option = result
                    .pointer("/outcome/optionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!({ "choice": option })
            }
            _ => json!({ "choice": "deny", "message": "The client cancelled the request." }),
        };
        let _ = self
            .rpc
            .call("request/answer", json!({ "request": request, "answer": answer }))
            .await;
        if let Some(state) = self.sessions.get_mut(&session) {
            state.requests.remove(&request);
        }
    }

    /// One runtime notification.
    async fn event(&mut self, event: Value) {
        if event.get("method").and_then(Value::as_str) != Some("update") {
            return;
        }
        let params = event.get("params").cloned().unwrap_or(Value::Null);
        let Some(session) = params.get("session").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        // Updates of a subscription the session no longer uses would repeat what it has.
        if self
            .sessions
            .get(&session)
            .is_some_and(|state| state.subscription != string_of(&params, "subscription"))
        {
            return;
        }
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        match update.get("type").and_then(Value::as_str) {
            Some("item") => {
                if let Some(item) = update.get("item") {
                    self.item(&session, item.clone());
                }
            }
            Some("turn") => {
                if let Some(turn) = update.get("turn")
                    && turn.get("status").and_then(Value::as_str) != Some("running")
                    && let Some(pending) = self.prompts.remove(&session)
                {
                    let (stop_reason, usage) = prompt_result(turn);
                    self.reply(
                        pending.acp_id,
                        json!({ "stopReason": stop_reason, "usage": usage }),
                    );
                }
            }
            Some("request") => {
                if let Some(request) = update.get("request") {
                    self.permission(&session, request.clone());
                }
            }
            Some("options") => {
                if let Some(options) = update.get("options").and_then(Value::as_array) {
                    if let Some(state) = self.sessions.get_mut(&session) {
                        state.options = options.clone();
                    }
                    let result = self.session_result(&session);
                    self.update(
                        &session,
                        json!({
                            "sessionUpdate": "config_option_update",
                            "configOptions": result.get("configOptions").cloned().unwrap_or_else(|| json!([])),
                        }),
                    );
                    if let Some(mode) = result.pointer("/modes/currentModeId") {
                        self.update(
                            &session,
                            json!({
                                "sessionUpdate": "current_mode_update",
                                "currentModeId": mode,
                            }),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// A Parrot item patch as an ACP update.
    fn item(&mut self, session: &str, patch: Value) {
        let Some(id) = patch.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let kind = patch.get("kind").and_then(Value::as_str);
        let set = patch.get("set").cloned().unwrap_or(Value::Null);
        let append = patch.get("append").cloned().unwrap_or(Value::Null);
        let Some(state) = self.sessions.get_mut(session) else {
            return;
        };
        // Only the first patch carries the kind; later ones are found by the id.
        if let Some(kind) = kind {
            state.sent.insert(format!("kind:{id}"), kind.to_owned());
        }
        let kind = kind
            .map(str::to_owned)
            .or_else(|| state.sent.get(&format!("kind:{id}")).cloned());
        match kind.as_deref() {
            Some("assistant") | Some("reasoning") => {
                let delivered = state.sent.get(&id).cloned().unwrap_or_default();
                let text = append
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        // A set may repeat what the client already has; send only the rest.
                        set.get("text")
                            .and_then(Value::as_str)
                            .map(|text| text.strip_prefix(delivered.as_str()).unwrap_or(text).to_owned())
                    });
                let Some(text) = text.filter(|t| !t.is_empty()) else {
                    return;
                };
                state.sent.insert(id.clone(), delivered + &text);
                state
                    .sent
                    .insert(format!("kind:{id}"), kind.clone().unwrap_or_default());
                let update = if kind.as_deref() == Some("reasoning") {
                    "agent_thought_chunk"
                } else {
                    "agent_message_chunk"
                };
                self.update(
                    session,
                    json!({
                        "sessionUpdate": update,
                        "messageId": id,
                        "content": { "type": "text", "text": text },
                    }),
                );
            }
            Some("tool") => {
                let status = set
                    .get("status")
                    .and_then(Value::as_str)
                    .map(acp_tool_status)
                    .unwrap_or("pending");
                let title = set
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| set.get("tool").and_then(Value::as_str))
                    .unwrap_or("tool");
                let input = set.get("input").cloned().unwrap_or_else(|| json!({}));
                let previous = self.sessions.get(session).and_then(|s| s.tools.get(&id)).cloned();
                if let Some(state) = self.sessions.get_mut(session) {
                    state.tools.insert(id.clone(), status.to_owned());
                }
                let mut update = json!({
                    "sessionUpdate": if previous.is_some() { "tool_call_update" } else { "tool_call" },
                    "toolCallId": id,
                    "title": title,
                    "status": status,
                });
                if previous.is_none() {
                    update["kind"] = json!("other");
                    update["rawInput"] = input;
                }
                if let Some(output) = set.get("output") {
                    let text = match output {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    update["content"] =
                        json!([{ "type": "content", "content": { "type": "text", "text": text } }]);
                }
                self.update(session, update);
            }
            Some("plan") => {
                let entries: Vec<Value> = set
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|e| {
                                json!({
                                    "content": e.get("text").cloned().unwrap_or(Value::Null),
                                    "priority": "medium",
                                    "status": match e.get("status").and_then(Value::as_str) {
                                        Some("active") => "in_progress",
                                        Some("done") => "completed",
                                        _ => "pending",
                                    },
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.update(session, json!({ "sessionUpdate": "plan", "entries": entries }));
            }
            _ => {}
        }
    }

    /// A Parrot request as `session/request_permission`.
    fn permission(&mut self, session: &str, request: Value) {
        let status = request.get("status").and_then(Value::as_str);
        if status != Some("open") {
            return;
        }
        let request_id = request
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if self
            .sessions
            .get(session)
            .is_some_and(|s| s.requests.contains_key(&request_id))
        {
            return;
        }
        self.next_permission += 1;
        let acp_id = self.next_permission;
        let tool_call_id = request
            .get("item")
            .and_then(Value::as_str)
            .unwrap_or(&request_id)
            .to_owned();
        let subject = request.get("subject").cloned().unwrap_or(Value::Null);
        let options: Vec<Value> = request
            .get("choices")
            .and_then(Value::as_array)
            .map(|choices| {
                choices
                    .iter()
                    .filter_map(|choice| {
                        let id = choice.get("id")?.as_str()?;
                        let intent = choice.get("intent").and_then(Value::as_str);
                        let scope = choice.get("scope").and_then(Value::as_str);
                        let kind = match (intent, scope) {
                            (Some("allow"), Some("once")) => "allow_once",
                            (Some("allow"), _) => "allow_always",
                            (Some("deny"), Some("once")) => "reject_once",
                            (Some("deny"), _) => "reject_always",
                            _ => "allow_once",
                        };
                        Some(json!({
                            "optionId": id,
                            "name": choice.get("label").cloned().unwrap_or(Value::Null),
                            "kind": kind,
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let options = if options.is_empty() {
            json!([
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                { "optionId": "deny", "name": "Deny", "kind": "reject_once" },
            ])
        } else {
            Value::Array(options)
        };
        if let Some(state) = self.sessions.get_mut(session) {
            state.requests.insert(request_id, acp_id.to_string());
        }
        self.send(json!({
            "jsonrpc": "2.0",
            "id": acp_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session,
                "toolCall": {
                    "toolCallId": tool_call_id,
                    "title": request.get("title").cloned().unwrap_or(Value::Null),
                    "kind": "other",
                    "status": "pending",
                    "rawInput": subject.get("input").cloned().unwrap_or_else(|| json!({})),
                },
                "options": options,
            },
        }));
    }
}

fn string_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The choices of a Parrot option as `(value, title, description)`.
fn choices(option: &Value) -> Vec<(String, String, Option<String>)> {
    option
        .pointer("/schema/oneOf")
        .and_then(Value::as_array)
        .map(|choices| {
            choices
                .iter()
                .filter_map(|choice| {
                    let value = choice.get("const")?.as_str()?.to_owned();
                    let title = choice
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or(&value)
                        .to_owned();
                    let description = choice
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    Some((value, title, description))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An ACP prompt's blocks as Parrot text, context and attachments.
fn prompt_parts(params: &Value) -> (String, Vec<Value>, Vec<Value>) {
    let mut texts = Vec::new();
    let mut context = Vec::new();
    let mut attachments = Vec::new();
    for block in params
        .get("prompt")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => texts.push(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some("image") => attachments.push(json!({
                "name": "image",
                "type": block.get("mimeType").cloned().unwrap_or_else(|| json!("image/png")),
                "data": block.get("data").cloned().unwrap_or(Value::Null),
            })),
            Some("resource") => {
                let resource = block.get("resource").cloned().unwrap_or(Value::Null);
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    let mut entry = json!({
                        "uri": resource.get("uri").cloned().unwrap_or(Value::Null),
                        "text": text,
                    });
                    if let Some(mime) = resource.get("mimeType") {
                        entry["title"] = mime.clone();
                    }
                    context.push(entry);
                } else if resource.get("blob").is_some() {
                    attachments.push(json!({
                        "name": resource.get("uri").and_then(Value::as_str).unwrap_or("file"),
                        "type": resource.get("mimeType").cloned().unwrap_or_else(|| json!("application/octet-stream")),
                        "data": resource.get("blob").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            Some("resource_link") => attachments.push(json!({
                "name": block.get("name").and_then(Value::as_str).unwrap_or("file"),
                "type": block.get("mimeType").cloned().unwrap_or_else(|| json!("application/octet-stream")),
                "uri": block.get("uri").cloned().unwrap_or(Value::Null),
            })),
            _ => {}
        }
    }
    (texts.join("\n"), context, attachments)
}

fn acp_tool_status(status: &str) -> &'static str {
    match status {
        "running" => "in_progress",
        "ok" => "completed",
        "error" => "failed",
        "cancelled" => "cancelled",
        _ => "pending",
    }
}

/// The ACP stop reason and usage for a finished Parrot turn.
fn prompt_result(turn: &Value) -> (&'static str, Value) {
    let stop = turn.get("stop").and_then(Value::as_str);
    let status = turn.get("status").and_then(Value::as_str);
    let reason = match (status, stop) {
        (Some("interrupted"), _) => "cancelled",
        (Some("failed"), _) => "refusal",
        (_, Some("max_tokens")) => "max_tokens",
        (_, Some("turn_limit")) => "max_turn_requests",
        (_, Some("refusal")) => "refusal",
        _ => "end_turn",
    };
    let tokens = turn.pointer("/usage/tokens").cloned().unwrap_or(Value::Null);
    let usage = json!({
        "inputTokens": tokens.get("input").cloned().unwrap_or_else(|| json!(0)),
        "outputTokens": tokens.get("output").cloned().unwrap_or_else(|| json!(0)),
        "cachedReadTokens": tokens.get("cacheRead").cloned().unwrap_or_else(|| json!(0)),
        "cachedWriteTokens": tokens.get("cacheWrite").cloned().unwrap_or_else(|| json!(0)),
    });
    (reason, usage)
}

/// A call waiting for its response: the result, or the runtime's error.
type Waiter = oneshot::Sender<Result<Value, (i64, String)>>;
type Waiting = Arc<Mutex<HashMap<u64, Waiter>>>;

/// A JSON-RPC client to the runtime's local socket, with the notifications on their own channel.
struct Runtime {
    write: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    events: mpsc::UnboundedReceiver<Value>,
    waiting: Waiting,
    next: u64,
}

impl Runtime {
    async fn connect(path: &Path) -> Result<Self, String> {
        let (read, write) = open(path).await.map_err(|e| {
            format!(
                "cannot connect to {}: {e} (is `parrot serve` running?)",
                path.display()
            )
        })?;
        let waiting: Waiting = Arc::default();
        let (events, rx) = mpsc::unbounded_channel();
        let inbox = waiting.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if msg.get("method").is_some() {
                    if events.send(msg).is_err() {
                        break;
                    }
                } else if let Some(id) = msg.get("id").and_then(Value::as_u64)
                    && let Some(tx) = inbox.lock().await.remove(&id)
                {
                    let result = match msg.get("error") {
                        Some(error) => Err((
                            error.get("code").and_then(Value::as_i64).unwrap_or(-32020),
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("the runtime refused the call")
                                .to_owned(),
                        )),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(result);
                }
            }
        });
        let mut runtime = Runtime {
            write: Mutex::new(write),
            events: rx,
            waiting,
            next: 1,
        };
        runtime
            .call(
                "initialize",
                json!({
                    "protocol": [1],
                    "peer": { "kind": "client", "name": "parrot-acp", "version": env!("CARGO_PKG_VERSION") },
                    "features": {},
                }),
            )
            .await
            .map_err(|(_, message)| message)?;
        Ok(runtime)
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        let id = self.next;
        self.next += 1;
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id, tx);
        let line =
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string() + "\n";
        {
            let mut write = self.write.lock().await;
            write
                .write_all(line.as_bytes())
                .await
                .map_err(|e| (-32020, format!("cannot send {method}: {e}")))?;
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err((-32020, "the runtime closed the connection".to_string())),
        }
    }

    async fn recv(&mut self) -> Option<Value> {
        self.events.recv().await
    }
}

type Halves = (
    Box<dyn AsyncRead + Unpin + Send>,
    Box<dyn AsyncWrite + Unpin + Send>,
);

#[cfg(unix)]
async fn open(path: &Path) -> std::io::Result<Halves> {
    let (r, w) = tokio::net::UnixStream::connect(path).await?.into_split();
    Ok((Box::new(r), Box::new(w)))
}

#[cfg(windows)]
async fn open(path: &Path) -> std::io::Result<Halves> {
    let pipe = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    let (r, w) = tokio::io::split(pipe);
    Ok((Box::new(r), Box::new(w)))
}
