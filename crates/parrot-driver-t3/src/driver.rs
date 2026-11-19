//! The T3 Code driver (spec Appendix B): Parrot driving T3 through its orchestration MCP tools.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parrot_proto::agent::{Agent, AgentDriver, AgentListResult, AuthStatus, Control, DriverKind};
use parrot_proto::driver::{
    BindingAnswerParams, BindingCancelParams, BindingCancelResult, BindingOptionsParams,
    BindingOptionsResult, BindingPromptParams, BindingPromptResult, BindingRef, BindingStart,
    BindingStartResult, DriverUpdate, DriverUpdateKind,
};
use parrot_proto::open::Open;
use parrot_proto::option::{Applies, ConfigOption, OptionRole, PermissionLevel};
use parrot_proto::principal::Isolation;
use parrot_proto::request::{
    Answerable, Choice, ChoiceIntent, Notify, Question, Request, RequestKind, RequestStatus, Subject,
};
use parrot_proto::rpc::{Message, Notification, Response, RpcError};
use parrot_proto::session::{Ref, RefStrength};
use parrot_proto::state::{State, Turn, TurnStatus, TurnStop};
use parrot_proto::subscribe::NoticeLevel;
use parrot_proto::usage::Usage;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::mcp::Mcp;

/// The variable that names the T3 MCP server command.
pub const MCP_BIN_ENV: &str = "PARROT_T3_MCP";
/// Extra arguments for the variable form.
pub const MCP_ARGS_ENV: &str = "PARROT_T3_MCP_ARGS";

/// The modes T3 offers for a thread.
pub const MODES: [&str; 4] = ["approval-required", "auto-accept-edits", "auto", "full-access"];

/// How the driver reaches T3.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Extra environment for the MCP server.
    pub env: Vec<(OsString, OsString)>,
    /// Start the server through `parrot guard --`.
    pub guard: bool,
    /// A command from the egg's `driver.kdl`.
    pub command: Option<Vec<String>>,
    /// A T3 server's `/mcp` address, such as `http://127.0.0.1:3773/mcp`.
    pub url: Option<String>,
    /// The bearer token the server wants, from the environment only.
    pub token: Option<String>,
}

impl Config {
    /// `PARROT_T3_URL` and `PARROT_T3_TOKEN` for a T3 server; otherwise the stdio command.
    pub fn from_env() -> Self {
        Config {
            url: std::env::var("PARROT_T3_URL").ok().filter(|u| !u.is_empty()),
            token: std::env::var("PARROT_T3_TOKEN").ok().filter(|t| !t.is_empty()),
            ..Default::default()
        }
    }
}

struct Binding {
    id: String,
    thread: String,
    state: Mutex<ThreadState>,
}

#[derive(Debug, Clone)]
struct ThreadState {
    turn: Option<Turn>,
    turns: u64,
    cancelled: bool,
    model: Option<String>,
    mode: Option<String>,
    /// The newest message position read from the thread.
    position: i64,
    seen: HashSet<String>,
    requests: HashSet<String>,
}

impl ThreadState {
    fn new() -> Self {
        ThreadState {
            turn: None,
            turns: 0,
            cancelled: false,
            model: None,
            mode: None,
            position: -1,
            seen: HashSet::new(),
            requests: HashSet::new(),
        }
    }

    fn level(&self) -> (PermissionLevel, bool) {
        match self.mode.as_deref() {
            Some("full-access") => (PermissionLevel::Host, true),
            Some("auto") => (PermissionLevel::Write, true),
            Some("auto-accept-edits") => (PermissionLevel::Write, false),
            _ => (PermissionLevel::Read, false),
        }
    }
}

struct Driver {
    config: Config,
    out: mpsc::UnboundedSender<String>,
    mcp: tokio::sync::Mutex<Option<Arc<Mcp>>>,
    bindings: Mutex<HashMap<String, Arc<Binding>>>,
}

/// Serves the driver protocol on `input` and `output` until `input` ends, then ends every binding.
pub async fn serve<R, W>(config: Config, input: R, mut output: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut lines) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(mut line) = lines.recv().await {
            line.push('\n');
            output.write_all(line.as_bytes()).await?;
            output.flush().await?;
        }
        std::io::Result::Ok(())
    });
    let driver = Arc::new(Driver {
        config,
        out,
        mcp: tokio::sync::Mutex::new(None),
        bindings: Mutex::default(),
    });
    let mut reader = BufReader::new(input).lines();
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req = match serde_json::from_str::<Message>(&line) {
            Ok(Message::Request(req)) => req,
            Ok(_) => continue,
            Err(e) => {
                let resp = Response::err(Value::Null, RpcError::new(-32700, format!("parse error: {e}")));
                driver.send(&resp);
                continue;
            }
        };
        let driver = driver.clone();
        tokio::spawn(async move {
            let resp = match driver.handle(&req.method, req.params).await {
                Ok(v) => Response::ok(req.id, v),
                Err(e) => Response::err(req.id, e),
            };
            driver.send(&resp);
        });
    }
    let all: Vec<_> = driver.bindings.lock().unwrap().drain().map(|(_, b)| b).collect();
    for b in all {
        let _ = b;
    }
    if let Some(mcp) = driver.mcp.lock().await.take() {
        mcp.stop().await;
    }
    drop(driver);
    writer.await.map_err(std::io::Error::other)?
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| RpcError::new(-32602, format!("invalid params: {e}")))
}

fn to_value<T: serde::Serialize>(t: &T) -> Value {
    serde_json::to_value(t).expect("protocol types serialize")
}

impl Driver {
    fn send<T: serde::Serialize>(&self, msg: &T) {
        let _ = self
            .out
            .send(serde_json::to_string(msg).expect("protocol types serialize"));
    }

    fn emit(&self, binding: &str, kinds: Vec<DriverUpdateKind>) {
        for kind in kinds {
            let update = DriverUpdate {
                binding: binding.to_owned(),
                subagent: None,
                update: Open::Known(kind),
                extra: Map::new(),
            };
            self.send(&Notification::new("update", to_value(&update)));
        }
    }

    fn binding(&self, id: &str) -> Result<Arc<Binding>, RpcError> {
        self.bindings
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::new(-32602, format!("no binding {id}")))
    }

    async fn mcp(&self, cwd: &str, options: &Map<String, Value>) -> Result<Arc<Mcp>, RpcError> {
        let mut slot = self.mcp.lock().await;
        if let Some(mcp) = slot.as_ref() {
            return Ok(mcp.clone());
        }
        let mcp = self.connect(cwd, options).await?;
        *slot = Some(mcp.clone());
        Ok(mcp)
    }

    /// A server from the options' `t3.url`, else the environment's, else a command over stdio.
    async fn connect(&self, cwd: &str, options: &Map<String, Value>) -> Result<Arc<Mcp>, RpcError> {
        connect(&self.config, cwd, options).await
    }

    async fn handle(self: &Arc<Self>, method: &str, p: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(json!({
                "protocol": 1,
                "peer": {"kind": "driver", "name": "parrot-driver-t3", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![probed(&self.config).await],
                hint: None,
            })),
            "binding/start" => self.start(params(p)?).await,
            "binding/prompt" => {
                let p: BindingPromptParams = params(p)?;
                if p.subagent.is_some() {
                    return Err(RpcError::new(-32001, "T3 has no subagents of its own"));
                }
                let b = self.binding(&p.binding)?;
                let mcp = self.mcp_for(&b).await?;
                let running = b.state.lock().unwrap().turn.is_some();
                if running {
                    if !p.steer {
                        return Err(
                            RpcError::new(-32004, "a turn is running").with_data(json!({"state": "running"}))
                        );
                    }
                    if p.attachments.as_ref().is_some_and(|a| !a.is_empty()) {
                        self.emit(
                            &b.id,
                            vec![DriverUpdateKind::Notice {
                                level: NoticeLevel::Warning,
                                text:
                                    "T3 cannot attach files to a running turn; the attachments were left out."
                                        .into(),
                                code: Some("attachment_unsupported".into()),
                            }],
                        );
                    }
                    let turn = b.state.lock().unwrap().turn.as_ref().map(|t| t.id.clone());
                    mcp.call(
                        "t3_thread_send",
                        json!({ "threadId": b.thread, "message": p.text, "mode": "steer" }),
                    )
                    .await
                    .map_err(harness)?;
                    if let Some(turn) = turn {
                        emit_user(self, &b.id, &p, &turn);
                    }
                    return Ok(to_value(&BindingPromptResult {
                        disposition: parrot_proto::driver::BindingDisposition::Steered,
                    }));
                }
                let turn = start_turn(&b, &p.message);
                emit_user(self, &b.id, &p, &turn.id);
                self.send_prompt(&b, &mcp, &p).await?;
                let driver = self.clone();
                let b2 = b.clone();
                tokio::spawn(async move {
                    driver.pump(b2, mcp).await;
                });
                Ok(to_value(&BindingPromptResult {
                    disposition: parrot_proto::driver::BindingDisposition::Started,
                }))
            }
            "binding/cancel" => {
                let p: BindingCancelParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let running = {
                    let mut state = b.state.lock().unwrap();
                    state.cancelled = true;
                    state.turn.is_some()
                };
                if !running {
                    return Ok(to_value(&BindingCancelResult { interrupted: false }));
                }
                let mcp = self.mcp_for(&b).await?;
                let _ = mcp
                    .call("t3_thread_interrupt", json!({ "threadId": b.thread }))
                    .await;
                Ok(to_value(&BindingCancelResult { interrupted: true }))
            }
            "binding/answer" => {
                let p: BindingAnswerParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let mcp = self.mcp_for(&b).await?;
                // A typed answer already names each question by its id.
                let answers = p
                    .answer
                    .answers
                    .clone()
                    .map(Value::Object)
                    .or_else(|| p.answer.value.clone())
                    .unwrap_or_else(|| json!(p.answer.choice.clone().map(|c| json!({ "choice": c }))));
                mcp.call(
                    "t3_pending_request",
                    json!({
                        "action": "respond",
                        "threadId": b.thread,
                        "requestId": p.request,
                        "answers": answers,
                    }),
                )
                .await
                .map_err(harness)?;
                Ok(json!({}))
            }
            "binding/options" => {
                let p: BindingOptionsParams = params(p)?;
                let b = self.binding(&p.binding)?;
                {
                    let mut state = b.state.lock().unwrap();
                    if let Some(model) = p.values.get("model").and_then(Value::as_str) {
                        state.model = Some(model.to_owned());
                    }
                    if let Some(mode) = p.values.get("mode").and_then(Value::as_str) {
                        state.mode = Some(mode.to_owned());
                    }
                }
                let (options, level, network) = {
                    let state = b.state.lock().unwrap();
                    (options_of(&state), state.level().0, state.level().1)
                };
                self.emit(
                    &b.id,
                    vec![DriverUpdateKind::Options {
                        options: options.clone(),
                        level,
                        network,
                    }],
                );
                Ok(to_value(&BindingOptionsResult {
                    options,
                    level,
                    network,
                }))
            }
            "binding/stop" => {
                let p: BindingRef = params(p)?;
                let b = self.bindings.lock().unwrap().remove(&p.binding);
                if let Some(b) = b {
                    let mcp = self.mcp.lock().await.clone();
                    if let Some(mcp) = mcp {
                        let _ = mcp
                            .call("t3_thread_interrupt", json!({ "threadId": b.thread }))
                            .await;
                    }
                    self.emit(&b.id, vec![state_update(State::Stopped, None)]);
                }
                Ok(json!({}))
            }
            "binding/action" => Err(RpcError::new(-32601, "T3 has no actions beyond threads")),
            _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
        }
    }

    async fn mcp_for(&self, b: &Binding) -> Result<Arc<Mcp>, RpcError> {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut slot = self.mcp.lock().await;
        if let Some(mcp) = slot.as_ref() {
            return Ok(mcp.clone());
        }
        let _ = b;
        let mcp = self.connect(&cwd, &Map::new()).await?;
        *slot = Some(mcp.clone());
        Ok(mcp)
    }

    async fn start(self: &Arc<Self>, start: BindingStart) -> Result<Value, RpcError> {
        let mcp = self.mcp(&start.cwd, &start.options).await?;
        let instance = start
            .options
            .get("instance")
            .or_else(|| start.options.get("providerInstanceId"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| start.agent.strip_prefix("t3:").unwrap_or("default").to_owned());
        let model = start.options.get("model").and_then(Value::as_str);
        let title = start
            .options
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Parrot session {}", start.session));
        let mut arguments = json!({ "title": title, "scratch": true });
        if let Some(project) = start.options.get("projectId") {
            arguments["projectId"] = project.clone();
            arguments.as_object_mut().map(|m| m.remove("scratch"));
        }
        if let Some(model) = model {
            arguments["modelSelection"] = json!({ "instanceId": instance, "model": model });
        }
        if let Some(mode) = start.options.get("mode").and_then(Value::as_str) {
            arguments["runtimeMode"] = json!(mode);
        }
        let launched = mcp.call("t3_thread_launch", arguments).await.map_err(harness)?;
        let thread = launched
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(-32020, format!("T3 returned no thread: {launched}")))?
            .to_owned();
        let mut state = ThreadState::new();
        state.model = start
            .options
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        state.mode = start
            .options
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let options = options_of(&state);
        let (level, network) = state.level();
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            thread: thread.clone(),
            state: Mutex::new(state),
        });
        self.bindings.lock().unwrap().insert(start.binding.clone(), b);
        self.emit(&start.binding, vec![state_update(State::Idle, None)]);
        Ok(to_value(&BindingStartResult {
            reference: Some(Ref {
                id: thread,
                strength: RefStrength::Strong,
            }),
            isolation: Isolation::None,
            level,
            network,
            options,
            actions: vec![],
        }))
    }

    /// Waits for the run, reads its new messages and ends the turn.
    async fn pump(self: Arc<Self>, b: Arc<Binding>, mcp: Arc<Mcp>) {
        loop {
            let waited = mcp
                .call(
                    "t3_thread_wait",
                    json!({ "threadId": b.thread, "timeoutMs": 10_000 }),
                )
                .await;
            let result = match waited {
                Ok(result) => result,
                Err(e) => {
                    let updates = end_turn(&b, Some(e));
                    self.emit(&b.id, updates);
                    return;
                }
            };
            match read_items(&b, &mcp).await {
                Ok(items) => self.emit(&b.id, items),
                Err(e) => {
                    self.emit(
                        &b.id,
                        vec![DriverUpdateKind::Notice {
                            level: NoticeLevel::Warning,
                            text: format!("T3 thread read failed: {e}"),
                            code: Some("x.parrot/t3_read".into()),
                        }],
                    );
                }
            }
            pending(&self, &b, &mcp).await;
            let status = result
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let timed_out = result.get("timedOut").and_then(Value::as_bool) == Some(true);
            if timed_out || !matches!(status, "running" | "queued" | "starting") {
                let updates = end_turn(&b, None);
                self.emit(&b.id, updates);
                return;
            }
        }
    }

    /// Sends a prompt, with its context and attachments. T3 takes an attachment only after an
    /// upload: prepare, post the bytes, then send the message that names it.
    async fn send_prompt(
        &self,
        b: &Binding,
        mcp: &Arc<Mcp>,
        p: &BindingPromptParams,
    ) -> Result<(), RpcError> {
        let mut text = String::new();
        for entry in p.context.iter().flatten() {
            text.push_str(&format!(
                "<context uri=\"{}\">\n{}\n</context>\n\n",
                entry.uri, entry.text
            ));
        }
        text.push_str(&p.text);
        let mut attachments = Vec::new();
        for attachment in p.attachments.iter().flatten() {
            let Some(data) = &attachment.data else {
                self.notice(
                    b,
                    format!(
                        "The attachment {} ({}) was not sent: it carries no data.",
                        attachment.name, attachment.media_type
                    ),
                );
                continue;
            };
            let bytes = decode_base64(data);
            let size = bytes.len();
            if size == 0 {
                self.notice(
                    b,
                    format!("The attachment {} was not sent: it is empty.", attachment.name),
                );
                continue;
            }
            let kind = if attachment.media_type.starts_with("image/") {
                "image"
            } else {
                "file"
            };
            let prepared = mcp
                .call(
                    "t3_attachment_prepare_upload",
                    json!({ "upload": {
                        "type": kind,
                        "name": attachment.name,
                        "mimeType": attachment.media_type,
                        "sizeBytes": size,
                    }}),
                )
                .await;
            let prepared = match prepared {
                Ok(value) => value,
                Err(e) => {
                    self.notice(
                        b,
                        format!(
                            "The attachment {} was not sent: T3 refused the upload ({e}).",
                            attachment.name
                        ),
                    );
                    continue;
                }
            };
            let (Some(id), Some(relative)) = (
                prepared.get("attachmentId").and_then(Value::as_str),
                prepared.get("relativeUrl").and_then(Value::as_str),
            ) else {
                self.notice(
                    b,
                    format!(
                        "The attachment {} was not sent: T3 named no upload.",
                        attachment.name
                    ),
                );
                continue;
            };
            if let Err(e) = mcp.upload(relative, &bytes, &attachment.media_type).await {
                self.notice(
                    b,
                    format!(
                        "The attachment {} was not sent: the upload failed ({e}).",
                        attachment.name
                    ),
                );
                continue;
            }
            attachments.push(json!({
                "type": kind,
                "id": id,
                "name": attachment.name,
                "mimeType": attachment.media_type,
                "sizeBytes": size,
            }));
        }
        if attachments.is_empty() {
            mcp.call(
                "t3_thread_send",
                json!({ "threadId": b.thread, "message": text, "mode": "auto" }),
            )
            .await
            .map_err(harness)?;
        } else {
            mcp.call(
                "t3_thread_send_attachments",
                json!({ "threadId": b.thread, "message": text, "attachments": attachments }),
            )
            .await
            .map_err(harness)?;
        }
        Ok(())
    }

    fn notice(&self, b: &Binding, text: String) {
        self.emit(
            &b.id,
            vec![DriverUpdateKind::Notice {
                level: NoticeLevel::Warning,
                text,
                code: Some("attachment_unsupported".into()),
            }],
        );
    }
}

/// A server from the options' `t3.url`, else the environment's, else a command over stdio.
async fn connect(config: &Config, cwd: &str, options: &Map<String, Value>) -> Result<Arc<Mcp>, RpcError> {
    let url = options
        .get("t3.url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| config.url.clone());
    if let Some(url) = url {
        return Mcp::connect_http(&url, config.token.clone())
            .await
            .map_err(|e| RpcError::new(-32020, format!("the T3 server did not answer: {e}")));
    }
    let argv = command_of(options, config)
        .ok_or_else(|| RpcError::new(-32020, "no T3 server; set PARROT_T3_URL or PARROT_T3_MCP"))?;
    Mcp::connect(&argv, cwd, &config.env, config.guard)
        .await
        .map_err(|e| RpcError::new(-32020, format!("the T3 MCP server did not start: {e}")))
}
/// Base64 as an attachment carries it.
fn decode_base64(data: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for c in data.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    out
}

fn command_of(options: &Map<String, Value>, config: &Config) -> Option<Vec<OsString>> {
    for key in ["t3.command", "command"] {
        if let Some(argv) = options.get(key).and_then(argv_of) {
            return Some(argv);
        }
    }
    if let Some(argv) = &config.command {
        return Some(argv.iter().map(OsString::from).collect());
    }
    let bin = std::env::var_os(MCP_BIN_ENV)?;
    let mut argv = vec![bin];
    if let Ok(args) = std::env::var(MCP_ARGS_ENV) {
        argv.extend(args.split_whitespace().map(OsString::from));
    }
    Some(argv)
}

fn argv_of(value: &Value) -> Option<Vec<OsString>> {
    match value {
        Value::String(s) => Some(s.split_whitespace().map(OsString::from).collect()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(OsString::from)
                .collect(),
        ),
        _ => None,
    }
}

fn options_of(state: &ThreadState) -> Vec<ConfigOption> {
    vec![
        ConfigOption {
            id: "model".into(),
            role: Some(Open::Known(OptionRole::Model)),
            label: "Model".into(),
            description: None,
            schema: json!({ "type": "string" }),
            value: state.model.clone().map_or(Value::Null, Value::String),
            default: None,
            applies: Applies::NextTurn,
            depends_on: None,
            complete: None,
            meta: None,
            extra: Map::new(),
        },
        ConfigOption {
            id: "mode".into(),
            role: Some(Open::Known(OptionRole::Mode)),
            label: "Runtime mode".into(),
            description: None,
            schema: json!({
                "type": "string",
                "oneOf": [
                    { "const": "approval-required", "title": "Ask before edits" },
                    { "const": "auto-accept-edits", "title": "Accept edits" },
                    { "const": "auto", "title": "Auto" },
                    { "const": "full-access", "title": "Full access" },
                ],
            }),
            value: state.mode.clone().map_or(Value::Null, Value::String),
            default: None,
            applies: Applies::NextTurn,
            depends_on: None,
            complete: None,
            meta: None,
            extra: Map::new(),
        },
    ]
}

fn start_turn(b: &Binding, message: &str) -> Turn {
    let mut state = b.state.lock().unwrap();
    state.turns += 1;
    let turn = Turn {
        id: format!("turn-{}", state.turns),
        binding: String::new(),
        status: TurnStatus::Running,
        stop: None,
        continuable: false,
        input: Some(message.to_owned()),
        started: now(),
        ended: None,
        error: None,
        usage: None,
        meta: None,
        extra: Map::new(),
    };
    state.turn = Some(turn.clone());
    turn
}

fn emit_user(driver: &Driver, binding: &str, p: &BindingPromptParams, turn: &str) {
    let mut set = Map::new();
    set.insert("text".into(), json!(p.text));
    set.insert("status".into(), json!("sent"));
    set.insert("message".into(), json!(p.message));
    driver.emit(
        binding,
        vec![
            state_update(State::Running, None),
            DriverUpdateKind::Turn {
                turn: Turn {
                    id: turn.to_owned(),
                    binding: String::new(),
                    status: TurnStatus::Running,
                    stop: None,
                    continuable: false,
                    input: Some(p.message.clone()),
                    started: now(),
                    ended: None,
                    error: None,
                    usage: None,
                    meta: None,
                    extra: Map::new(),
                },
            },
            DriverUpdateKind::Item {
                item: parrot_proto::item::ItemPatch {
                    id: p.message.clone(),
                    kind: Some(Open::Known(parrot_proto::item::ItemKind::User)),
                    turn: Some(turn.to_owned()),
                    set: Some(set),
                    ..Default::default()
                },
            },
        ],
    );
}

fn state_update(state: State, detail: Option<String>) -> DriverUpdateKind {
    DriverUpdateKind::State {
        state,
        background: vec![],
        detail,
    }
}

/// The thread's messages that are new, as item updates.
async fn read_items(b: &Binding, mcp: &Mcp) -> Result<Vec<DriverUpdateKind>, String> {
    let position = b.state.lock().unwrap().position;
    let read = mcp
        .call(
            "t3_thread_read",
            json!({
                "threadId": b.thread,
                "view": "messages",
                "afterPosition": position,
                "limit": 100,
            }),
        )
        .await?;
    let mut out = Vec::new();
    let mut newest = position;
    for item in read
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let position = item.get("position").and_then(Value::as_i64).unwrap_or(newest + 1);
        newest = newest.max(position);
        let id = item
            .get("itemId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("item-{position}"));
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind.contains("user") {
            continue;
        }
        if b.state.lock().unwrap().seen.contains(&id) {
            continue;
        }
        b.state.lock().unwrap().seen.insert(id.clone());
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if kind.contains("tool") {
            let status = match item.get("status").and_then(Value::as_str) {
                Some("completed") => "ok",
                Some("failed") | Some("error") => "error",
                Some("cancelled") | Some("interrupted") | Some("rolled_back") => "cancelled",
                _ => "running",
            };
            out.push(DriverUpdateKind::Item {
                item: parrot_proto::item::ItemPatch {
                    id,
                    kind: Some(Open::Known(parrot_proto::item::ItemKind::Tool)),
                    turn: b.state.lock().unwrap().turn.as_ref().map(|t| t.id.clone()),
                    set: Some({
                        let mut set = Map::new();
                        set.insert("tool".into(), json!("t3"));
                        set.insert(
                            "title".into(),
                            item.get("title").cloned().unwrap_or_else(|| json!("T3 tool")),
                        );
                        set.insert("input".into(), json!({}));
                        set.insert("status".into(), json!(status));
                        set.insert("caller".into(), json!("model"));
                        set.insert("output".into(), json!(text));
                        set
                    }),
                    ..Default::default()
                },
            });
        } else if !text.is_empty() {
            out.push(DriverUpdateKind::Item {
                item: parrot_proto::item::ItemPatch {
                    id,
                    kind: Some(Open::Known(parrot_proto::item::ItemKind::Assistant)),
                    turn: b.state.lock().unwrap().turn.as_ref().map(|t| t.id.clone()),
                    set: Some({
                        let mut set = Map::new();
                        set.insert("text".into(), json!(text));
                        set
                    }),
                    streaming: Some(false),
                    ..Default::default()
                },
            });
        }
    }
    b.state.lock().unwrap().position = newest;
    Ok(out)
}

/// T3's pending questions become Parrot requests (Appendix B).
async fn pending(driver: &Driver, b: &Binding, mcp: &Mcp) {
    let Ok(list) = mcp
        .call(
            "t3_pending_request",
            json!({ "action": "list", "threadId": b.thread }),
        )
        .await
    else {
        return;
    };
    for id in list
        .get("requestIds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(id) = id.as_str().map(str::to_owned) else {
            continue;
        };
        if !b.state.lock().unwrap().requests.insert(id.clone()) {
            continue;
        }
        let Ok(read) = mcp
            .call(
                "t3_pending_request",
                json!({ "action": "read", "threadId": b.thread, "requestId": id }),
            )
            .await
        else {
            continue;
        };
        let questions = read.get("questions").cloned().unwrap_or(Value::Null);
        let typed = questions_of(&questions);
        let request = Request {
            id: id.clone(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: b.state.lock().unwrap().turn.as_ref().map(|t| t.id.clone()),
            item: None,
            kind: Open::Known(RequestKind::Question),
            title: "T3 asks a question".into(),
            description: read.get("title").and_then(Value::as_str).map(str::to_owned),
            subject: Some(Open::Known(Subject::Tool {
                tool: "t3_pending_request".into(),
                input: questions.clone(),
                editable: Some(false),
            })),
            level: PermissionLevel::Read,
            network: None,
            choices: Some(Vec::<Choice>::new()),
            questions: (!typed.is_empty()).then_some(typed),
            input: Some(questions),
            blocking: true,
            answerable: Answerable::Message,
            expires: None,
            fallback: None,
            notify: Notify::Normal,
            status: RequestStatus::Open,
            answer: None,
            answered_by: None,
            meta: None,
            extra: Map::new(),
        };
        driver.emit(
            &b.id,
            vec![
                DriverUpdateKind::Request {
                    request: Box::new(request),
                },
                state_update(State::RequiresAction, None),
            ],
        );
    }
}

fn end_turn(b: &Binding, error: Option<String>) -> Vec<DriverUpdateKind> {
    let mut state = b.state.lock().unwrap();
    let Some(turn) = state.turn.take() else {
        return vec![];
    };
    let cancelled = std::mem::take(&mut state.cancelled);
    let (status, stop, continuable) = if cancelled {
        (TurnStatus::Interrupted, TurnStop::Cancelled, true)
    } else if error.is_some() {
        (TurnStatus::Failed, TurnStop::Error, false)
    } else {
        (TurnStatus::Completed, TurnStop::End, false)
    };
    let usage = Usage::default();
    vec![
        DriverUpdateKind::Turn {
            turn: Turn {
                id: turn.id,
                binding: String::new(),
                status,
                stop: Some(Open::Known(stop)),
                continuable,
                input: turn.input,
                started: turn.started,
                ended: Some(now()),
                error: error.map(|message| parrot_proto::state::TurnError {
                    code: "harness_error".into(),
                    message,
                    retryable: true,
                }),
                usage: Some(usage.clone()),
                meta: None,
                extra: Map::new(),
            },
        },
        DriverUpdateKind::Usage { usage },
        state_update(State::Idle, None),
    ]
}

fn harness(e: String) -> RpcError {
    RpcError::new(-32020, e)
}

/// The `t3` agent as `agent/list` reports it.
pub fn agent() -> Agent {
    let mut features = Map::new();
    features.insert("attachments".into(), json!({"kinds": ["image", "file"]}));
    Agent {
        id: parrot_proto::driver_data::agent_id("t3"),
        env: String::new(),
        name: parrot_proto::driver_data::agent_name("T3 Code"),
        driver: AgentDriver {
            name: "t3".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: DriverKind::Process,
            data: "1".into(),
        },
        harness: None,
        installed: true,
        auth: AuthStatus::Unknown,
        usable: true,
        detail: None,
        controls: vec![Control::Native],
        features,
        levels: vec![
            PermissionLevel::Read,
            PermissionLevel::Write,
            PermissionLevel::Host,
        ],
        isolation: vec![Isolation::None],
        options: options_of(&ThreadState::new()),
        actions: vec![],
        meta: None,
        extra: Map::new(),
    }
}

/// [`agent`] with the version T3 reports in the MCP handshake, when the server answers.
pub async fn probed(config: &Config) -> Agent {
    let mut a = agent();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let options = Map::new();
    let connected = tokio::time::timeout(Duration::from_secs(10), connect(config, &cwd, &options)).await;
    if let Ok(Ok(mcp)) = connected {
        a.harness = mcp
            .server_info()
            .and_then(|info| {
                info.get("version")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            })
            .map(|version| parrot_proto::agent::HarnessInfo {
                version,
                verified: true,
            });
        mcp.stop().await;
    }
    a
}

/// The current time in RFC 3339, UTC, to the millisecond.
pub fn now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// T3's pending questions as typed questions (spec 14). Its shape is read defensively: an entry
/// needs a question text, and takes an id, a header, a multi-select flag and label options.
fn questions_of(questions: &Value) -> Vec<Question> {
    let Some(list) = questions.as_array() else {
        return Vec::new();
    };
    list.iter()
        .enumerate()
        .filter_map(|(i, q)| {
            let question = q.get("question").and_then(Value::as_str)?.to_owned();
            let id = q
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("question-{i}"));
            Some(Question {
                id,
                header: q.get("header").and_then(Value::as_str).map(str::to_owned),
                question,
                multi_select: q.get("multiSelect").and_then(Value::as_bool),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|options| {
                        options
                            .iter()
                            .enumerate()
                            .filter_map(|(j, o)| {
                                let label = o.as_str().or_else(|| o.get("label").and_then(Value::as_str))?;
                                Some(Choice {
                                    id: format!("option-{j}"),
                                    label: label.to_owned(),
                                    intent: Some(ChoiceIntent::Neutral),
                                    scope: None,
                                    amends: None,
                                    description: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}
