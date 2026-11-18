//! The opencode driver: the driver protocol (spec 30) over an `opencode serve` HTTP API, one server
//! per binding.
//!
//! The driver starts the server through `parrot guard --`, on a port of its own and with a password
//! it generates, so nothing else on the machine can drive it. It kills only the child it started.

use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parrot_proto::action::ActionStatus;
use parrot_proto::agent::{Agent, AgentDriver, AgentListResult, AuthStatus, Control, DriverKind};
use parrot_proto::driver::{
    BindingActionParams, BindingActionResult, BindingAnswerParams, BindingCancelParams, BindingCancelResult,
    BindingDisposition, BindingOptionsParams, BindingOptionsResult, BindingPromptParams, BindingPromptResult,
    BindingRef, BindingStart, BindingStartResult, DriverUpdate, DriverUpdateKind,
};
use parrot_proto::open::Open;
use parrot_proto::option::{CompletionValue, OptionCompleteResult, PermissionLevel};
use parrot_proto::principal::Isolation;
use parrot_proto::rpc::{Message, Notification, Response, RpcError};
use parrot_proto::state::State;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout};
use tokio::sync::{Notify, mpsc};

use crate::data::{DriverData, Sources};
use crate::http::Client;
use crate::mapping::MapState;
use crate::wire::{self};

/// The variable that names the `opencode` binary.
pub const OPENCODE_BIN_ENV: &str = "PARROT_OPENCODE_BIN";
/// The variable opencode expands in its MCP config, so the token is never in argv or a file.
pub const MCP_TOKEN_ENV: &str = "PARROT_MCP_TOKEN";
/// The variable opencode reads its server password from.
pub const SERVER_PASSWORD_ENV: &str = "OPENCODE_PASSWORD";
/// The variable opencode reads inline config from, which is where the MCP server goes.
pub const CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";
/// How long the server has to print the address it listens on.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `binding/stop` waits after closing the server before killing it.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How the driver starts opencode.
#[derive(Debug, Clone)]
pub struct Config {
    pub bin: OsString,
    /// Arguments before the port the driver adds; `serve` for the real CLI.
    pub args: Vec<OsString>,
    /// Extra environment for the CLI.
    pub env: Vec<(OsString, OsString)>,
    /// Start the CLI through `parrot guard --`, which needs the current executable to be `parrot`.
    pub guard: bool,
    /// The driver data, from `$PARROT_DRIVER_DATA` or compiled in.
    pub data: DriverData,
}

impl Config {
    /// `PARROT_OPENCODE_BIN` or `opencode`, guarded when running as `parrot`.
    pub fn from_env() -> Self {
        let guard = parrot_guard::can_guard();
        let mut data = DriverData::load(builtin);
        data.fill(default_data());
        Config {
            bin: std::env::var_os(OPENCODE_BIN_ENV).unwrap_or_else(|| "opencode".into()),
            args: vec!["serve".into(), "--hostname".into(), "127.0.0.1".into()],
            env: vec![],
            guard,
            data,
        }
    }
}

/// The texts compiled into the driver, used when `$PARROT_DRIVER_DATA` is not set.
pub fn builtin() -> Sources {
    Sources {
        options: Some(include_str!("../../../eggs/opencode/parrot/options.kdl").to_owned()),
        levels: Some(include_str!("../../../eggs/opencode/parrot/levels.kdl").to_owned()),
        correlate: Some(include_str!("../../../eggs/opencode/parrot/correlate.kdl").to_owned()),
    }
}

/// Values the driver needs even when a data file leaves them out.
pub fn default_data() -> DriverData {
    let mut data = DriverData {
        levels: vec![
            PermissionLevel::Read,
            PermissionLevel::Write,
            PermissionLevel::Host,
        ],
        ..Default::default()
    };
    // opencode asks before these tools run; the level is what the request is judged by (spec 15.2).
    for tool in [
        "read", "grep", "glob", "list", "edit", "write", "patch", "bash", "webfetch", "task",
    ] {
        let level = match tool {
            "read" | "grep" | "glob" | "list" => PermissionLevel::Read,
            "edit" | "write" | "patch" => PermissionLevel::Write,
            _ => PermissionLevel::Host,
        };
        data.tools.insert(tool.into(), level);
    }
    data.labels.insert("model".into(), "Model".into());
    data.labels.insert("provider".into(), "Provider".into());
    data
}

struct Binding {
    id: String,
    client: Client,
    child: tokio::sync::Mutex<Child>,
    state: Mutex<MapState>,
    stopping: AtomicBool,
    events: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Signalled when opencode asks for something, so a start can return on it (Appendix E4).
    raised: Notify,
}

struct Driver {
    config: Config,
    out: mpsc::UnboundedSender<String>,
    bindings: Mutex<std::collections::HashMap<String, Arc<Binding>>>,
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
        for u in updates {
            self.send(&Notification::new("update", to_value(&u)));
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
                "peer": {"kind": "driver", "name": "parrot-driver-opencode", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![agent(&self.config.data)],
                hint: None,
            })),
            "binding/start" => self.start(params(p)?).await,
            "binding/prompt" => {
                let p: BindingPromptParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let disposition = self.prompt(&b, &p).await?;
                Ok(to_value(&BindingPromptResult { disposition }))
            }
            "binding/cancel" => {
                let p: BindingCancelParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let running = {
                    let mut state = b.state.lock().unwrap();
                    state.cancel();
                    state.turn.is_some()
                };
                if !running {
                    return Ok(to_value(&BindingCancelResult { interrupted: false }));
                }
                let session = b.session_id()?;
                let answer = b
                    .client
                    .request(
                        "POST",
                        &format!("/api/session/{session}/interrupt"),
                        Some(&json!({})),
                    )
                    .await;
                let interrupted = answer
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("interrupted"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| answer.is_ok());
                Ok(to_value(&BindingCancelResult { interrupted }))
            }
            "binding/answer" => {
                let p: BindingAnswerParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (body, session, updates) = b
                    .state
                    .lock()
                    .unwrap()
                    .answer(&p.request, &p.answer)
                    .map_err(|e| RpcError::new(-32602, e))?;
                b.client
                    .request(
                        "POST",
                        &format!("/api/session/{session}/permission/{}/reply", p.request),
                        Some(&body),
                    )
                    .await
                    .map_err(|e| RpcError::new(-32603, format!("opencode refused the answer: {e}")))?;
                self.emit(updates);
                Ok(json!({}))
            }
            "binding/options" => {
                let p: BindingOptionsParams = params(p)?;
                let b = self.binding(&p.binding)?;
                for (key, value) in &p.values {
                    let text = value.as_str().map(str::to_owned);
                    match key.as_str() {
                        "model" => {
                            let Some(model) = text else {
                                b.state.lock().unwrap().model = None;
                                continue;
                            };
                            self.switch_model(&b, &model).await?;
                        }
                        "provider" => {
                            let Some(provider) = text else { continue };
                            let model = b
                                .state
                                .lock()
                                .unwrap()
                                .models
                                .iter()
                                .find(|m| m.enabled && m.provider == provider)
                                .map(|m| m.key.clone());
                            let Some(model) = model else {
                                return Err(RpcError::new(
                                    -32602,
                                    format!("no model for provider {provider}"),
                                ));
                            };
                            self.switch_model(&b, &model).await?;
                        }
                        other => return Err(RpcError::new(-32602, format!("unknown option {other}"))),
                    }
                }
                let (result, update) = {
                    let state = b.state.lock().unwrap();
                    (
                        BindingOptionsResult {
                            options: state.options(),
                            level: state.level(),
                            network: state.network(),
                        },
                        state.options_update(),
                    )
                };
                self.emit(vec![update]);
                Ok(to_value(&result))
            }
            "option/complete" => {
                // The runtime asks this of a binding for a `complete: true` option (spec 15.1).
                let binding = p.get("binding").and_then(Value::as_str);
                let b = match binding {
                    Some(id) => self.binding(id)?,
                    None => {
                        return Ok(to_value(&OptionCompleteResult { values: vec![] }));
                    }
                };
                let option = p.get("option").and_then(Value::as_str).unwrap_or_default();
                let prefix = p.get("prefix").and_then(Value::as_str).unwrap_or_default();
                let limit = p.get("limit").and_then(Value::as_u64).map(|l| l as u32);
                let values = b
                    .state
                    .lock()
                    .unwrap()
                    .complete(option, prefix, limit)
                    .into_iter()
                    .map(|(value, title)| CompletionValue { value, title })
                    .collect();
                Ok(to_value(&OptionCompleteResult { values }))
            }
            "binding/action" => {
                let p: BindingActionParams = params(p)?;
                let id = p
                    .binding
                    .clone()
                    .ok_or_else(|| RpcError::new(-32602, "opencode actions need a binding"))?;
                let b = self.binding(&id)?;
                let session = b.session_id()?;
                if p.action == "compact" {
                    b.client
                        .request(
                            "POST",
                            &format!("/api/session/{session}/compact"),
                            Some(&json!({})),
                        )
                        .await
                        .map_err(|e| {
                            RpcError::new(-32603, format!("opencode refused the compaction: {e}"))
                        })?;
                } else {
                    let text = match (&p.args, &p.input) {
                        (Some(args), _) if !args.trim().is_empty() => args.trim().to_owned(),
                        (_, Some(input)) => input
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        _ => String::new(),
                    };
                    let body = json!({"name": p.action.trim_start_matches('/'), "text": text});
                    b.client
                        .request("POST", &format!("/api/session/{session}/command"), Some(&body))
                        .await
                        .map_err(|e| RpcError::new(-32603, format!("opencode refused the command: {e}")))?;
                }
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
                    self.emit(vec![state_update(&b.id, State::Stopped, None)]);
                }
                Ok(json!({}))
            }
            _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
        }
    }

    /// Switches a binding's model and remembers it.
    async fn switch_model(&self, b: &Arc<Binding>, model: &str) -> Result<(), RpcError> {
        let reference = wire::model_ref(model)
            .ok_or_else(|| RpcError::new(-32602, format!("model {model} is not provider/id")))?;
        let session = b.session_id()?;
        b.client
            .request(
                "POST",
                &format!("/api/session/{session}/model"),
                Some(&json!({"model": reference})),
            )
            .await
            .map_err(|e| RpcError::new(-32603, format!("opencode refused the model: {e}")))?;
        b.state.lock().unwrap().model = Some(model.to_owned());
        Ok(())
    }

    /// Composes the text and files for a prompt and sends them to opencode.
    async fn prompt(
        &self,
        b: &Arc<Binding>,
        p: &BindingPromptParams,
    ) -> Result<BindingDisposition, RpcError> {
        let (text, files, notices) = compose(p, &b.state.lock().unwrap());
        let running = b.state.lock().unwrap().turn.is_some();
        let disposition = if running && p.steer {
            BindingDisposition::Steered
        } else {
            BindingDisposition::Started
        };
        let updates = {
            let mut state = b.state.lock().unwrap();
            let mut updates = state.prompt(p, disposition, &now());
            updates.splice(0..0, notices);
            updates
        };
        self.emit(updates);
        let body = wire::prompt_body(&text, files, disposition == BindingDisposition::Steered);
        let session = b.session_id()?;
        match b
            .client
            .request("POST", &format!("/api/session/{session}/prompt"), Some(&body))
            .await
        {
            Ok(_) => Ok(disposition),
            Err(e) => {
                let text = format!("opencode did not take the prompt: {e}");
                let updates = b.state.lock().unwrap().fail(text.clone(), &now());
                self.emit(updates);
                Err(RpcError::new(-32603, text))
            }
        }
    }

    async fn start(self: &Arc<Self>, start: BindingStart) -> Result<Value, RpcError> {
        let mut state = MapState::new(start.binding.clone(), self.config.data.clone());
        if let Some(r) = &start.resume {
            state.session_id = Some(r.id.clone());
        }
        let port = free_port().map_err(|e| RpcError::new(-32603, format!("no free port: {e}")))?;
        let password = format!(
            "parrot-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let mut cmd = if self.config.guard {
            parrot_guard::guarded_command(&self.config.bin)
        } else {
            let mut c = tokio::process::Command::new(&self.config.bin);
            #[cfg(unix)]
            c.process_group(0);
            c
        };
        cmd.args(&self.config.args)
            .arg("--port")
            .arg(port.to_string())
            .envs(self.config.env.iter().cloned())
            .env(SERVER_PASSWORD_ENV, &password)
            .current_dir(&start.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(mcp) = &start.mcp {
            cmd.env(MCP_TOKEN_ENV, &mcp.token);
            if let Some(config) = mcp_config(mcp) {
                cmd.env(CONFIG_ENV, config);
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::new(-32603, format!("cannot start opencode: {e}")))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let authority = match listen(stdout).await {
            Ok((authority, drain)) => {
                tokio::spawn(drain);
                authority
            }
            Err(e) => {
                let _ = child.kill().await;
                return Err(RpcError::new(-32603, format!("opencode did not start: {e}")));
            }
        };
        let client = Client::new(authority.clone()).with_password(password.clone());
        let models = client
            .request("GET", "/api/model", None)
            .await
            .map(|v| wire::models(&v))
            .unwrap_or_default();
        let providers = client
            .request("GET", "/api/provider", None)
            .await
            .map(|v| wire::providers(&v))
            .unwrap_or_default();
        let commands = client
            .request("GET", "/api/command", None)
            .await
            .map(|v| wire::commands(&v))
            .unwrap_or_default();
        let session = match &start.resume {
            Some(r)
                if client
                    .request("GET", &format!("/api/session/{}", r.id), None)
                    .await
                    .is_ok() =>
            {
                r.id.clone()
            }
            _ => {
                let mut body = json!({});
                if let Some(model) = start.options.get("model").and_then(Value::as_str)
                    && let Some(reference) = wire::model_ref(model)
                {
                    body["model"] = reference;
                }
                let created = client
                    .request("POST", "/api/session", Some(&body))
                    .await
                    .map_err(|e| RpcError::new(-32603, format!("opencode refused the session: {e}")))?;
                created
                    .get("data")
                    .and_then(|d| d.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| RpcError::new(-32603, "opencode made no session"))?
            }
        };
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            client: client.clone(),
            child: tokio::sync::Mutex::new(child),
            state: Mutex::new(state),
            stopping: AtomicBool::new(false),
            events: tokio::sync::Mutex::default(),
            raised: Notify::new(),
        });
        self.bindings
            .lock()
            .unwrap()
            .insert(start.binding.clone(), b.clone());
        // Subscribe before reporting the open, so no event of the first turn is lost.
        match client.events().await {
            Ok(events) => {
                let driver = self.clone();
                let binding = b.clone();
                let task = tokio::spawn(read_events(driver, binding, events));
                *b.events.lock().await = Some(task);
            }
            Err(e) => {
                self.bindings.lock().unwrap().remove(&start.binding);
                b.stop().await;
                return Err(RpcError::new(-32603, format!("cannot follow opencode: {e}")));
            }
        }
        let updates = b
            .state
            .lock()
            .unwrap()
            .initialized(&session, &models, &providers, &commands);
        self.emit(updates);
        let result = {
            let state = b.state.lock().unwrap();
            BindingStartResult {
                reference: start.resume.clone().or_else(|| {
                    state.session_id.clone().map(|id| parrot_proto::session::Ref {
                        id,
                        strength: parrot_proto::session::RefStrength::Strong,
                    })
                }),
                isolation: Isolation::None,
                level: state.level(),
                network: state.network(),
                options: state.options(),
                actions: state.actions.clone(),
            }
        };
        self.emit(vec![state_update(&start.binding, State::Idle, None)]);
        Ok(to_value(&result))
    }
}

impl Binding {
    fn session_id(&self) -> Result<String, RpcError> {
        self.state
            .lock()
            .unwrap()
            .session_id
            .clone()
            .ok_or_else(|| RpcError::new(-32000, "opencode has no session"))
    }

    async fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(task) = self.events.lock().await.take() {
            task.abort();
        }
        let mut child = self.child.lock().await;
        // The guard forwards SIGTERM to the server's process group; a SIGKILL would leave the server
        // running because only the guard knows that group.
        if let Some(pid) = child.id() {
            #[cfg(unix)]
            // SAFETY: kill has no memory effects.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        if let Ok(Ok(_)) = tokio::time::timeout(STOP_GRACE, child.wait()).await {
            return;
        }
        let Some(pid) = child.id() else { return };
        parrot_guard::kill_tree(pid);
        let _ = child.kill().await;
    }
}

/// Waits for the address opencode prints, and keeps draining its output after that.
async fn listen(stdout: ChildStdout) -> Result<(String, impl std::future::Future<Output = ()>), String> {
    let mut reader = BufReader::new(stdout);
    let url = tokio::time::timeout(START_TIMEOUT, async {
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return Err("the server exited before it listened".to_owned());
            }
            if let Some(url) = line.trim().strip_prefix("server listening on ") {
                return Ok(url.to_owned());
            }
        }
    })
    .await
    .map_err(|_| "no address within the start timeout".to_owned())??;
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(&url)
        .trim_end_matches('/')
        .to_owned();
    if authority.is_empty() {
        return Err("the server printed no address".to_owned());
    }
    let drain = async move {
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            line.clear();
        }
    };
    Ok((authority, drain))
}

/// Reads the event stream until it ends, then reports opencode as lost unless it is stopping.
async fn read_events(driver: Arc<Driver>, b: Arc<Binding>, mut events: crate::http::EventStream) {
    while let Some(value) = events.next_event().await {
        let Some(event) = wire::event(&value) else {
            continue;
        };
        let raised = event.kind == "permission.asked";
        let updates = b.state.lock().unwrap().step(&event, &now());
        driver.emit(updates);
        if raised {
            b.raised.notify_one();
        }
    }
    if !b.stopping.load(Ordering::Relaxed) {
        let updates = b.state.lock().unwrap().exited(&now());
        driver.emit(updates);
        driver.emit(vec![state_update(
            &b.id,
            State::Lost,
            Some("opencode exited".into()),
        )]);
        driver.bindings.lock().unwrap().remove(&b.id);
    }
}

/// What a prompt becomes for opencode: its text with the context, the files opencode takes, and a
/// notice for every attachment it cannot take.
fn compose(p: &BindingPromptParams, state: &MapState) -> (String, Vec<Value>, Vec<DriverUpdate>) {
    let mut parts: Vec<String> = Vec::new();
    for entry in p.context.iter().flatten() {
        parts.push(format!(
            "<context uri=\"{}\">\n{}\n</context>",
            entry.uri, entry.text
        ));
    }
    parts.push(p.text.clone());
    let takes_images = state
        .model
        .as_ref()
        .and_then(|key| state.models.iter().find(|m| &m.key == key))
        .map(|m| m.images)
        .unwrap_or(true);
    let mut files = Vec::new();
    let mut notices = Vec::new();
    for attachment in p.attachments.iter().flatten() {
        let image = attachment.media_type.starts_with("image/");
        let data = attachment.data.clone().or_else(|| {
            attachment
                .uri
                .as_ref()
                .and_then(|uri| uri.split_once("base64,").map(|(_, data)| data.to_owned()))
        });
        let reason = if image && !takes_images {
            Some("the selected model does not take images".to_owned())
        } else if data.is_none() {
            Some("it carries no data".to_owned())
        } else {
            None
        };
        let Some(reason) = reason else {
            let data = data.unwrap_or_default();
            files.push(json!({
                "uri": wire::data_uri(&attachment.media_type, &data),
                "name": attachment.name,
            }));
            continue;
        };
        notices.push(state.notice_update(
            parrot_proto::subscribe::NoticeLevel::Warning,
            format!(
                "The attachment {} ({}) was not sent: {reason}.",
                attachment.name, attachment.media_type
            ),
            "attachment_unsupported",
        ));
    }
    (parts.join("\n\n"), files, notices)
}

/// The inline config opencode reads from `OPENCODE_CONFIG_CONTENT`, with the token left to the
/// environment so it never appears in the config or in argv.
fn mcp_config(mcp: &parrot_proto::driver::McpInjection) -> Option<String> {
    let server = match (&mcp.url, &mcp.command) {
        (Some(url), _) => json!({
            "type": "remote",
            "url": url,
            "headers": {"Authorization": format!("Bearer {{env:{MCP_TOKEN_ENV}}}")},
        }),
        (None, Some(command)) if !command.is_empty() => json!({
            "type": "local",
            "command": command,
            "environment": {MCP_TOKEN_ENV: format!("{{env:{MCP_TOKEN_ENV}}}")},
        }),
        _ => return None,
    };
    Some(json!({"mcp": {"servers": {"parrot": server}}}).to_string())
}

/// A port nothing listens on now, for the server to take.
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
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

/// The `opencode` agent as `agent/list` reports it.
pub fn agent(data: &DriverData) -> Agent {
    let mut features = Map::new();
    features.insert("session.stream".into(), json!({}));
    features.insert("session.resume".into(), json!({}));
    features.insert("session.steer".into(), json!({"when": "next_step"}));
    features.insert("attachments".into(), json!({"kinds": ["image", "file"]}));
    features.insert("functions.mcp".into(), json!({}));
    features.insert(
        "usage".into(),
        json!({"cost": true, "context": true, "rateLimits": false}),
    );
    Agent {
        id: parrot_proto::driver_data::agent_id("opencode"),
        env: String::new(),
        name: parrot_proto::driver_data::agent_name("opencode"),
        driver: AgentDriver {
            name: "opencode".into(),
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
        levels: if data.levels.is_empty() {
            vec![PermissionLevel::Host]
        } else {
            data.levels.clone()
        },
        isolation: vec![Isolation::None],
        options: MapState::new("", data.clone()).options(),
        actions: vec![],
        meta: None,
        extra: Map::new(),
    }
}

/// The current time in RFC 3339, UTC, to the millisecond.
pub fn now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
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
