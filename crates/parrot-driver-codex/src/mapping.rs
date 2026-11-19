//! The Codex app-server's messages as driver updates (spec 30.2, Appendix C).
//!
//! [`MapState`] holds what the mapping remembers between lines, and its methods are pure: a line in,
//! the updates it causes out, so the mapping is tested without processes. Driver keys are stable:
//! Codex's item ids for items, its turn ids for turns, `req-<id>` for its requests, and the runtime's
//! message id for prompts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use parrot_proto::action::{Action, ActionScope, ActionSource, ActionWhen, Effect};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams, DriverUpdate, DriverUpdateKind, Pin};
use parrot_proto::ids::MessageId;
use parrot_proto::item::{ItemKind, ItemPatch};
use parrot_proto::open::Open;
use parrot_proto::option::{Applies, ConfigOption, DependsOn, OptionRole, PermissionLevel};
use parrot_proto::principal::Isolation;
use parrot_proto::request::{
    Answer, Answerable, Choice, ChoiceIntent, ChoiceScope, Notify, Question, Request, RequestKind,
    RequestStatus, Subject,
};
use parrot_proto::session::{McpServer, McpStatus, Ref, RefStrength};
use parrot_proto::state::{State, Turn, TurnError, TurnStatus, TurnStop};
use parrot_proto::subscribe::NoticeLevel;
use parrot_proto::usage::{ContextUsage, RateLimit, TokenUsage, Usage};
use serde_json::{Map, Value, json};

use crate::data::data;
use crate::wire::{
    self, Message, RequestId, ServerNotification, ThreadItem, ThreadTokenUsage, server_request as sr,
};

/// Longest command or tool output kept on an item.
pub const OUTPUT_MAX: usize = 20_000;

/// The level and network access an approval policy and sandbox give (spec 15.2, `levels.kdl`).
/// Unmapped values count as `host` with network.
pub fn level_of(approval: &str, sandbox: &str) -> (PermissionLevel, bool) {
    let values = BTreeMap::from([
        ("approvalPolicy".to_owned(), json!(approval)),
        ("sandbox".to_owned(), json!(sandbox)),
    ]);
    data().levels.level_of(&values)
}

/// Whether `value` is a choice of the data option `id`.
pub fn is_choice(id: &str, value: &str) -> bool {
    data().options.get(id).is_some_and(|o| o.choice(value).is_some())
}

/// The isolation a sandbox mode gives.
pub fn isolation_of(sandbox: &str) -> Isolation {
    match sandbox {
        "read-only" | "workspace-write" => Isolation::Sandbox,
        _ => Isolation::None,
    }
}

/// The `sandboxPolicy` object `turn/start` takes, where `thread/start` takes the mode's name.
pub fn sandbox_policy(sandbox: &str, cwd: &str) -> Value {
    match sandbox {
        "read-only" => json!({"type": "readOnly", "networkAccess": false}),
        "danger-full-access" => json!({"type": "dangerFullAccess"}),
        _ => json!({
            "type": "workspaceWrite",
            "writableRoots": [cwd],
            "networkAccess": false,
            "excludeTmpdirEnvVar": false,
            "excludeSlashTmp": false,
        }),
    }
}

/// The turn in progress.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnState {
    /// Codex's turn id.
    pub key: String,
    pub input: Option<MessageId>,
    pub started: String,
    pub cancelled: bool,
}

/// A request from Codex waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRequest {
    pub raw: RequestId,
    pub method: String,
    pub params: Value,
    pub request: Request,
    /// The decision each choice sends, by choice id.
    pub decisions: BTreeMap<String, Value>,
    /// Codex resolves it by itself when its time runs out (`autoResolutionMs`).
    pub auto: bool,
}

/// A skill from `skills/list`, offered as an action.
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
}

/// What the mapping remembers between lines.
#[derive(Debug, Clone, PartialEq)]
pub struct MapState {
    pub binding: String,
    pub cwd: String,
    pub thread_id: Option<String>,
    pub turn: Option<TurnState>,
    /// The key of the last turn that ended.
    pub last_turn: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub approval: String,
    pub sandbox: String,
    pub models: Vec<wire::Model>,
    pub requests: BTreeMap<String, PendingRequest>,
    /// Prompts sent whose echo, when Codex reports it, is already reported: (text, message id).
    pub echoes: Vec<(String, MessageId)>,
    /// Codex user items that were echoes.
    echoed: HashSet<String>,
    /// Kinds of the items seen, by Codex id.
    items: HashMap<String, ItemKind>,
    /// Items whose text stream is open.
    streaming: BTreeSet<String>,
    /// The paths of file changes, by item id, so an approval can name them.
    changes: HashMap<String, Vec<String>>,
    /// Reasoning text seen so far, by item id; empty reasoning items are not shown.
    reasoning: HashMap<String, String>,
    /// The latest usage, attached to the turn when it ends.
    usage: Option<Usage>,
    /// Set by the compact action, so the compaction item reads as manual.
    pub compact_requested: bool,
    unnamed: u64,
    /// The states of subagent threads, by thread id (spec 30.3). Their updates carry `subagent`.
    pub subagents: BTreeMap<String, MapState>,
    /// The subagent key this state's updates carry, when it is a subagent's thread.
    pub sub_key: Option<String>,
    /// Skills from `skills/list`, offered as actions.
    pub skills: Vec<Skill>,
    /// The running turn's latest diff (`turn/diff/updated`), put on the turn when it ends.
    diff: Option<String>,
    /// MCP servers by name, from `mcpServer/startupStatus/updated`.
    mcp: BTreeMap<String, McpStatus>,
    /// The sign-in the `login` action started: its login id and the request's key.
    pub login: Option<(String, String)>,
}

impl Default for MapState {
    fn default() -> Self {
        MapState::new("")
    }
}

impl MapState {
    pub fn new(binding: impl Into<String>) -> Self {
        MapState {
            binding: binding.into(),
            cwd: String::new(),
            thread_id: None,
            turn: None,
            model: None,
            effort: None,
            approval: "on-request".into(),
            sandbox: "workspace-write".into(),
            models: vec![],
            requests: BTreeMap::new(),
            echoes: vec![],
            echoed: HashSet::new(),
            last_turn: None,
            items: HashMap::new(),
            streaming: BTreeSet::new(),
            changes: HashMap::new(),
            reasoning: HashMap::new(),
            usage: None,
            compact_requested: false,
            unnamed: 0,
            subagents: BTreeMap::new(),
            sub_key: None,
            skills: vec![],
            diff: None,
            mcp: BTreeMap::new(),
            login: None,
        }
    }

    /// The subagent thread that a message about `thread` belongs to: any thread but this
    /// binding's own, once that is known.
    pub fn sub_thread<'a>(&self, thread: Option<&'a str>) -> Option<&'a str> {
        let t = thread?;
        match &self.thread_id {
            Some(main) if main != t && self.sub_key.is_none() => Some(t),
            _ => None,
        }
    }

    /// The state of subagent thread `thread`, made on first use with this thread's settings.
    pub fn sub(&mut self, thread: &str) -> &mut MapState {
        if !self.subagents.contains_key(thread) {
            let mut s = MapState::new(self.binding.clone());
            s.cwd = self.cwd.clone();
            s.thread_id = Some(thread.to_owned());
            s.sub_key = Some(thread.to_owned());
            s.approval = self.approval.clone();
            s.sandbox = self.sandbox.clone();
            s.model = self.model.clone();
            self.subagents.insert(thread.to_owned(), s);
        }
        self.subagents.get_mut(thread).expect("just made")
    }

    /// [`MapState::step`] for a notification about `thread`; a subagent's goes to its state.
    pub fn step_in(
        &mut self,
        thread: Option<&str>,
        n: &Open<ServerNotification>,
        now: &str,
    ) -> Vec<DriverUpdate> {
        match self.sub_thread(thread).map(str::to_owned) {
            Some(t) => self.sub(&t).step(n, now),
            None => self.step(n, now),
        }
    }

    /// [`MapState::server_request_at`] for a request about `thread`.
    pub fn server_request_in(
        &mut self,
        thread: Option<&str>,
        raw: &RequestId,
        method: &str,
        params: &Value,
        now: &str,
    ) -> (Vec<DriverUpdate>, Option<Message>) {
        match self.sub_thread(thread).map(str::to_owned) {
            Some(t) => self.sub(&t).server_request_at(raw, method, params, now),
            None => self.server_request_at(raw, method, params, now),
        }
    }

    fn update(&self, kind: DriverUpdateKind) -> DriverUpdate {
        DriverUpdate {
            binding: self.binding.clone(),
            subagent: self.sub_key.clone(),
            update: Open::Known(kind),
            extra: Map::new(),
        }
    }

    fn item(&self, patch: ItemPatch) -> DriverUpdate {
        self.update(DriverUpdateKind::Item { item: patch })
    }

    fn state(&self, state: State) -> DriverUpdate {
        self.update(DriverUpdateKind::State {
            state,
            background: vec![],
            detail: None,
        })
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.unnamed += 1;
        format!("{prefix}-{}", self.unnamed)
    }

    fn turn_key(&self) -> Option<String> {
        self.turn.as_ref().map(|t| t.key.clone())
    }

    pub fn level(&self) -> PermissionLevel {
        level_of(&self.approval, &self.sandbox).0
    }

    pub fn network(&self) -> bool {
        level_of(&self.approval, &self.sandbox).1
    }

    pub fn isolation(&self) -> Isolation {
        isolation_of(&self.sandbox)
    }

    /// Lowers the approval policy and sandbox to fit `pin`, if they are above it.
    pub fn apply_pin(&mut self, pin: &Pin) {
        let (level, network) = level_of(&self.approval, &self.sandbox);
        if level <= pin.level && (pin.network || !network) {
            return;
        }
        let (approval, sandbox) = match pin.level {
            PermissionLevel::Read => ("on-request", "read-only"),
            PermissionLevel::Write => ("untrusted", "workspace-write"),
            PermissionLevel::Sandboxed | PermissionLevel::Host => ("on-request", "workspace-write"),
        };
        self.approval = approval.into();
        self.sandbox = sandbox.into();
    }

    /// Takes the visible models from `model/list`.
    pub fn models_listed(&mut self, list: &wire::ModelList) {
        self.models = list.data.iter().filter(|m| !m.hidden).cloned().collect();
    }

    fn default_model(&self) -> Option<&wire::Model> {
        self.models
            .iter()
            .find(|m| m.is_default)
            .or_else(|| self.models.first())
    }

    fn current_model(&self) -> Option<&wire::Model> {
        match &self.model {
            Some(id) => self
                .models
                .iter()
                .find(|m| m.id == *id || m.model.as_deref() == Some(id)),
            None => self.default_model(),
        }
    }

    /// `model`, `effort`, `approvalPolicy` and `sandbox`, with their current values.
    pub fn options(&self) -> Vec<ConfigOption> {
        let model_schema = if self.models.is_empty() {
            json!({"type": "string"})
        } else {
            let one_of: Vec<Value> = self
                .models
                .iter()
                .map(|m| {
                    let mut e = json!({"const": m.id, "title": m.display_name.clone().unwrap_or_else(|| m.id.clone())});
                    if let Some(d) = &m.description {
                        e["description"] = json!(d);
                    }
                    e
                })
                .collect();
            json!({"type": "string", "oneOf": one_of})
        };
        let model_value = self
            .model
            .clone()
            .or_else(|| self.default_model().map(|m| m.id.clone()));
        let mut model = option(
            "model",
            OptionRole::Model,
            "Model",
            model_schema,
            model_value.map_or(Value::Null, Value::String),
        );
        model.default = self.default_model().map(|m| json!(m.id));

        let mut schemas = Map::new();
        let mut all: Vec<(String, Option<String>)> = Vec::new();
        for m in &self.models {
            let efforts: Vec<Value> = m
                .supported_reasoning_efforts
                .iter()
                .map(|e| effort_entry(&e.reasoning_effort, e.description.as_deref()))
                .collect();
            for e in &m.supported_reasoning_efforts {
                if !all.iter().any(|(id, _)| *id == e.reasoning_effort) {
                    all.push((e.reasoning_effort.clone(), e.description.clone()));
                }
            }
            let mut s = json!({"type": "string", "oneOf": efforts});
            if let Some(d) = &m.default_reasoning_effort {
                s["default"] = json!(d);
            }
            schemas.insert(m.id.clone(), s);
        }
        let effort_schema = if all.is_empty() {
            json!({"type": "string"})
        } else {
            json!({"type": "string", "oneOf": all.iter().map(|(id, d)| effort_entry(id, d.as_deref())).collect::<Vec<_>>()})
        };
        let effort_value = self.effort.clone().or_else(|| {
            self.current_model()
                .and_then(|m| m.default_reasoning_effort.clone())
        });
        let mut effort = option(
            "effort",
            OptionRole::Effort,
            "Reasoning effort",
            effort_schema,
            effort_value.map_or(Value::Null, Value::String),
        );
        effort.default = self
            .current_model()
            .and_then(|m| m.default_reasoning_effort.clone())
            .map(Value::String);
        if !schemas.is_empty() {
            effort.depends_on = Some(DependsOn {
                option: "model".into(),
                schemas,
            });
        }

        let options = &data().options;
        let from_data =
            |id: &str, value: &str| options.get(id).map(|o| o.to_option(Some(json!(value)), None));
        let approval = from_data("approvalPolicy", &self.approval).expect("options.kdl has approvalPolicy");
        let sandbox = from_data("sandbox", &self.sandbox).expect("options.kdl has sandbox");
        vec![model, effort, approval, sandbox]
    }

    pub fn options_update(&self) -> DriverUpdate {
        self.update(DriverUpdateKind::Options {
            options: self.options(),
            level: self.level(),
            network: self.network(),
        })
    }

    /// The thread is open: its ref, and the model and effort Codex chose.
    pub fn thread_ready(&mut self, result: &Value) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if let Some(id) = result.pointer("/thread/id").and_then(Value::as_str) {
            self.thread_id = Some(id.to_owned());
            out.push(self.update(DriverUpdateKind::Ref {
                reference: Ref {
                    id: id.to_owned(),
                    strength: RefStrength::Strong,
                },
            }));
        }
        if self.model.is_none()
            && let Some(m) = result.get("model").and_then(Value::as_str)
        {
            self.model = Some(m.to_owned());
        }
        if self.effort.is_none()
            && let Some(e) = result.get("reasoningEffort").and_then(Value::as_str)
        {
            self.effort = Some(e.to_owned());
        }
        out
    }

    /// The `turn/start` parameters for a prompt: context first, then the text.
    pub fn turn_start_params(&self, p: &BindingPromptParams) -> Value {
        let mut params = json!({
            "threadId": self.thread_id,
            "input": input_of(p),
            "clientUserMessageId": p.message,
            "approvalPolicy": self.approval,
            "sandboxPolicy": sandbox_policy(&self.sandbox, &self.cwd),
        });
        if let Some(m) = &self.model {
            params["model"] = json!(m);
        }
        if let Some(e) = &self.effort {
            params["effort"] = json!(e);
        }
        params
    }

    /// The `turn/steer` parameters, or `None` when no turn is running.
    pub fn turn_steer_params(&self, p: &BindingPromptParams) -> Option<Value> {
        let turn = self.turn.as_ref()?;
        Some(json!({
            "threadId": self.thread_id,
            "input": input_of(p),
            "clientUserMessageId": p.message,
            "expectedTurnId": turn.key,
        }))
    }

    /// A prompt Codex accepted: the user item, and the turn when it started one.
    pub fn prompt_accepted(
        &mut self,
        p: &BindingPromptParams,
        turn_id: Option<&str>,
        disposition: BindingDisposition,
        now: &str,
    ) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if disposition == BindingDisposition::Started
            && let Some(id) = turn_id
        {
            out.extend(self.turn_started(id, Some(p.message.clone()), now));
        }
        let mut set = Map::new();
        set.insert("text".into(), json!(p.text));
        set.insert("status".into(), json!("sent"));
        set.insert("message".into(), json!(p.message));
        if let Some(c) = p.context.as_ref().filter(|c| !c.is_empty()) {
            set.insert("context".into(), json!(c));
        }
        if let Some(a) = p.attachments.as_ref().filter(|a| !a.is_empty()) {
            // The item names the attachments; their bytes stay with the harness.
            let named: Vec<Value> = a
                .iter()
                .map(|a| {
                    let mut v = json!({"name": a.name, "type": a.media_type});
                    if let Some(u) = &a.uri {
                        v["uri"] = json!(u);
                    }
                    v
                })
                .collect();
            set.insert("attachments".into(), Value::Array(named));
        }
        out.push(self.item(ItemPatch {
            id: p.message.clone(),
            kind: Some(Open::Known(ItemKind::User)),
            turn: self.turn_key(),
            set: Some(set),
            ..Default::default()
        }));
        self.echoes.push((p.text.clone(), p.message.clone()));
        out
    }

    /// Marks the running turn as cancelled.
    pub fn cancel(&mut self) {
        if let Some(t) = self.turn.as_mut() {
            t.cancelled = true;
        }
    }

    /// A turn Codex started, or the same turn again (`turn/start`'s answer and `turn/started` both
    /// say so, in either order).
    pub fn turn_started(&mut self, id: &str, input: Option<MessageId>, now: &str) -> Vec<DriverUpdate> {
        if let Some(t) = self.turn.as_mut()
            && t.key == id
        {
            if t.input.is_none() && input.is_some() {
                t.input = input;
                let t = t.clone();
                return vec![self.turn_update(&t, TurnStatus::Running, None, None, None)];
            }
            return vec![];
        }
        let mut out = Vec::new();
        if self.turn.is_some() {
            // A turn that ended without `turn/completed`.
            out.extend(self.end_turn(Some("interrupted"), None, now));
        }
        let t = TurnState {
            key: id.to_owned(),
            input,
            started: now.to_owned(),
            cancelled: false,
        };
        self.turn = Some(t.clone());
        out.push(self.state(State::Running));
        out.push(self.turn_update(&t, TurnStatus::Running, None, None, None));
        out
    }

    fn turn_update(
        &self,
        t: &TurnState,
        status: TurnStatus,
        stop: Option<TurnStop>,
        error: Option<TurnError>,
        ended: Option<&str>,
    ) -> DriverUpdate {
        self.update(DriverUpdateKind::Turn {
            turn: Turn {
                id: t.key.clone(),
                binding: String::new(),
                status,
                stop: stop.map(Open::Known),
                continuable: matches!(stop, Some(TurnStop::Cancelled)),
                input: t.input.clone(),
                started: t.started.clone(),
                ended: ended.map(str::to_owned),
                error,
                usage: if ended.is_some() { self.usage.clone() } else { None },
                meta: ended.and(self.diff.as_ref()).map(|d| {
                    let mut m = Map::new();
                    m.insert("diff".into(), json!(d));
                    m
                }),
                extra: Map::new(),
            },
        })
    }

    fn end_turn(
        &mut self,
        status: Option<&str>,
        error: Option<&wire::TurnErrorInfo>,
        now: &str,
    ) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        let Some(t) = self.turn.take() else {
            return out;
        };
        self.last_turn = Some(t.key.clone());
        for id in std::mem::take(&mut self.streaming) {
            out.push(self.item(ItemPatch {
                id,
                streaming: Some(false),
                ..Default::default()
            }));
        }
        for p in std::mem::take(&mut self.requests).into_values() {
            let mut request = p.request;
            request.status = RequestStatus::Cancelled;
            out.push(self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            }));
        }
        let (status, stop, error) = match status {
            _ if t.cancelled => (TurnStatus::Interrupted, TurnStop::Cancelled, None),
            Some("interrupted") => (TurnStatus::Interrupted, TurnStop::Cancelled, None),
            Some("failed") => {
                let code = error
                    .and_then(|e| e.codex_error_info.as_ref())
                    .and_then(|c| {
                        c.as_str()
                            .map(str::to_owned)
                            .or_else(|| c.as_object().and_then(|o| o.keys().next().cloned()))
                    })
                    .unwrap_or_else(|| "failed".into());
                let message = error
                    .map(|e| e.message.clone())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "the turn failed".into());
                (
                    TurnStatus::Failed,
                    TurnStop::Error,
                    Some(TurnError {
                        code,
                        message,
                        retryable: true,
                    }),
                )
            }
            _ => (TurnStatus::Completed, TurnStop::End, None),
        };
        out.push(self.turn_update(&t, status, Some(stop), error, Some(now)));
        out.push(self.state(State::Idle));
        self.diff = None;
        self.echoes.clear();
        self.compact_requested = false;
        out
    }

    /// [`MapState::server_request_at`] without a clock, so a request Codex resolves by itself
    /// gets no `expires`.
    pub fn server_request(
        &mut self,
        raw: &RequestId,
        method: &str,
        params: &Value,
    ) -> (Vec<DriverUpdate>, Option<Message>) {
        self.server_request_at(raw, method, params, "")
    }

    /// A request from Codex, at time `now`. Requests the driver does not take get an error reply,
    /// returned as the second value.
    pub fn server_request_at(
        &mut self,
        raw: &RequestId,
        method: &str,
        params: &Value,
        now: &str,
    ) -> (Vec<DriverUpdate>, Option<Message>) {
        let key = format!("req-{raw}");
        if method == sr::AUTH_TOKENS_REFRESH {
            // Parrot holds no ChatGPT tokens to hand over; Codex falls back to its own sign-in.
            let error = Message::Error {
                id: raw.clone(),
                error: wire::RpcError {
                    code: -32601,
                    message: "parrot holds no ChatGPT tokens".into(),
                    data: None,
                },
            };
            let text = "Codex asked for new ChatGPT tokens. Sign in again with the login action if its requests fail.";
            return (
                vec![self.update(DriverUpdateKind::Notice {
                    level: NoticeLevel::Warning,
                    text: text.into(),
                    code: Some(AUTH_NOTICE.into()),
                })],
                Some(error),
            );
        }
        // `autoResolutionMs`: Codex answers by itself when the time runs out.
        let auto_ms = params.get("autoResolutionMs").and_then(Value::as_u64);
        let blocking = params.get("isBlocking").and_then(Value::as_bool).unwrap_or(true);
        let item = params.get("itemId").and_then(Value::as_str).map(str::to_owned);
        let reason = params.get("reason").and_then(Value::as_str).map(str::to_owned);
        let mut decisions = BTreeMap::new();
        let mut input = None;
        // Codex's questions, typed for clients that don't read the JSON Schema in `input`.
        let typed_questions = (method == sr::USER_INPUT)
            .then(|| questions_of(params.get("questions")))
            .filter(|q| !q.is_empty());
        let (kind, title, subject, level, network, choices) = match method {
            sr::COMMAND_APPROVAL | sr::EXEC_APPROVAL_V1 => {
                let raw_cmd = match params.get("command") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                    _ => String::new(),
                };
                let command = wire::short_command(&raw_cmd);
                let cwd = params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.cwd)
                    .to_owned();
                let network = params.get("networkApprovalContext").is_some_and(|v| !v.is_null());
                let choices = if method == sr::EXEC_APPROVAL_V1 {
                    v1_choices(&mut decisions)
                } else {
                    command_choices(params, &mut decisions)
                };
                (
                    RequestKind::Approval,
                    format!("Run {command}"),
                    Subject::Command { command, cwd },
                    PermissionLevel::Host,
                    network,
                    choices,
                )
            }
            sr::FILE_CHANGE_APPROVAL | sr::PATCH_APPROVAL_V1 => {
                let paths: Vec<String> = match params.get("fileChanges").and_then(Value::as_object) {
                    Some(m) => m.keys().cloned().collect(),
                    None => item
                        .as_ref()
                        .and_then(|i| self.changes.get(i).cloned())
                        .unwrap_or_default(),
                };
                let choices = if method == sr::PATCH_APPROVAL_V1 {
                    v1_choices(&mut decisions)
                } else {
                    simple_choices(
                        &[
                            ("accept", true, ChoiceScope::Once),
                            ("acceptForSession", true, ChoiceScope::Session),
                        ],
                        &mut decisions,
                    )
                };
                let title = match params.get("grantRoot").and_then(Value::as_str) {
                    Some(root) => format!("Allow writes under {root}"),
                    None => "Edit files".into(),
                };
                (
                    RequestKind::Approval,
                    title,
                    Subject::Files { paths, diff: None },
                    PermissionLevel::Write,
                    false,
                    choices,
                )
            }
            sr::PERMISSIONS_APPROVAL => {
                let perms = params.get("permissions").cloned().unwrap_or(json!({}));
                let network = perms
                    .get("network")
                    .is_some_and(|n| !n.is_null() && n != &json!(false));
                decisions.insert("allow".into(), json!({"permissions": perms, "scope": "turn"}));
                decisions.insert(
                    "allowSession".into(),
                    json!({"permissions": perms, "scope": "session"}),
                );
                decisions.insert("deny".into(), json!({"permissions": {}, "scope": "turn"}));
                (
                    RequestKind::Approval,
                    "Allow more access".into(),
                    Subject::Tool {
                        tool: "permissions".into(),
                        input: perms,
                        editable: None,
                    },
                    PermissionLevel::Host,
                    network,
                    vec![
                        choice(
                            "allow",
                            "Allow for this turn",
                            ChoiceIntent::Allow,
                            Some(ChoiceScope::Once),
                            None,
                        ),
                        choice(
                            "allowSession",
                            "Allow for the session",
                            ChoiceIntent::Allow,
                            Some(ChoiceScope::Session),
                            None,
                        ),
                        choice("deny", "Deny", ChoiceIntent::Deny, None, None),
                    ],
                )
            }
            sr::ELICITATION => {
                let server = params
                    .get("serverName")
                    .and_then(Value::as_str)
                    .unwrap_or("mcp")
                    .to_owned();
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{server} asks to go ahead"));
                input = params.get("requestedSchema").filter(|s| !s.is_null()).cloned();
                decisions.insert("accept".into(), json!("accept"));
                decisions.insert("decline".into(), json!("decline"));
                decisions.insert("cancel".into(), json!("cancel"));
                (
                    RequestKind::Input,
                    message,
                    Subject::Tool {
                        tool: format!("mcp:{server}"),
                        input: params.clone(),
                        editable: None,
                    },
                    PermissionLevel::Host,
                    false,
                    vec![
                        choice(
                            "accept",
                            "Accept",
                            ChoiceIntent::Allow,
                            Some(ChoiceScope::Once),
                            None,
                        ),
                        choice("decline", "Decline", ChoiceIntent::Deny, None, None),
                        choice("cancel", "Cancel", ChoiceIntent::Deny, None, None),
                    ],
                )
            }
            sr::USER_INPUT => {
                let questions = params
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut props = Map::new();
                let mut required = Vec::new();
                for q in &questions {
                    let Some(id) = q.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let opts: Vec<Value> = q
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|o| {
                            o.iter()
                                .filter_map(|o| o.get("label").and_then(Value::as_str))
                                .map(|l| json!({"const": l, "title": l}))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut s =
                        json!({"type": "string", "title": q.get("question").cloned().unwrap_or(json!(id))});
                    if let Some(h) = q.get("header").and_then(Value::as_str) {
                        s["description"] = json!(h);
                    }
                    let other = q.get("isOther").and_then(Value::as_bool).unwrap_or(false);
                    if !opts.is_empty() && !other {
                        s["oneOf"] = json!(opts);
                    } else if !opts.is_empty() {
                        s["examples"] = json!(opts.iter().map(|o| o["const"].clone()).collect::<Vec<_>>());
                    }
                    props.insert(id.to_owned(), s);
                    required.push(json!(id));
                }
                input = Some(json!({"type": "object", "properties": props, "required": required}));
                let first = questions.first();
                let title = first
                    .and_then(|q| q.get("question"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex has a question")
                    .to_owned();
                let choices = if questions.len() == 1 {
                    first
                        .and_then(|q| q.get("options"))
                        .and_then(Value::as_array)
                        .map(|o| {
                            o.iter()
                                .enumerate()
                                .filter_map(|(i, o)| {
                                    let label = o.get("label")?.as_str()?;
                                    let id = format!("option-{i}");
                                    decisions.insert(id.clone(), json!(label));
                                    let mut c = choice(&id, label, ChoiceIntent::Neutral, None, None);
                                    c.description = o
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .filter(|d| !d.is_empty())
                                        .map(str::to_owned);
                                    Some(c)
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                (
                    RequestKind::Question,
                    title,
                    Subject::Tool {
                        tool: "request_user_input".into(),
                        input: Value::Array(questions),
                        editable: None,
                    },
                    PermissionLevel::Read,
                    false,
                    choices,
                )
            }
            _ => {
                let error = Message::Error {
                    id: raw.clone(),
                    error: wire::RpcError {
                        code: -32601,
                        message: format!("parrot does not answer {method}"),
                        data: None,
                    },
                };
                return (vec![], Some(error));
            }
        };
        let request = Request {
            id: key.clone(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: self.turn_key(),
            item,
            kind: Open::Known(kind),
            title,
            description: reason,
            subject: Some(Open::Known(subject)),
            level,
            network: Some(network),
            choices: (!choices.is_empty()).then_some(choices),
            questions: typed_questions,
            input,
            blocking,
            answerable: Answerable::Connection,
            expires: auto_ms.and_then(|ms| add_ms(now, ms)),
            fallback: auto_ms.map(|_| Answer {
                choice: None,
                value: None,
                answers: None,
                input: None,
                message: Some("Codex goes on without an answer".into()),
            }),
            notify: Notify::Normal,
            status: RequestStatus::Open,
            answer: None,
            answered_by: None,
            meta: None,
            extra: Map::new(),
        };
        let out = vec![
            self.update(DriverUpdateKind::Request {
                request: Box::new(request.clone()),
            }),
            self.state(State::RequiresAction),
        ];
        self.requests.insert(
            key,
            PendingRequest {
                raw: raw.clone(),
                method: method.to_owned(),
                params: params.clone(),
                request,
                decisions,
                auto: auto_ms.is_some(),
            },
        );
        (out, None)
    }

    /// The answer to the request with driver key `key`, as the response to write.
    pub fn answer(&mut self, key: &str, answer: &Answer) -> Result<(Message, Vec<DriverUpdate>), String> {
        if !self.requests.contains_key(key)
            && let Some(sub) = self.subagents.values_mut().find(|s| s.requests.contains_key(key))
        {
            return sub.answer(key, answer);
        }
        let pending = self
            .requests
            .remove(key)
            .ok_or_else(|| format!("no open request {key}"))?;
        let result = response_of(&pending, answer);
        let mut request = pending.request;
        request.status = RequestStatus::Answered;
        request.answer = Some(answer.clone());
        let mut out = vec![self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        })];
        if self.requests.is_empty() && self.turn.is_some() {
            out.push(self.state(State::Running));
        }
        Ok((
            Message::Response {
                id: pending.raw,
                result,
            },
            out,
        ))
    }

    /// One notification from Codex.
    pub fn step(&mut self, n: &Open<ServerNotification>, now: &str) -> Vec<DriverUpdate> {
        let Open::Known(n) = n else {
            return vec![];
        };
        match n {
            ServerNotification::ThreadStarted(_) => vec![],
            ServerNotification::TurnStarted(t) => self.turn_started(&t.turn.id, None, now),
            ServerNotification::TurnCompleted(t) => {
                if self.turn.as_ref().is_some_and(|c| c.key != t.turn.id) {
                    return vec![];
                }
                if self.turn.is_none() {
                    // A turn whose start was not seen, such as one a review or compaction ran.
                    let mut out = self.turn_started(&t.turn.id, None, now);
                    out.extend(self.end_turn(t.turn.status.as_deref(), t.turn.error.as_ref(), now));
                    return out;
                }
                self.end_turn(t.turn.status.as_deref(), t.turn.error.as_ref(), now)
            }
            ServerNotification::ItemStarted(i) => match &i.item {
                Open::Known(item) => self.item_started(item),
                Open::Unknown(_) => vec![],
            },
            ServerNotification::ItemCompleted(i) => match &i.item {
                Open::Known(item) => self.item_completed(item),
                Open::Unknown(_) => vec![],
            },
            ServerNotification::AgentMessageDelta(d) => {
                self.append(&d.item_id, ItemKind::Assistant, "text", &d.delta)
            }
            ServerNotification::PlanDelta(d) => self.append(&d.item_id, ItemKind::Plan, "text", &d.delta),
            ServerNotification::ReasoningSummaryDelta(d) | ServerNotification::ReasoningTextDelta(d) => {
                self.reasoning_delta(&d.item_id, &d.delta)
            }
            ServerNotification::ReasoningSummaryPartAdded(d) => {
                if self.reasoning.get(&d.item_id).is_some_and(|t| !t.is_empty()) {
                    self.reasoning_delta(&d.item_id, "\n\n")
                } else {
                    vec![]
                }
            }
            ServerNotification::CommandOutputDelta(d) => {
                self.append(&d.item_id, ItemKind::Command, "output", &d.delta)
            }
            ServerNotification::FileChangeOutputDelta(d) => {
                self.append(&d.item_id, ItemKind::FileChange, "output", &d.delta)
            }
            ServerNotification::FileChangePatchUpdated(p) => {
                self.changes.insert(
                    p.item_id.clone(),
                    p.changes.iter().map(|c| c.path.clone()).collect(),
                );
                let first = !self.items.contains_key(&p.item_id);
                if first {
                    self.items.insert(p.item_id.clone(), ItemKind::FileChange);
                }
                let mut set = Map::new();
                set.insert("changes".into(), changes_of(&p.changes));
                vec![self.item(ItemPatch {
                    id: p.item_id.clone(),
                    kind: first.then_some(Open::Known(ItemKind::FileChange)),
                    turn: if first { self.turn_key() } else { None },
                    set: Some(set),
                    ..Default::default()
                })]
            }
            ServerNotification::TurnDiffUpdated(d) => {
                if self.turn.as_ref().is_some_and(|t| t.key == d.turn_id) {
                    self.diff = Some(d.diff.clone());
                }
                vec![]
            }
            ServerNotification::Warning(w) => {
                vec![self.notice(NoticeLevel::Warning, w.message.clone(), "warning")]
            }
            ServerNotification::GuardianWarning(w) => {
                vec![self.notice(NoticeLevel::Warning, w.message.clone(), "guardian")]
            }
            ServerNotification::DeprecationNotice(d) => {
                let text = match &d.details {
                    Some(more) if !more.is_empty() => format!("{} {more}", d.summary),
                    _ => d.summary.clone(),
                };
                vec![self.notice(NoticeLevel::Info, text, "deprecation")]
            }
            ServerNotification::ThreadStatusChanged(t) => match t.status["type"].as_str() {
                Some("systemError") => vec![self.notice(
                    NoticeLevel::Error,
                    "Codex reports a system error on the thread".into(),
                    "thread-status",
                )],
                _ => vec![],
            },
            ServerNotification::McpServerStatusUpdated(m) => {
                let status = match m.status.as_str() {
                    "ready" => McpStatus::Connected,
                    "starting" => McpStatus::Pending,
                    _ if m
                        .error
                        .as_deref()
                        .is_some_and(|e| e.to_lowercase().contains("auth")) =>
                    {
                        McpStatus::NeedsAuth
                    }
                    _ => McpStatus::Failed,
                };
                if self.mcp.get(&m.name) == Some(&status) {
                    return vec![];
                }
                self.mcp.insert(m.name.clone(), status);
                let servers = self
                    .mcp
                    .iter()
                    .map(|(name, status)| McpServer {
                        name: name.clone(),
                        status: *status,
                        tools: None,
                    })
                    .collect();
                vec![self.update(DriverUpdateKind::Mcp { servers })]
            }
            ServerNotification::AccountUpdated(a) => match &a.auth_mode {
                None => vec![self.notice(NoticeLevel::Warning, "Codex is signed out".into(), AUTH_NOTICE)],
                Some(_) => vec![],
            },
            ServerNotification::AccountLoginCompleted(l) => self.login_completed(l),
            ServerNotification::SkillsChanged(_) => vec![],
            ServerNotification::TurnPlanUpdated(p) => {
                let entries: Vec<Value> = p
                    .plan
                    .iter()
                    .map(|s| {
                        let status = match s.status.as_str() {
                            "completed" => "done",
                            "inProgress" | "in_progress" => "active",
                            _ => "pending",
                        };
                        json!({"text": s.step, "status": status})
                    })
                    .collect();
                let mut set = Map::new();
                set.insert("entries".into(), Value::Array(entries));
                if let Some(e) = &p.explanation {
                    set.insert("text".into(), json!(e));
                }
                let id = format!("plan-{}", p.turn_id);
                let first = self.items.insert(id.clone(), ItemKind::Plan).is_none();
                vec![self.item(ItemPatch {
                    id,
                    kind: first.then_some(Open::Known(ItemKind::Plan)),
                    turn: self.turn_key(),
                    set: Some(set),
                    ..Default::default()
                })]
            }
            ServerNotification::TokenUsageUpdated(u) => {
                let usage = usage_of(&u.token_usage);
                self.usage = Some(usage.clone());
                vec![self.update(DriverUpdateKind::Usage { usage })]
            }
            ServerNotification::RateLimitsUpdated(r) => {
                let rate_limits = rate_limits_of(&r.rate_limits);
                if rate_limits.is_empty() {
                    return vec![];
                }
                vec![self.update(DriverUpdateKind::Usage {
                    usage: Usage {
                        rate_limits: Some(rate_limits),
                        ..Default::default()
                    },
                })]
            }
            ServerNotification::ServerRequestResolved(r) => {
                let key = format!("req-{}", r.request_id);
                let Some(p) = self.requests.remove(&key) else {
                    return vec![];
                };
                let mut request = p.request;
                if p.auto {
                    // Its time ran out and Codex went on with its own answer.
                    request.status = RequestStatus::Expired;
                    request.answered_by = Some("fallback".into());
                } else {
                    request.status = RequestStatus::Cancelled;
                    request.answered_by = Some("harness".into());
                }
                let mut out = vec![self.update(DriverUpdateKind::Request {
                    request: Box::new(request),
                })];
                if self.requests.is_empty() && self.turn.is_some() {
                    out.push(self.state(State::Running));
                }
                out
            }
            ServerNotification::ModelRerouted(m) => match &m.to_model {
                Some(model) if self.model.as_ref() != Some(model) => {
                    self.model = Some(model.clone());
                    vec![self.options_update()]
                }
                _ => vec![],
            },
            ServerNotification::Error(e) => {
                if e.will_retry {
                    vec![self.update(DriverUpdateKind::Notice {
                        level: NoticeLevel::Warning,
                        text: format!("Codex retries: {}", e.error.message),
                        code: Some("retry".into()),
                    })]
                } else {
                    let id = self.fresh("error");
                    let mut set = Map::new();
                    set.insert("message".into(), json!(e.error.message));
                    vec![self.item(ItemPatch {
                        id,
                        kind: Some(Open::Known(ItemKind::Error)),
                        turn: self.turn_key(),
                        set: Some(set),
                        ..Default::default()
                    })]
                }
            }
        }
    }

    fn notice(&self, level: NoticeLevel, text: String, code: &str) -> DriverUpdate {
        self.update(DriverUpdateKind::Notice {
            level,
            text,
            code: Some(code.to_owned()),
        })
    }

    /// The `auth` request for a sign-in the `login` action started (spec 16.1): a device code
    /// to enter at a URL, which works from any machine.
    pub fn login_started(
        &mut self,
        login_id: &str,
        url: &str,
        code: Option<&str>,
    ) -> (String, Vec<DriverUpdate>) {
        let key = format!("login-{login_id}");
        self.login = Some((login_id.to_owned(), key.clone()));
        let title = match code {
            Some(c) => format!("Sign in to Codex: open {url} and enter {c}"),
            None => format!("Sign in to Codex: open {url}"),
        };
        let mut subject = json!({"type": "url", "url": url});
        if let Some(c) = code {
            subject["code"] = json!(c);
        }
        let request = Request {
            id: key.clone(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: None,
            item: None,
            kind: Open::Known(RequestKind::Auth),
            title,
            description: Some("Codex finishes the sign-in by itself once the code is entered.".into()),
            subject: Some(Open::Unknown(subject)),
            level: PermissionLevel::Write,
            network: Some(true),
            choices: Some(vec![choice("cancel", "Cancel", ChoiceIntent::Deny, None, None)]),
            questions: None,
            input: None,
            blocking: false,
            answerable: Answerable::Connection,
            expires: None,
            fallback: None,
            notify: Notify::Normal,
            status: RequestStatus::Open,
            answer: None,
            answered_by: None,
            meta: None,
            extra: Map::new(),
        };
        let out = vec![self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        })];
        (key, out)
    }

    /// The open sign-in's request, taken so it can be closed.
    pub fn take_login(&mut self, key: &str) -> Option<String> {
        match &self.login {
            Some((id, k)) if k == key => {
                let id = id.clone();
                self.login = None;
                Some(id)
            }
            _ => None,
        }
    }

    fn login_completed(&mut self, l: &wire::LoginCompleted) -> Vec<DriverUpdate> {
        let Some((id, key)) = self.login.clone() else {
            return vec![];
        };
        if l.login_id.as_ref().is_some_and(|i| *i != id) {
            return vec![];
        }
        self.login = None;
        let mut out = vec![self.login_closed(&key, l.success)];
        if !l.success {
            let why = l.error.clone().unwrap_or_else(|| "the sign-in failed".into());
            out.push(self.notice(
                NoticeLevel::Error,
                format!("Codex sign-in failed: {why}"),
                AUTH_NOTICE,
            ));
        }
        out
    }

    /// The update that closes the sign-in request `key`.
    pub fn login_closed(&self, key: &str, success: bool) -> DriverUpdate {
        let (status, by) = if success {
            (RequestStatus::Answered, "harness")
        } else {
            (RequestStatus::Cancelled, "harness")
        };
        self.update(DriverUpdateKind::Request {
            request: Box::new(Request {
                id: key.to_owned(),
                session: String::new(),
                path: vec![],
                reference: None,
                turn: None,
                item: None,
                kind: Open::Known(RequestKind::Auth),
                title: "Sign in to Codex".into(),
                description: None,
                subject: None,
                level: PermissionLevel::Write,
                network: Some(true),
                choices: None,
                questions: None,
                input: None,
                blocking: false,
                answerable: Answerable::Connection,
                expires: None,
                fallback: None,
                notify: Notify::Normal,
                status,
                answer: None,
                answered_by: Some(by.into()),
                meta: None,
                extra: Map::new(),
            }),
        })
    }

    /// The actions: compact, review, sign-in, and a skill action per skill.
    pub fn actions(&self) -> Vec<Action> {
        let mut out = actions();
        out.extend(self.skills.iter().map(skill_action));
        out
    }

    /// Takes the skills `skills/list` returned. Returns the `actions` update when they changed.
    pub fn skills_listed(&mut self, result: &Value) -> Option<DriverUpdate> {
        let mut skills: Vec<Skill> = result["data"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|e| e["skills"].as_array().cloned().unwrap_or_default())
            .filter(|s| s["enabled"].as_bool() != Some(false))
            .filter_map(|s| {
                Some(Skill {
                    name: s["name"].as_str()?.to_owned(),
                    path: s["path"].as_str()?.to_owned(),
                    description: s["shortDescription"]
                        .as_str()
                        .or_else(|| s["description"].as_str())
                        .map(str::to_owned),
                })
            })
            .collect();
        skills.dedup_by(|a, b| a.name == b.name);
        if skills == self.skills {
            return None;
        }
        self.skills = skills;
        Some(self.update(DriverUpdateKind::Actions {
            actions: self.actions(),
        }))
    }

    /// The `turn/start` parameters that run skill `name` with `text`.
    pub fn skill_turn_params(&self, name: &str, text: &str, message: &str) -> Option<Value> {
        let skill = self.skills.iter().find(|s| s.name == name)?;
        Some(json!({
            "threadId": self.thread_id,
            "input": [
                {"type": "text", "text": format!("${} {text}", skill.name).trim_end(), "text_elements": []},
                {"type": "skill", "name": skill.name, "path": skill.path},
            ],
            "clientUserMessageId": message,
            "approvalPolicy": self.approval,
            "sandboxPolicy": sandbox_policy(&self.sandbox, &self.cwd),
        }))
    }

    fn append(&mut self, id: &str, kind: ItemKind, field: &str, text: &str) -> Vec<DriverUpdate> {
        if text.is_empty() {
            return vec![];
        }
        let first = !self.items.contains_key(id);
        if first {
            self.items.insert(id.to_owned(), kind);
        }
        self.streaming.insert(id.to_owned());
        let mut append = BTreeMap::new();
        append.insert(field.to_owned(), text.to_owned());
        vec![self.item(ItemPatch {
            id: id.to_owned(),
            kind: first.then_some(Open::Known(kind)),
            turn: if first { self.turn_key() } else { None },
            append: Some(append),
            streaming: first.then_some(true),
            ..Default::default()
        })]
    }

    fn reasoning_delta(&mut self, id: &str, text: &str) -> Vec<DriverUpdate> {
        let seen = self.reasoning.entry(id.to_owned()).or_default();
        if seen.is_empty() && text.trim().is_empty() {
            return vec![];
        }
        seen.push_str(text);
        let first = !self.items.contains_key(id);
        let mut out = Vec::new();
        if first {
            self.items.insert(id.to_owned(), ItemKind::Reasoning);
            self.streaming.insert(id.to_owned());
            let mut set = Map::new();
            set.insert("text".into(), json!(text));
            set.insert("summary".into(), json!(true));
            out.push(self.item(ItemPatch {
                id: id.to_owned(),
                kind: Some(Open::Known(ItemKind::Reasoning)),
                turn: self.turn_key(),
                set: Some(set),
                streaming: Some(true),
                ..Default::default()
            }));
        } else {
            let mut append = BTreeMap::new();
            append.insert("text".to_owned(), text.to_owned());
            out.push(self.item(ItemPatch {
                id: id.to_owned(),
                append: Some(append),
                ..Default::default()
            }));
        }
        out
    }

    /// Whether a user item is the echo of a prompt sent; records it so its completion is dropped too.
    fn echo_of(&mut self, u: &wire::UserMessage) -> Option<MessageId> {
        let text = u.text();
        let pos = self
            .echoes
            .iter()
            .position(|(_, m)| u.client_id.as_deref() == Some(m.as_str()))
            .or_else(|| self.echoes.iter().position(|(t, _)| *t == text))
            // Codex 0.167 reports `clientId: null` and the text with context and attachments
            // folded in, so otherwise the oldest prompt not yet echoed is the one.
            .or_else(|| (!self.echoes.is_empty()).then_some(0))?;
        let (_, message) = self.echoes.remove(pos);
        self.echoed.insert(u.id.clone());
        Some(message)
    }

    fn item_started(&mut self, item: &ThreadItem) -> Vec<DriverUpdate> {
        let id = item.id().to_owned();
        match item {
            ThreadItem::UserMessage(u) => self.user_item(u),
            ThreadItem::Reasoning(_) => vec![],
            _ => {
                let Some((kind, set)) = fields_of(item, false) else {
                    return vec![];
                };
                self.items.insert(id.clone(), kind);
                if let ThreadItem::FileChange(f) = item {
                    self.changes
                        .insert(id.clone(), f.changes.iter().map(|c| c.path.clone()).collect());
                }
                let streams = matches!(kind, ItemKind::Assistant | ItemKind::Plan | ItemKind::Command);
                if streams {
                    self.streaming.insert(id.clone());
                }
                vec![self.item(ItemPatch {
                    id,
                    kind: Some(Open::Known(kind)),
                    turn: self.turn_key(),
                    set: Some(set),
                    streaming: streams.then_some(true),
                    ..Default::default()
                })]
            }
        }
    }

    fn item_completed(&mut self, item: &ThreadItem) -> Vec<DriverUpdate> {
        let id = item.id().to_owned();
        match item {
            ThreadItem::UserMessage(u) => self.user_item(u),
            ThreadItem::Reasoning(r) => {
                let parts = if r.summary.is_empty() {
                    &r.content
                } else {
                    &r.summary
                };
                let text = parts.join("\n\n");
                let seen = self.reasoning.remove(&id).unwrap_or_default();
                if text.trim().is_empty() && seen.is_empty() {
                    return vec![];
                }
                let first = self.items.insert(id.clone(), ItemKind::Reasoning).is_none();
                self.streaming.remove(&id);
                let mut set = Map::new();
                set.insert("text".into(), json!(if text.is_empty() { seen } else { text }));
                set.insert("summary".into(), json!(!r.summary.is_empty()));
                vec![self.item(ItemPatch {
                    id,
                    kind: Some(Open::Known(ItemKind::Reasoning)),
                    turn: if first { self.turn_key() } else { None },
                    set: Some(set),
                    streaming: Some(false),
                    ..Default::default()
                })]
            }
            _ => {
                let Some((kind, mut set)) = fields_of(item, true) else {
                    return vec![];
                };
                if kind == ItemKind::Compaction {
                    let trigger = if self.compact_requested { "manual" } else { "auto" };
                    set.insert("trigger".into(), json!(trigger));
                }
                let first = self.items.insert(id.clone(), kind).is_none();
                self.streaming.remove(&id);
                vec![self.item(ItemPatch {
                    id,
                    kind: Some(Open::Known(kind)),
                    turn: if first { self.turn_key() } else { None },
                    set: Some(set),
                    streaming: Some(false),
                    ..Default::default()
                })]
            }
        }
    }

    fn user_item(&mut self, u: &wire::UserMessage) -> Vec<DriverUpdate> {
        if self.echoed.contains(&u.id) {
            return vec![];
        }
        if let Some(message) = self.echo_of(u) {
            // Ties the runtime's user item to Codex's, so history read later deduplicates.
            return vec![self.item(ItemPatch {
                id: message,
                reference: Some(Ref {
                    id: u.id.clone(),
                    strength: RefStrength::Strong,
                }),
                ..Default::default()
            })];
        }
        let text = u.text();
        if text.is_empty() || self.items.contains_key(&u.id) {
            return vec![];
        }
        self.items.insert(u.id.clone(), ItemKind::User);
        let mut set = Map::new();
        set.insert("text".into(), json!(text));
        set.insert("status".into(), json!("handled"));
        vec![self.item(ItemPatch {
            id: u.id.clone(),
            kind: Some(Open::Known(ItemKind::User)),
            turn: self.turn_key(),
            set: Some(set),
            ..Default::default()
        })]
    }

    /// Codex exited: open requests and the turn end, in subagents too.
    pub fn exited(&mut self, now: &str) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        for sub in self.subagents.values_mut() {
            out.extend(sub.exited(now));
        }
        if self.turn.is_some() {
            out.extend(self.end_turn(
                Some("failed"),
                Some(&wire::TurnErrorInfo {
                    message: "codex exited".into(),
                    codex_error_info: Some(json!("exited")),
                    extra: Map::new(),
                }),
                now,
            ));
        }
        out
    }
}

/// The notice code for sign-in news.
pub const AUTH_NOTICE: &str = "x.parrot/auth";

/// The action id prefix of a skill.
pub const SKILL_PREFIX: &str = "skill:";

fn skill_action(s: &Skill) -> Action {
    Action {
        id: format!("{SKILL_PREFIX}{}", s.name),
        name: Some(format!("${}", s.name)),
        label: s.name.clone(),
        description: s.description.clone(),
        input: Some(json!({"type": "object", "properties": {"text": {"type": "string", "title": "Text"}}})),
        hint: Some("[text]".into()),
        scope: ActionScope::Session,
        when: vec![ActionWhen::Idle],
        effects: vec![Effect::Turn],
        requires: None,
        source: Open::Known(ActionSource::Skill),
        meta: None,
        extra: Map::new(),
    }
}

fn account_action(id: &str, label: &str, description: &str) -> Action {
    Action {
        id: id.into(),
        name: Some(format!("/{id}")),
        label: label.into(),
        description: Some(description.into()),
        input: None,
        hint: None,
        scope: ActionScope::Agent,
        when: vec![ActionWhen::Idle, ActionWhen::Running],
        effects: vec![Effect::Write],
        requires: None,
        source: Open::Known(ActionSource::Harness),
        meta: None,
        extra: Map::new(),
    }
}

/// The `compact`, `review`, `login` and `logout` actions, which Codex offers as methods (spec 16).
pub fn actions() -> Vec<Action> {
    vec![
        account_action("login", "Sign in", "Sign in to ChatGPT with a device code"),
        account_action("logout", "Sign out", "Sign Codex out"),
        Action {
            id: "compact".into(),
            name: Some("/compact".into()),
            label: "Compact".into(),
            description: Some("Summarise the conversation to free context".into()),
            input: Some(json!({"type": "object", "properties": {}})),
            hint: None,
            scope: ActionScope::Session,
            when: vec![ActionWhen::Idle],
            effects: vec![Effect::Turn],
            requires: None,
            source: Open::Known(ActionSource::Harness),
            meta: None,
            extra: Map::new(),
        },
        Action {
            id: "review".into(),
            name: Some("/review".into()),
            label: "Review".into(),
            description: Some("Review changes: uncommitted ones by default".into()),
            input: Some(json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "oneOf": [
                            {"const": "uncommittedChanges", "title": "Uncommitted changes"},
                            {"const": "baseBranch", "title": "Against a base branch"},
                            {"const": "commit", "title": "One commit"},
                            {"const": "custom", "title": "Custom instructions"},
                        ],
                        "default": "uncommittedChanges",
                    },
                    "branch": {"type": "string", "title": "Base branch"},
                    "sha": {"type": "string", "title": "Commit"},
                    "instructions": {"type": "string", "title": "Instructions"},
                },
            })),
            hint: Some("[instructions]".into()),
            scope: ActionScope::Session,
            when: vec![ActionWhen::Idle],
            effects: vec![Effect::Turn],
            requires: None,
            source: Open::Known(ActionSource::Harness),
            meta: None,
            extra: Map::new(),
        },
    ]
}

/// The `review/start` target for the review action's input or argument text.
pub fn review_target(input: Option<&Value>, args: Option<&str>) -> Result<Value, String> {
    let args = args.map(str::trim).filter(|a| !a.is_empty());
    let field = |k: &str| {
        input
            .and_then(|i| i.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let target = field("target").unwrap_or_else(|| {
        if args.is_some() || field("instructions").is_some() {
            "custom".into()
        } else {
            "uncommittedChanges".into()
        }
    });
    match target.as_str() {
        "uncommittedChanges" => Ok(json!({"type": "uncommittedChanges"})),
        "baseBranch" => field("branch")
            .map(|b| json!({"type": "baseBranch", "branch": b}))
            .ok_or_else(|| "baseBranch needs branch".into()),
        "commit" => field("sha")
            .map(|s| json!({"type": "commit", "sha": s}))
            .ok_or_else(|| "commit needs sha".into()),
        "custom" => field("instructions")
            .or(args.map(str::to_owned))
            .map(|i| json!({"type": "custom", "instructions": i}))
            .ok_or_else(|| "custom needs instructions".into()),
        other => Err(format!("unknown review target {other}")),
    }
}

/// The input array of a turn (see [`crate::input`]).
fn input_of(p: &BindingPromptParams) -> Value {
    Value::Array(crate::input::input(p).items)
}

fn status_of(status: Option<&str>, done: bool) -> &'static str {
    match status {
        Some("failed") => "error",
        Some("declined") => "cancelled",
        Some("completed") => "ok",
        Some("inProgress") | None if !done => "running",
        _ if done => "ok",
        _ => "running",
    }
}

fn truncate(s: &str) -> String {
    if s.len() <= OUTPUT_MAX {
        return s.to_owned();
    }
    let mut end = OUTPUT_MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &s[..end])
}

/// File changes as `file_change` entries: `{ path, op, from?, diff? }`.
fn changes_of(changes: &[wire::FileUpdateChange]) -> Value {
    let changes: Vec<Value> = changes
        .iter()
        .map(|c| {
            let ty = c
                .kind
                .as_ref()
                .and_then(|k| k.get("type"))
                .and_then(Value::as_str);
            let moved = c
                .kind
                .as_ref()
                .and_then(|k| k.get("move_path"))
                .and_then(Value::as_str);
            let op = match (ty, moved) {
                (Some("add"), _) => "add",
                (Some("delete"), _) => "delete",
                (_, Some(_)) => "move",
                _ => "modify",
            };
            let mut e = json!({"path": moved.unwrap_or(&c.path), "op": op});
            if moved.is_some() {
                e["from"] = json!(c.path);
            }
            if let Some(d) = &c.diff {
                e["diff"] = json!(d);
            }
            e
        })
        .collect();
    Value::Array(changes)
}

/// The kind and fields of an item, at its start or at its end.
fn fields_of(item: &ThreadItem, done: bool) -> Option<(ItemKind, Map<String, Value>)> {
    let mut set = Map::new();
    let kind = match item {
        ThreadItem::AgentMessage(m) => {
            set.insert("text".into(), json!(m.text));
            ItemKind::Assistant
        }
        ThreadItem::Plan(p) => {
            set.insert("text".into(), json!(p.text));
            ItemKind::Plan
        }
        ThreadItem::CommandExecution(c) => {
            set.insert("command".into(), json!(wire::short_command(&c.command)));
            set.insert("cwd".into(), json!(c.cwd));
            let mut status = status_of(c.status.as_deref(), done);
            if done && status == "ok" && c.exit_code.is_some_and(|e| e != 0) {
                status = "error";
            }
            set.insert("status".into(), json!(status));
            if let Some(o) = &c.aggregated_output {
                set.insert("output".into(), json!(truncate(o)));
            }
            if let Some(e) = c.exit_code {
                set.insert("exit".into(), json!(e));
            }
            ItemKind::Command
        }
        ThreadItem::FileChange(f) => {
            set.insert("changes".into(), changes_of(&f.changes));
            set.insert("status".into(), json!(status_of(f.status.as_deref(), done)));
            ItemKind::FileChange
        }
        ThreadItem::McpToolCall(t) => {
            set.extend(tool_fields(
                &t.tool,
                &format!("{}: {}", t.server, t.tool),
                &t.arguments,
            ));
            set.insert("server".into(), json!(t.server));
            let mut status = status_of(t.status.as_deref(), done);
            if let Some(e) = t.error.as_ref().filter(|e| !e.is_null()) {
                status = "error";
                let msg = e
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or(e.to_string());
                set.insert("output".into(), json!(truncate(&msg)));
            } else if let Some(r) = &t.result {
                let text = r
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|c| {
                        c.iter()
                            .filter_map(|c| match c["type"].as_str() {
                                Some("text") => c["text"].as_str().map(str::to_owned),
                                Some("image") => Some("[image]".into()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                set.insert("output".into(), json!(truncate(&text)));
            }
            set.insert("status".into(), json!(status));
            ItemKind::Tool
        }
        ThreadItem::DynamicToolCall(t) => {
            set.extend(tool_fields(&t.tool, &t.tool, &t.arguments));
            let mut status = status_of(t.status.as_deref(), done);
            if t.success == Some(false) {
                status = "error";
            }
            if let Some(items) = &t.content_items {
                let text: Vec<&str> = items.iter().filter_map(|c| c["text"].as_str()).collect();
                set.insert("output".into(), json!(truncate(&text.join("\n"))));
            }
            set.insert("status".into(), json!(status));
            ItemKind::Tool
        }
        ThreadItem::ImageView(v) => {
            set.extend(tool_fields("view_image", &v.path, &json!({"path": v.path})));
            set.insert("status".into(), json!(if done { "ok" } else { "running" }));
            ItemKind::Tool
        }
        ThreadItem::CollabAgentToolCall(c) => {
            let tool = c.tool.clone().unwrap_or_else(|| "agent".into());
            let status = status_of(c.status.as_deref(), done);
            if tool == "spawnAgent" {
                // The new agent runs in its own thread, whose id is the subagent's key (30.3).
                let title = c
                    .prompt
                    .as_deref()
                    .and_then(|p| p.lines().find(|l| !l.trim().is_empty()))
                    .map(|l| l.trim().chars().take(80).collect::<String>())
                    .unwrap_or_else(|| "Subagent".into());
                set.insert("title".into(), json!(title));
                set.insert("status".into(), json!(status));
                if let Some(thread) = c.receiver_thread_ids.first() {
                    set.insert("key".into(), json!(thread));
                }
                if let Some(p) = &c.prompt {
                    set.insert("prompt".into(), json!(p));
                }
                if let Some(m) = &c.model {
                    set.insert("model".into(), json!(m));
                }
                ItemKind::Subagent
            } else {
                let title = match c.receiver_thread_ids.as_slice() {
                    [] => tool.clone(),
                    agents => format!("{tool}: {}", agents.join(", ")),
                };
                let input = json!({"prompt": c.prompt, "agents": c.receiver_thread_ids});
                set.extend(tool_fields(&tool, &title, &input));
                set.insert("status".into(), json!(status));
                if !c.agents_states.is_empty() {
                    set.insert("output".into(), Value::Object(c.agents_states.clone()));
                }
                ItemKind::Tool
            }
        }
        ThreadItem::WebSearch(w) => {
            if let Some(q) = &w.query {
                set.insert("query".into(), json!(q));
            }
            if let Some(r) = w.results.as_ref().filter(|r| !r.is_null()) {
                set.insert("results".into(), r.clone());
            }
            set.insert("status".into(), json!(if done { "ok" } else { "running" }));
            ItemKind::Web
        }
        ThreadItem::EnteredReviewMode(r) => {
            set.insert("target".into(), json!(r.review));
            ItemKind::Review
        }
        ThreadItem::ExitedReviewMode(r) => {
            set.insert("target".into(), json!("review"));
            set.insert("findings".into(), json!(r.review));
            ItemKind::Review
        }
        ThreadItem::ContextCompaction(_) => {
            set.insert("trigger".into(), json!("auto"));
            ItemKind::Compaction
        }
        ThreadItem::UserMessage(_) | ThreadItem::Reasoning(_) => return None,
    };
    Some((kind, set))
}

fn tool_fields(tool: &str, title: &str, input: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("tool".into(), json!(tool));
    m.insert("title".into(), json!(title));
    m.insert(
        "input".into(),
        if input.is_null() { json!({}) } else { input.clone() },
    );
    m.insert("caller".into(), json!("model"));
    m
}

/// Tokens and context from `thread/tokenUsage/updated`. Codex counts cached input inside
/// `inputTokens`; Parrot's `input` leaves cache reads out.
pub fn usage_of(u: &ThreadTokenUsage) -> Usage {
    let t = &u.total;
    let tokens = TokenUsage {
        input: t.input_tokens.saturating_sub(t.cached_input_tokens),
        output: t.output_tokens,
        cache_read: Some(t.cached_input_tokens),
        cache_write: (t.cache_write_input_tokens > 0).then_some(t.cache_write_input_tokens),
        reasoning: (t.reasoning_output_tokens > 0).then_some(t.reasoning_output_tokens),
    };
    let used = if u.last.total_tokens > 0 {
        u.last.total_tokens
    } else {
        u.last.input_tokens + u.last.output_tokens
    };
    Usage {
        tokens: Some(tokens),
        context: u.model_context_window.map(|window| ContextUsage { used, window }),
        ..Default::default()
    }
}

/// Rate limits as percentages of 100, one per window.
pub fn rate_limits_of(s: &wire::RateLimitSnapshot) -> Vec<RateLimit> {
    let name = s
        .limit_name
        .clone()
        .or_else(|| s.limit_id.clone())
        .unwrap_or_else(|| "codex".into());
    [("primary", &s.primary), ("secondary", &s.secondary)]
        .into_iter()
        .filter_map(|(which, w)| {
            let w = w.as_ref()?;
            let label = match w.window_duration_mins {
                Some(m) if m % 1440 == 0 => format!("{name} ({}d)", m / 1440),
                Some(m) if m % 60 == 0 => format!("{name} ({}h)", m / 60),
                Some(m) => format!("{name} ({m}m)"),
                None => format!("{name} {which}"),
            };
            Some(RateLimit {
                name: label,
                used: w.used_percent,
                limit: Some(100.0),
                resets: w.resets_at.map(rfc3339),
            })
        })
        .collect()
}

/// Seconds since the epoch in RFC 3339, UTC.
pub fn rfc3339(secs: i64) -> String {
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `now` (RFC 3339, UTC) plus `ms` milliseconds, or `None` when `now` can't be read.
pub fn add_ms(now: &str, ms: u64) -> Option<String> {
    let ms_now = epoch_ms(now)?;
    let t = ms_now + ms as i64;
    let s = rfc3339(t.div_euclid(1000));
    Some(format!("{}.{:03}Z", s.trim_end_matches('Z'), t.rem_euclid(1000)))
}

/// Milliseconds since the epoch of an RFC 3339 time in UTC, such as `2026-11-18T08:23:03.250Z`.
pub fn epoch_ms(t: &str) -> Option<i64> {
    let t = t.strip_suffix('Z')?;
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut h = hms.split(':').map(|p| p.parse::<i64>());
    let (hh, mm, ss) = (h.next()?.ok()?, h.next()?.ok()?, h.next()?.ok()?);
    let frac: String = frac.chars().chain("000".chars()).take(3).collect();
    let millis: i64 = frac.parse().ok()?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) + millis)
}

/// The result to send for an answer.
fn response_of(p: &PendingRequest, a: &Answer) -> Value {
    let chosen = a.choice.as_ref().and_then(|c| p.decisions.get(c)).cloned();
    match p.method.as_str() {
        sr::USER_INPUT => {
            let mut answers = Map::new();
            let questions = p
                .params
                .get("questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for q in &questions {
                let Some(id) = q.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let given = a
                    .answers
                    .as_ref()
                    .and_then(|m| m.get(id))
                    .cloned()
                    .or_else(|| a.input.as_ref().and_then(|i| i.get(id)).cloned())
                    .or_else(|| {
                        (questions.len() == 1)
                            .then(|| {
                                chosen
                                    .clone()
                                    .or_else(|| a.value.clone())
                                    .or_else(|| a.message.clone().map(Value::String))
                            })
                            .flatten()
                    });
                // An option id, as the typed question gives it, becomes its label.
                let label = |v: Value| {
                    let Some(given) = v.as_str() else {
                        return v;
                    };
                    let options = q.get("options").and_then(Value::as_array);
                    if let Some(options) = options {
                        for (i, o) in options.iter().enumerate() {
                            let by_id = given == format!("option-{i}");
                            let by_label = o.get("label").and_then(Value::as_str) == Some(given);
                            if by_id || by_label {
                                return o.get("label").cloned().unwrap_or_else(|| json!(given));
                            }
                        }
                    }
                    json!(given)
                };
                let list: Vec<Value> = match given {
                    Some(Value::Array(v)) => v.into_iter().map(label).collect(),
                    Some(v @ Value::String(_)) => vec![label(v)],
                    Some(Value::Null) | None => continue,
                    Some(other) => vec![Value::String(other.to_string())],
                };
                answers.insert(id.to_owned(), json!({"answers": list}));
            }
            json!({"answers": answers})
        }
        sr::PERMISSIONS_APPROVAL => chosen.unwrap_or_else(|| json!({"permissions": {}, "scope": "turn"})),
        sr::ELICITATION => {
            let action =
                chosen.unwrap_or_else(|| json!(if a.input.is_some() { "accept" } else { "decline" }));
            let content = if action == "accept" {
                a.input.clone().unwrap_or(json!({}))
            } else {
                Value::Null
            };
            json!({"action": action, "content": content, "_meta": null})
        }
        sr::EXEC_APPROVAL_V1 | sr::PATCH_APPROVAL_V1 => {
            json!({"decision": chosen.unwrap_or_else(|| json!("denied"))})
        }
        _ => json!({"decision": chosen.unwrap_or_else(|| json!("decline"))}),
    }
}

fn simple_choices(
    allows: &[(&str, bool, ChoiceScope)],
    decisions: &mut BTreeMap<String, Value>,
) -> Vec<Choice> {
    let mut out = Vec::new();
    for (id, _, scope) in allows {
        decisions.insert((*id).into(), json!(id));
        let label = if *scope == ChoiceScope::Session {
            "Allow for the session"
        } else {
            "Allow"
        };
        out.push(choice(id, label, ChoiceIntent::Allow, Some(*scope), None));
    }
    decisions.insert("decline".into(), json!("decline"));
    decisions.insert("cancel".into(), json!("cancel"));
    out.push(choice("decline", "Deny", ChoiceIntent::Deny, None, None));
    out.push(choice(
        "cancel",
        "Deny and stop the turn",
        ChoiceIntent::Deny,
        None,
        None,
    ));
    out
}

fn v1_choices(decisions: &mut BTreeMap<String, Value>) -> Vec<Choice> {
    for (id, d) in [
        ("accept", "approved"),
        ("acceptForSession", "approved_for_session"),
        ("decline", "denied"),
        ("cancel", "abort"),
    ] {
        decisions.insert(id.into(), json!(d));
    }
    vec![
        choice(
            "accept",
            "Allow",
            ChoiceIntent::Allow,
            Some(ChoiceScope::Once),
            None,
        ),
        choice(
            "acceptForSession",
            "Allow for the session",
            ChoiceIntent::Allow,
            Some(ChoiceScope::Session),
            None,
        ),
        choice("decline", "Deny", ChoiceIntent::Deny, None, None),
        choice("cancel", "Deny and stop the turn", ChoiceIntent::Deny, None, None),
    ]
}

/// Choices for a command approval: the decisions Codex offers, or the usual ones with any proposed
/// policy amendments.
fn command_choices(params: &Value, decisions: &mut BTreeMap<String, Value>) -> Vec<Choice> {
    let offered: Vec<Value> = match params.get("availableDecisions").and_then(Value::as_array) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => {
            let mut d = vec![json!("accept"), json!("acceptForSession")];
            if let Some(a) = params.get("proposedExecpolicyAmendment").filter(|a| !a.is_null()) {
                d.push(json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": a}}));
            }
            for a in params
                .get("proposedNetworkPolicyAmendments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                d.push(json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": a}}));
            }
            d.extend([json!("decline"), json!("cancel")]);
            d
        }
    };
    let mut out = Vec::new();
    let mut networks = 0;
    for d in offered {
        let c = match &d {
            Value::String(s) => match s.as_str() {
                "accept" => choice(
                    "accept",
                    "Allow",
                    ChoiceIntent::Allow,
                    Some(ChoiceScope::Once),
                    None,
                ),
                "acceptForSession" => choice(
                    "acceptForSession",
                    "Allow for the session",
                    ChoiceIntent::Allow,
                    Some(ChoiceScope::Session),
                    None,
                ),
                "decline" => choice("decline", "Deny", ChoiceIntent::Deny, None, None),
                "cancel" => choice("cancel", "Deny and stop the turn", ChoiceIntent::Deny, None, None),
                other => choice(other, other, ChoiceIntent::Neutral, None, None),
            },
            Value::Object(o) if o.contains_key("acceptWithExecpolicyAmendment") => {
                let amends = d
                    .pointer("/acceptWithExecpolicyAmendment/execpolicy_amendment")
                    .cloned();
                let prefix = amends
                    .as_ref()
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                    .filter(|p| !p.is_empty());
                let label = match prefix {
                    Some(p) => format!("Always allow commands starting with {p}"),
                    None => "Always allow commands like this".into(),
                };
                choice(
                    "execpolicy",
                    &label,
                    ChoiceIntent::Allow,
                    Some(ChoiceScope::Always),
                    amends.map(|a| json!({"execpolicy_amendment": a})),
                )
            }
            Value::Object(o) if o.contains_key("applyNetworkPolicyAmendment") => {
                let a = d
                    .pointer("/applyNetworkPolicyAmendment/network_policy_amendment")
                    .cloned()
                    .unwrap_or_default();
                let host = a.get("host").and_then(Value::as_str).unwrap_or("this host");
                let allow = a.get("action").and_then(Value::as_str) != Some("deny");
                networks += 1;
                choice(
                    &format!("network-{networks}"),
                    &if allow {
                        format!("Always allow {host}")
                    } else {
                        format!("Always block {host}")
                    },
                    if allow {
                        ChoiceIntent::Allow
                    } else {
                        ChoiceIntent::Deny
                    },
                    Some(ChoiceScope::Always),
                    Some(json!({"network_policy_amendment": a})),
                )
            }
            _ => continue,
        };
        decisions.insert(c.id.clone(), d);
        out.push(c);
    }
    out
}

fn effort_entry(id: &str, description: Option<&str>) -> Value {
    let title = match id {
        "xhigh" => "Extra high".to_owned(),
        _ => {
            let mut c = id.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    };
    let mut e = json!({"const": id, "title": title});
    if let Some(d) = description.filter(|d| !d.is_empty()) {
        e["description"] = json!(d);
    }
    e
}

fn option(id: &str, role: OptionRole, label: &str, schema: Value, value: Value) -> ConfigOption {
    ConfigOption {
        id: id.into(),
        role: Some(Open::Known(role)),
        label: label.into(),
        description: None,
        schema,
        value,
        default: None,
        applies: Applies::NextTurn,
        depends_on: None,
        complete: None,
        meta: None,
        extra: Map::new(),
    }
}

fn choice(
    id: &str,
    label: &str,
    intent: ChoiceIntent,
    scope: Option<ChoiceScope>,
    amends: Option<Value>,
) -> Choice {
    Choice {
        id: id.into(),
        label: label.into(),
        intent: Some(intent),
        scope,
        amends,
        description: None,
    }
}

/// Codex's `questions`, typed for clients (spec 14). An entry without an id or text is skipped.
fn questions_of(questions: Option<&Value>) -> Vec<Question> {
    questions
        .and_then(Value::as_array)
        .map(|qs| {
            qs.iter()
                .filter_map(|q| {
                    Some(Question {
                        id: q.get("id").and_then(Value::as_str)?.to_owned(),
                        header: q.get("header").and_then(Value::as_str).map(str::to_owned),
                        question: q.get("question").and_then(Value::as_str)?.to_owned(),
                        multi_select: q.get("multiSelect").and_then(Value::as_bool),
                        options: q
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|options| {
                                options
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(i, o)| {
                                        let label = o.get("label").and_then(Value::as_str)?;
                                        Some(Choice {
                                            id: format!("option-{i}"),
                                            label: label.to_owned(),
                                            intent: Some(ChoiceIntent::Neutral),
                                            scope: None,
                                            amends: None,
                                            description: o
                                                .get("description")
                                                .and_then(Value::as_str)
                                                .filter(|d| !d.is_empty())
                                                .map(str::to_owned),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
