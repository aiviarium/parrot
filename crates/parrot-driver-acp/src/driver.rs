//! The ACP driver: the driver protocol (spec 30) as JSON-RPC lines, one ACP agent per binding.
//!
//! The agent command comes from the binding's `command` option, else the egg's `driver.kdl`, else
//! `PARROT_ACP_BIN`, else `acp` on the path.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parrot_proto::action::ActionStatus;
use parrot_proto::agent::{Agent, AgentDriver, AgentListResult, AuthStatus, Control, DriverKind};
use parrot_proto::driver::{
    BindingActionParams, BindingActionResult, BindingAnswerParams, BindingCancelParams, BindingCancelResult,
    BindingOptionsParams, BindingOptionsResult, BindingPromptParams, BindingPromptResult, BindingRef,
    BindingStart, BindingStartResult, DriverUpdate, DriverUpdateKind, McpInjection,
};
use parrot_proto::open::Open;
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::Isolation;
use parrot_proto::rpc::{Message, Notification, Response, RpcError};
use parrot_proto::state::State;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use crate::data::{self, Data};
use crate::mapping::{Capabilities, MapState};
use crate::wire::{self, SessionResult, SessionUpdate};

/// The variable that names the ACP agent binary.
pub const ACP_BIN_ENV: &str = "PARROT_ACP_BIN";
/// Extra arguments, for the variable form.
pub const ACP_ARGS_ENV: &str = "PARROT_ACP_ARGS";
/// How long the agent has to answer a request that is not a prompt.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `binding/stop` waits after closing stdin before killing the agent.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How the driver starts ACP agents.
#[derive(Debug, Clone)]
pub struct Config {
    /// Extra environment for the agent.
    pub env: Vec<(OsString, OsString)>,
    /// Start the agent through `parrot guard --`, which owns the process group.
    pub guard: bool,
    /// The driver data.
    pub data: Data,
}

impl Config {
    /// The environment's configuration and the installed egg's data.
    pub fn from_env() -> Self {
        let guard = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s == "parrot"))
            .unwrap_or(false);
        Config {
            env: vec![],
            guard,
            data: data::load(),
        }
    }
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

struct Binding {
    id: String,
    out: mpsc::UnboundedSender<String>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Child>,
    state: Mutex<MapState>,
    pending: Pending,
    next: AtomicU64,
    stopping: AtomicBool,
}

struct Driver {
    config: Config,
    out: mpsc::UnboundedSender<String>,
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
        b.stop().await;
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

    fn emit(&self, updates: Vec<DriverUpdate>) {
        let mut frames = Vec::new();
        for u in updates {
            if let Open::Known(DriverUpdateKind::Frame { .. }) = &u.update {
                frames.push(u);
            } else {
                self.send(&Notification::new("update", to_value(&u)));
            }
        }
        for frame in frames {
            self.send(&Notification::new("update", to_value(&frame)));
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

    async fn handle(self: &Arc<Self>, method: &str, p: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(json!({
                "protocol": 1,
                "peer": {"kind": "driver", "name": "parrot-driver-acp", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![agent(&self.config.data)],
            })),
            "binding/start" => self.start(params(p)?).await,
            "binding/prompt" => {
                let p: BindingPromptParams = params(p)?;
                if p.subagent.is_some() {
                    return Err(RpcError::new(-32001, "ACP has no subagents of its own"));
                }
                let b = self.binding(&p.binding)?;
                let (updates, disposition, blocks) = {
                    let mut state = b.state.lock().unwrap();
                    if state.turn.is_some() {
                        return Err(
                            RpcError::new(-32004, "a turn is running").with_data(json!({"state": "running"}))
                        );
                    }
                    state.prompt(&p, &now())
                };
                self.emit(updates);
                let b2 = b.clone();
                let driver = self.clone();
                tokio::spawn(async move {
                    let result = b2
                        .request(
                            "session/prompt",
                            json!({ "sessionId": b2.session_id(), "prompt": blocks }),
                            None,
                        )
                        .await;
                    let updates = {
                        let mut state = b2.state.lock().unwrap();
                        match result {
                            Ok(value) => {
                                let prompt_result: wire::PromptResult =
                                    serde_json::from_value(value).unwrap_or_default();
                                state.turn_end(Some(&prompt_result), None, &now())
                            }
                            Err(e) => state.turn_end(None, Some(e), &now()),
                        }
                    };
                    driver.emit(updates);
                });
                Ok(to_value(&BindingPromptResult { disposition }))
            }
            "binding/cancel" => {
                let p: BindingCancelParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (session, running, answers, updates) = {
                    let mut state = b.state.lock().unwrap();
                    let running = state.turn.is_some();
                    state.cancel();
                    let (answers, updates) = state.cancel_requests();
                    (
                        state.session_id.clone().or_else(|| state.session_hint()),
                        running,
                        answers,
                        updates,
                    )
                };
                self.emit(updates);
                for answer in answers {
                    b.write_value(&answer).await?;
                }
                if running && let Some(session) = session {
                    b.notify("session/cancel", json!({ "sessionId": session }))
                        .await?;
                }
                Ok(to_value(&BindingCancelResult { interrupted: running }))
            }
            "binding/answer" => {
                let p: BindingAnswerParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (raw, result, updates) = {
                    let mut state = b.state.lock().unwrap();
                    let raw = state
                        .requests
                        .get(&p.request)
                        .map(|r| r.raw.clone())
                        .unwrap_or(Value::Null);
                    let (result, updates) = state
                        .answer(&p.request, &p.answer)
                        .map_err(|e| RpcError::new(-32602, e))?;
                    (raw, result, updates)
                };
                self.emit(updates);
                b.write_value(&json!({
                    "jsonrpc": "2.0",
                    "id": raw,
                    "result": result,
                }))
                .await?;
                Ok(json!({}))
            }
            "binding/options" => {
                let p: BindingOptionsParams = params(p)?;
                let b = self.binding(&p.binding)?;
                b.set_options(&p.values).await?;
                let result = {
                    let state = b.state.lock().unwrap();
                    BindingOptionsResult {
                        options: state.options(),
                        level: state.level(),
                        network: state.network(),
                    }
                };
                let update = b.state.lock().unwrap().options_update();
                self.emit(vec![update]);
                Ok(to_value(&result))
            }
            "binding/action" => {
                let p: BindingActionParams = params(p)?;
                let Some(binding) = p.binding.as_deref() else {
                    return Err(RpcError::new(-32601, "the ACP agent has no agent-scoped actions"));
                };
                let b = self.binding(binding)?;
                let message = p
                    .message
                    .clone()
                    .ok_or_else(|| RpcError::new(-32602, "a turn action needs a message id"))?;
                let name = b
                    .state
                    .lock()
                    .unwrap()
                    .actions
                    .iter()
                    .find(|a| a.id == p.action)
                    .and_then(|a| a.name.clone())
                    .unwrap_or_else(|| format!("/{}", p.action));
                let mut text = name;
                if let Some(args) = &p.args {
                    text.push(' ');
                    text.push_str(args);
                }
                let prompt = BindingPromptParams {
                    binding: binding.to_owned(),
                    subagent: None,
                    message,
                    text,
                    context: None,
                    attachments: None,
                    steer: false,
                };
                let (updates, disposition, blocks) = {
                    let mut state = b.state.lock().unwrap();
                    if state.turn.is_some() {
                        return Err(
                            RpcError::new(-32004, "a turn is running").with_data(json!({"state": "running"}))
                        );
                    }
                    state.prompt(&prompt, &now())
                };
                self.emit(updates);
                let b2 = b.clone();
                let driver = self.clone();
                tokio::spawn(async move {
                    let result = b2
                        .request(
                            "session/prompt",
                            json!({ "sessionId": b2.session_id(), "prompt": blocks }),
                            None,
                        )
                        .await;
                    let updates = {
                        let mut state = b2.state.lock().unwrap();
                        match result {
                            Ok(value) => {
                                let prompt_result: wire::PromptResult =
                                    serde_json::from_value(value).unwrap_or_default();
                                state.turn_end(Some(&prompt_result), None, &now())
                            }
                            Err(e) => state.turn_end(None, Some(e), &now()),
                        }
                    };
                    driver.emit(updates);
                });
                let _ = disposition;
                Ok(to_value(&BindingActionResult {
                    status: ActionStatus::Started,
                    request: None,
                    result: None,
                }))
            }
            "binding/stop" => {
                let p: BindingRef = params(p)?;
                let b = self.bindings.lock().unwrap().remove(&p.binding);
                if let Some(b) = b {
                    b.stop().await;
                    let update = b.state.lock().unwrap().binding.clone();
                    self.emit(vec![state_update(&update, State::Stopped, None)]);
                }
                Ok(json!({}))
            }
            _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
        }
    }

    /// The agent's argv: the binding's `command` option, the egg, the environment, `acp`.
    fn command(&self, options: &Map<String, Value>) -> Vec<OsString> {
        for key in ["command", "agent.command"] {
            if let Some(argv) = options.get(key).and_then(argv_of) {
                return argv;
            }
        }
        if let Some(argv) = &self.config.data.command {
            return argv.iter().map(OsString::from).collect();
        }
        if let Some(bin) = std::env::var_os(ACP_BIN_ENV) {
            let mut argv = vec![bin];
            if let Ok(args) = std::env::var(ACP_ARGS_ENV) {
                argv.extend(args.split_whitespace().map(OsString::from));
            }
            return argv;
        }
        vec!["acp".into()]
    }

    async fn start(self: &Arc<Self>, start: BindingStart) -> Result<Value, RpcError> {
        let mut argv = self.command(&start.options);
        if argv.is_empty() {
            return Err(RpcError::new(-32020, "no ACP agent command"));
        }
        let program = argv.remove(0);
        let mut cmd = if self.config.guard {
            parrot_guard::guarded_command(&program)
        } else {
            let mut c = tokio::process::Command::new(&program);
            #[cfg(unix)]
            c.process_group(0);
            c
        };
        cmd.args(&argv)
            .envs(self.config.env.iter().cloned())
            .current_dir(&start.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::new(-32020, format!("cannot start {}: {e}", program.to_string_lossy())))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            out: self.out.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(child),
            state: Mutex::new(MapState::new(start.binding.clone(), self.config.data.clone())),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: Default::default(),
        });
        self.bindings
            .lock()
            .unwrap()
            .insert(start.binding.clone(), b.clone());
        tokio::spawn(read_loop(self.clone(), b.clone(), stdout));
        match self.open_binding(&b, &start).await {
            Ok((result, updates)) => {
                self.emit(updates);
                Ok(to_value(&result))
            }
            Err(e) => {
                self.bindings.lock().unwrap().remove(&start.binding);
                b.stop().await;
                Err(e)
            }
        }
    }

    async fn open_binding(
        &self,
        b: &Arc<Binding>,
        start: &BindingStart,
    ) -> Result<(BindingStartResult, Vec<DriverUpdate>), RpcError> {
        let init = b
            .request("initialize", wire::initialize_params(), Some(REQUEST_TIMEOUT))
            .await
            .map_err(|e| RpcError::new(-32020, format!("the agent did not initialize: {e}")))?;
        let init: wire::InitializeResult = serde_json::from_value(init).unwrap_or_default();
        let capabilities = Capabilities::from_initialize(&init);
        let mut updates = Vec::new();
        if !init.auth_methods.is_empty() {
            updates.push(b.notice(
                "warning",
                "The agent offers sign-in; Parrot does not sign in for it.",
                "x.parrot/acp_auth",
            ));
        }
        let mcp_servers = mcp_servers(start.mcp.as_ref(), &capabilities);
        let (method, params, replaying) = match &start.resume {
            Some(reference) if capabilities.resume => (
                "session/resume",
                json!({ "sessionId": reference.id, "cwd": start.cwd, "mcpServers": mcp_servers }),
                false,
            ),
            Some(reference) if capabilities.load => (
                "session/load",
                json!({ "sessionId": reference.id, "cwd": start.cwd, "mcpServers": mcp_servers }),
                true,
            ),
            Some(_) => {
                return Err(RpcError::new(
                    -32001,
                    "the agent can neither resume nor load a session",
                ));
            }
            None => (
                "session/new",
                json!({ "cwd": start.cwd, "mcpServers": mcp_servers }),
                false,
            ),
        };
        if replaying {
            b.state.lock().unwrap().replaying = true;
        }
        let opened = b
            .request(method, params, Some(REQUEST_TIMEOUT))
            .await
            .map_err(|e| RpcError::new(-32020, format!("the agent did not open a session: {e}")));
        if replaying {
            b.state.lock().unwrap().replaying = false;
        }
        let opened = opened?;
        let result: SessionResult = serde_json::from_value(opened).unwrap_or_default();
        {
            let mut state = b.state.lock().unwrap();
            state.capabilities = capabilities.clone();
            if let Some(reference) = &start.resume {
                state.session_id = Some(reference.id.clone());
            }
            updates.extend(state.opened(&result));
        }
        for (key, value) in &start.options {
            if key == "command" || key == "agent.command" {
                continue;
            }
            // A value the agent does not offer is reported, not fatal: the option maps may be
            // older than the agent.
            if let Err(e) = b.apply_option(key, value).await {
                updates.push(b.notice("warning", e, "x.parrot/acp_option"));
            }
        }
        let reference = {
            let state = b.state.lock().unwrap();
            state.session_id.clone().map(|id| parrot_proto::session::Ref {
                id,
                strength: parrot_proto::session::RefStrength::Strong,
            })
        };
        let result = {
            let state = b.state.lock().unwrap();
            BindingStartResult {
                reference,
                isolation: Isolation::None,
                level: state.level(),
                network: state.network(),
                options: state.options(),
                actions: state.actions.clone(),
            }
        };
        updates.push(state_update(&start.binding, State::Idle, None));
        Ok((result, updates))
    }
}

fn argv_of(value: &Value) -> Option<Vec<OsString>> {
    match value {
        Value::String(s) => {
            let mut argv: Vec<OsString> = Vec::new();
            for part in s.split_whitespace() {
                argv.push(part.into());
            }
            (!argv.is_empty()).then_some(argv)
        }
        Value::Array(items) => {
            let argv: Vec<OsString> = items
                .iter()
                .filter_map(Value::as_str)
                .map(OsString::from)
                .collect();
            (!argv.is_empty()).then_some(argv)
        }
        _ => None,
    }
}

/// The MCP servers a `session/new` carries: the runtime's HTTP server with its token in a header.
fn mcp_servers(mcp: Option<&McpInjection>, capabilities: &Capabilities) -> Vec<Value> {
    let Some(mcp) = mcp else {
        return vec![];
    };
    if !capabilities.mcp_http {
        return vec![];
    }
    let Some(url) = &mcp.url else {
        return vec![];
    };
    vec![json!({
        "type": "http",
        "name": "parrot",
        "url": url,
        "headers": [{ "name": "Authorization", "value": format!("Bearer {}", mcp.token) }],
    })]
}

impl Binding {
    fn session_id(&self) -> Option<String> {
        self.state.lock().unwrap().session_id.clone()
    }

    fn notice(&self, level: &str, text: impl Into<String>, code: &str) -> DriverUpdate {
        let level = match level {
            "error" => parrot_proto::subscribe::NoticeLevel::Error,
            "warning" => parrot_proto::subscribe::NoticeLevel::Warning,
            _ => parrot_proto::subscribe::NoticeLevel::Info,
        };
        self.state.lock().unwrap().update(DriverUpdateKind::Notice {
            level,
            text: text.into(),
            code: Some(code.into()),
        })
    }

    async fn write_value(&self, msg: &Value) -> Result<(), RpcError> {
        // Every raw ACP message goes to the frame log, without the values that carry secrets.
        let frame = self.state.lock().unwrap().update(DriverUpdateKind::Frame {
            source: "acp".into(),
            data: wire::redact(msg),
        });
        if let Ok(line) = serde_json::to_string(&Notification::new("update", to_value(&frame))) {
            let _ = self.out.send(line);
        }
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| RpcError::new(-32000, "the agent has exited"))?;
        let line = serde_json::to_string(msg).expect("JSON messages serialize") + "\n";
        let res = async {
            pipe.write_all(line.as_bytes()).await?;
            pipe.flush().await
        };
        res.await
            .map_err(|e| RpcError::new(-32000, format!("cannot write to the agent: {e}")))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    /// Sends a request and waits for its response, or for the timeout when one is given.
    async fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let sent = self
            .write_value(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .await;
        if let Err(e) = sent {
            self.pending.lock().unwrap().remove(&id);
            return Err(e.message);
        }
        let waited = match timeout {
            Some(timeout) => match tokio::time::timeout(timeout, rx).await {
                Ok(result) => result,
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    return Err(format!("the agent did not answer {method}"));
                }
            },
            None => rx.await,
        };
        match waited {
            Ok(result) => result,
            Err(_) => Err("the agent exited".into()),
        }
    }

    async fn set_options(&self, values: &Map<String, Value>) -> Result<(), RpcError> {
        for (key, value) in values {
            self.apply_option(key, value)
                .await
                .map_err(|e| RpcError::new(-32602, e))?;
        }
        Ok(())
    }

    async fn apply_option(&self, key: &str, value: &Value) -> Result<(), String> {
        let (session, mode_config, effort_config) = {
            let state = self.state.lock().unwrap();
            (
                state.session_id.clone().unwrap_or_default(),
                state.model_config_id(),
                state.effort_config_id(),
            )
        };
        match key {
            "mode" => {
                let Some(mode) = value.as_str() else {
                    return Err("mode takes a string".into());
                };
                self.request(
                    "session/set_mode",
                    json!({ "sessionId": session, "modeId": mode }),
                    Some(REQUEST_TIMEOUT),
                )
                .await?;
                self.state.lock().unwrap().mode = Some(mode.to_owned());
            }
            "model" => {
                let Some(model) = value.as_str() else {
                    return Err("model takes a string".into());
                };
                if let Some(config) = mode_config {
                    self.request(
                        "session/set_config_option",
                        json!({ "sessionId": session, "configId": config, "value": model }),
                        Some(REQUEST_TIMEOUT),
                    )
                    .await?;
                } else {
                    self.request(
                        "session/set_model",
                        json!({ "sessionId": session, "modelId": model }),
                        Some(REQUEST_TIMEOUT),
                    )
                    .await?;
                }
                self.state.lock().unwrap().model = Some(model.to_owned());
            }
            "effort" => {
                let Some(effort) = value.as_str() else {
                    return Err("effort takes a string".into());
                };
                let Some(config) = effort_config else {
                    return Err("the agent has no reasoning level".into());
                };
                self.request(
                    "session/set_config_option",
                    json!({ "sessionId": session, "configId": config, "value": effort }),
                    Some(REQUEST_TIMEOUT),
                )
                .await?;
                self.state.lock().unwrap().effort = Some(effort.to_owned());
            }
            other => return Err(format!("unknown option {other}")),
        }
        Ok(())
    }

    async fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(mut stdin) = self.stdin.lock().await.take() {
            let _ = stdin.shutdown().await;
        }
        let mut child = self.child.lock().await;
        if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() {
            if let Some(pid) = child.id() {
                parrot_guard::kill_tree(pid);
            }
            let _ = child.kill().await;
        }
    }
}

async fn read_loop(driver: Arc<Driver>, b: Arc<Binding>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Message>(&line) else {
            continue;
        };
        let frame = match &msg {
            Message::Request(r) => {
                Some(json!({"jsonrpc": "2.0", "id": r.id, "method": r.method, "params": r.params}))
            }
            Message::Notification(n) => {
                Some(json!({"jsonrpc": "2.0", "method": n.method, "params": n.params}))
            }
            Message::Response(r) => {
                Some(json!({"jsonrpc": "2.0", "id": r.id, "result": r.result, "error": r.error}))
            }
        };
        if let Some(frame) = frame {
            let update = b.state.lock().unwrap().update(DriverUpdateKind::Frame {
                source: "acp".into(),
                data: wire::redact(&frame),
            });
            driver.emit(vec![update]);
        }
        match msg {
            Message::Response(resp) => {
                let Some(id) = resp.id.as_u64() else {
                    continue;
                };
                let Some(tx) = b.pending.lock().unwrap().remove(&id) else {
                    continue;
                };
                let _ = tx.send(match resp.error {
                    Some(err) => Err(err.message),
                    None => Ok(resp.result.unwrap_or(Value::Null)),
                });
            }
            Message::Request(req) => {
                if req.method == "session/request_permission" {
                    let request =
                        serde_json::from_value::<wire::PermissionRequest>(req.params).unwrap_or_default();
                    let updates = {
                        let mut state = b.state.lock().unwrap();
                        state.permission(&req.id, &request, &now())
                    };
                    driver.emit(updates);
                } else {
                    // Parrot offers no file system and no terminal, so nothing else is served.
                    let resp = Response::err(
                        req.id,
                        RpcError::new(-32601, format!("Parrot does not offer {}", req.method)),
                    );
                    let _ = b
                        .write_value(&serde_json::to_value(&resp).unwrap_or_default())
                        .await;
                }
            }
            Message::Notification(notification) => {
                if notification.method != "session/update" {
                    continue;
                }
                let update = notification.params.get("update").cloned().unwrap_or(Value::Null);
                let Ok(update) = serde_json::from_value::<SessionUpdate>(update) else {
                    continue;
                };
                let updates = {
                    let mut state = b.state.lock().unwrap();
                    if state.replaying {
                        Vec::new()
                    } else {
                        state.step(&update, &now())
                    }
                };
                driver.emit(updates);
            }
        }
    }
    b.pending.lock().unwrap().clear();
    if !b.stopping.load(Ordering::Relaxed) {
        let detail = {
            let mut state = b.state.lock().unwrap();
            if state.turn.is_some() {
                state.turn_end(None, Some("the agent exited".into()), &now())
            } else {
                vec![state_update(&b.id, State::Lost, Some("the agent exited".into()))]
            }
        };
        driver.emit(detail);
        driver.bindings.lock().unwrap().remove(&b.id);
    }
}

fn state_update(binding: &str, state: State, detail: Option<String>) -> DriverUpdate {
    DriverUpdate {
        binding: binding.to_owned(),
        subagent: None,
        update: Open::Known(DriverUpdateKind::State {
            state,
            background: vec![],
            detail,
        }),
        extra: Map::new(),
    }
}

/// The `acp` agent as `agent/list` reports it.
pub fn agent(data: &Data) -> Agent {
    let mut features = Map::new();
    features.insert("session.stream".into(), json!({}));
    features.insert("session.resume".into(), json!({}));
    features.insert("usage".into(), json!({"context": true, "cost": true}));
    let mut levels: Vec<PermissionLevel> = data.modes.iter().map(|r| r.level).collect();
    levels.sort();
    levels.dedup();
    if levels.is_empty() {
        levels = vec![
            PermissionLevel::Read,
            PermissionLevel::Write,
            PermissionLevel::Host,
        ];
    }
    Agent {
        id: "acp".into(),
        env: String::new(),
        name: data.agent_name.clone(),
        driver: AgentDriver {
            name: "acp".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: DriverKind::Process,
            data: data.version.clone(),
        },
        harness: None,
        installed: true,
        auth: AuthStatus::Unknown,
        usable: true,
        detail: None,
        controls: vec![Control::Native],
        features,
        levels,
        isolation: vec![Isolation::None],
        options: vec![],
        actions: vec![],
        meta: None,
        extra: Map::new(),
    }
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
