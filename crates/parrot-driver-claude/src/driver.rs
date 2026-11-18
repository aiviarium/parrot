//! The Claude Code driver: the driver protocol (spec 30) as JSON-RPC lines, one `claude -p`
//! stream-json process per binding.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parrot_proto::agent::{Agent, AgentDriver, AgentListResult, AuthStatus, Control, DriverKind};
use parrot_proto::driver::{
    BindingAnswerParams, BindingCancelParams, BindingCancelResult, BindingForkParams, BindingForkResult,
    BindingOptionsParams, BindingOptionsResult, BindingPromptParams, BindingPromptResult, BindingRef,
    BindingStart, BindingStartResult, DriverUpdate, DriverUpdateKind, HistoryDiscoverParams,
    HistoryReadParams, McpInjection,
};
use parrot_proto::open::Open;
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::Isolation;
use parrot_proto::rpc::{Message, Notification, Response, RpcError};
use parrot_proto::session::{Ref, RefStrength};
use parrot_proto::state::State;
use parrot_proto::subscribe::NoticeLevel;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use crate::data::{data, load_error};
use crate::mapping::MapState;
use crate::wire::{self, ClaudeIn, ClaudeInMsg, ControlRequest, ControlRequestBody, Extra, User};
use crate::{frame, history, input};

/// The variable that names the `claude` binary.
pub const CLAUDE_BIN_ENV: &str = "PARROT_CLAUDE_BIN";
/// How long the CLI has to answer a control request.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `binding/stop` waits after closing stdin before killing the CLI.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How the driver starts Claude Code.
#[derive(Debug, Clone)]
pub struct Config {
    pub claude_bin: OsString,
    /// Extra environment for the CLI.
    pub env: Vec<(OsString, OsString)>,
    /// Start the CLI through `parrot guard --`, which needs the current executable to be `parrot`.
    pub guard: bool,
}

impl Config {
    /// `PARROT_CLAUDE_BIN` or `claude`, guarded when running as `parrot`.
    pub fn from_env() -> Self {
        let guard = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s == "parrot"))
            .unwrap_or(false);
        Config {
            claude_bin: std::env::var_os(CLAUDE_BIN_ENV).unwrap_or_else(|| "claude".into()),
            env: vec![],
            guard,
        }
    }
}

type Pending = Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>;

struct Binding {
    id: String,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Child>,
    state: Mutex<MapState>,
    pending: Pending,
    next: AtomicU64,
    stopping: std::sync::atomic::AtomicBool,
}

struct Driver {
    config: Config,
    out: mpsc::UnboundedSender<String>,
    bindings: Mutex<HashMap<String, Arc<Binding>>>,
    /// Sessions `binding/fork` returned, which the next start resumes as a fork.
    forks: Mutex<std::collections::HashSet<String>>,
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
        forks: Mutex::default(),
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
                "peer": {"kind": "driver", "name": "parrot-driver-claude", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![agent()],
            })),
            "binding/start" => self.start(params(p)?).await,
            "binding/fork" => {
                let p: BindingForkParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let state = b.state.lock().unwrap();
                if p.at.is_some()
                    && p.at != state.turn.as_ref().map(|t| t.key.clone())
                    && p.at != state.last_turn_key()
                {
                    return Err(RpcError::new(-32001, "Claude Code forks only at the latest turn"));
                }
                let id = state
                    .session_id
                    .clone()
                    .ok_or_else(|| RpcError::new(-32004, "the session has no harness id yet"))?;
                self.forks.lock().unwrap().insert(id.clone());
                // The runtime may start the fork in another driver process, so the ref itself
                // says it is a fork.
                Ok(to_value(&BindingForkResult {
                    reference: Ref {
                        id: format!("{FORK_PREFIX}{id}"),
                        strength: RefStrength::Strong,
                    },
                }))
            }
            "history/discover" => {
                let p: HistoryDiscoverParams = params(p)?;
                let root = history::projects_root().ok_or_else(|| RpcError::new(-32603, "no home folder"))?;
                Ok(to_value(&history::discover(&root, &p)))
            }
            "history/read" => {
                let p: HistoryReadParams = params(p)?;
                let root = history::projects_root().ok_or_else(|| RpcError::new(-32603, "no home folder"))?;
                let path = history::find(&root, &p.reference.id)
                    .ok_or_else(|| RpcError::new(-32602, format!("no transcript for {}", p.reference.id)))?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| RpcError::new(-32603, format!("cannot read the transcript: {e}")))?;
                Ok(to_value(&history::read(&text, p.cursor.as_deref())))
            }
            "binding/prompt" => {
                let p: BindingPromptParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let input = input::content(
                    &p.text,
                    p.context.as_deref().unwrap_or_default(),
                    p.attachments.as_deref().unwrap_or_default(),
                );
                let mut user = if input.is_plain() {
                    User::prompt(p.text.clone())
                } else {
                    User::blocks(input.blocks.clone())
                };
                let at = now();
                let (updates, disposition) = {
                    let mut state = b.state.lock().unwrap();
                    user.session_id = Some(state.session_id.clone().unwrap_or_default());
                    state.prompt(&p, &at)
                };
                let mut out = vec![frame::frame(&b.id, &at, frame::prompt_record(&p))];
                out.extend(updates);
                out.extend(input.skipped.iter().map(|(name, why)| {
                    notice(
                        &b.id,
                        NoticeLevel::Warning,
                        format!("attachment {name} was left out: {why}"),
                        "x.parrot/attachment",
                    )
                }));
                self.emit(out);
                b.write(&Open::Known(ClaudeInMsg::User(user))).await?;
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
                self.emit(vec![frame::frame(&b.id, &now(), json!({"cancel": {}}))]);
                if !running {
                    return Ok(to_value(&BindingCancelResult { interrupted: false }));
                }
                let body = ControlRequestBody::Interrupt { extra: Extra::new() };
                let interrupted = b.control("interrupt", body).await.is_ok();
                Ok(to_value(&BindingCancelResult { interrupted }))
            }
            "binding/answer" => {
                let p: BindingAnswerParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (line, updates) = b
                    .state
                    .lock()
                    .unwrap()
                    .answer(&p.request, &p.answer)
                    .map_err(|e| RpcError::new(-32602, e))?;
                // The updates go out before the answer reaches the CLI: a fast turn could
                // otherwise end before `running` is sent and leave the session `running`.
                let mut out = vec![frame::frame(
                    &b.id,
                    &now(),
                    frame::answer_record(&p.request, &p.answer),
                )];
                out.extend(updates);
                self.emit(out);
                b.write(&line).await?;
                Ok(json!({}))
            }
            "binding/options" => {
                let p: BindingOptionsParams = params(p)?;
                let b = self.binding(&p.binding)?;
                for (key, value) in &p.values {
                    match key.as_str() {
                        "model" => {
                            let model = value.as_str().map(str::to_owned);
                            let body = ControlRequestBody::SetModel {
                                model: model.clone(),
                                extra: Extra::new(),
                            };
                            b.control("model", body)
                                .await
                                .map_err(|e| RpcError::new(-32603, e))?;
                            b.state.lock().unwrap().model = model;
                        }
                        "permissionMode" => {
                            let mode: Open<wire::PermissionMode> = serde_json::from_value(value.clone())
                                .map_err(|e| RpcError::new(-32602, e.to_string()))?;
                            let body = ControlRequestBody::SetPermissionMode {
                                mode,
                                extra: Extra::new(),
                            };
                            b.control("mode", body)
                                .await
                                .map_err(|e| RpcError::new(-32603, e))?;
                            b.state.lock().unwrap().permission_mode = value.as_str().map(str::to_owned);
                        }
                        other => return Err(RpcError::new(-32602, format!("unknown option {other}"))),
                    }
                }
                self.emit(vec![frame::frame(&b.id, &now(), json!({"options": p.values}))]);
                let (result, update) = {
                    let state = b.state.lock().unwrap();
                    let result = BindingOptionsResult {
                        options: state.options(),
                        level: state.level(),
                        network: state.network(),
                    };
                    (result, state.options_update())
                };
                self.emit(vec![update]);
                Ok(to_value(&result))
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

    async fn start(self: &Arc<Self>, start: BindingStart) -> Result<Value, RpcError> {
        let mut args: Vec<OsString> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        if let Some(r) = &start.resume {
            let (id, fork) = match r.id.strip_prefix(FORK_PREFIX) {
                Some(id) => (id.to_owned(), true),
                None => (r.id.clone(), false),
            };
            args.extend(["--resume".into(), id.clone().into()]);
            if fork || self.forks.lock().unwrap().remove(&id) {
                args.push("--fork-session".into());
            }
        }
        let model = start
            .options
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(m) = &model {
            args.extend(["--model".into(), m.into()]);
        }
        let mode = start
            .options
            .get("permissionMode")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(m) = &mode {
            args.extend(["--permission-mode".into(), m.into()]);
        }
        if start.pin.is_some() {
            args.extend(data().levels.pin_args().into_iter().map(OsString::from));
        }
        let mut env = self.config.env.clone();
        if let Some(mcp) = &start.mcp {
            args.extend(["--mcp-config".into(), mcp_config(mcp).to_string().into()]);
            // The token reaches the CLI only through its environment; the config names the variable.
            env.push((MCP_TOKEN_ENV.into(), mcp.token.clone().into()));
        }
        let mut cmd = if self.config.guard {
            parrot_guard::guarded_command(&self.config.claude_bin)
        } else {
            let mut c = tokio::process::Command::new(&self.config.claude_bin);
            #[cfg(unix)]
            c.process_group(0);
            c
        };
        cmd.args(&args)
            .envs(env)
            .current_dir(&start.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::new(-32603, format!("cannot start claude: {e}")))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut state = MapState::new(start.binding.clone());
        state.model = model;
        state.permission_mode = mode;
        if let Some(r) = &start.resume {
            state.session_id = Some(r.id.clone());
        }
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(child),
            state: Mutex::new(state),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: Default::default(),
        });
        self.bindings
            .lock()
            .unwrap()
            .insert(start.binding.clone(), b.clone());
        tokio::spawn(read_loop(self.clone(), b.clone(), stdout));
        let body = ControlRequestBody::Initialize { extra: Extra::new() };
        let init = match b.control("init", body).await {
            Ok(v) => v,
            Err(e) => {
                self.bindings.lock().unwrap().remove(&start.binding);
                b.stop().await;
                return Err(RpcError::new(-32603, format!("claude did not initialize: {e}")));
            }
        };
        let mut notices = vec![frame::frame(&start.binding, &now(), json!({"init": init}))];
        if let Some(e) = load_error() {
            notices.push(notice(
                &start.binding,
                NoticeLevel::Warning,
                e.to_owned(),
                "x.parrot/driver-data",
            ));
        }
        self.emit(notices);
        let result = {
            let mut state = b.state.lock().unwrap();
            let mode = state.permission_mode.clone();
            state.initialized(&init);
            // A mode asked for at start wins over what the CLI reports before it applies it.
            if mode.is_some() {
                state.permission_mode = mode;
            }
            BindingStartResult {
                reference: start.resume.clone(),
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
    async fn write(&self, msg: &ClaudeIn) -> Result<(), RpcError> {
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| RpcError::new(-32000, "claude has exited"))?;
        let line = wire::to_line(msg) + "\n";
        let res = async {
            pipe.write_all(line.as_bytes()).await?;
            pipe.flush().await
        };
        res.await
            .map_err(|e| RpcError::new(-32000, format!("cannot write to claude: {e}")))
    }

    /// Sends a control request and waits for its response.
    async fn control(&self, prefix: &str, body: ControlRequestBody) -> Result<Value, String> {
        let id = format!("{prefix}-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let msg = Open::Known(ClaudeInMsg::ControlRequest(ControlRequest::new(id.clone(), body)));
        if let Err(e) = self.write(&msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e.message);
        }
        match tokio::time::timeout(CONTROL_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("claude exited".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("no answer to {id}"))
            }
        }
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
        let Ok(msg) = wire::parse_line(&line) else {
            continue;
        };
        // The frame and the updates share one time, so a rebuild from frames gives the same items.
        let at = now();
        if let Ok(raw) = serde_json::from_str::<Value>(&line) {
            driver.emit(vec![frame::frame(&b.id, &at, json!({"out": raw}))]);
        }
        match &msg {
            Open::Known(wire::ClaudeOutMsg::ControlResponse(r)) => {
                let Some(id) = r.request_id() else { continue };
                let Some(tx) = b.pending.lock().unwrap().remove(id) else {
                    continue;
                };
                let result = match &r.response {
                    Open::Known(wire::ControlResponseBody::Success { response, .. }) => {
                        Ok(response.clone().unwrap_or(Value::Object(Map::new())))
                    }
                    Open::Known(wire::ControlResponseBody::Error { error, .. }) => Err(error.clone()),
                    Open::Unknown(v) => Ok(v.clone()),
                };
                let _ = tx.send(result);
            }
            Open::Known(wire::ClaudeOutMsg::ControlRequest(req))
                if !matches!(req.request, Open::Known(ControlRequestBody::CanUseTool(_))) =>
            {
                // Hooks and SDK MCP servers are not registered, so other requests get an error.
                let resp = wire::ControlResponse::error(&req.request_id, "not supported by parrot");
                let _ = b.write(&Open::Known(ClaudeInMsg::ControlResponse(resp))).await;
            }
            _ => {
                let updates = b.state.lock().unwrap().step(&msg, &at);
                driver.emit(updates);
            }
        }
    }
    b.pending.lock().unwrap().clear();
    if !b.stopping.load(Ordering::Relaxed) {
        driver.emit(vec![state_update(
            &b.id,
            State::Lost,
            Some("claude exited".into()),
        )]);
        driver.bindings.lock().unwrap().remove(&b.id);
    }
}

/// The variable that carries the per-binding MCP token to the CLI.
pub const MCP_TOKEN_ENV: &str = "PARROT_MCP_TOKEN";

/// The `--mcp-config` for the injected `parrot` server (spec 26). It names the token's variable,
/// which Claude Code expands, so the token itself is never in argv.
pub fn mcp_config(mcp: &McpInjection) -> Value {
    let server = match (&mcp.url, &mcp.command) {
        (Some(url), _) => json!({
            "type": "http",
            "url": url,
            "headers": {"Authorization": format!("Bearer ${{{MCP_TOKEN_ENV}}}")},
        }),
        (None, Some(cmd)) => json!({
            "type": "stdio",
            "command": cmd.first().cloned().unwrap_or_default(),
            "args": cmd.iter().skip(1).collect::<Vec<_>>(),
            "env": {MCP_TOKEN_ENV: format!("${{{MCP_TOKEN_ENV}}}")},
        }),
        (None, None) => json!({}),
    };
    json!({"mcpServers": {"parrot": server}})
}

fn notice(binding: &str, level: NoticeLevel, text: String, code: &str) -> DriverUpdate {
    DriverUpdate {
        binding: binding.to_owned(),
        subagent: None,
        update: Open::Known(DriverUpdateKind::Notice {
            level,
            text,
            code: Some(code.to_owned()),
        }),
        extra: Map::new(),
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

/// Marks a ref from `binding/fork`: resuming it forks the harness session (`--fork-session`).
pub const FORK_PREFIX: &str = "fork:";

/// The `claude` agent as `agent/list` reports it.
pub fn agent() -> Agent {
    let mut features = Map::new();
    features.insert("session.stream".into(), json!({}));
    features.insert("session.resume".into(), json!({}));
    features.insert("session.steer".into(), json!({"when": "next_step"}));
    features.insert("usage".into(), json!({"cost": true, "context": true}));
    features.insert("attachments".into(), input::feature());
    features.insert("functions.mcp".into(), json!({}));
    features.insert("session.history".into(), json!({}));
    features.insert("session.fork".into(), json!({"at": ["turn"]}));
    Agent {
        id: "claude".into(),
        env: String::new(),
        name: "Claude Code".into(),
        driver: AgentDriver {
            name: "claude".into(),
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
        options: MapState::default().options(),
        actions: vec![],
        meta: None,
        extra: Map::new(),
    }
}

/// The current time in RFC 3339, UTC, to the millisecond.
pub fn now() -> String {
    rfc3339(SystemTime::now())
}

/// `t` in RFC 3339, UTC, to the millisecond.
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
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
