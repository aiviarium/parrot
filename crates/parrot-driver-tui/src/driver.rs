//! The TUI driver: one PTY per binding, read through a profile, answered with keys.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use parrot_driver::traits::{Driver, DriverContext};
use parrot_proto::agent::{Agent, AgentDriver, AgentListResult, AuthStatus, Control, DriverKind};
use parrot_proto::driver::{
    BindingAnswerParams, BindingCancelParams, BindingCancelResult, BindingDisposition, BindingKeysParams,
    BindingPromptParams, BindingPromptResult, BindingRef, BindingStart, BindingStartResult, DriverUpdate,
    DriverUpdateKind,
};
use parrot_proto::item::{ItemKind, ItemPatch, ItemSource};
use parrot_proto::open::Open;
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::Isolation;
use parrot_proto::request::{Answerable, Choice, Notify, Request, RequestKind, RequestStatus, Subject};
use parrot_proto::rpc::RpcError;
use parrot_proto::session::{Ref, RefStrength};
use parrot_proto::state::{State, Turn, TurnStatus, TurnStop};
use parrot_term::screen::Size;
use parrot_term::{Pty, PtyCommand, Session, SessionConfig};
use parrot_tui::{AutomateOptions, Automation, Profile, SessionExt, TuiEvent, TuiSnapshot};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc, watch};

use crate::dialog::{self, Dialog};
use crate::transcript::{ClaudeTranscript, Tail};

/// Where a binding's transcript is written.
#[derive(Debug, Clone)]
pub enum TranscriptDir {
    /// Claude Code's folder for the binding's working directory.
    ClaudeProject,
    /// A fixed folder; the newest `.jsonl` written after the start is followed.
    Dir(PathBuf),
}

/// How the driver starts and reads one harness.
#[derive(Debug, Clone)]
pub struct Config {
    /// The agent id `agent/list` reports.
    pub agent: String,
    pub name: String,
    /// The harness command and its arguments.
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Start the harness as `<exe> guard -- <command>` (E4).
    pub guard: Option<PathBuf>,
    pub profile: Profile,
    pub size: Size,
    /// How long a screen no state matches is held before it counts as `unrecognised` (32.2).
    pub unrecognised_after: Duration,
    /// How long `binding/start` waits for a ready screen or a dialog.
    pub start_timeout: Duration,
    /// Profile states and the Parrot state each one is.
    pub states: HashMap<String, State>,
    /// Profile states that are dialogs, and the request kind each one opens.
    pub dialogs: HashMap<String, RequestKind>,
    pub transcript: Option<TranscriptDir>,
}

impl Config {
    /// A harness under `profile`, with the state names the Claude Code and Codex profiles use.
    pub fn new(agent: &str, command: Vec<String>, profile: Profile) -> Self {
        let states = [
            ("idle", State::Idle),
            ("working", State::Running),
            ("starting", State::Starting),
            ("awaiting-approval", State::RequiresAction),
            ("awaiting-input", State::RequiresAction),
            ("onboarding", State::RequiresAction),
        ];
        let dialogs = [
            ("awaiting-approval", RequestKind::Approval),
            ("awaiting-input", RequestKind::Question),
            ("onboarding", RequestKind::Approval),
        ];
        Config {
            agent: agent.into(),
            name: profile.file.profile.name.clone().unwrap_or_else(|| agent.into()),
            command,
            env: vec![],
            guard: guard_exe(),
            profile,
            size: Size::new(40, 120),
            unrecognised_after: Duration::from_secs(2),
            start_timeout: Duration::from_secs(60),
            states: states.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            dialogs: dialogs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            transcript: None,
        }
    }

    /// Claude Code in its terminal, with its transcript as the source of items.
    pub fn claude_code(profile: Profile) -> Self {
        let bin = std::env::var("PARROT_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
        let mut c = Config::new("claude-tui", vec![bin], profile);
        c.transcript = Some(TranscriptDir::ClaudeProject);
        c
    }
}

/// The current executable when it is `parrot`, which has the `guard` subcommand.
fn guard_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    (exe.file_stem()? == "parrot").then_some(exe)
}

/// What a request was opened for, so the same dialog is not opened twice.
#[derive(Debug, Clone, PartialEq)]
enum Origin {
    /// A dialog the profile knows: its state and text.
    Known(String, String),
    /// A screen no state matches: its text.
    Unknown(String),
}

struct OpenRequest {
    origin: Origin,
    request: Request,
    dialog: Option<Dialog>,
}

struct TurnInfo {
    key: String,
    input: Option<String>,
    started: String,
    at: Instant,
    /// The screen has left `idle` since the prompt.
    busy: bool,
    cancelled: bool,
}

#[derive(Default)]
struct Inner {
    state: Option<State>,
    profile_state: Option<String>,
    turn: Option<TurnInfo>,
    last_turn: Option<String>,
    turns: u64,
    requests: u64,
    open: Option<OpenRequest>,
    /// An answered dialog still on screen, and when it was answered; not opened again.
    answered: Option<(Origin, Instant)>,
    stopping: bool,
}

struct Binding {
    id: String,
    session: Session,
    pty: Pty,
    tui: Automation,
    inner: Mutex<Inner>,
    state_tx: watch::Sender<Option<State>>,
    transcript: Mutex<ClaudeTranscript>,
}

/// If a turn sees no busy screen at all, it ends once idle has held this long after the prompt.
const QUIET_TURN: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_millis(250);
const STOP_GRACE: Duration = Duration::from_secs(5);

/// The TUI driver (spec 32.2): implements [`parrot_driver::Driver`], including `binding/screen`
/// and `binding/keys`.
pub struct TuiDriver {
    shared: Arc<Shared>,
}

struct Shared {
    config: Config,
    updates: mpsc::UnboundedSender<Value>,
    bindings: Mutex<HashMap<String, Arc<Binding>>>,
}

impl TuiDriver {
    pub fn new(config: Config, ctx: DriverContext) -> Self {
        TuiDriver {
            shared: Arc::new(Shared {
                config,
                updates: ctx.updates,
                bindings: Mutex::default(),
            }),
        }
    }
}

impl Driver for TuiDriver {
    fn call(
        &mut self,
        method: &str,
        params: Value,
    ) -> impl std::future::Future<Output = Result<Value, RpcError>> + Send {
        let shared = self.shared.clone();
        let method = method.to_owned();
        async move { shared.handle(&method, params).await }
    }
}

impl Drop for TuiDriver {
    fn drop(&mut self) {
        for b in self.shared.bindings.lock().unwrap().values() {
            b.kill();
        }
    }
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| RpcError::new(-32602, format!("invalid params: {e}")))
}

fn to_value<T: serde::Serialize>(t: &T) -> Value {
    serde_json::to_value(t).expect("protocol types serialize")
}

fn failed(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(-32603, e.to_string())
}

impl Shared {
    fn emit(&self, binding: &str, kind: DriverUpdateKind) {
        let _ = self.updates.send(to_value(&DriverUpdate {
            binding: binding.to_owned(),
            subagent: None,
            update: Open::Known(kind),
            extra: Map::new(),
        }));
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
                "peer": {"kind": "driver", "name": "parrot-driver-tui", "version": env!("CARGO_PKG_VERSION")},
                "features": {},
            })),
            "agent/list" => Ok(to_value(&AgentListResult {
                agents: vec![self.agent()],
            })),
            "binding/start" => self.start(params(p)?).await,
            "binding/prompt" => self.prompt(params(p)?).await,
            "binding/cancel" => self.cancel(params(p)?).await,
            "binding/answer" => self.answer(params(p)?).await,
            "binding/screen" => {
                let p: BindingRef = params(p)?;
                Ok(self.binding(&p.binding)?.screen())
            }
            "binding/keys" => {
                let p: BindingKeysParams = params(p)?;
                let b = self.binding(&p.binding)?;
                if let Some(t) = &p.text {
                    b.session.type_text(t).await.map_err(failed)?;
                }
                for k in p.press.iter().flatten() {
                    b.session.press(k).await.map_err(failed)?;
                }
                Ok(json!({}))
            }
            "binding/options" => {
                Ok(json!({ "options": [], "level": PermissionLevel::Host, "network": true }))
            }
            "binding/stop" => {
                let p: BindingRef = params(p)?;
                let b = self.bindings.lock().unwrap().remove(&p.binding);
                if let Some(b) = b {
                    b.inner.lock().unwrap().stopping = true;
                    b.stop().await;
                    self.emit(&b.id, state_kind(State::Stopped, None));
                }
                Ok(json!({}))
            }
            _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
        }
    }

    fn agent(&self) -> Agent {
        let mut features = Map::new();
        features.insert("session.stream".into(), json!({}));
        Agent {
            id: self.config.agent.clone(),
            env: String::new(),
            name: self.config.name.clone(),
            driver: AgentDriver {
                name: "tui".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                kind: DriverKind::Builtin,
                data: self.config.profile.id().into(),
            },
            harness: None,
            installed: true,
            auth: AuthStatus::Unknown,
            usable: true,
            detail: None,
            controls: vec![Control::Tui],
            features,
            levels: vec![PermissionLevel::Host],
            isolation: vec![Isolation::None],
            options: vec![],
            actions: vec![],
            meta: None,
            extra: Map::new(),
        }
    }

    async fn start(self: &Arc<Self>, start: BindingStart) -> Result<Value, RpcError> {
        let c = &self.config;
        let (program, args) = c
            .command
            .split_first()
            .ok_or_else(|| RpcError::new(-32603, "no harness command"))?;
        let mut cmd = match &c.guard {
            Some(exe) => PtyCommand::new(exe.to_string_lossy())
                .args(["guard", "--", program.as_str()])
                .args(args.iter().cloned()),
            None => PtyCommand::new(program.as_str()).args(args.iter().cloned()),
        };
        cmd = cmd.cwd(&start.cwd);
        for (k, v) in &c.env {
            cmd = cmd.env(k, v);
        }
        let started_at = SystemTime::now();
        let (session, pty) = Session::spawn(&cmd, SessionConfig::for_automation(c.size))
            .map_err(|e| failed(format!("cannot start the harness: {e}")))?;
        let tui = session.automate_with(
            Some(c.profile.clone()),
            AutomateOptions {
                unrecognised_after: Some(c.unrecognised_after),
                ..Default::default()
            },
        );
        let events = tui.changes();
        let (state_tx, mut state_rx) = watch::channel(None);
        let b = Arc::new(Binding {
            id: start.binding.clone(),
            session,
            pty,
            tui,
            inner: Mutex::default(),
            state_tx,
            transcript: Mutex::default(),
        });
        self.bindings.lock().unwrap().insert(b.id.clone(), b.clone());
        tokio::spawn(watch_screen(self.clone(), b.clone(), events));
        if let Some(t) = &c.transcript {
            let dir = match t {
                TranscriptDir::ClaudeProject => {
                    crate::transcript::claude_project_dir(std::path::Path::new(&start.cwd))
                }
                TranscriptDir::Dir(d) => Some(d.clone()),
            };
            if let Some(dir) = dir {
                let file = start.resume.as_ref().map(|r| dir.join(format!("{}.jsonl", r.id)));
                let tail = Tail::new(dir, file, started_at);
                tokio::spawn(follow_transcript(self.clone(), b.clone(), tail));
            }
        }
        let ready = tokio::time::timeout(c.start_timeout, async {
            loop {
                let s = *state_rx.borrow_and_update();
                match s {
                    Some(State::Idle | State::RequiresAction | State::Unrecognised) => return Ok(()),
                    Some(State::Lost) => return Err("the harness exited while starting".to_owned()),
                    _ => {}
                }
                if state_rx.changed().await.is_err() {
                    return Err("the harness exited while starting".into());
                }
            }
        })
        .await;
        let err = match ready {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(_) => Some(format!(
                "no ready screen within {:?}:\n{}",
                c.start_timeout,
                b.tui.snapshot()
            )),
        };
        if let Some(e) = err {
            self.bindings.lock().unwrap().remove(&b.id);
            b.inner.lock().unwrap().stopping = true;
            b.stop().await;
            return Err(failed(e));
        }
        Ok(to_value(&BindingStartResult {
            reference: start.resume.clone(),
            isolation: Isolation::None,
            level: PermissionLevel::Host,
            network: true,
            options: vec![],
            actions: vec![],
        }))
    }

    async fn prompt(self: &Arc<Self>, p: BindingPromptParams) -> Result<Value, RpcError> {
        let b = self.binding(&p.binding)?;
        let key = {
            let mut inner = b.inner.lock().unwrap();
            if inner.turn.is_some() || inner.state != Some(State::Idle) {
                return Err(RpcError::new(
                    -32001,
                    "the harness is not idle; this driver cannot steer",
                ));
            }
            inner.turns += 1;
            let key = format!("turn-{}", inner.turns);
            inner.turn = Some(TurnInfo {
                key: key.clone(),
                input: Some(p.message.clone()),
                started: crate::now(),
                at: Instant::now(),
                busy: false,
                cancelled: false,
            });
            inner.last_turn = Some(key.clone());
            inner.state = Some(State::Running);
            key
        };
        b.transcript.lock().unwrap().expect_echo(&p.text);
        self.emit(&b.id, state_kind(State::Running, None));
        let turn = b
            .inner
            .lock()
            .unwrap()
            .turn
            .as_ref()
            .map(turn_of)
            .expect("just set");
        self.emit(&b.id, DriverUpdateKind::Turn { turn });
        let mut set = Map::new();
        set.insert("text".into(), json!(p.text));
        set.insert("status".into(), json!("sent"));
        set.insert("message".into(), json!(p.message));
        self.emit(
            &b.id,
            DriverUpdateKind::Item {
                item: ItemPatch {
                    id: p.message.clone(),
                    kind: Some(Open::Known(ItemKind::User)),
                    turn: Some(key),
                    source: Some(ItemSource::Tui),
                    set: Some(set),
                    ..Default::default()
                },
            },
        );
        // A newline typed into most TUIs sends the first line alone, so several lines go in as a paste.
        let profile = &self.config.profile;
        let action = if p.text.contains('\n') && profile.action("submit-paste").is_some() {
            "submit-paste"
        } else {
            "submit"
        };
        let result = if profile.action(action).is_some() {
            let vars = HashMap::from([("text".to_string(), p.text.clone())]);
            b.tui
                .act(action, &vars)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else {
            let typed = if p.text.contains('\n') {
                b.session.paste(&p.text).await
            } else {
                b.session.type_text(&p.text).await
            };
            match typed {
                Ok(()) => b.session.press("enter").await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        };
        if let Err(e) = result {
            self.end_turn(&b, TurnStatus::Failed, Some(e.clone()));
            return Err(failed(format!("the prompt was not sent: {e}")));
        }
        Ok(to_value(&BindingPromptResult {
            disposition: BindingDisposition::Started,
        }))
    }

    async fn cancel(self: &Arc<Self>, p: BindingCancelParams) -> Result<Value, RpcError> {
        let b = self.binding(&p.binding)?;
        let busy = {
            let mut inner = b.inner.lock().unwrap();
            let busy = matches!(inner.state, Some(State::Running | State::RequiresAction));
            if let Some(t) = inner.turn.as_mut() {
                t.cancelled = true;
            }
            busy
        };
        if !busy {
            return Ok(to_value(&BindingCancelResult { interrupted: false }));
        }
        let interrupted = if self.config.profile.action("interrupt").is_some()
            && b.tui.state().as_deref() == Some("working")
        {
            b.tui.act("interrupt", &HashMap::new()).await.is_ok()
        } else {
            b.session.press("esc").await.is_ok()
        };
        Ok(to_value(&BindingCancelResult { interrupted }))
    }

    async fn answer(self: &Arc<Self>, p: BindingAnswerParams) -> Result<Value, RpcError> {
        let b = self.binding(&p.binding)?;
        let (origin, dialog, request) = {
            let inner = b.inner.lock().unwrap();
            let open = inner
                .open
                .as_ref()
                .filter(|o| o.request.id == p.request)
                .ok_or_else(|| RpcError::new(-32602, format!("no open request {}", p.request)))?;
            (open.origin.clone(), open.dialog.clone(), open.request.clone())
        };
        let a = &p.answer;
        if let Some(choice) = &a.choice {
            let opt = dialog
                .as_ref()
                .and_then(|d| d.options.iter().find(|o| &o.id == choice))
                .ok_or_else(|| RpcError::new(-32602, format!("no choice {choice}")))?
                .clone();
            let known = matches!(origin, Origin::Known(..));
            if known && opt.key.is_some() && self.config.profile.action("choose").is_some() {
                let vars = HashMap::from([("option".to_string(), opt.key.clone().unwrap_or_default())]);
                b.tui.act("choose", &vars).await.map_err(failed)?;
            } else if let Some(surface) = dialog.as_ref().and_then(|d| d.surface.clone()) {
                b.tui.choose(&surface, opt.index).await.map_err(failed)?;
            } else {
                for k in &opt.keys {
                    b.session.press(k).await.map_err(failed)?;
                }
            }
        } else if let Some(v) = &a.value {
            match v {
                Value::String(s) => {
                    b.session.type_text(s).await.map_err(failed)?;
                    b.session.press("enter").await.map_err(failed)?;
                }
                Value::Object(o) => {
                    if let Some(t) = o.get("text").and_then(Value::as_str) {
                        b.session.type_text(t).await.map_err(failed)?;
                    }
                    for k in o.get("press").and_then(Value::as_array).into_iter().flatten() {
                        let k = k
                            .as_str()
                            .ok_or_else(|| RpcError::new(-32602, "press takes key names"))?;
                        b.session.press(k).await.map_err(failed)?;
                    }
                }
                _ => return Err(RpcError::new(-32602, "value is text, or { text?, press? }")),
            }
        } else {
            return Err(RpcError::new(-32602, "the answer needs a choice or a value"));
        }
        let mut request = request;
        request.status = RequestStatus::Answered;
        request.answer = Some(a.clone());
        {
            let mut inner = b.inner.lock().unwrap();
            if inner.open.as_ref().is_some_and(|o| o.request.id == p.request) {
                inner.open = None;
                inner.answered = Some((origin, Instant::now()));
            }
        }
        self.emit(
            &b.id,
            DriverUpdateKind::Request {
                request: Box::new(request),
            },
        );
        Ok(json!({}))
    }

    fn end_turn(&self, b: &Binding, status: TurnStatus, error: Option<String>) {
        let Some(t) = b.inner.lock().unwrap().turn.take() else {
            return;
        };
        let mut turn = turn_of(&t);
        let status = if t.cancelled && status == TurnStatus::Completed {
            TurnStatus::Interrupted
        } else {
            status
        };
        turn.status = status;
        turn.ended = Some(crate::now());
        turn.stop = Some(Open::Known(match status {
            TurnStatus::Interrupted => TurnStop::Cancelled,
            TurnStatus::Failed => TurnStop::Error,
            _ => TurnStop::End,
        }));
        turn.error = error.map(|message| parrot_proto::state::TurnError {
            code: "tui".into(),
            message,
            retryable: true,
        });
        self.emit(&b.id, DriverUpdateKind::Turn { turn });
    }

    /// Brings states, requests and the turn in line with the screen.
    fn evaluate(&self, b: &Binding, snap: &TuiSnapshot) {
        let c = &self.config;
        let screen_state = if snap.closed {
            Some(State::Lost)
        } else if snap.unrecognised {
            Some(State::Unrecognised)
        } else {
            snap.state
                .as_ref()
                .map(|s| c.states.get(s).copied().unwrap_or(State::Unknown))
        };
        let mut out: Vec<DriverUpdateKind> = Vec::new();
        let mut end: Option<TurnStatus> = None;
        {
            let mut inner = b.inner.lock().unwrap();
            if inner.stopping {
                return;
            }
            inner.profile_state = snap.state.clone();
            // A screen that has not yet matched anything keeps the last state until the hold is up.
            let Some(mut state) = screen_state.or(inner.state) else {
                return;
            };
            if let Some(t) = inner.turn.as_mut() {
                if state == State::Idle {
                    if t.busy || t.at.elapsed() >= QUIET_TURN {
                        end = Some(TurnStatus::Completed);
                    } else {
                        // The prompt was just sent; the screen has not caught up.
                        state = State::Running;
                    }
                } else if state != State::Starting {
                    t.busy = true;
                }
            }
            // The request this screen calls for.
            let screen = b.session.screen();
            let want = match state {
                State::RequiresAction => {
                    snap.state
                        .as_ref()
                        .filter(|s| c.dialogs.contains_key(*s))
                        .map(|s| {
                            let d = snap.tree.as_ref().and_then(dialog::read);
                            (
                                Origin::Known(
                                    s.clone(),
                                    d.as_ref().map(|d| d.text.clone()).unwrap_or_default(),
                                ),
                                d,
                            )
                        })
                }
                State::Unrecognised => {
                    let text = dialog::screen_text(&screen);
                    Some((Origin::Unknown(text), snap.tree.as_ref().and_then(dialog::read)))
                }
                _ => None,
            };
            let want_origin = want.as_ref().map(|(o, _)| o.clone());
            // The screen after an answer to an unknown screen is still unknown for a moment (the
            // echo of what was typed); it counts as the same screen until the hold has passed again.
            let settled = |(o, at): &(Origin, Instant)| match (o, &want_origin) {
                (Origin::Unknown(_), Some(Origin::Unknown(_))) => at.elapsed() < c.unrecognised_after,
                _ => Some(o) == want_origin.as_ref(),
            };
            if inner.answered.as_ref().is_some_and(|a| !settled(a)) {
                inner.answered = None;
            }
            let stale = inner
                .open
                .as_ref()
                .is_some_and(|o| Some(&o.origin) != want_origin.as_ref());
            // An unknown screen that changes while it stays unknown keeps its request.
            let same_unknown = matches!(
                (inner.open.as_ref().map(|o| &o.origin), &want_origin),
                (Some(Origin::Unknown(_)), Some(Origin::Unknown(_)))
            );
            if stale && !same_unknown {
                let mut o = inner.open.take().expect("stale means open").request;
                o.status = RequestStatus::Cancelled;
                o.answered_by = Some("harness".into());
                out.push(DriverUpdateKind::Request { request: Box::new(o) });
            }
            if inner.state != Some(state) {
                inner.state = Some(state);
                let detail = match state {
                    State::Unrecognised => Some("no profile state matches the screen".to_owned()),
                    State::Unknown => snap.state.clone(),
                    _ => None,
                };
                out.push(state_kind(state, detail));
            }
            if let Some((origin, d)) = want
                && inner.open.is_none()
                && inner.answered.is_none()
            {
                inner.requests += 1;
                let key = format!("req-{}", inner.requests);
                let turn = inner.turn.as_ref().map(|t| t.key.clone());
                let request = build_request(key, turn, &origin, d.as_ref(), c, &screen);
                out.push(DriverUpdateKind::Request {
                    request: Box::new(request.clone()),
                });
                inner.open = Some(OpenRequest {
                    origin,
                    request,
                    dialog: d,
                });
            }
            let _ = b.state_tx.send(inner.state);
        }
        for k in out {
            self.emit(&b.id, k);
        }
        if let Some(s) = end {
            self.end_turn(b, s, None);
        }
    }
}

fn build_request(
    key: String,
    turn: Option<String>,
    origin: &Origin,
    d: Option<&Dialog>,
    c: &Config,
    screen: &parrot_term::screen::Screen,
) -> Request {
    let choices = d.filter(|d| !d.options.is_empty()).map(|d| {
        d.options
            .iter()
            .map(|o| Choice {
                id: o.id.clone(),
                label: o.label.clone(),
                intent: None,
                scope: None,
                amends: None,
                description: None,
            })
            .collect::<Vec<_>>()
    });
    let (kind, title, description, subject, input) = match origin {
        Origin::Known(state, text) => {
            let kind = c.dialogs.get(state).copied().unwrap_or(RequestKind::Input);
            let title = d
                .and_then(|d| d.title.clone())
                .or_else(|| {
                    text.lines()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| l.trim().to_owned())
                })
                .unwrap_or_else(|| state.clone());
            (kind, title, Some(text.clone()), None, None)
        }
        Origin::Unknown(text) => {
            let cursor = screen.cursor();
            let subject = Open::<Subject>::Unknown(json!({
                "type": "screen",
                "text": text,
                "cursor": { "row": cursor.row, "col": cursor.col },
                "hints": d.map(|d| d.hints.clone()).unwrap_or_default(),
                "options": d.map(|d| d.options.clone()).unwrap_or_default(),
            }));
            let input = json!({
                "type": "string",
                "description": "Text to type, then Enter. Or { text?, press? } with key names.",
            });
            (
                RequestKind::Input,
                "The terminal shows a screen the profile does not know".to_owned(),
                Some(text.clone()),
                Some(subject),
                Some(input),
            )
        }
    };
    Request {
        id: key,
        session: String::new(),
        path: vec![],
        reference: None,
        turn,
        item: None,
        kind: Open::Known(kind),
        title,
        description,
        subject,
        level: PermissionLevel::Host,
        network: None,
        choices,
        input,
        blocking: true,
        answerable: Answerable::Connection,
        expires: None,
        fallback: None,
        notify: Notify::Normal,
        status: RequestStatus::Open,
        answer: None,
        answered_by: None,
        meta: None,
        extra: Map::new(),
    }
}

fn turn_of(t: &TurnInfo) -> Turn {
    Turn {
        id: t.key.clone(),
        binding: String::new(),
        status: TurnStatus::Running,
        stop: None,
        continuable: false,
        input: t.input.clone(),
        started: t.started.clone(),
        ended: None,
        error: None,
        usage: None,
        meta: None,
        extra: Map::new(),
    }
}

fn state_kind(state: State, detail: Option<String>) -> DriverUpdateKind {
    DriverUpdateKind::State {
        state,
        background: vec![],
        detail,
    }
}

async fn watch_screen(shared: Arc<Shared>, b: Arc<Binding>, mut events: broadcast::Receiver<TuiEvent>) {
    shared.evaluate(&b, &b.tui.snapshot());
    let mut tick = tokio::time::interval(TICK);
    loop {
        let closed = tokio::select! {
            ev = events.recv() => match ev {
                Ok(TuiEvent::Closed { .. }) | Err(broadcast::error::RecvError::Closed) => true,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => false,
            },
            _ = tick.tick() => false,
        };
        let mut snap = b.tui.snapshot();
        snap.closed |= closed;
        shared.evaluate(&b, &snap);
        if snap.closed {
            break;
        }
    }
    let stopping = b.inner.lock().unwrap().stopping;
    if !stopping {
        shared.end_turn(&b, TurnStatus::Failed, Some("the harness exited".into()));
        shared.bindings.lock().unwrap().remove(&b.id);
    }
}

async fn follow_transcript(shared: Arc<Shared>, b: Arc<Binding>, mut tail: Tail) {
    let mut known_ref: Option<String> = None;
    loop {
        if b.session.is_closed() && b.inner.lock().unwrap().stopping {
            return;
        }
        for line in tail.poll() {
            let ev = b.transcript.lock().unwrap().line(&line);
            if let Some(s) = ev.session
                && known_ref.as_ref() != Some(&s)
            {
                known_ref = Some(s.clone());
                shared.emit(
                    &b.id,
                    DriverUpdateKind::Ref {
                        reference: Ref {
                            id: s,
                            strength: RefStrength::Strong,
                        },
                    },
                );
            }
            let turn = {
                let inner = b.inner.lock().unwrap();
                inner
                    .turn
                    .as_ref()
                    .map(|t| t.key.clone())
                    .or(inner.last_turn.clone())
            };
            for mut item in ev.items {
                if item.kind.is_some() {
                    item.turn = turn.clone();
                }
                shared.emit(&b.id, DriverUpdateKind::Item { item });
            }
        }
        if b.session.is_closed() {
            return;
        }
        tokio::time::sleep(crate::transcript::POLL).await;
    }
}

impl Binding {
    fn screen(&self) -> Value {
        let screen = self.session.screen();
        let cursor = screen.cursor();
        let size = screen.size();
        let snap = self.tui.snapshot();
        json!({
            "text": dialog::screen_text(&screen),
            "cursor": { "row": cursor.row, "col": cursor.col },
            "size": { "rows": size.rows, "cols": size.cols },
            "state": snap.state,
            "unrecognised": snap.unrecognised,
            "tree": snap.tree,
            "surfaces": snap.surfaces,
        })
    }

    fn kill(&self) {
        if let Some(pid) = self.pty.pid() {
            parrot_guard::kill_tree(pid);
        }
        let _ = self.pty.kill();
    }

    async fn stop(&self) {
        if self.pty.try_wait().is_none() {
            // A hang-up is what a closed terminal sends; most TUIs exit on it.
            let _ = self.pty.signal(rustix::process::Signal::HUP);
            if tokio::time::timeout(STOP_GRACE, self.pty.wait()).await.is_err() {
                self.kill();
            }
        }
    }
}
