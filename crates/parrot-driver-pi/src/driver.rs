//! The pi driver: the driver protocol (spec 30) as JSON-RPC lines, one `pi --mode rpc` process per
//! binding.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parrot_proto::action::ActionStatus;
use parrot_proto::agent::{Agent, AgentDriver, AgentListResult, AuthStatus, Control, DriverKind};
use parrot_proto::driver::{
    BindingActionParams, BindingActionResult, BindingAnswerParams, BindingCancelParams, BindingCancelResult,
    BindingDisposition, BindingForkParams, BindingForkResult, BindingOptionsParams, BindingOptionsResult,
    BindingPromptParams, BindingPromptResult, BindingRef, BindingStart, BindingStartResult, DriverUpdate,
    DriverUpdateKind, HistoryDiscoverParams, HistoryReadParams,
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
use tokio::sync::{Notify, mpsc, oneshot};

use crate::data::{DriverData, Sources};
use crate::history;
use crate::mapping::MapState;
use crate::wire::{self, Command, Image, Inbound};

/// The variable that names the `pi` binary.
pub const PI_BIN_ENV: &str = "PARROT_PI_BIN";
/// The variable pi expands in its MCP headers, so the token is never in argv or the file.
pub const MCP_TOKEN_ENV: &str = "PARROT_MCP_TOKEN";
/// How long pi has to answer a command.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long pi may take to compact the context.
pub const COMPACT_TIMEOUT: Duration = Duration::from_secs(600);
/// How long `binding/stop` waits after closing stdin before killing pi.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How the driver starts pi.
#[derive(Debug, Clone)]
pub struct Config {
    pub pi_bin: OsString,
    /// Arguments before any the driver adds; `--mode rpc` for the real CLI.
    pub args: Vec<OsString>,
    /// Extra environment for the CLI.
    pub env: Vec<(OsString, OsString)>,
    /// Start the CLI through `parrot guard --`, which needs the current executable to be `parrot`.
    pub guard: bool,
    /// The driver data, from `$PARROT_DRIVER_DATA` or compiled in.
    pub data: DriverData,
}

impl Config {
    /// `PARROT_PI_BIN` or `pi`, guarded when running as `parrot`.
    pub fn from_env() -> Self {
        let guard = parrot_guard::can_guard();
        let mut data = DriverData::load(builtin);
        data.fill(default_data());
        Config {
            pi_bin: std::env::var_os(PI_BIN_ENV).unwrap_or_else(|| "pi".into()),
            args: vec!["--mode".into(), "rpc".into()],
            env: vec![],
            guard,
            data,
        }
    }
}

/// The texts compiled into the driver, used when `$PARROT_DRIVER_DATA` is not set.
pub fn builtin() -> Sources {
    Sources {
        options: Some(include_str!("../../../eggs/pi/parrot/options.kdl").to_owned()),
        levels: Some(include_str!("../../../eggs/pi/parrot/levels.kdl").to_owned()),
        correlate: Some(include_str!("../../../eggs/pi/parrot/correlate.kdl").to_owned()),
    }
}

/// Values the driver needs even when a data file leaves them out.
pub fn default_data() -> DriverData {
    let mut data = DriverData {
        levels: vec![PermissionLevel::Host],
        ..Default::default()
    };
    // pi itself asks nothing before it runs a tool; its extensions can.
    for tool in ["read", "grep", "list", "bash", "write", "edit", "Pi extension"] {
        let level = match tool {
            "read" | "grep" | "list" => PermissionLevel::Read,
            "write" | "edit" => PermissionLevel::Write,
            _ => PermissionLevel::Host,
        };
        data.tools.insert(tool.into(), level);
    }
    data.labels.insert("model".into(), "Model".into());
    data.labels.insert("effort".into(), "Thinking".into());
    data
}

type Pending = Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>;

struct Binding {
    id: String,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Child>,
    state: Mutex<MapState>,
    pending: Pending,
    next: AtomicU64,
    stopping: AtomicBool,
    /// Signalled when pi raises a dialog, so a start can return on it (Appendix E4).
    raised: Notify,
    /// The folder with the `mcp.json` this binding started pi with.
    mcp_dir: Option<PathBuf>,
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
                "peer": {"kind": "driver", "name": "parrot-driver-pi", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![probed(&self.config.data, &self.config.pi_bin).await],
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
                let interrupted = b.command(Command::new("", "abort"), None).await.is_ok();
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
                b.write_value(&line).await?;
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
                            let (provider, id) = model.split_once('/').ok_or_else(|| {
                                RpcError::new(-32602, format!("model {model} is not provider/id"))
                            })?;
                            b.command(
                                Command::new("", "set_model")
                                    .with("provider", json!(provider))
                                    .with("modelId", json!(id)),
                                None,
                            )
                            .await
                            .map_err(|e| RpcError::new(-32603, format!("pi refused the model: {e}")))?;
                            b.state.lock().unwrap().model = Some(model);
                        }
                        "effort" => {
                            let level = text.unwrap_or_else(|| "off".into());
                            b.command(
                                Command::new("", "set_thinking_level").with("level", json!(level)),
                                None,
                            )
                            .await
                            .map_err(|e| RpcError::new(-32603, format!("pi refused the level: {e}")))?;
                            let mut state = b.state.lock().unwrap();
                            state.current_thinking = (level != "off").then_some(level);
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
            "binding/action" => {
                let p: BindingActionParams = params(p)?;
                let id = p
                    .binding
                    .clone()
                    .ok_or_else(|| RpcError::new(-32602, "pi actions need a binding"))?;
                let b = self.binding(&id)?;
                if p.action == "compact" {
                    let mut command = Command::new("", "compact");
                    let instructions = p
                        .args
                        .as_deref()
                        .map(str::trim)
                        .filter(|a| !a.is_empty())
                        .map(str::to_owned)
                        .or_else(|| {
                            p.input
                                .as_ref()
                                .and_then(|i| i.get("note"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        });
                    if let Some(instructions) = instructions {
                        command = command.with("customInstructions", json!(instructions));
                    }
                    let answer = b.command(command, Some(COMPACT_TIMEOUT)).await.map_err(|e| {
                        RpcError::new(-32020, format!("pi refused the compaction: {e}"))
                    })?;
                    let updates = b.state.lock().unwrap().compacted(&answer, "manual");
                    self.emit(updates);
                    return Ok(to_value(&BindingActionResult {
                        status: ActionStatus::Done,
                        request: None,
                        result: Some(answer),
                    }));
                }
                let text = match (&p.args, &p.input) {
                    (Some(args), _) if !args.trim().is_empty() => {
                        format!("/{} {}", p.action, args.trim())
                    }
                    (_, Some(input)) => match input.get("note").and_then(Value::as_str) {
                        Some(note) if !note.trim().is_empty() => {
                            format!("/{} {}", p.action, note.trim())
                        }
                        _ => format!("/{}", p.action),
                    },
                    _ => format!("/{}", p.action),
                };
                let message = p
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("action-{}", b.next.fetch_add(1, Ordering::Relaxed)));
                let prompt = BindingPromptParams {
                    binding: id,
                    subagent: None,
                    message,
                    text: text.clone(),
                    context: None,
                    attachments: None,
                    steer: false,
                };
                self.prompt(&b, &prompt).await?;
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
            "binding/fork" => {
                let p: BindingForkParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let text = {
                    let state = b.state.lock().unwrap();
                    match &p.at {
                        Some(turn) => Some(
                            state
                                .turn_input_text(turn)
                                .ok_or_else(|| {
                                    RpcError::new(-32001, "pi forks only at a turn it has seen")
                                })?
                                .to_owned(),
                        ),
                        None => None,
                    }
                };
                let messages: wire::ForkMessages = b
                    .command_json(Command::new("", "get_fork_messages"), None)
                    .await
                    .map_err(|e| RpcError::new(-32020, format!("pi listed no forks: {e}")))?
                    .unwrap_or_default();
                let entry = match text {
                    Some(text) => messages
                        .messages
                        .iter()
                        .rev()
                        .find(|m| m.text == text)
                        .cloned()
                        .ok_or_else(|| {
                            RpcError::new(-32002, "pi has no message for that turn any more")
                        })?,
                    None => messages.messages.last().cloned().ok_or_else(|| {
                        RpcError::new(-32004, "pi has no message to fork from")
                    })?,
                };
                b.command(
                    Command::new("", "fork").with("entryId", json!(entry.entry_id)),
                    None,
                )
                .await
                .map_err(|e| RpcError::new(-32020, format!("pi refused the fork: {e}")))?;
                let state: wire::SessionState = b
                    .command_json(Command::new("", "get_state"), None)
                    .await
                    .map_err(|e| RpcError::new(-32020, format!("pi has no session after the fork: {e}")))?
                    .unwrap_or_default();
                let id = state
                    .session_id
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| RpcError::new(-32020, "pi did not name the forked session"))?;
                Ok(to_value(&BindingForkResult {
                    reference: parrot_proto::session::Ref {
                        id,
                        strength: parrot_proto::session::RefStrength::Strong,
                    },
                }))
            }
            "history/discover" => {
                let p: HistoryDiscoverParams = params(p)?;
                let root = history::root().ok_or_else(|| RpcError::new(-32603, "no home folder"))?;
                Ok(to_value(&history::discover(&root, &p)))
            }
            "history/read" => {
                let p: HistoryReadParams = params(p)?;
                let root = history::root().ok_or_else(|| RpcError::new(-32603, "no home folder"))?;
                let path = history::find(&root, &p.reference.id).ok_or_else(|| {
                    RpcError::new(-32602, format!("no session file for {}", p.reference.id))
                })?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| RpcError::new(-32603, format!("cannot read the session: {e}")))?;
                Ok(to_value(&history::read(&text, p.cursor.as_deref())))
            }
            _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
        }
    }

    /// Composes the text and images for a prompt and sends them to pi.
    async fn prompt(
        &self,
        b: &Arc<Binding>,
        p: &BindingPromptParams,
    ) -> Result<BindingDisposition, RpcError> {
        let (message, images, notices) = compose(p, &b.state.lock().unwrap());
        let running = b.state.lock().unwrap().turn.is_some();
        let disposition = if running && p.steer {
            BindingDisposition::Steered
        } else {
            BindingDisposition::Started
        };
        let command = if disposition == BindingDisposition::Steered {
            Command::new("", "steer").with("message", json!(message))
        } else if running {
            Command::new("", "prompt")
                .with("message", json!(message))
                .with("streamingBehavior", json!("followUp"))
        } else {
            Command::new("", "prompt").with("message", json!(message))
        };
        let command = if images.is_empty() {
            command
        } else {
            command.with("images", json!(images))
        };
        // Report the user item and the turn first, so nothing pi sends can pre-empt the turn's input.
        let updates = {
            let mut state = b.state.lock().unwrap();
            let mut updates = state.prompt(p, disposition, &now());
            updates.splice(0..0, notices);
            updates
        };
        self.emit(updates);
        match b.command(command, None).await {
            Ok(answer) => {
                let handled = answer
                    .get("disposition")
                    .and_then(Value::as_str)
                    .map(|d| d == "handled")
                    .unwrap_or(false);
                if handled {
                    let updates = b.state.lock().unwrap().handled(&p.message, &now());
                    self.emit(updates);
                    return Ok(BindingDisposition::Handled);
                }
                Ok(disposition)
            }
            Err(e) => {
                let text = format!("pi did not take the prompt: {e}");
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
        let mut args: Vec<OsString> = self.config.args.clone();
        if let Some(model) = start.options.get("model").and_then(Value::as_str) {
            args.extend(["--model".into(), model.into()]);
        }
        if let Some(effort) = start.options.get("effort").and_then(Value::as_str) {
            args.extend(["--thinking".into(), effort.into()]);
        }
        if let Some(r) = &start.resume {
            args.extend(["--session-id".into(), r.id.clone().into()]);
        }
        let mut cmd = if self.config.guard {
            parrot_guard::guarded_command(&self.config.pi_bin)
        } else {
            let mut c = tokio::process::Command::new(&self.config.pi_bin);
            #[cfg(unix)]
            c.process_group(0);
            c
        };
        let mut mcp_dir = None;
        if let Some(mcp) = &start.mcp
            && let Some(dir) = write_mcp(&start.binding, mcp)
        {
            cmd.env("PI_CODING_AGENT_DIR", &dir)
                .env(MCP_TOKEN_ENV, &mcp.token);
            mcp_dir = Some(dir);
        }
        cmd.args(&args)
            .envs(self.config.env.iter().cloned())
            .current_dir(&start.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::new(-32603, format!("cannot start pi: {e}")))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(child),
            state: Mutex::new(state),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            raised: Notify::new(),
            mcp_dir,
        });
        self.bindings
            .lock()
            .unwrap()
            .insert(start.binding.clone(), b.clone());
        tokio::spawn(read_loop(self.clone(), b.clone(), stdout));

        let opening = tokio::spawn(open(self.clone(), b.clone()));
        let raised = b.raised.notified();
        tokio::pin!(raised);
        tokio::select! {
            r = opening => {
                if let Err(e) = r.map_err(|e| RpcError::new(-32603, format!("starting pi: {e}")))? {
                    self.bindings.lock().unwrap().remove(&start.binding);
                    b.stop().await;
                    return Err(RpcError::new(-32603, format!("pi did not open: {e}")));
                }
            }
            // A start-time dialog: the start returns now and pi opens once it is answered.
            _ = &mut raised => {}
        }
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

/// What a prompt becomes for pi: its text with the context, the images pi takes, and a notice for
/// every attachment pi cannot take.
fn compose(p: &BindingPromptParams, state: &MapState) -> (String, Vec<Image>, Vec<DriverUpdate>) {
    let mut parts: Vec<String> = Vec::new();
    for entry in p.context.iter().flatten() {
        parts.push(format!(
            "<context uri=\"{}\">\n{}\n</context>",
            entry.uri, entry.text
        ));
    }
    parts.push(p.text.clone());
    let mut images = Vec::new();
    let mut notices = Vec::new();
    let model = state
        .model
        .as_ref()
        .and_then(|key| state.models.iter().find(|m| &m.key() == key));
    for attachment in p.attachments.iter().flatten() {
        let image = attachment.media_type.starts_with("image/");
        let takes_images = model.map(|m| m.takes("image")).unwrap_or(true);
        let data = attachment.data.clone().or_else(|| match &attachment.uri {
            Some(uri) => uri
                .strip_prefix("data:")
                .and_then(|rest| rest.split_once("base64,").map(|(_, data)| data.to_owned())),
            None => None,
        });
        if let (true, true, Some(data)) = (image, takes_images, data) {
            let mut image = Image::png(data);
            image.mime_type = attachment.media_type.clone();
            images.push(image);
            continue;
        }
        let reason = if !image {
            "pi takes images only".to_owned()
        } else if !takes_images {
            "the selected model does not take images".to_owned()
        } else {
            "it has no data".to_owned()
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
    (parts.join("\n\n"), images, notices)
}

/// Writes the `mcp.json` pi reads from `$PI_CODING_AGENT_DIR`, with the token left to the environment.
fn write_mcp(binding: &str, mcp: &parrot_proto::driver::McpInjection) -> Option<PathBuf> {
    let server = match (&mcp.url, &mcp.command) {
        (Some(url), _) => json!({
            "url": url,
            "headers": {"Authorization": format!("Bearer ${{{MCP_TOKEN_ENV}}}")},
        }),
        (None, Some(command)) if !command.is_empty() => json!({
            "command": command[0],
            "args": &command[1..],
            "env": {MCP_TOKEN_ENV: format!("${{{MCP_TOKEN_ENV}}}")},
        }),
        _ => return None,
    };
    let safe: String = binding
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = std::env::temp_dir().join(format!("parrot-pi-{}-{safe}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join("mcp.json");
    let text = json!({"mcpServers": {"parrot": server}}).to_string();
    std::fs::write(&file, text).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    Some(dir)
}

/// What pi reports when it opens: its session, models, thinking levels and commands.
async fn open(driver: Arc<Driver>, b: Arc<Binding>) -> Result<(), String> {
    let state: wire::SessionState = b
        .command_json(Command::new("", "get_state"), None)
        .await?
        .unwrap_or_default();
    let models: wire::ModelList = b
        .command_json(Command::new("", "get_available_models"), None)
        .await
        .unwrap_or_default()
        .unwrap_or_default();
    let levels: wire::LevelList = b
        .command_json(Command::new("", "get_available_thinking_levels"), None)
        .await
        .unwrap_or_default()
        .unwrap_or_default();
    let commands: wire::CommandList = b
        .command_json(Command::new("", "get_commands"), None)
        .await
        .unwrap_or_default()
        .unwrap_or_default();
    let updates =
        b.state
            .lock()
            .unwrap()
            .initialized(&state, &models.models, &levels.levels, &commands.commands);
    driver.emit(updates);
    Ok(())
}

impl Binding {
    async fn write_value(&self, value: &Value) -> Result<(), RpcError> {
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| RpcError::new(-32000, "pi has exited"))?;
        let line = format!("{value}\n");
        let res = async {
            pipe.write_all(line.as_bytes()).await?;
            pipe.flush().await
        };
        res.await
            .map_err(|e| RpcError::new(-32000, format!("cannot write to pi: {e}")))
    }

    /// Sends a command and waits for its answer.
    async fn command(&self, mut command: Command, timeout: Option<Duration>) -> Result<Value, String> {
        if command.id.is_empty() {
            command.id = format!("parrot-{}", self.next.fetch_add(1, Ordering::Relaxed));
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(command.id.clone(), tx);
        if let Err(e) = self.write_value(&json!(command)).await {
            self.pending.lock().unwrap().remove(&command.id);
            return Err(e.message);
        }
        match tokio::time::timeout(timeout.unwrap_or(REQUEST_TIMEOUT), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("pi exited".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&command.id);
                Err(format!("no answer to {}", command.r#type))
            }
        }
    }

    /// Sends a command whose answer carries data of a known shape.
    async fn command_json<T: DeserializeOwned>(
        &self,
        command: Command,
        timeout: Option<Duration>,
    ) -> Result<Option<T>, String> {
        let value = self.command(command, timeout).await?;
        if value.is_null() {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .map_err(|e| format!("unexpected answer: {e}"))
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
        if let Some(dir) = &self.mcp_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

async fn read_loop(driver: Arc<Driver>, b: Arc<Binding>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Some(parsed) = wire::parse_inbound(&line) else {
            continue;
        };
        match parsed {
            Open::Known(Inbound::Response(r)) => {
                let Some(key) = r.key() else { continue };
                let Some(tx) = b.pending.lock().unwrap().remove(&key) else {
                    continue;
                };
                let result = if r.success {
                    Ok(r.data.unwrap_or(Value::Object(Map::new())))
                } else {
                    Err(r.error.unwrap_or_else(|| "pi refused the command".into()))
                };
                let _ = tx.send(result);
            }
            other => {
                let value = match other {
                    Open::Known(Inbound::Event(_)) => {
                        serde_json::from_str::<Value>(&line).unwrap_or(Value::Null)
                    }
                    Open::Unknown(v) => v,
                    Open::Known(Inbound::Response(_)) => continue,
                };
                let updates = {
                    let mut state = b.state.lock().unwrap();
                    let raised = value.get("type").and_then(Value::as_str) == Some("extension_ui_request");
                    let updates = state.step(&value, &now());
                    (updates, raised)
                };
                driver.emit(updates.0);
                if updates.1 {
                    b.raised.notify_one();
                }
            }
        }
    }
    b.pending.lock().unwrap().clear();
    if !b.stopping.load(Ordering::Relaxed) {
        let updates = b.state.lock().unwrap().exited(&now());
        driver.emit(updates);
        driver.emit(vec![state_update(&b.id, State::Lost, Some("pi exited".into()))]);
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

/// The `pi` agent as `agent/list` reports it.
pub fn agent(data: &DriverData) -> Agent {
    let mut features = Map::new();
    features.insert("session.stream".into(), json!({}));
    features.insert("session.resume".into(), json!({}));
    features.insert("session.steer".into(), json!({"when": "next_step"}));
    features.insert("session.history".into(), json!({}));
    features.insert("session.fork".into(), json!({"at": ["turn"]}));
    features.insert("attachments".into(), json!({"kinds": ["image"]}));
    features.insert(
        "usage".into(),
        json!({"cost": true, "context": true, "rateLimits": false}),
    );
    let levels = if data.levels.is_empty() {
        vec![PermissionLevel::Host]
    } else {
        data.levels.clone()
    };
    Agent {
        id: parrot_proto::driver_data::agent_id("pi"),
        env: String::new(),
        name: parrot_proto::driver_data::agent_name("Pi"),
        driver: AgentDriver {
            name: "pi".into(),
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
        levels,
        isolation: vec![Isolation::None],
        options: MapState::new("", data.clone()).options(),
        actions: vec![],
        meta: None,
        extra: Map::new(),
    }
}

/// [`agent`] with the version the installed `pi` prints, when it answers.
pub async fn probed(data: &DriverData, bin: &std::ffi::OsStr) -> Agent {
    let mut a = agent(data);
    if let Some(version) = parrot_driver::probe::version(bin).await {
        a.harness = Some(parrot_proto::agent::HarnessInfo {
            version,
            verified: true,
        });
    }
    a
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
