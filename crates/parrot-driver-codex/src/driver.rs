//! The Codex driver: the driver protocol (spec 30) as JSON-RPC lines, one `codex app-server`
//! process per binding.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::Isolation;
use parrot_proto::rpc::{Message as RpcMessage, Notification, Response, RpcError};
use parrot_proto::state::State;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::data::load_error;
use crate::mapping::{self, MapState};
use crate::rollout;
use crate::wire::{self, Message, RequestId, ServerNotification};
use crate::{frame, input};
use parrot_driver::correlate::{self, Fold};
use parrot_proto::driver::{BindingForkParams, BindingForkResult, HistoryDiscoverParams, HistoryReadParams};
use parrot_proto::session::{Ref, RefStrength};

/// The variable that names the `codex` binary.
pub const CODEX_BIN_ENV: &str = "PARROT_CODEX_BIN";
/// The variable the MCP server's bearer token reaches Codex in, so it is never in argv.
pub const MCP_TOKEN_ENV: &str = "PARROT_MCP_TOKEN";
/// How long Codex has to answer a request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `binding/stop` waits after closing stdin before killing Codex.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How the driver starts Codex.
#[derive(Debug, Clone)]
pub struct Config {
    pub codex_bin: OsString,
    /// Arguments before any the driver adds; `app-server` for the real CLI.
    pub args: Vec<OsString>,
    /// Extra environment for the CLI.
    pub env: Vec<(OsString, OsString)>,
    /// Start the CLI through `parrot guard --`, which needs the current executable to be `parrot`.
    pub guard: bool,
    /// Where Codex keeps its rollouts; `None` for `$CODEX_HOME/sessions` or `~/.codex/sessions`.
    pub sessions: Option<std::path::PathBuf>,
    /// Shadow mode (32.1): read the rollout beside the app-server and report differences.
    pub shadow: bool,
}

impl Config {
    /// `PARROT_CODEX_BIN` or `codex`, guarded when running as `parrot`.
    pub fn from_env() -> Self {
        let guard = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s == "parrot"))
            .unwrap_or(false);
        Config {
            codex_bin: std::env::var_os(CODEX_BIN_ENV).unwrap_or_else(|| "codex".into()),
            args: vec!["app-server".into()],
            env: vec![],
            guard,
            sessions: None,
            shadow: parrot_driver::correlate::shadow_from_env(),
        }
    }
}

/// Runs in the read loop when a request succeeds, before any later line is read, so updates for
/// the answer come before updates for what follows it.
type OnOk = Box<dyn FnOnce(&mut MapState, &Value) -> Vec<DriverUpdate> + Send>;

struct Waiting {
    tx: oneshot::Sender<Result<Value, wire::RpcError>>,
    on_ok: Option<OnOk>,
}

struct Binding {
    id: String,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Child>,
    state: Mutex<MapState>,
    waiting: Mutex<HashMap<RequestId, Waiting>>,
    next: AtomicU64,
    stopping: AtomicBool,
    /// Signalled when Codex raises a request, so a start can return on it (Appendix E4).
    raised: Notify,
    /// The items of this binding, kept for shadow mode.
    fold: Mutex<Fold>,
}

impl Binding {
    fn record(&self, shadow: bool, updates: &[DriverUpdate]) {
        if shadow {
            let mut f = self.fold.lock().unwrap();
            for u in updates {
                f.push(&to_value(u));
            }
        }
    }
}

/// Whether the updates end a turn.
fn ends_turn(updates: &[DriverUpdate]) -> bool {
    updates.iter().any(|u| {
        matches!(&u.update, Open::Known(DriverUpdateKind::Turn { turn }) if turn.status != parrot_proto::state::TurnStatus::Running)
    })
}

/// Shadow mode at the end of a turn: reads the rollout and reports where its last turn differs
/// from what the app-server said. Codex writes the rollout as it goes, so this waits briefly.
async fn shadow_turn(driver: Arc<Driver>, b: Arc<Binding>) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let Some(root) = driver.sessions() else {
        return;
    };
    let thread = b.state.lock().unwrap().thread_id.clone();
    let Some(thread) = thread else {
        return;
    };
    let notices: Vec<Value> =
        match rollout::find(&root, &thread).and_then(|p| std::fs::read_to_string(p).ok()) {
            None => vec![correlate::notice(
                &b.id,
                &correlate::Difference {
                    kind: "*".into(),
                    item: thread.clone(),
                    text: format!("no rollout for thread {thread}"),
                },
            )],
            Some(text) => {
                let mut disk = Fold::new();
                for u in rollout::updates(&b.id, &text) {
                    disk.push(&to_value(&u));
                }
                let live = b.fold.lock().unwrap().clone();
                let data = &crate::data::data().correlate;
                // Turn ids are the same in both sources, so the turn that ended is found by its id.
                let pairs =
                    correlate::correlate(data, &live.last_turn(), &disk.turn_or_last(live.last_turn_id()));
                correlate::differences(data, &pairs, "native", "transcript")
                    .iter()
                    .map(|d| correlate::notice(&b.id, d))
                    .collect()
            }
        };
    for n in notices {
        driver.send(&Notification::new("update", n));
    }
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
        let req = match serde_json::from_str::<RpcMessage>(&line) {
            Ok(RpcMessage::Request(req)) => req,
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

fn codex_error(e: wire::RpcError) -> RpcError {
    RpcError::new(-32603, format!("codex: {}", e.message))
}

impl Driver {
    fn sessions(&self) -> Option<std::path::PathBuf> {
        self.config.sessions.clone().or_else(rollout::sessions_root)
    }

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
                "peer": {"kind": "driver", "name": "parrot-driver-codex", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![agent()],
            })),
            "binding/start" => self.start(params(p)?).await,
            "binding/fork" => {
                let p: BindingForkParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (thread, latest) = {
                    let s = b.state.lock().unwrap();
                    (
                        s.thread_id.clone(),
                        s.turn.as_ref().map(|t| t.key.clone()).or(s.last_turn.clone()),
                    )
                };
                if p.at.is_some() && p.at != latest {
                    return Err(RpcError::new(-32001, "Codex forks only at the latest turn"));
                }
                let thread = thread.ok_or_else(|| RpcError::new(-32004, "the thread has not started yet"))?;
                let r = b
                    .request("thread/fork", json!({"threadId": thread}), None)
                    .await
                    .map_err(codex_error)?;
                let id = r
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(-32603, "codex thread/fork gave no thread id"))?;
                Ok(to_value(&BindingForkResult {
                    reference: Ref {
                        id: id.to_owned(),
                        strength: RefStrength::Strong,
                    },
                }))
            }
            "history/discover" => {
                let p: HistoryDiscoverParams = params(p)?;
                let root = self
                    .sessions()
                    .ok_or_else(|| RpcError::new(-32603, "no home folder"))?;
                Ok(to_value(&rollout::discover(&root, &p)))
            }
            "history/read" => {
                let p: HistoryReadParams = params(p)?;
                let root = self
                    .sessions()
                    .ok_or_else(|| RpcError::new(-32603, "no home folder"))?;
                let path = rollout::find(&root, &p.reference.id)
                    .ok_or_else(|| RpcError::new(-32602, format!("no rollout for {}", p.reference.id)))?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| RpcError::new(-32603, format!("cannot read the rollout: {e}")))?;
                Ok(to_value(&rollout::read(&text, p.cursor.as_deref())))
            }
            "binding/prompt" => self.prompt(params(p)?).await,
            "binding/cancel" => {
                let p: BindingCancelParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let target = {
                    let mut state = b.state.lock().unwrap();
                    state.cancel();
                    self.emit(vec![frame::frame(&b.id, &now(), json!({"cancel": {}}))]);
                    state
                        .turn
                        .as_ref()
                        .map(|t| (state.thread_id.clone(), t.key.clone()))
                };
                let Some((thread, turn)) = target else {
                    return Ok(to_value(&BindingCancelResult { interrupted: false }));
                };
                let r = b
                    .request(
                        "turn/interrupt",
                        json!({"threadId": thread, "turnId": turn}),
                        None,
                    )
                    .await;
                Ok(to_value(&BindingCancelResult {
                    interrupted: r.is_ok(),
                }))
            }
            "binding/answer" => {
                let p: BindingAnswerParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (msg, updates) = b
                    .state
                    .lock()
                    .unwrap()
                    .answer(&p.request, &p.answer)
                    .map_err(|e| RpcError::new(-32602, e))?;
                // The updates go out before the answer reaches Codex, so a fast turn can't end
                // before `running` is sent.
                let record = json!({"answer": {"request": p.request, "answer": p.answer}});
                let mut out = vec![frame::frame(&b.id, &now(), record)];
                out.extend(updates);
                self.emit(out);
                b.write(&msg).await?;
                Ok(json!({}))
            }
            "binding/options" => {
                let p: BindingOptionsParams = params(p)?;
                let b = self.binding(&p.binding)?;
                let (result, update) = {
                    let mut state = b.state.lock().unwrap();
                    let mut next = state.clone();
                    for (key, value) in &p.values {
                        let s = value.as_str().map(str::to_owned);
                        match key.as_str() {
                            "model" => next.model = s,
                            "effort" => next.effort = s,
                            "approvalPolicy" => {
                                next.approval = known(s, key)?;
                            }
                            "sandbox" => next.sandbox = known(s, key)?,
                            other => return Err(RpcError::new(-32602, format!("unknown option {other}"))),
                        }
                    }
                    *state = next;
                    self.emit(vec![frame::frame(&b.id, &now(), json!({"options": p.values}))]);
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
            "binding/action" => {
                let p: BindingActionParams = params(p)?;
                let id = p
                    .binding
                    .clone()
                    .ok_or_else(|| RpcError::new(-32602, "codex actions need a binding"))?;
                let b = self.binding(&id)?;
                let thread = b.state.lock().unwrap().thread_id.clone();
                match p.action.as_str() {
                    "compact" => {
                        b.state.lock().unwrap().compact_requested = true;
                        b.request("thread/compact/start", json!({"threadId": thread}), None)
                            .await
                            .map_err(codex_error)?;
                    }
                    "review" => {
                        let target = mapping::review_target(p.input.as_ref(), p.args.as_deref())
                            .map_err(|e| RpcError::new(-32602, e))?;
                        b.request(
                            "review/start",
                            json!({"threadId": thread, "target": target, "delivery": "inline"}),
                            None,
                        )
                        .await
                        .map_err(codex_error)?;
                    }
                    other => return Err(RpcError::new(-32602, format!("unknown action {other}"))),
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

    async fn prompt(self: &Arc<Self>, p: BindingPromptParams) -> Result<Value, RpcError> {
        let b = self.binding(&p.binding)?;
        let skipped = input::input(&p).skipped;
        self.emit(
            skipped
                .into_iter()
                .map(|(name, why)| {
                    notice(
                        &b.id,
                        format!("attachment {name} was left out: {why}"),
                        "x.parrot/attachment",
                    )
                })
                .collect(),
        );
        let steer = b.state.lock().unwrap().turn_steer_params(&p);
        if let Some(params) = steer {
            let prompt = p.clone();
            let on_ok: OnOk = Box::new(move |state, _| {
                let at = now();
                let d = BindingDisposition::Steered;
                let mut out = vec![frame::frame(
                    &state.binding,
                    &at,
                    frame::accepted_record(&prompt, None, d),
                )];
                out.extend(state.prompt_accepted(&prompt, None, d, &at));
                out
            });
            b.request("turn/steer", params, Some(on_ok))
                .await
                .map_err(|e| RpcError::new(-32001, format!("codex refused the steer: {}", e.message)))?;
            return Ok(to_value(&BindingPromptResult {
                disposition: BindingDisposition::Steered,
            }));
        }
        let params = b.state.lock().unwrap().turn_start_params(&p);
        let prompt = p.clone();
        let on_ok: OnOk = Box::new(move |state, result| {
            let turn = result.pointer("/turn/id").and_then(Value::as_str);
            let at = now();
            let d = BindingDisposition::Started;
            let mut out = vec![frame::frame(
                &state.binding,
                &at,
                frame::accepted_record(&prompt, turn, d),
            )];
            out.extend(state.prompt_accepted(&prompt, turn, d, &at));
            out
        });
        b.request("turn/start", params, Some(on_ok))
            .await
            .map_err(codex_error)?;
        Ok(to_value(&BindingPromptResult {
            disposition: BindingDisposition::Started,
        }))
    }

    async fn start(self: &Arc<Self>, start: BindingStart) -> Result<Value, RpcError> {
        let mut state = MapState::new(start.binding.clone());
        state.cwd = start.cwd.clone();
        let opt = |k: &str| start.options.get(k).and_then(Value::as_str).map(str::to_owned);
        state.model = opt("model").filter(|m| m != "default");
        state.effort = opt("effort");
        if let Some(a) = opt("approvalPolicy") {
            state.approval = known(Some(a), "approvalPolicy")?;
        }
        if let Some(s) = opt("sandbox") {
            state.sandbox = known(Some(s), "sandbox")?;
        }
        if let Some(pin) = &start.pin {
            state.apply_pin(pin);
        }

        let mut cmd = if self.config.guard {
            parrot_guard::guarded_command(&self.config.codex_bin)
        } else {
            let mut c = tokio::process::Command::new(&self.config.codex_bin);
            #[cfg(unix)]
            c.process_group(0);
            c
        };
        let mut config = Map::new();
        if let Some(mcp) = &start.mcp {
            cmd.env(MCP_TOKEN_ENV, &mcp.token);
            let server = match (&mcp.url, &mcp.command) {
                (Some(url), _) => json!({"url": url, "bearer_token_env_var": MCP_TOKEN_ENV}),
                (None, Some(c)) if !c.is_empty() => {
                    json!({"command": c[0], "args": &c[1..], "env_vars": [MCP_TOKEN_ENV]})
                }
                _ => Value::Null,
            };
            if !server.is_null() {
                config.insert("mcp_servers".into(), json!({"parrot": server}));
            }
        }
        cmd.args(&self.config.args)
            .envs(self.config.env.iter().cloned())
            .current_dir(&start.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::new(-32603, format!("cannot start codex: {e}")))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(child),
            state: Mutex::new(state),
            waiting: Mutex::default(),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            raised: Notify::new(),
            fold: Mutex::new(Fold::new()),
        });
        self.bindings
            .lock()
            .unwrap()
            .insert(start.binding.clone(), b.clone());
        tokio::spawn(read_loop(self.clone(), b.clone(), stdout));

        let opening = tokio::spawn(open_thread(
            self.clone(),
            b.clone(),
            start.clone(),
            Value::Object(config),
        ));
        let raised = b.raised.notified();
        tokio::pin!(raised);
        let reference = tokio::select! {
            r = opening => match r {
                Ok(Ok(reference)) => reference,
                Ok(Err(e)) => {
                    self.bindings.lock().unwrap().remove(&start.binding);
                    b.stop().await;
                    return Err(e);
                }
                Err(e) => return Err(RpcError::new(-32603, format!("starting codex: {e}"))),
            },
            // A start-time request: the start returns and the thread opens once it is answered.
            _ = &mut raised => start.resume.clone(),
        };
        let result = {
            let state = b.state.lock().unwrap();
            BindingStartResult {
                reference,
                isolation: state.isolation(),
                level: state.level(),
                network: state.network(),
                options: state.options(),
                actions: mapping::actions(),
            }
        };
        Ok(to_value(&result))
    }
}

/// `initialize`, the models, then a new thread or the old one. Returns the thread's ref.
async fn open_thread(
    driver: Arc<Driver>,
    b: Arc<Binding>,
    start: BindingStart,
    config: Value,
) -> Result<Option<parrot_proto::session::Ref>, RpcError> {
    let init = json!({
        "clientInfo": {"name": "parrot", "title": "Parrot", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi": true},
    });
    b.request("initialize", init, None)
        .await
        .map_err(|e| RpcError::new(-32603, format!("codex did not initialize: {}", e.message)))?;
    b.write(&Message::Notification {
        method: "initialized".into(),
        params: Value::Null,
    })
    .await?;
    let mut cursor: Option<String> = None;
    let mut list = wire::ModelList::default();
    for _ in 0..10 {
        let mut p = json!({"limit": 100});
        if let Some(c) = &cursor {
            p["cursor"] = json!(c);
        }
        // An older Codex without model/list keeps the options without a model list.
        let Ok(r) = b.request("model/list", p, None).await else {
            break;
        };
        let Ok(page) = serde_json::from_value::<wire::ModelList>(r) else {
            break;
        };
        list.data.extend(page.data);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let params = {
        driver.emit(vec![frame::frame(&b.id, &now(), json!({"models": list}))]);
        let mut state = b.state.lock().unwrap();
        state.models_listed(&list);
        let mut p = json!({
            "cwd": start.cwd,
            "approvalPolicy": state.approval,
            "sandbox": state.sandbox,
            "config": config,
        });
        if let Some(m) = &state.model {
            p["model"] = json!(m);
        }
        p
    };
    let (method, params) = match &start.resume {
        Some(r) => {
            let mut p = params;
            p["threadId"] = json!(r.id);
            p["excludeTurns"] = json!(true);
            ("thread/resume", p)
        }
        None => {
            let mut p = params;
            p["serviceName"] = json!("parrot");
            ("thread/start", p)
        }
    };
    let result = b
        .request(method, params, None)
        .await
        .map_err(|e| RpcError::new(-32603, format!("codex {method}: {}", e.message)))?;
    let mut updates = vec![frame::frame(&b.id, &now(), json!({"ready": result}))];
    if let Some(e) = load_error() {
        updates.push(notice(&b.id, e.to_owned(), "x.parrot/driver-data"));
    }
    updates.extend(b.state.lock().unwrap().thread_ready(&result));
    let reference = updates.iter().find_map(|u| match &u.update {
        Open::Known(DriverUpdateKind::Ref { reference }) => Some(reference.clone()),
        _ => None,
    });
    let options = b.state.lock().unwrap().options_update();
    driver.emit(updates);
    driver.emit(vec![options, state_update(&b.id, State::Idle, None)]);
    Ok(reference)
}

fn known(value: Option<String>, key: &str) -> Result<String, RpcError> {
    match value {
        Some(v) if mapping::is_choice(key, &v) => Ok(v),
        other => Err(RpcError::new(-32602, format!("{key} cannot be {other:?}"))),
    }
}

impl Binding {
    async fn write(&self, msg: &Message) -> Result<(), RpcError> {
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| RpcError::new(-32000, "codex has exited"))?;
        let line = msg.to_line() + "\n";
        let res = async {
            pipe.write_all(line.as_bytes()).await?;
            pipe.flush().await
        };
        res.await
            .map_err(|e| RpcError::new(-32000, format!("cannot write to codex: {e}")))
    }

    /// Sends a request and waits for its answer.
    async fn request(
        &self,
        method: &str,
        params: Value,
        on_ok: Option<OnOk>,
    ) -> Result<Value, wire::RpcError> {
        let id = RequestId::Str(format!("parrot-{}", self.next.fetch_add(1, Ordering::Relaxed)));
        let (tx, rx) = oneshot::channel();
        self.waiting
            .lock()
            .unwrap()
            .insert(id.clone(), Waiting { tx, on_ok });
        let msg = Message::Request {
            id: id.clone(),
            method: method.to_owned(),
            params,
        };
        let fail = |message: String| wire::RpcError {
            code: -32000,
            message,
            data: None,
        };
        if let Err(e) = self.write(&msg).await {
            self.waiting.lock().unwrap().remove(&id);
            return Err(fail(e.message));
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(fail("codex exited".into())),
            Err(_) => {
                self.waiting.lock().unwrap().remove(&id);
                Err(fail(format!("no answer to {method}")))
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
        let Ok(msg) = Message::parse(&line) else {
            continue;
        };
        let at = now();
        if let Ok(raw) = serde_json::from_str::<Value>(&line) {
            driver.emit(vec![frame::frame(&b.id, &at, json!({"out": raw}))]);
        }
        match msg {
            Message::Response { id, result } => {
                let Some(w) = b.waiting.lock().unwrap().remove(&id) else {
                    continue;
                };
                if let Some(on_ok) = w.on_ok {
                    let updates = on_ok(&mut b.state.lock().unwrap(), &result);
                    b.record(driver.config.shadow, &updates);
                    driver.emit(updates);
                }
                let _ = w.tx.send(Ok(result));
            }
            Message::Error { id, error } => {
                if let Some(w) = b.waiting.lock().unwrap().remove(&id) {
                    let _ = w.tx.send(Err(error));
                }
            }
            Message::Request { id, method, params } => {
                let (updates, reply) = b.state.lock().unwrap().server_request(&id, &method, &params);
                b.record(driver.config.shadow, &updates);
                let raised = !updates.is_empty();
                driver.emit(updates);
                if let Some(reply) = reply {
                    let _ = b.write(&reply).await;
                }
                if raised {
                    b.raised.notify_one();
                }
            }
            Message::Notification { method, params } => {
                let n = ServerNotification::parse(&method, &params);
                let updates = b.state.lock().unwrap().step(&n, &at);
                b.record(driver.config.shadow, &updates);
                let ended = ends_turn(&updates);
                driver.emit(updates);
                if ended && driver.config.shadow {
                    tokio::spawn(shadow_turn(driver.clone(), b.clone()));
                }
            }
        }
    }
    b.waiting.lock().unwrap().clear();
    if !b.stopping.load(Ordering::Relaxed) {
        let updates = b.state.lock().unwrap().exited(&now());
        driver.emit(updates);
        driver.emit(vec![state_update(
            &b.id,
            State::Lost,
            Some("codex exited".into()),
        )]);
        driver.bindings.lock().unwrap().remove(&b.id);
    }
}

fn notice(binding: &str, text: String, code: &str) -> DriverUpdate {
    DriverUpdate {
        binding: binding.to_owned(),
        subagent: None,
        update: Open::Known(DriverUpdateKind::Notice {
            level: parrot_proto::subscribe::NoticeLevel::Warning,
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

/// The `codex` agent as `agent/list` reports it.
pub fn agent() -> Agent {
    let mut features = Map::new();
    features.insert("session.stream".into(), json!({}));
    features.insert("session.resume".into(), json!({}));
    features.insert("session.history".into(), json!({}));
    features.insert("session.fork".into(), json!({"at": ["turn"]}));
    features.insert("session.steer".into(), json!({"when": "next_step"}));
    features.insert("attachments".into(), input::feature());
    features.insert(
        "usage".into(),
        json!({"cost": false, "context": true, "rateLimits": true}),
    );
    Agent {
        id: "codex".into(),
        env: String::new(),
        name: "Codex".into(),
        driver: AgentDriver {
            name: "codex".into(),
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
            PermissionLevel::Sandboxed,
            PermissionLevel::Host,
        ],
        isolation: vec![Isolation::None, Isolation::Sandbox],
        options: MapState::default().options(),
        actions: mapping::actions(),
        meta: None,
        extra: Map::new(),
    }
}

/// The current time in RFC 3339, UTC, to the millisecond.
pub fn now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let s = mapping::rfc3339(d.as_secs() as i64);
    format!("{}.{:03}Z", s.trim_end_matches('Z'), d.subsec_millis())
}
