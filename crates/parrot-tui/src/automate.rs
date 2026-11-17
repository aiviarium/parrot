//! Driving a terminal program through its labelled tree.
//!
//! - [`Tracker`] is the synchronous core: feed it settled screens with [`Tracker::update`] and
//!   it segments them, applies the profile, keeps ids stable, finds the current state and
//!   reports [`TuiEvent`]s. Replays and measurements use it directly.
//! - [`Automation`], from [`Session::automate`], runs a tracker on a live session's settled
//!   frames and adds waits ([`Automation::wait_for`]), actions ([`Automation::act`]),
//!   queries ([`Automation::find`]) and a stream of changes ([`Automation::changes`]).
//!
//! An action never types into a screen it did not expect: `require` is checked first, every
//! step that waits does so on tree conditions, never on fixed sleeps, and `expect` is checked
//! afterwards. Any failure is a [`TuiError`] that carries the tree it failed on.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::profile::{Cond, Ctx, Hooks, Profile, SelectStep, StateEval, Step, eval_wait};
use crate::tree::{Node, Role, Surface, Tree, TreeDiff, assign_ids, diff, render, segment, surfaces};
use parrot_term::screen::Screen;
use parrot_term::session::Session;

/// Something that changed, in the order it happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TuiEvent {
    /// Nodes were added, removed or changed.
    Tree { time: f64, diff: TreeDiff },
    /// The current state changed.
    State {
        time: f64,
        from: Option<String>,
        to: Option<String>,
    },
    /// No state has held since `since`, for longer than the profile's threshold.
    #[serde(rename_all = "camelCase")]
    Unrecognised { time: f64, since: f64 },
    /// A state holds again after `unrecognised`.
    Recognised { time: f64, state: String },
    /// The session's output ended.
    Closed { time: f64 },
}

/// The tracker's view at one moment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiSnapshot {
    /// Seconds on the session's clock.
    pub time: f64,
    /// Settled screens seen so far.
    pub frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The CLI version the profile's variants were chosen for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub state: Option<String>,
    /// Every state whose rule holds.
    pub states: Vec<String>,
    /// When the current state (or the absence of one) began.
    pub since: f64,
    pub unrecognised: bool,
    pub tree: Option<Tree>,
    pub surfaces: Vec<Surface>,
    pub closed: bool,
}

impl fmt::Display for TuiSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "state {} (since {:.3}s{}) holding [{}] at {:.3}s",
            self.state.as_deref().unwrap_or("none"),
            self.since,
            if self.unrecognised { ", unrecognised" } else { "" },
            self.states.join(", "),
            self.time
        )?;
        match &self.tree {
            Some(t) => write!(f, "{}", render(t)),
            None => writeln!(f, "(no screen yet)"),
        }
    }
}

/// Finds nodes by tag, role or text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub tag: Option<String>,
    pub role: Option<Role>,
    pub id: Option<String>,
    /// Case-insensitive substring of the text or label.
    pub text: Option<String>,
}

impl Query {
    pub fn tag(tag: &str) -> Query {
        Query {
            tag: Some(tag.into()),
            ..Default::default()
        }
    }
    pub fn role(role: Role) -> Query {
        Query {
            role: Some(role),
            ..Default::default()
        }
    }
    pub fn matches(&self, n: &Node) -> bool {
        self.tag.as_ref().is_none_or(|t| n.has_tag(t))
            && self.role.is_none_or(|r| n.role == r)
            && self.id.as_ref().is_none_or(|i| &n.id == i)
            && self.text.as_ref().is_none_or(|t| {
                let t = t.to_lowercase();
                n.text.to_lowercase().contains(&t)
                    || n.label.as_ref().is_some_and(|l| l.to_lowercase().contains(&t))
            })
    }
}

pub fn find(tree: &Tree, q: &Query) -> Vec<Node> {
    tree.nodes()
        .into_iter()
        .filter(|n| q.matches(n))
        .cloned()
        .collect()
}

/// The synchronous core of automation. Times are seconds on any clock, as long as it is the
/// same for every call.
pub struct Tracker {
    base: Option<Profile>,
    profile: Option<Profile>,
    hooks: Hooks,
    version: Option<String>,
    tree: Option<Tree>,
    text: String,
    counter: u64,
    seen: HashSet<String>,
    eval: StateEval,
    since: f64,
    unknown_since: Option<f64>,
    unrecognised: bool,
    unrecognised_after: f64,
    /// The caller's threshold, which a profile reload keeps.
    unrecognised_override: Option<f64>,
    time: f64,
    frames: u64,
    closed: bool,
}

impl Tracker {
    pub fn new(profile: Option<Profile>, hooks: Hooks) -> Tracker {
        let mut t = Tracker {
            base: None,
            profile: None,
            hooks,
            version: None,
            tree: None,
            text: String::new(),
            counter: 0,
            seen: HashSet::new(),
            eval: StateEval::default(),
            since: 0.0,
            unknown_since: None,
            unrecognised: false,
            unrecognised_after: 10.0,
            unrecognised_override: None,
            time: 0.0,
            frames: 0,
            closed: false,
        };
        t.set_profile(profile);
        t
    }

    /// Uses a new profile from the next screen on (the inspector reloads edited files this
    /// way). Ids and memory of seen states are kept.
    pub fn set_profile(&mut self, profile: Option<Profile>) {
        self.unrecognised_after = self.unrecognised_override.unwrap_or_else(|| {
            profile
                .as_ref()
                .map_or(10.0, |p| p.unrecognised_after().as_secs_f64())
        });
        self.profile = profile.as_ref().map(|p| p.resolve(self.version.as_deref()));
        self.base = profile;
    }

    /// How long no state may match before the screen counts as unrecognised, instead of the
    /// profile's `unrecognised-after`.
    pub fn set_unrecognised_after(&mut self, after: Duration) {
        self.unrecognised_override = Some(after.as_secs_f64());
        self.unrecognised_after = after.as_secs_f64();
    }

    /// Chooses the profile's variants for this CLI version.
    pub fn set_version(&mut self, version: Option<String>) {
        self.version = version;
        self.profile = self.base.as_ref().map(|p| p.resolve(self.version.as_deref()));
    }

    pub fn profile(&self) -> Option<&Profile> {
        self.profile.as_ref()
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    pub fn state(&self) -> Option<&str> {
        self.eval.current.as_deref()
    }

    pub fn is_unrecognised(&self) -> bool {
        self.unrecognised
    }

    /// Builds the labelled tree for a settled screen.
    pub fn update(&mut self, time: f64, screen: &Screen) -> Vec<TuiEvent> {
        if self.version.is_none()
            && let Some(v) = self.base.as_ref().and_then(|p| p.version_from_screen(screen))
        {
            self.set_version(Some(v));
        }
        let mut tree = segment(screen);
        if let Some(p) = &self.profile {
            p.label(&mut tree, screen, &self.hooks);
        }
        assign_ids(self.tree.as_ref(), &mut tree, &mut self.counter);
        self.text = screen.lines().join("\n");
        let eval = match &self.profile {
            Some(p) => p.evaluate(&Ctx {
                tree: &tree,
                text: &self.text,
                seen: &self.seen,
                before: None,
            }),
            None => StateEval::default(),
        };
        let mut events = Vec::new();
        if let Some(old) = &self.tree {
            let d = diff(old, &tree);
            if !d.is_empty() {
                events.push(TuiEvent::Tree { time, diff: d });
            }
        }
        if eval.current != self.eval.current || self.frames == 0 {
            if eval.current != self.eval.current || eval.current.is_some() {
                events.push(TuiEvent::State {
                    time,
                    from: self.eval.current.clone(),
                    to: eval.current.clone(),
                });
            }
            self.since = time;
        }
        if let Some(c) = &eval.current {
            self.seen.insert(c.clone());
        }
        self.eval = eval;
        self.tree = Some(tree);
        self.frames += 1;
        self.time = time;
        events.extend(self.tick(time));
        events
    }

    /// Checks the `unrecognised` threshold at `time`, between screens.
    pub fn tick(&mut self, time: f64) -> Vec<TuiEvent> {
        self.time = self.time.max(time);
        let mut events = Vec::new();
        if self.profile.is_none() {
            return events;
        }
        match &self.eval.current {
            None => {
                let since = *self.unknown_since.get_or_insert(time);
                if !self.unrecognised && time - since >= self.unrecognised_after {
                    self.unrecognised = true;
                    events.push(TuiEvent::Unrecognised { time, since });
                }
            }
            Some(s) => {
                self.unknown_since = None;
                if self.unrecognised {
                    self.unrecognised = false;
                    events.push(TuiEvent::Recognised {
                        time,
                        state: s.clone(),
                    });
                }
            }
        }
        events
    }

    /// When [`Tracker::tick`] should next run to notice `unrecognised` in time.
    pub fn next_deadline(&self) -> Option<f64> {
        if self.unrecognised || self.profile.is_none() || self.eval.current.is_some() {
            return None;
        }
        self.unknown_since.map(|s| s + self.unrecognised_after)
    }

    pub fn close(&mut self, time: f64) -> TuiEvent {
        self.closed = true;
        TuiEvent::Closed { time }
    }

    /// Checks a condition against the current tree; `state = X` means the current state.
    pub fn check(&self, cond: &Cond) -> Result<(), String> {
        self.check_since(cond, None)
    }

    /// Like [`Tracker::check`], with the tag texts from when an action started, for
    /// `changed`.
    pub fn check_since(&self, cond: &Cond, before: Option<&HashMap<String, String>>) -> Result<(), String> {
        let Some(tree) = &self.tree else {
            return Err("no screen yet".into());
        };
        eval_wait(
            cond,
            &Ctx {
                tree,
                text: &self.text,
                seen: &self.seen,
                before,
            },
            self.eval.current.as_deref(),
        )
    }

    pub fn snapshot(&self) -> TuiSnapshot {
        TuiSnapshot {
            time: self.time,
            frames: self.frames,
            profile: self.base.as_ref().map(|p| p.id().to_string()),
            version: self.version.clone(),
            state: self.eval.current.clone(),
            states: self.eval.holding.clone(),
            since: self.since,
            unrecognised: self.unrecognised,
            surfaces: self.tree.as_ref().map(surfaces).unwrap_or_default(),
            tree: self.tree.clone(),
            closed: self.closed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TuiErrorKind {
    /// The action's `require` did not hold.
    Require,
    /// A step failed: its wait timed out, or a menu or option was missing.
    Step,
    /// The action ran but its `expect` did not hold in time.
    Expect,
    /// A wait timed out.
    Timeout,
    /// The session ended.
    Closed,
    NoProfile,
    UnknownAction,
    /// Writing input failed.
    Input,
}

/// A failed wait or action, with the tree it failed on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiError {
    pub kind: TuiErrorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// 1-based step number, for [`TuiErrorKind::Step`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
    pub message: String,
    pub snapshot: Box<TuiSnapshot>,
}

impl fmt::Display for TuiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(a) = &self.action {
            write!(f, "action {a}: ")?;
        }
        let kind = match self.kind {
            TuiErrorKind::Require => "require failed".to_string(),
            TuiErrorKind::Step => format!("step {} failed", self.step.unwrap_or(0)),
            TuiErrorKind::Expect => "expect failed".into(),
            TuiErrorKind::Timeout => "timed out".into(),
            TuiErrorKind::Closed => "session closed".into(),
            TuiErrorKind::NoProfile => "no profile".into(),
            TuiErrorKind::UnknownAction => "unknown action".into(),
            TuiErrorKind::Input => "input failed".into(),
        };
        writeln!(f, "{kind}: {}", self.message)?;
        write!(f, "{}", self.snapshot)
    }
}

impl std::error::Error for TuiError {}

/// What an action did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionReport {
    pub action: String,
    pub state_before: Option<String>,
    pub state_after: Option<String>,
    pub steps: usize,
    pub millis: u64,
}

#[derive(Clone, Default)]
pub struct AutomateOptions {
    pub hooks: Hooks,
    /// The CLI's version, for the profile's variants. Otherwise read from the screen if the
    /// profile says how.
    pub version: Option<String>,
    /// Overrides the profile's `unrecognised-after`.
    pub unrecognised_after: Option<Duration>,
}

struct Shared {
    tracker: Mutex<Tracker>,
    events: broadcast::Sender<TuiEvent>,
    version: watch::Sender<u64>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Tracker> {
        self.tracker.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn publish(&self, events: Vec<TuiEvent>) {
        for e in events {
            let _ = self.events.send(e);
        }
        self.version.send_modify(|v| *v += 1);
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A live session driven through its tree. Clones share the tracker.
#[derive(Clone)]
pub struct Automation {
    session: Session,
    shared: Arc<Shared>,
    _task: Arc<AbortOnDrop>,
}

/// TUI automation and locator waits on a terminal session.
pub trait SessionExt {
    /// Tracks the session's tree with the given profile.
    fn automate(&self, profile: Option<Profile>) -> Automation;

    /// Tracks the session's tree with explicit automation options.
    fn automate_with(&self, profile: Option<Profile>, opts: AutomateOptions) -> Automation;

    /// Waits until `locator` finds something, and returns it.
    fn wait_for_locator(
        &self,
        locator: &crate::Locator,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<crate::TextMatch, parrot_term::WaitError>> + Send;
}

impl SessionExt for Session {
    /// Starts tracking this session's tree with `profile` (or none: segmentation and generic
    /// surfaces only). Must be called inside a tokio runtime. For programs that never go quiet,
    /// open the session with [`parrot_term::SessionConfig::for_automation`].
    fn automate(&self, profile: Option<Profile>) -> Automation {
        self.automate_with(profile, AutomateOptions::default())
    }

    fn automate_with(&self, profile: Option<Profile>, opts: AutomateOptions) -> Automation {
        let mut tracker = Tracker::new(profile, opts.hooks);
        if let Some(d) = opts.unrecognised_after {
            tracker.set_unrecognised_after(d);
        }
        if opts.version.is_some() {
            tracker.set_version(opts.version);
        }
        let mut frames = self.settled();
        tracker.update(self.elapsed(), &self.screen());
        let (events, _) = broadcast::channel(1024);
        let shared = Arc::new(Shared {
            tracker: Mutex::new(tracker),
            events,
            version: watch::channel(0).0,
        });
        let sh = shared.clone();
        let session = self.clone();
        let task = tokio::spawn(async move {
            loop {
                let deadline = sh.lock().next_deadline();
                let sleep = async {
                    match deadline {
                        Some(d) => {
                            let wait = (d - session.elapsed()).max(0.0);
                            tokio::time::sleep(Duration::from_secs_f64(wait)).await
                        }
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    f = frames.next() => match f {
                        Some(f) => {
                            let ev = sh.lock().update(f.time, &f.screen);
                            sh.publish(ev);
                        }
                        None => {
                            let ev = sh.lock().close(session.elapsed());
                            sh.publish(vec![ev]);
                            break;
                        }
                    },
                    _ = sleep => {
                        let ev = sh.lock().tick(session.elapsed());
                        sh.publish(ev);
                    }
                }
            }
        });
        Automation {
            session: self.clone(),
            shared,
            _task: Arc::new(AbortOnDrop(task)),
        }
    }
    /// Waits until `locator` finds something, and returns it.
    async fn wait_for_locator(
        &self,
        locator: &crate::Locator,
        timeout: Duration,
    ) -> Result<crate::TextMatch, parrot_term::WaitError> {
        let screen = self.wait_for(|s| locator.exists(s), timeout).await?;
        locator
            .find(&screen)
            .ok_or(parrot_term::WaitError::Timeout { screen, timeout })
    }
}

const DEFAULT_EXPECT: Duration = Duration::from_secs(10);
const DEFAULT_SELECT: Duration = Duration::from_secs(5);
const DEFAULT_SELECT_HOLD: Duration = Duration::from_millis(200);

impl Automation {
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn snapshot(&self) -> TuiSnapshot {
        self.shared.lock().snapshot()
    }

    pub fn state(&self) -> Option<String> {
        self.shared.lock().state().map(str::to_string)
    }

    pub fn tree(&self) -> Option<Tree> {
        self.shared.lock().tree().cloned()
    }

    pub fn find(&self, q: &Query) -> Vec<Node> {
        self.shared.lock().tree().map(|t| find(t, q)).unwrap_or_default()
    }

    pub fn surfaces(&self) -> Vec<Surface> {
        self.shared.lock().tree().map(surfaces).unwrap_or_default()
    }

    /// Tree diffs and state changes from now on.
    pub fn changes(&self) -> broadcast::Receiver<TuiEvent> {
        self.shared.events.subscribe()
    }

    /// Uses another profile, applied to the current screen at once (an inspector shows the change
    /// without waiting for output) and to every settled screen after.
    pub fn reload(&self, profile: Option<Profile>) {
        let events = {
            let mut t = self.shared.lock();
            t.set_profile(profile);
            t.update(self.session.elapsed(), &self.session.screen())
        };
        self.shared.publish(events);
    }

    fn error(
        &self,
        kind: TuiErrorKind,
        action: Option<&str>,
        step: Option<usize>,
        message: impl Into<String>,
    ) -> TuiError {
        TuiError {
            kind,
            action: action.map(str::to_string),
            step,
            message: message.into(),
            snapshot: Box::new(self.snapshot()),
        }
    }

    /// Waits until `cond` holds on a settled screen. `state = X` means the current state.
    pub async fn wait_for(&self, cond: &Cond, timeout: Duration) -> Result<TuiSnapshot, TuiError> {
        self.wait_since(cond, timeout, None).await
    }

    async fn wait_since(
        &self,
        cond: &Cond,
        timeout: Duration,
        before: Option<&HashMap<String, String>>,
    ) -> Result<TuiSnapshot, TuiError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let hold = cond.hold.map(|h| h.0);
        let mut held_since: Option<tokio::time::Instant> = None;
        let mut rx = self.shared.version.subscribe();
        loop {
            rx.borrow_and_update();
            let why = {
                let t = self.shared.lock();
                match t.check_since(cond, before) {
                    Ok(()) => {
                        let now = tokio::time::Instant::now();
                        let since = *held_since.get_or_insert(now);
                        match hold {
                            Some(h) if now - since < h => {
                                format!("held for {:?} of {h:?}", now - since)
                            }
                            _ => return Ok(t.snapshot()),
                        }
                    }
                    Err(e) => {
                        held_since = None;
                        e
                    }
                }
            };
            if self.session.is_closed() && self.shared.lock().snapshot().closed {
                return Err(self.error(TuiErrorKind::Closed, None, None, why));
            }
            // While the condition holds, wake when the hold is up even if nothing changes.
            let wake = match (held_since, hold) {
                (Some(s), Some(h)) => (s + h).min(deadline),
                _ => deadline,
            };
            match tokio::time::timeout_at(wake, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(self.error(TuiErrorKind::Closed, None, None, why)),
                Err(_) if wake < deadline => {}
                Err(_) => {
                    return Err(self.error(
                        TuiErrorKind::Timeout,
                        None,
                        None,
                        format!("after {timeout:?}: {why}"),
                    ));
                }
            }
        }
    }

    /// Waits until the current state is `state`.
    pub async fn wait_for_state(&self, state: &str, timeout: Duration) -> Result<TuiSnapshot, TuiError> {
        let cond = Cond {
            state: Some(crate::profile::OneOrMany::One(state.to_string())),
            ..Default::default()
        };
        self.wait_for(&cond, timeout).await
    }

    async fn input(
        &self,
        action: &str,
        step: usize,
        f: impl std::future::Future<Output = std::io::Result<()>>,
    ) -> Result<(), TuiError> {
        f.await
            .map_err(|e| self.error(TuiErrorKind::Input, Some(action), Some(step), e.to_string()))
    }

    /// Runs a profile action with variables.
    pub async fn act(&self, name: &str, vars: &HashMap<String, String>) -> Result<ActionReport, TuiError> {
        let started = Instant::now();
        let compiled = {
            let t = self.shared.lock();
            let Some(p) = t.profile() else {
                drop(t);
                return Err(self.error(TuiErrorKind::NoProfile, Some(name), None, "no profile"));
            };
            p.compile_action(name, vars)
        };
        let action = compiled.map_err(|e| self.error(TuiErrorKind::UnknownAction, Some(name), None, e))?;
        let before = self.state();
        let texts = self
            .shared
            .lock()
            .tree()
            .map(crate::profile::tag_texts)
            .unwrap_or_default();
        if let Some(req) = &action.require {
            let wait = match (req.within, req.hold) {
                (Some(w), _) => Some(w.0),
                (None, Some(h)) => Some(h.0 + Duration::from_secs(2)),
                (None, None) => None,
            };
            let r = match wait {
                Some(w) => self.wait_for(req, w).await.map(|_| ()),
                None => {
                    // The lock must be released before the error takes a snapshot.
                    let checked = self.shared.lock().check(req);
                    checked.map_err(|e| self.error(TuiErrorKind::Require, Some(name), None, e))
                }
            };
            if let Err(mut e) = r {
                e.kind = TuiErrorKind::Require;
                e.action = Some(name.to_string());
                return Err(e);
            }
        }
        for (i, step) in action.steps.iter().enumerate() {
            let n = i + 1;
            match step {
                Step::Key(keys) => {
                    for k in keys {
                        self.input(name, n, self.session.press(k)).await?;
                    }
                }
                Step::Type(t) => self.input(name, n, self.session.type_text(t)).await?,
                Step::Paste(t) => self.input(name, n, self.session.paste(t)).await?,
                Step::Wait { cond, timeout } => {
                    if let Err(mut e) = self.wait_for(cond, *timeout).await {
                        e.kind = TuiErrorKind::Step;
                        e.action = Some(name.to_string());
                        e.step = Some(n);
                        return Err(e);
                    }
                }
                Step::Select(sel) => {
                    if let Err(mut e) = self.select(sel).await {
                        e.kind = TuiErrorKind::Step;
                        e.action = Some(name.to_string());
                        e.step = Some(n);
                        return Err(e);
                    }
                }
            }
        }
        if let Some(exp) = &action.expect {
            let within = exp.within.map_or(DEFAULT_EXPECT, |d| d.0);
            if let Err(mut e) = self.wait_since(exp, within, Some(&texts)).await {
                e.kind = TuiErrorKind::Expect;
                e.action = Some(name.to_string());
                return Err(e);
            }
        }
        Ok(ActionReport {
            action: name.to_string(),
            state_before: before,
            state_after: self.state(),
            steps: action.steps.len(),
            millis: started.elapsed().as_millis() as u64,
        })
    }

    /// Moves a menu's selection to an item, one arrow key at a time, waiting for the
    /// selection to move after each, then presses Enter if asked.
    async fn select(&self, sel: &SelectStep) -> Result<(), TuiError> {
        let timeout = sel.timeout.map_or(DEFAULT_SELECT, |d| d.0);
        let hold = sel.hold.map_or(DEFAULT_SELECT_HOLD, |d| d.0);
        self.move_selection(
            &MenuRef::Tag(sel.menu.clone()),
            &|items| pick(items, sel),
            timeout,
            hold,
        )
        .await?;
        if sel.confirm {
            self.session
                .press("enter")
                .await
                .map_err(|e| self.error(TuiErrorKind::Input, None, None, e.to_string()))?;
        }
        Ok(())
    }

    /// Chooses option `index` of a generic surface (by the surface's id), with arrows and
    /// Enter. Works without a profile.
    pub async fn choose(&self, surface: &str, index: usize) -> Result<(), TuiError> {
        let menu = self
            .surfaces()
            .into_iter()
            .find(|s| s.id == surface)
            .and_then(|s| s.menu)
            .ok_or_else(|| {
                self.error(
                    TuiErrorKind::Step,
                    None,
                    None,
                    format!("no surface {surface} with options"),
                )
            })?;
        self.move_selection(
            &MenuRef::Id(menu),
            &|items| {
                (index < items.len())
                    .then_some(index)
                    .ok_or_else(|| format!("no option {index}"))
            },
            DEFAULT_SELECT,
            DEFAULT_SELECT_HOLD,
        )
        .await?;
        self.session
            .press("enter")
            .await
            .map_err(|e| self.error(TuiErrorKind::Input, None, None, e.to_string()))
    }

    async fn move_selection(
        &self,
        menu: &MenuRef,
        target: &(dyn Fn(&[&Node]) -> Result<usize, String> + Send + Sync),
        timeout: Duration,
        hold: Duration,
    ) -> Result<(), TuiError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rx = self.shared.version.subscribe();
        let mut last_press: Option<usize> = None;
        let mut at_target: Option<tokio::time::Instant> = None;
        loop {
            rx.borrow_and_update();
            let found = {
                let t = self.shared.lock();
                t.tree().and_then(|tree| {
                    let m = match menu {
                        MenuRef::Tag(tag) => tree.find_tag(tag),
                        MenuRef::Id(id) => tree.by_id(id),
                    }?;
                    let items = m.items();
                    Some((target(&items), m.selected_index(), items.len()))
                })
            };
            match found {
                Some((Err(e), _, _)) => {
                    return Err(self.error(TuiErrorKind::Step, None, None, e));
                }
                Some((Ok(want), Some(cur), _)) if cur == want => {
                    let now = tokio::time::Instant::now();
                    if now - *at_target.get_or_insert(now) >= hold {
                        return Ok(());
                    }
                }
                Some((Ok(want), Some(cur), _)) if last_press != Some(cur) => {
                    at_target = None;
                    let key = if want > cur { "down" } else { "up" };
                    self.session
                        .press(key)
                        .await
                        .map_err(|e| self.error(TuiErrorKind::Input, None, None, e.to_string()))?;
                    last_press = Some(cur);
                }
                _ => at_target = None,
            }
            let wake = at_target.map_or(deadline, |s| (s + hold).min(deadline));
            match tokio::time::timeout_at(wake, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return Err(self.error(TuiErrorKind::Closed, None, None, "session closed"));
                }
                Err(_) if wake < deadline => {}
                Err(_) => {
                    let what = match menu {
                        MenuRef::Tag(t) => format!("menu tagged {t}"),
                        MenuRef::Id(i) => format!("menu {i}"),
                    };
                    return Err(self.error(
                        TuiErrorKind::Timeout,
                        None,
                        None,
                        format!("selection in {what} did not reach its target in {timeout:?}"),
                    ));
                }
            }
        }
    }
}

enum MenuRef {
    Tag(String),
    Id(String),
}

fn pick(items: &[&Node], sel: &SelectStep) -> Result<usize, String> {
    let names = || {
        items
            .iter()
            .map(|i| i.display().to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    if let Some(i) = &sel.index {
        let i = i.get().ok_or("index is not a number")?;
        let n = items.len() as i64;
        let idx = if i < 0 { n + i } else { i };
        return (0..n)
            .contains(&idx)
            .then_some(idx as usize)
            .ok_or_else(|| format!("no option {i}; options: {}", names()));
    }
    let want = sel
        .option
        .as_ref()
        .or(sel.label.as_ref())
        .ok_or("nothing to select")?;
    let w = want.to_lowercase();
    if sel.option.is_some()
        && let Some(i) = items.iter().position(|it| {
            it.key.as_deref() == Some(want.as_str()) || it.shortcut.as_deref() == Some(want.as_str())
        })
    {
        return Ok(i);
    }
    items
        .iter()
        .position(|it| it.display().to_lowercase().contains(&w))
        .or_else(|| items.iter().position(|it| it.text.to_lowercase().contains(&w)))
        .ok_or_else(|| format!("no option {want:?}; options: {}", names()))
}
