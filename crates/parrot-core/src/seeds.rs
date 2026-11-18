//! Seeds (spec 9.3 within one machine, 9.4, 23, 35, Appendix E6, E9): fork, handoff, rebind,
//! rewind, bundles and checkpoints.

use crate::principals::{check_session, parse};
use crate::{CallOwner, Cx, Effect, Reply, State};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::session::{BindingStatus, BindingVia, SessionRebindParams, SessionRewindParams};
use parrot_proto::state::State as SessionState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What seeds keep per session and per call.
#[derive(Debug, Default)]
pub struct Seeds {
    pub by_session: BTreeMap<String, SeedState>,
    /// Rebinds in progress, by session. At most one per session.
    pub rebinding: BTreeMap<String, Rebind>,
    /// What to do once a binding has started, by binding.
    pub after_start: BTreeMap<String, AfterStart>,
    /// Workspace calls this module waits on.
    pub calls: BTreeMap<u64, SeedCall>,
    /// Turn ids of each session in the order they started.
    pub turns: BTreeMap<String, Vec<String>>,
    /// Summaries the source harness is writing, by source session.
    pub summarizing: BTreeMap<String, Vec<SummaryJob>>,
    /// Finished summaries a re-run `session/new` picks up, by token.
    pub summaries: BTreeMap<String, String>,
    /// Stopped sources started only to fork or summarize; they stop again once that is done.
    pub woken: std::collections::BTreeSet<String>,
}

/// `summary: "source"`: a turn on the source asks its harness for a summary (spec 23.2).
#[derive(Debug, Clone)]
pub struct SummaryJob {
    /// The summarizing prompt's message.
    pub message: String,
    /// The source's last turn before the summary, where the bundle is cut.
    pub before: Option<String>,
    pub conn: String,
    pub id: Option<RequestId>,
    pub then: AfterSummary,
}

#[derive(Debug, Clone)]
pub enum AfterSummary {
    Bundle { at: Option<String> },
    New { params: Value },
}

/// What the source's harness is asked. No harness has a native summarize action yet, so the
/// summary is a prompt, as `continue` is (spec 33).
pub const SUMMARY_PROMPT: &str = "Summarize this session for another agent who will continue the work: the goal, what has been done, what is left and anything they need to know. Answer with the summary only.";

/// The seed `_meta` key a re-run `session/new` finds its summary under.
const SUMMARY_KEY: &str = "x.parrot/summary";

/// Stored per session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedState {
    /// A bundle waiting to be prepended to the next prompt (`Binding.pending`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Value>,
    /// The `Ref` `binding/fork` returned, which the next binding starts from (spec 30.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<Value>,
    /// The number the next checkpoint's ref gets. It only grows, so a rewind never reuses a ref.
    #[serde(default)]
    pub next: u32,
    /// Recorded checkpoints by the message of the prompt they were taken before.
    #[serde(default)]
    pub by_message: BTreeMap<String, u32>,
    /// The message each turn started with, by turn id.
    #[serde(default)]
    pub turn_input: BTreeMap<String, String>,
    /// The message of the last prompt sent whose turn hasn't been seen yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting: Option<String>,
    #[serde(default)]
    pub turns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Rebind {
    pub old: String,
    pub old_agent: String,
    pub agent: String,
    pub interrupt: bool,
    /// The new binding, once started.
    pub new: Option<String>,
    /// When the running turn is interrupted if it hasn't ended (E9: 30 seconds).
    pub deadline_ms: u64,
}

#[derive(Debug, Clone)]
pub enum AfterStart {
    /// A seed's `source` action once the target started (spec 23.1).
    Source { source: String, action: String },
    /// A rebind's new binding: stop the old one once it runs (E6).
    Rebind { session: String },
    /// A stopped source started to fork from: `binding/fork` goes out once it runs.
    Fork { session: String, plan: Box<SeedPlan> },
    /// A stopped source started to summarize itself; its summary prompt is queued.
    Summary { source: String },
}

/// Where a native fork is taken: the source's live binding and the driver's turn key.
#[derive(Debug, Clone)]
pub struct ForkFrom {
    pub binding: String,
    pub at: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SeedCall {
    /// `binding/fork` for a new session; it starts once the driver answers.
    ForkNew {
        session: String,
        plan: Box<SeedPlan>,
    },
    /// `binding/fork` for a rewind of `session`, whose `old` binding stops once the driver answers.
    ForkRewind {
        session: String,
        old: String,
    },
    /// A checkpoint taken before the prompt of `message`; the prompt waits for it.
    Checkpoint {
        session: String,
        message: String,
        number: u32,
    },
    Rewind {
        conn: String,
        id: Option<RequestId>,
    },
    Ignore,
}

/// What `session/new { seed }` decided before the session exists.
#[derive(Debug, Clone)]
pub struct SeedPlan {
    pub source: String,
    pub at: Option<String>,
    pub via: &'static str,
    /// The bundle, also kept for a native fork in case the driver refuses it.
    pub bundle: Option<Value>,
    pub fork: Option<ForkFrom>,
    pub brief: Option<String>,
    pub source_action: String,
}

/// `SeedRequest` (spec 23).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeedRequest {
    session: String,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    carry: Option<Vec<String>>,
    #[serde(default)]
    brief: Option<String>,
    #[serde(default)]
    require: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

fn has_feature(state: &State, agent: &str, feature: &str) -> bool {
    state
        .sessions
        .agents
        .get(agent)
        .and_then(|a| a.get("features"))
        .is_some_and(|f| f.get(feature).is_some())
}

/// Where to fork `session` for `binding/fork`: its live binding, or an empty binding for a
/// stopped source whose harness session can be resumed to fork from; and the driver key of
/// `turn`, from whichever of the session's bindings ran it.
fn fork_from(state: &State, session: &str, turn: Option<&str>) -> Option<ForkFrom> {
    let binding = if crate::sessions::active(state, session) {
        crate::directory::binding_of(state, session)?
    } else {
        current_ref(state, session)?;
        String::new()
    };
    let at = match turn {
        Some(t) => {
            let info = state.sessions.info.get(session)?;
            Some(info.bindings.iter().rev().find_map(|b| {
                state
                    .bindings
                    .info
                    .get(b)?
                    .turns
                    .iter()
                    .find(|(_, id)| id.as_str() == t)
                    .map(|(key, _)| key.clone())
            })?)
        }
        None => None,
    };
    Some(ForkFrom { binding, at })
}

/// Start a stopped session's binding for the runtime's own use: resumed, with its queue left as
/// it was. `then` runs once it is ready.
fn wake(cx: &mut Cx, session: &str, then: AfterStart) -> bool {
    let Some(binding) = crate::sessions::start_binding(cx, session, BindingVia::Resume) else {
        return false;
    };
    cx.state.seeds.woken.insert(session.to_string());
    cx.state.seeds.after_start.insert(binding, then);
    true
}

/// A woken session has done what it was started for: it stops again.
fn sleep_again(cx: &mut Cx, session: &str) {
    let busy = cx.state.seeds.summarizing.contains_key(session)
        || cx
            .state
            .seeds
            .after_start
            .values()
            .any(|a| matches!(a, AfterStart::Fork { plan, .. } if plan.source == session));
    if !busy && cx.state.seeds.woken.remove(session) {
        crate::sessions::stop_session(cx, session);
    }
}

fn current_ref(state: &State, session: &str) -> Option<Value> {
    let info = state.sessions.info.get(session)?;
    info.bindings
        .iter()
        .rev()
        .find_map(|b| state.bindings.info.get(b).and_then(|i| i.reference.clone()))
        .map(|r| json!(r))
}

/// `session/new` with a seed that carries the workspace and no `workspace`: a worktree on the
/// source's current commit, in the source's repository (spec 23.1). A source in a worktree is
/// followed by its branch, so the new worktree starts where the source's commits are.
pub fn fill_workspace(state: &State, mut params: Value) -> Value {
    if params.get("workspace").is_some_and(|w| !w.is_null()) {
        return params;
    }
    let Some(seed) = params.get("seed") else {
        return params;
    };
    let carries = seed
        .get("carry")
        .and_then(Value::as_array)
        .is_none_or(|c| c.iter().any(|x| x == "workspace"));
    let Some(source) = seed.get("session").and_then(Value::as_str) else {
        return params;
    };
    let Some(info) = state.sessions.info.get(source).filter(|_| carries) else {
        return params;
    };
    let Some(path) = state
        .sessions
        .workspaces
        .get(&info.workspace)
        .map(|w| w.path.clone())
    else {
        return params;
    };
    let meta = state.tree.workspaces.get(&info.workspace);
    let request = match meta.filter(|m| m.kind == "worktree") {
        Some(m) => json!({ "kind": "worktree", "repo": m.repo.clone().unwrap_or(path),
            "base": m.branch }),
        None => json!({ "kind": "worktree", "repo": path }),
    };
    params["workspace"] = request;
    params
}

/// Plan a seed for `session/new` (spec 23.1).
pub fn plan(cx: &mut Cx, conn: &str, seed: &Value, agent: &str) -> Result<SeedPlan, RpcError> {
    let req: SeedRequest = parse(seed.clone())?;
    check_session(cx.state, conn, Scope::Read, &req.session)?;
    let summary = seed
        .get("_meta")
        .and_then(|m| m.get(SUMMARY_KEY))
        .and_then(Value::as_str)
        .and_then(|t| cx.state.seeds.summaries.remove(t));
    let source_agent = cx
        .state
        .sessions
        .by_id
        .get(&req.session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let fork = (source_agent == agent && has_feature(cx.state, agent, "session.fork"))
        .then(|| fork_from(cx.state, &req.session, req.at.as_deref()))
        .flatten();
    if fork.is_none() && req.require.as_deref() == Some("exact") {
        return Err(RpcError::new(-32001, "an exact fork isn't possible")
            .with_data(json!({ "feature": "session.fork" })));
    }
    let carry_history = req
        .carry
        .as_ref()
        .is_none_or(|c| c.iter().any(|x| x == "history"));
    let bundle = carry_history.then(|| {
        let mut b = crate::bundle::build(
            cx.state,
            &req.session,
            req.at.as_deref(),
            crate::bundle::DEFAULT_CHARS,
        );
        if let Some(text) = &summary {
            b["summary"] = json!(text);
        }
        b
    });
    let via = if fork.is_some() { "fork" } else { "bundle" };
    Ok(SeedPlan {
        source: req.session,
        at: req.at,
        via,
        bundle,
        fork,
        brief: req.brief,
        source_action: req.source.unwrap_or_else(|| "keep".into()),
    })
}

/// Apply a planned seed to the new session before its binding starts.
pub fn apply(cx: &mut Cx, session: &str, plan: &SeedPlan) {
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    if plan.fork.is_none() {
        st.pending = plan.bundle.clone();
    }
    let mut seed = json!({ "session": plan.source, "via": plan.via });
    if let Some(at) = &plan.at {
        seed["at"] = json!(at);
    }
    if let Some(n) = cx.state.tree.nodes.get_mut(session) {
        n.seed = Some(seed);
    }
    persist(cx, session);
}

/// Called right after a binding is made, before `StartBinding` goes out: a native fork, a pending
/// bundle and the ceiling's pin go into its `BindingStart`.
pub fn start_extra(cx: &mut Cx, session: &str, binding: &str, start: &mut Value) -> Option<BindingVia> {
    let mut via = None;
    let ceiling = crate::ceilings::of(cx.state, session);
    if let Some(level) = ceiling.level {
        start["pin"] = json!({ "level": level, "network": ceiling.network.unwrap_or(true) });
    }
    if let Some(iso) = ceiling.isolation {
        start["isolation"] = json!(iso);
    }
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    if let Some(f) = st.fork.take() {
        // The driver's fork ref is resumed as is: the driver knows it is a fork (spec 30.1).
        start["resume"] = f;
        via = Some(BindingVia::Fork);
    } else if st.pending.is_some() {
        if let Some(o) = start.as_object_mut() {
            o.remove("resume");
        }
        via = Some(BindingVia::Bundle);
    }
    let _ = binding;
    persist(cx, session);
    via
}

/// Whether a binding has a pending bundle, for `Binding.pending`.
pub fn pending(state: &State, session: &str) -> bool {
    state
        .seeds
        .by_session
        .get(session)
        .is_some_and(|s| s.pending.is_some())
}

/// The queue is about to send a prompt: prepend a pending bundle and, for worktree workspaces,
/// record a checkpoint (spec 23.2, 35). Returns the workspace call when a checkpoint is being
/// taken: the prompt must wait for its answer, so a harness that edits files at once can't race it.
pub fn before_prompt(cx: &mut Cx, session: &str, message: Option<&str>, params: &mut Value) -> Option<u64> {
    let bundle = cx
        .state
        .seeds
        .by_session
        .get_mut(session)
        .and_then(|s| s.pending.take());
    if let Some(b) = bundle {
        let text = params.get("text").and_then(Value::as_str).map(str::to_string);
        params["text"] = json!(crate::bundle::render(&b, text.as_deref()));
    }
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    st.awaiting = message.map(str::to_string);
    let mut call = None;
    let info = cx.state.sessions.info.get(session).cloned();
    if let (Some(info), Some(message)) = (info, message)
        && crate::tree::is_worktree(cx.state, &info.workspace)
        && let Some(w) = cx.state.sessions.workspaces.get(&info.workspace).cloned()
    {
        let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
        let number = st.next;
        st.next += 1;
        let c = cx.call_workspace(
            CallOwner::Seeds,
            "checkpoint.record",
            json!({ "path": w.path, "session": session, "turn": number }),
        );
        cx.state.seeds.calls.insert(
            c,
            SeedCall::Checkpoint {
                session: session.to_string(),
                message: message.to_string(),
                number,
            },
        );
        call = Some(c);
    }
    persist(cx, session);
    call
}

/// A turn id was seen for the first time. `input` is the message it started with, when the driver
/// says; otherwise it is the last prompt sent.
pub fn on_turn(cx: &mut Cx, session: &str, turn: &str, input: Option<&str>) {
    let st = cx.state.seeds.by_session.entry(session.to_string()).or_default();
    if st.turns.iter().any(|t| t == turn) {
        return;
    }
    st.turns.push(turn.to_string());
    let message = match input {
        Some(m) => {
            if st.awaiting.as_deref() == Some(m) {
                st.awaiting = None;
            }
            Some(m.to_string())
        }
        None => st.awaiting.take(),
    };
    if let Some(m) = message {
        st.turn_input.insert(turn.to_string(), m);
    }
    let turns = st.turns.clone();
    cx.state.seeds.turns.insert(session.to_string(), turns);
    persist(cx, session);
}

/// A prompt failed before the harness started a turn for it.
pub fn on_prompt_failed(cx: &mut Cx, session: &str, message: &str) {
    if let Some(st) = cx.state.seeds.by_session.get_mut(session)
        && st.awaiting.as_deref() == Some(message)
    {
        st.awaiting = None;
    }
}

/// The checkpoint that holds the workspace as it was when `turn` ended: the one taken before the
/// next turn's prompt (spec 9.4).
fn checkpoint_after(state: &State, session: &str, turn: &str) -> Option<u32> {
    let st = state.seeds.by_session.get(session)?;
    let idx = st.turns.iter().position(|t| t == turn)?;
    let message = match st.turns.get(idx + 1) {
        Some(next) => st.turn_input.get(next)?,
        // A prompt was sent after the turn, and its turn hasn't been reported yet.
        None => st.awaiting.as_ref()?,
    };
    st.by_message.get(message).copied()
}

pub fn persist(cx: &mut Cx, session: &str) {
    if let Some(s) = cx.state.seeds.by_session.get(session) {
        let v = serde_json::to_value(s).unwrap_or_default();
        cx.persist(json!({ "op": "put", "kind": "seed", "id": session, "value": v }));
    }
}

pub fn restore(state: &mut State, session: &str, value: &Value) {
    if let Ok(s) = serde_json::from_value::<SeedState>(value.clone()) {
        state.seeds.turns.insert(session.to_string(), s.turns.clone());
        state.seeds.by_session.insert(session.to_string(), s);
    }
}

/// Whether a rebind holds the session's queue (E6).
pub fn rebinding(state: &State, session: &str) -> bool {
    state.seeds.rebinding.contains_key(session)
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "session/bundle" => bundle(cx, conn, id, params),
        "session/rebind" => Reply::Now(rebind(cx, conn, params)),
        "session/rewind" => rewind(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn bundle(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    match bundle_now(cx, conn, id, params) {
        Ok(Some(v)) => Reply::Now(Ok(v)),
        Ok(None) => Reply::Later,
        Err(e) => Reply::Now(Err(e)),
    }
}

/// Ask the source's harness for a summary; the call continues in `on_idle` (spec 23.2).
fn summarize(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    source: &str,
    at: Option<String>,
    then: AfterSummary,
) -> Result<(), RpcError> {
    let stopped = !crate::sessions::active(cx.state, source);
    if stopped && current_ref(cx.state, source).is_none() {
        return Err(RpcError::new(
            -32004,
            "a stopped source needs a harness session to resume to summarize itself",
        )
        .with_data(json!({ "state": "stopped" })));
    }
    let before = at.or_else(|| cx.state.seeds.turns.get(source).and_then(|t| t.last().cloned()));
    let message = crate::queue::enqueue(
        cx,
        source,
        parrot_proto::prompt::QueueEntryKind::Prompt,
        SUMMARY_PROMPT.to_string(),
        json!({ "kind": "runtime", "action": "summarize" }),
        json!({}),
    )
    .unwrap_or_default();
    cx.state
        .seeds
        .summarizing
        .entry(source.to_string())
        .or_default()
        .push(SummaryJob {
            message,
            before,
            conn: conn.to_string(),
            id: id.cloned(),
            then,
        });
    if stopped
        && !cx.state.seeds.woken.contains(source)
        && !wake(
            cx,
            source,
            AfterStart::Summary {
                source: source.to_string(),
            },
        )
    {
        cx.state.seeds.summarizing.remove(source);
        return Err(
            RpcError::new(-32004, "the source could not be started").with_data(json!({ "state": "stopped" }))
        );
    }
    crate::queue::dispatch(cx, source);
    Ok(())
}

/// The summary the source's harness gave for `message`: the last assistant text of its turn.
fn summary_of(state: &State, session: &str, message: &str) -> Option<String> {
    let st = state.seeds.by_session.get(session)?;
    let turn = st
        .turn_input
        .iter()
        .find(|(_, m)| m.as_str() == message)
        .map(|(t, _)| t.clone())?;
    let items = state.items.by_session.get(session)?;
    items.order.iter().rev().find_map(|i| {
        let m = items.by_id.get(i)?;
        (m.get("turn").and_then(Value::as_str) == Some(turn.as_str())
            && m.get("kind").and_then(Value::as_str) == Some("assistant"))
        .then(|| m.get("text").and_then(Value::as_str).map(str::to_string))
        .flatten()
    })
}

/// A summarizing source went idle: finish the calls whose summary turn has ended.
fn finish_summaries(cx: &mut Cx, session: &str) {
    let Some(jobs) = cx.state.seeds.summarizing.remove(session) else {
        return;
    };
    let mut waiting = Vec::new();
    for job in jobs {
        let started = cx
            .state
            .seeds
            .by_session
            .get(session)
            .is_some_and(|st| st.turn_input.values().any(|m| m == &job.message));
        if !started {
            waiting.push(job);
            continue;
        }
        let text = summary_of(cx.state, session, &job.message).unwrap_or_default();
        match job.then {
            AfterSummary::Bundle { at } => {
                let cut = at.or(job.before.clone());
                let mut b =
                    crate::bundle::build(cx.state, session, cut.as_deref(), crate::bundle::DEFAULT_CHARS);
                b["summary"] = json!(text);
                let rendered = crate::bundle::render(&b, None);
                if let Some(id) = job.id {
                    cx.reply(&job.conn, id, Ok(json!({ "bundle": b, "rendered": rendered })));
                }
            }
            AfterSummary::New { mut params } => {
                let token = cx.ids.next("sum");
                cx.state.seeds.summaries.insert(token.clone(), text);
                let seed = &mut params["seed"];
                seed["summary"] = json!("none");
                if seed.get("at").is_none()
                    && let Some(b) = &job.before
                {
                    seed["at"] = json!(b);
                }
                seed["_meta"] = json!({ SUMMARY_KEY: token });
                let r = crate::sessions::call(cx, &job.conn, job.id.as_ref(), "session/new", params);
                if let (Reply::Now(result), Some(id)) = (r, job.id) {
                    cx.reply(&job.conn, id, result);
                }
            }
        }
    }
    if !waiting.is_empty() {
        cx.state.seeds.summarizing.insert(session.to_string(), waiting);
    }
    sleep_again(cx, session);
}

/// `session/new` with `summary: "source"`: the session is made once the source has summarized.
/// Returns false when the seed asks for no summary.
pub fn summarize_first(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    params: &Value,
) -> Result<bool, RpcError> {
    let Some(seed) = params.get("seed") else {
        return Ok(false);
    };
    if seed.get("summary").and_then(Value::as_str) != Some("source") {
        return Ok(false);
    }
    let source = seed
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: seed.session"))?
        .to_string();
    check_session(cx.state, conn, Scope::Delegate, &source)?;
    let at = seed.get("at").and_then(Value::as_str).map(str::to_string);
    summarize(
        cx,
        conn,
        id,
        &source,
        at,
        AfterSummary::New {
            params: params.clone(),
        },
    )?;
    Ok(true)
}

fn bundle_now(
    cx: &mut Cx,
    conn: &str,
    id: Option<&RequestId>,
    params: Value,
) -> Result<Option<Value>, RpcError> {
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: session"))?
        .to_string();
    let summary = params.get("summary").and_then(Value::as_str);
    check_session(
        cx.state,
        conn,
        if summary.is_some() {
            Scope::Delegate
        } else {
            Scope::Read
        },
        &session,
    )?;
    let at = params.get("at").and_then(Value::as_str);
    if summary == Some("source") {
        let at = at.map(str::to_string);
        summarize(cx, conn, id, &session, at.clone(), AfterSummary::Bundle { at })?;
        return Ok(None);
    }
    let b = crate::bundle::build(cx.state, &session, at, crate::bundle::DEFAULT_CHARS);
    let rendered = crate::bundle::render(&b, None);
    Ok(Some(json!({ "bundle": b, "rendered": rendered })))
}

/// `session/rebind` within one machine (spec 9.3, E6, E9). Returns at once; the outcome arrives
/// as a `handoff` item.
fn rebind(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: SessionRebindParams = parse(params)?;
    check_session(cx.state, conn, Scope::Delegate, &p.session)?;
    if p.env.as_ref().is_some_and(|e| e != &cx.state.principals.env) || p.workspace.is_some() {
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "profile.link" })));
    }
    if rebinding(cx.state, &p.session) {
        return Err(
            RpcError::new(-32004, "a rebind is in progress").with_data(json!({ "state": "rebinding" }))
        );
    }
    let old_agent = cx
        .state
        .sessions
        .by_id
        .get(&p.session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let agent = p.agent.clone().unwrap_or_else(|| old_agent.clone());
    if !cx.state.sessions.agents.contains_key(&agent) {
        return Err(RpcError::new(-32002, format!("agent not found: {agent}")));
    }
    let ceiling = crate::ceilings::of(cx.state, &p.session);
    crate::ceilings::check_agent(&ceiling, &agent, &cx.state.principals.env)?;
    let same = agent == old_agent && current_ref(cx.state, &p.session).is_some();
    if p.require == Some(parrot_proto::session::RebindRequire::Exact) && !same {
        return Err(RpcError::new(-32001, "an exact rebind isn't possible")
            .with_data(json!({ "feature": "session.resume" })));
    }
    let old = crate::directory::binding_of(cx.state, &p.session).unwrap_or_default();
    cx.state.seeds.rebinding.insert(
        p.session.clone(),
        Rebind {
            old,
            old_agent,
            agent,
            interrupt: p.interrupt.unwrap_or(false),
            new: None,
            deadline_ms: cx.now_ms + 30_000,
        },
    );
    let running = cx
        .state
        .sessions
        .info
        .get(&p.session)
        .is_some_and(|i| matches!(i.state, SessionState::Running | SessionState::RequiresAction));
    if running && crate::sessions::active(cx.state, &p.session) {
        if p.interrupt.unwrap_or(false) {
            cancel_turn(cx, &p.session);
        } else {
            cx.emit(Effect::TimerAt {
                at_ms: cx.now_ms + 30_000,
            });
        }
    } else {
        proceed(cx, &p.session);
    }
    let b = crate::directory::binding_of(cx.state, &p.session)
        .and_then(|b| crate::bindings::binding_json(cx.state, &b));
    Ok(json!({ "binding": b }))
}

fn cancel_turn(cx: &mut Cx, session: &str) {
    if let Some(b) = crate::directory::binding_of(cx.state, session) {
        cx.call_driver(CallOwner::Seeds, &b, "binding/cancel", json!({ "binding": b }));
    }
}

/// Stop one binding, whether or not it is the session's current one.
fn stop_binding(cx: &mut Cx, binding: &str) {
    let live = cx
        .state
        .bindings
        .info
        .get(binding)
        .is_some_and(|b| matches!(b.status, BindingStatus::Starting | BindingStatus::Running));
    if !live {
        return;
    }
    if let Some(b) = cx.state.bindings.info.get_mut(binding) {
        b.stop_requested = true;
    }
    cx.call_driver(
        CallOwner::Seeds,
        binding,
        "binding/stop",
        json!({ "binding": binding }),
    );
    cx.emit(Effect::StopBinding {
        binding: binding.to_string(),
    });
}

/// No turn runs any more: start the new binding (E9 order).
fn proceed(cx: &mut Cx, session: &str) {
    let Some(r) = cx.state.seeds.rebinding.get(session).cloned() else {
        return;
    };
    if r.new.is_some() {
        return;
    }
    let same_harness = r.agent == r.old_agent && current_ref(cx.state, session).is_some();
    if !same_harness {
        // A bundle carries the history to the new harness.
        let b = crate::bundle::build(cx.state, session, None, crate::bundle::DEFAULT_CHARS);
        cx.state
            .seeds
            .by_session
            .entry(session.to_string())
            .or_default()
            .pending = Some(b);
    } else {
        // The same harness session resumes: the old binding stops first.
        stop_binding(cx, &r.old);
    }
    if let Some(rec) = cx.state.sessions.by_id.get_mut(session) {
        rec.agent = r.agent.clone();
    }
    let via = if same_harness {
        BindingVia::Resume
    } else {
        BindingVia::Bundle
    };
    let new = crate::sessions::start_binding(cx, session, via);
    if let Some(n) = &new {
        cx.state.seeds.after_start.insert(
            n.clone(),
            AfterStart::Rebind {
                session: session.to_string(),
            },
        );
    }
    if let Some(rb) = cx.state.seeds.rebinding.get_mut(session) {
        rb.new = new;
    }
}

/// A session went idle: a waiting rebind goes on.
pub fn on_idle(cx: &mut Cx, session: &str) {
    finish_summaries(cx, session);
    if cx
        .state
        .seeds
        .rebinding
        .get(session)
        .is_some_and(|r| r.new.is_none())
    {
        proceed(cx, session);
    }
}

/// Time passed: interrupt turns a rebind has waited 30 seconds for.
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let due: Vec<String> = cx
        .state
        .seeds
        .rebinding
        .iter()
        .filter(|(_, r)| r.new.is_none() && !r.interrupt && r.deadline_ms <= now)
        .map(|(s, _)| s.clone())
        .collect();
    for s in due {
        if let Some(r) = cx.state.seeds.rebinding.get_mut(&s) {
            r.interrupt = true;
        }
        cancel_turn(cx, &s);
    }
}

/// A binding answered `binding/start`. Runs the step that waited for it.
pub fn on_started(cx: &mut Cx, session: &str, binding: &str, ok: bool) {
    let Some(after) = cx.state.seeds.after_start.remove(binding) else {
        return;
    };
    match after {
        AfterStart::Source { source, action } => {
            if !ok {
                return;
            }
            match action.as_str() {
                "stop" => crate::sessions::stop_session(cx, &source),
                "archive" => {
                    crate::sessions::stop_session(cx, &source);
                    if let Some(i) = cx.state.sessions.info.get_mut(&source) {
                        i.archived = true;
                    }
                    crate::sessions::changed(cx, &source);
                }
                _ => {}
            }
        }
        AfterStart::Fork { session: s, mut plan } => {
            if !ok || !cx.state.sessions.by_id.contains_key(&s) {
                cx.state.seeds.woken.remove(&plan.source);
                fall_back(cx, &s, plan.bundle.clone());
                if let Some(b) = crate::sessions::start_binding(cx, &s, BindingVia::New) {
                    after_seed(cx, &b, &plan);
                }
                crate::sessions::changed(cx, &s);
                return;
            }
            if let Some(from) = plan.fork.as_mut() {
                from.binding = binding.to_string();
            }
            start_seeded(cx, &s, &plan);
        }
        AfterStart::Summary { source } => {
            if ok {
                crate::queue::dispatch(cx, &source);
                return;
            }
            cx.state.seeds.woken.remove(&source);
            for job in cx.state.seeds.summarizing.remove(&source).unwrap_or_default() {
                if let Some(id) = job.id {
                    let e = RpcError::new(-32020, "the source could not be started to summarize itself");
                    cx.reply(&job.conn, id, Err(e));
                }
            }
        }
        AfterStart::Rebind { session: s } => {
            let Some(r) = cx.state.seeds.rebinding.remove(&s) else {
                return;
            };
            let item = cx.ids.next("itm");
            if ok {
                stop_binding(cx, &r.old);
                let via = cx
                    .state
                    .bindings
                    .info
                    .get(binding)
                    .map(|b| json!(b.via))
                    .unwrap_or(json!("bundle"));
                cx.publish(
                    Some(s.clone()),
                    json!({ "type": "item", "item": { "id": item, "kind": "handoff", "source": "runtime",
                        "set": { "from": r.old, "to": binding, "via": via, "status": "ok" } } }),
                );
            } else {
                // The old binding starts again where it can, and the handoff is reported failed.
                if let Some(rec) = cx.state.sessions.by_id.get_mut(&s) {
                    rec.agent = r.old_agent.clone();
                }
                if let Some(st) = cx.state.seeds.by_session.get_mut(&s) {
                    st.pending = None;
                }
                let live =
                    cx.state.bindings.info.get(&r.old).is_some_and(|b| {
                        matches!(b.status, BindingStatus::Starting | BindingStatus::Running)
                    });
                if live {
                    if let Some(rec) = cx.state.sessions.by_id.get_mut(&s) {
                        rec.binding = Some(r.old.clone());
                    }
                    if let Some(i) = cx.state.sessions.info.get_mut(&s) {
                        i.state = SessionState::Idle;
                    }
                } else {
                    crate::sessions::start_binding(cx, &s, BindingVia::Resume);
                }
                cx.publish(
                    Some(s.clone()),
                    json!({ "type": "item", "item": { "id": item, "kind": "handoff", "source": "runtime",
                        "set": { "from": r.old, "to": binding, "status": "error" } } }),
                );
            }
            crate::sessions::changed(cx, &s);
            crate::queue::dispatch(cx, &s);
        }
    }
    let _ = session;
}

/// Start a seeded session's first binding: at once for a bundle, after `binding/fork` on the
/// source's binding for a native fork.
pub fn start_seeded(cx: &mut Cx, session: &str, plan: &SeedPlan) {
    match &plan.fork {
        // The source is stopped: it is resumed to fork from, then stops again.
        Some(from) if from.binding.is_empty() => {
            let then = AfterStart::Fork {
                session: session.to_string(),
                plan: Box::new(plan.clone()),
            };
            if !wake(cx, &plan.source, then) {
                fall_back(cx, session, plan.bundle.clone());
                if let Some(b) = crate::sessions::start_binding(cx, session, BindingVia::New) {
                    after_seed(cx, &b, plan);
                }
            }
        }
        Some(from) => {
            let mut params = json!({ "binding": from.binding });
            if let Some(at) = &from.at {
                params["at"] = json!(at);
            }
            let call = cx.call_driver(CallOwner::Seeds, &from.binding, "binding/fork", params);
            cx.state.seeds.calls.insert(
                call,
                SeedCall::ForkNew {
                    session: session.to_string(),
                    plan: Box::new(plan.clone()),
                },
            );
        }
        None => {
            if let Some(b) = crate::sessions::start_binding(cx, session, BindingVia::New) {
                after_seed(cx, &b, plan);
            }
        }
    }
}

/// A seed that won't be started now: a fork can only be taken from a live binding, so the bundle
/// stands in for it.
pub fn defer_seeded(cx: &mut Cx, session: &str, plan: &SeedPlan) {
    if plan.fork.is_some() {
        fall_back(cx, session, plan.bundle.clone());
    }
}

/// A native fork wasn't possible: the session gets the bundle instead.
fn fall_back(cx: &mut Cx, session: &str, bundle: Option<Value>) {
    let b =
        bundle.unwrap_or_else(|| crate::bundle::build(cx.state, session, None, crate::bundle::DEFAULT_CHARS));
    cx.state
        .seeds
        .by_session
        .entry(session.to_string())
        .or_default()
        .pending = Some(b);
    if let Some(n) = cx.state.tree.nodes.get_mut(session)
        && let Some(seed) = n.seed.as_mut()
    {
        seed["via"] = json!("bundle");
    }
    persist(cx, session);
}

/// Remember a seed's `source` action for when the new binding started.
pub fn after_seed(cx: &mut Cx, binding: &str, plan: &SeedPlan) {
    if plan.source_action != "keep" {
        cx.state.seeds.after_start.insert(
            binding.to_string(),
            AfterStart::Source {
                source: plan.source.clone(),
                action: plan.source_action.clone(),
            },
        );
    }
}

/// `session/rewind` (spec 9.4).
fn rewind(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    let p: SessionRewindParams = match parse(params) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(e)),
    };
    if let Err(e) = check_session(cx.state, conn, Scope::Destructive, &p.session) {
        return Reply::Now(Err(e));
    }
    let turns = cx.state.seeds.turns.get(&p.session).cloned().unwrap_or_default();
    let Some(idx) = turns.iter().position(|t| t == &p.turn) else {
        return Reply::Now(Err(RpcError::new(-32002, format!("turn not found: {}", p.turn))));
    };
    let info = cx
        .state
        .sessions
        .info
        .get(&p.session)
        .cloned()
        .unwrap_or_default();
    let checkpoint = checkpoint_after(cx.state, &p.session, &p.turn);
    let mut restore = None;
    if p.files {
        if !crate::tree::is_worktree(cx.state, &info.workspace) {
            return Reply::Now(Err(RpcError::new(
                -32004,
                "rewinding files needs a worktree workspace",
            )
            .with_data(json!({ "reason": "dir" }))));
        }
        let others = cx
            .state
            .sessions
            .info
            .iter()
            .any(|(s, i)| s != &p.session && i.workspace == info.workspace);
        if others {
            return Reply::Now(Err(RpcError::new(-32004, "other sessions use the workspace")));
        }
        if checkpoint.is_none() {
            return Reply::Now(Err(RpcError::new(-32004, "there is no checkpoint for that turn")));
        }
        restore = cx
            .state
            .sessions
            .workspaces
            .get(&info.workspace)
            .map(|w| w.path.clone());
    }
    // Items after the turn are removed.
    let keep: Vec<String> = turns[..=idx].to_vec();
    let removed: Vec<String> = cx
        .state
        .items
        .by_session
        .get(&p.session)
        .map(|items| {
            let last = items
                .order
                .iter()
                .rposition(|i| {
                    items
                        .by_id
                        .get(i)
                        .and_then(|m| m.get("turn"))
                        .and_then(Value::as_str)
                        == Some(p.turn.as_str())
                })
                .unwrap_or(0);
            items
                .order
                .iter()
                .enumerate()
                .filter(|(n, i)| {
                    *n > last
                        || items
                            .by_id
                            .get(*i)
                            .and_then(|m| m.get("turn"))
                            .and_then(Value::as_str)
                            .is_some_and(|t| !keep.iter().any(|k| k == t))
                })
                .map(|(_, i)| i.clone())
                .collect()
        })
        .unwrap_or_default();
    for item in &removed {
        cx.publish(
            Some(p.session.clone()),
            json!({ "type": "item", "item": { "id": item, "removed": true } }),
        );
    }
    let bundle = crate::bundle::build(cx.state, &p.session, Some(&p.turn), crate::bundle::DEFAULT_CHARS);
    let agent = cx
        .state
        .sessions
        .by_id
        .get(&p.session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let reference = current_ref(cx.state, &p.session);
    {
        let st = cx.state.seeds.by_session.entry(p.session.clone()).or_default();
        st.turns = keep.clone();
        st.turn_input.retain(|t, _| keep.contains(t));
        st.awaiting = None;
        st.pending = Some(bundle);
    }
    let fork = if has_feature(cx.state, &agent, "session.fork") && reference.is_some() {
        fork_from(cx.state, &p.session, Some(&p.turn))
    } else {
        None
    };
    cx.state.seeds.turns.insert(p.session.clone(), keep);
    persist(cx, &p.session);
    let was_active = crate::sessions::active(cx.state, &p.session);
    match fork {
        // The harness forks at the turn; the bundle stays in case it refuses.
        Some(from) => {
            let params = json!({ "binding": from.binding, "at": from.at });
            let call = cx.call_driver(CallOwner::Seeds, &from.binding, "binding/fork", params);
            cx.state.seeds.calls.insert(
                call,
                SeedCall::ForkRewind {
                    session: p.session.clone(),
                    old: from.binding,
                },
            );
        }
        None if was_active => {
            if let Some(b) = crate::directory::binding_of(cx.state, &p.session) {
                stop_binding(cx, &b);
            }
            crate::sessions::start_binding(cx, &p.session, BindingVia::Bundle);
        }
        None => {}
    }
    match restore {
        Some(path) => {
            let call = cx.call_workspace(
                CallOwner::Seeds,
                "checkpoint.restore",
                json!({ "path": path, "session": p.session, "turn": checkpoint.unwrap_or_default() }),
            );
            cx.state.seeds.calls.insert(
                call,
                SeedCall::Rewind {
                    conn: conn.to_string(),
                    id: id.cloned(),
                },
            );
            Reply::Later
        }
        None => Reply::Now(Ok(json!({}))),
    }
}

/// Prune a deleted session's checkpoints.
pub fn on_delete(cx: &mut Cx, session: &str, workspace: &str) {
    if crate::tree::is_worktree(cx.state, workspace)
        && let Some(w) = cx.state.sessions.workspaces.get(workspace).cloned()
    {
        let call = cx.call_workspace(
            CallOwner::Seeds,
            "checkpoint.prune",
            json!({ "path": w.path, "session": session }),
        );
        cx.state.seeds.calls.insert(call, SeedCall::Ignore);
    }
    cx.state.seeds.by_session.remove(session);
    cx.state.seeds.turns.remove(session);
    cx.state.seeds.rebinding.remove(session);
    for job in cx.state.seeds.summarizing.remove(session).unwrap_or_default() {
        if let Some(id) = job.id {
            cx.reply(
                &job.conn,
                id,
                Err(RpcError::new(-32004, "the source was deleted")),
            );
        }
    }
    cx.persist(json!({ "op": "delete", "kind": "seed", "id": session }));
}

/// A driver or workspace reply to a call this module made.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(c) = cx.state.seeds.calls.remove(&call) else {
        return;
    };
    match c {
        SeedCall::Ignore => {}
        SeedCall::ForkNew { session, plan } => {
            if !cx.state.sessions.by_id.contains_key(&session) {
                return;
            }
            let reference = result.ok().and_then(|r| r.get("ref").cloned());
            match reference {
                Some(r) => {
                    let st = cx.state.seeds.by_session.entry(session.clone()).or_default();
                    st.fork = Some(r);
                    persist(cx, &session);
                }
                None => fall_back(cx, &session, plan.bundle.clone()),
            }
            if let Some(b) = crate::sessions::start_binding(cx, &session, BindingVia::New) {
                after_seed(cx, &b, &plan);
            }
            crate::sessions::changed(cx, &session);
            sleep_again(cx, &plan.source);
        }
        SeedCall::ForkRewind { session, old } => {
            let reference = result.ok().and_then(|r| r.get("ref").cloned());
            if let Some(r) = reference {
                let st = cx.state.seeds.by_session.entry(session.clone()).or_default();
                st.fork = Some(r);
                st.pending = None;
                persist(cx, &session);
            }
            stop_binding(cx, &old);
            crate::sessions::start_binding(cx, &session, BindingVia::Bundle);
        }
        SeedCall::Checkpoint {
            session,
            message,
            number,
        } => {
            match &result {
                Ok(_) => {
                    if let Some(st) = cx.state.seeds.by_session.get_mut(&session) {
                        st.by_message.insert(message, number);
                        persist(cx, &session);
                    }
                }
                Err(e) => cx.publish(
                    Some(session.clone()),
                    json!({ "type": "notice", "level": "warning",
                        "text": format!("no checkpoint for this turn: {}", e.message), "code": "checkpoint" }),
                ),
            }
            // The prompt waited for the checkpoint; it goes out now either way.
            crate::queue::checkpointed(cx, call);
        }
        SeedCall::Rewind { conn, id } => {
            if let Some(id) = id {
                cx.reply(&conn, id, result.map(|_| json!({})));
            }
        }
    }
}
