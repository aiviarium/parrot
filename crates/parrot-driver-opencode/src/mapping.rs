//! opencode's event stream as driver updates (spec 30.2).
//!
//! [`MapState`] holds what the mapping remembers between events. Driver keys are stable: the
//! runtime's message id for prompts, `<assistant message>:<ordinal>` for text and reasoning,
//! opencode's own tool-call id for tools, and the permission's own id for requests.

use std::collections::BTreeMap;

use parrot_proto::action::{Action, ActionScope, ActionSource, ActionWhen, Effect};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams, DriverUpdate, DriverUpdateKind};
use parrot_proto::ids::MessageId;
use parrot_proto::item::{ItemKind, ItemPatch, ItemSource};
use parrot_proto::open::Open;
use parrot_proto::option::{Applies, ConfigOption, OptionRole, PermissionLevel};
use parrot_proto::prompt::Attachment;
use parrot_proto::request::{
    Answer, Answerable, Choice, ChoiceIntent, ChoiceScope, Notify, Request, RequestKind, RequestStatus,
    Subject,
};
use parrot_proto::session::{Ref, RefStrength};
use parrot_proto::state::{State, Turn, TurnError, TurnStatus, TurnStop};
use parrot_proto::subscribe::NoticeLevel;
use parrot_proto::usage::{ContextUsage, Cost, Usage};
use serde_json::{Map, Value, json};

use crate::data::DriverData;
use crate::wire::{self, Command, Event, Model, Provider};

/// How much of a tool's output an item keeps; the rest is the harness's to hold.
pub const OUTPUT_MAX: usize = 20_000;

/// The turn in progress.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnState {
    pub key: String,
    pub input: Option<MessageId>,
    pub started: String,
    pub cancelled: bool,
}

/// A permission waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPermission {
    pub request: Request,
    /// The session opencode asked for, which the reply goes to.
    pub session: String,
}

/// What the mapping remembers between events.
#[derive(Debug, Clone, PartialEq)]
pub struct MapState {
    pub binding: String,
    pub turn: Option<TurnState>,
    pub turns: u64,
    pub session_id: Option<String>,
    /// `provider/model`, the value of the `model` option.
    pub model: Option<String>,
    pub models: Vec<Model>,
    pub providers: Vec<Provider>,
    pub actions: Vec<Action>,
    pub requests: BTreeMap<String, PendingPermission>,
    /// The level this binding is pinned to, and whether it may reach the network.
    pub level: PermissionLevel,
    pub network: bool,
    /// Tool input streamed before the call, by tool-call id: its name and the text so far.
    pub tool_inputs: BTreeMap<String, (String, String)>,
    /// Child sessions of this one, by opencode session id, with the title reported for them.
    pub subagents: BTreeMap<String, String>,
    /// Each child session's own mapping, so its events keep their own turns.
    pub children: std::collections::HashMap<String, Box<MapState>>,
    /// The assistant message each finished turn ended with, for a fork at that turn.
    pub turn_messages: BTreeMap<String, String>,
    pub data: DriverData,
    /// Token and cost totals over the whole session.
    usage: Usage,
    /// Token and cost totals of the turn running now; its turn update reports these.
    turn_usage: Usage,
    last_error: Option<String>,
    unnamed: u64,
}

impl MapState {
    pub fn new(binding: impl Into<String>, data: DriverData) -> Self {
        MapState {
            binding: binding.into(),
            turn: None,
            turns: 0,
            session_id: None,
            model: None,
            models: Vec::new(),
            providers: Vec::new(),
            actions: Vec::new(),
            requests: BTreeMap::new(),
            level: PermissionLevel::Host,
            network: true,
            tool_inputs: BTreeMap::new(),
            subagents: BTreeMap::new(),
            children: std::collections::HashMap::new(),
            turn_messages: BTreeMap::new(),
            data,
            usage: Usage::default(),
            turn_usage: Usage::default(),
            last_error: None,
            unnamed: 1,
        }
    }

    fn update(&self, kind: DriverUpdateKind) -> DriverUpdate {
        DriverUpdate {
            binding: self.binding.clone(),
            subagent: None,
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

    /// A `notice` update, for what the harness could not take.
    pub fn notice_update(&self, level: NoticeLevel, text: String, code: &str) -> DriverUpdate {
        self.update(DriverUpdateKind::Notice {
            level,
            text,
            code: Some(code.into()),
        })
    }

    /// opencode asks before a tool by default and runs commands on the host, so a session counts as
    /// `host` with network unless a pin holds it lower (spec 15.2).
    pub fn level(&self) -> PermissionLevel {
        self.level
    }

    pub fn network(&self) -> bool {
        self.network
    }

    /// Holds the binding to a pinned level and network reach.
    pub fn pin(&mut self, pin: &parrot_proto::driver::Pin) {
        self.level = pin.level;
        self.network = pin.network;
    }

    /// The `model` and `provider` options with their current values.
    pub fn options(&self) -> Vec<ConfigOption> {
        let provider_schema = if self.providers.is_empty() {
            json!({"type": "string"})
        } else {
            json!({
                "type": "string",
                "enum": self.providers.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            })
        };
        // The model list can run to hundreds, so clients ask for completions (spec 15.1).
        let model_schema = json!({"type": "string"});
        vec![
            ConfigOption {
                complete: Some(true),
                ..option(
                    "model",
                    OptionRole::Model,
                    &self.data.label("model", "Model"),
                    model_schema,
                    self.model.clone(),
                )
            },
            option(
                "provider",
                OptionRole::Model,
                &self.data.label("provider", "Provider"),
                provider_schema,
                self.provider_value(),
            ),
        ]
    }

    fn provider_value(&self) -> Option<String> {
        let model = self.model.as_ref()?;
        let (provider, _) = model.split_once('/')?;
        Some(provider.to_owned())
    }

    /// The `options` update for the current values.
    pub fn options_update(&self) -> DriverUpdate {
        self.update(DriverUpdateKind::Options {
            options: self.options(),
            level: self.level(),
            network: self.network(),
        })
    }

    /// Completion values for an option (spec 15.1), filtered by the prefix.
    pub fn complete(&self, option: &str, prefix: &str, limit: Option<u32>) -> Vec<(Value, String)> {
        let limit = limit.unwrap_or(50) as usize;
        let values: Vec<(Value, String)> = match option {
            "model" => self
                .models
                .iter()
                .filter(|m| m.enabled)
                .filter(|m| m.key.starts_with(prefix))
                .map(|m| (json!(m.key), format!("{} ({})", m.name, m.provider)))
                .collect(),
            "provider" => self
                .providers
                .iter()
                .filter(|p| p.id.starts_with(prefix))
                .map(|p| (json!(p.id), p.name.clone()))
                .collect(),
            _ => vec![],
        };
        values.into_iter().take(limit).collect()
    }

    /// What opencode reports at open: its session, models, providers and commands.
    pub fn initialized(
        &mut self,
        session: &str,
        models: &[Model],
        providers: &[Provider],
        commands: &[Command],
    ) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        self.models = models.to_vec();
        self.providers = providers.to_vec();
        if self.model.is_none() {
            self.model = models
                .iter()
                .find(|m| m.enabled)
                .map(|m| m.key.clone())
                .or_else(|| models.first().map(|m| m.key.clone()));
        }
        if self.session_id.as_deref() != Some(session) {
            self.session_id = Some(session.to_owned());
            out.push(self.update(DriverUpdateKind::Ref {
                reference: Ref {
                    id: session.to_owned(),
                    strength: RefStrength::Strong,
                },
            }));
        }
        let actions = command_actions(commands);
        if actions != self.actions {
            self.actions = actions.clone();
            out.push(self.update(DriverUpdateKind::Actions { actions }));
        }
        out.push(self.options_update());
        out
    }

    /// A prompt written to opencode: the user item, and a new turn unless one runs.
    pub fn prompt(
        &mut self,
        p: &BindingPromptParams,
        disposition: BindingDisposition,
        now: &str,
    ) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if self.turn.is_none() && disposition != BindingDisposition::Handled {
            out.extend(self.begin_turn(Some(p.message.clone()), now));
        }
        let mut set = Map::new();
        set.insert("text".into(), json!(p.text));
        set.insert(
            "status".into(),
            json!(match disposition {
                BindingDisposition::Handled => "handled",
                _ => "sent",
            }),
        );
        set.insert("message".into(), json!(p.message));
        if let Some(context) = &p.context
            && !context.is_empty()
        {
            set.insert("context".into(), json!(context));
        }
        if let Some(attachments) = &p.attachments {
            let named: Vec<Attachment> = attachments
                .iter()
                .map(|a| Attachment {
                    name: a.name.clone(),
                    media_type: a.media_type.clone(),
                    data: None,
                    uri: a.uri.clone(),
                })
                .collect();
            set.insert("attachments".into(), json!(named));
        }
        out.push(self.item(ItemPatch {
            id: p.message.clone(),
            kind: Some(Open::Known(ItemKind::User)),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            set: Some(set),
            ..Default::default()
        }));
        out
    }

    /// Marks the running turn as cancelled, so its end reads as an interruption.
    pub fn cancel(&mut self) {
        if let Some(t) = self.turn.as_mut() {
            t.cancelled = true;
        }
    }

    fn begin_turn(&mut self, input: Option<MessageId>, now: &str) -> Vec<DriverUpdate> {
        self.turns += 1;
        self.turn_usage = Usage::default();
        let turn = TurnState {
            key: format!("turn-{}", self.turns),
            input,
            started: now.to_owned(),
            cancelled: false,
        };
        self.turn = Some(turn.clone());
        vec![
            self.state(State::Running),
            self.update(DriverUpdateKind::Turn {
                turn: Turn {
                    id: turn.key,
                    binding: String::new(),
                    status: TurnStatus::Running,
                    stop: None,
                    continuable: false,
                    input: turn.input,
                    started: turn.started,
                    ended: None,
                    error: None,
                    usage: None,
                    meta: None,
                    extra: Map::new(),
                },
            }),
        ]
    }

    fn ensure_turn(&mut self, now: &str) -> Vec<DriverUpdate> {
        if self.turn.is_some() {
            vec![]
        } else {
            self.begin_turn(None, now)
        }
    }

    fn turn_key(&self) -> Option<String> {
        self.turn.as_ref().map(|t| t.key.clone())
    }

    /// The answer to the permission with driver key `key`, as the reply opencode expects, with the
    /// session the reply goes to.
    pub fn answer(
        &mut self,
        key: &str,
        answer: &Answer,
    ) -> Result<(Value, String, Vec<DriverUpdate>), String> {
        let pending = self
            .requests
            .remove(key)
            .ok_or_else(|| format!("no open request {key}"))?;
        let decision = match answer.choice.as_deref() {
            Some("allow") => "once",
            Some("allow-session") | Some("always") => "always",
            _ => "reject",
        };
        let mut body = json!({"decision": decision});
        if let Some(message) = &answer.message {
            body["message"] = json!(message);
        }
        let mut request = pending.request;
        request.status = RequestStatus::Answered;
        request.answer = Some(answer.clone());
        let mut out = vec![self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        })];
        if self.requests.is_empty() && self.turn.is_some() {
            out.push(self.state(State::Running));
        }
        Ok((body, pending.session, out))
    }

    /// One event from opencode.
    pub fn step(&mut self, event: &Event, now: &str) -> Vec<DriverUpdate> {
        let mut out = vec![self.update(DriverUpdateKind::Frame {
            source: "opencode".into(),
            data: event.raw.clone(),
        })];
        let data = &event.data;
        if let Some(message) = data.get("assistantMessageID").and_then(Value::as_str)
            && let Some(turn) = self.turn_key()
        {
            self.turn_messages.insert(turn, message.to_owned());
        }
        match event.kind.as_str() {
            "session.created" => {
                if let Some(id) = data.get("sessionID").and_then(Value::as_str)
                    && self.session_id.as_deref() != Some(id)
                    && data.get("parentID").is_none()
                {
                    self.session_id = Some(id.to_owned());
                    out.push(self.update(DriverUpdateKind::Ref {
                        reference: Ref {
                            id: id.to_owned(),
                            strength: RefStrength::Strong,
                        },
                    }));
                }
                // A child session is a subagent: an item in the parent, then its own updates.
                if let (Some(parent), Some(id)) = (
                    data.get("parentID").and_then(Value::as_str),
                    data.get("sessionID").and_then(Value::as_str),
                ) && self.session_id.as_deref() == Some(parent)
                {
                    let title = data
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("Subagent")
                        .to_owned();
                    self.subagents.insert(id.to_owned(), title.clone());
                    self.children.insert(
                        id.to_owned(),
                        Box::new(MapState::new(self.binding.clone(), self.data.clone())),
                    );
                    let mut set = map_of("key", json!(id));
                    set.insert("title".into(), json!(title));
                    set.insert("ref".into(), json!({"id": id, "strength": "strong"}));
                    set.insert("status".into(), json!("running"));
                    out.push(self.item(ItemPatch {
                        id: format!("subagent-{id}"),
                        kind: Some(Open::Known(ItemKind::Subagent)),
                        turn: self.turn_key(),
                        source: Some(ItemSource::Native),
                        set: Some(set),
                        ..Default::default()
                    }));
                }
            }
            "session.execution.started" | "session.step.started" => {
                out.extend(self.ensure_turn(now));
            }
            "session.text.started" | "session.reasoning.started" => {
                out.extend(self.ensure_turn(now));
                out.push(self.block_start(data, &event.kind));
            }
            "session.text.delta" | "session.reasoning.delta" => {
                out.extend(self.ensure_turn(now));
                out.push(self.block_delta(data, &event.kind));
            }
            "session.text.ended" | "session.reasoning.ended" => {
                out.push(self.block_end(data, &event.kind));
            }
            "session.tool.input.started" => {
                if let Some(id) = data.get("id").and_then(Value::as_str) {
                    let name = data
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned();
                    self.tool_inputs
                        .insert(id.to_owned(), (name.clone(), String::new()));
                    out.extend(self.ensure_turn(now));
                    let mut set = map_of("tool", json!(name));
                    set.insert("title".into(), json!(name));
                    set.insert("inputText".into(), json!(""));
                    set.insert("status".into(), json!("running"));
                    set.insert("caller".into(), json!("model"));
                    out.push(self.item(ItemPatch {
                        id: id.to_owned(),
                        kind: Some(Open::Known(ItemKind::Tool)),
                        turn: self.turn_key(),
                        source: Some(ItemSource::Native),
                        set: Some(set),
                        streaming: Some(true),
                        ..Default::default()
                    }));
                }
            }
            "session.tool.input.delta" => {
                if let Some(id) = data.get("id").and_then(Value::as_str) {
                    let delta = data.get("delta").and_then(Value::as_str).unwrap_or_default();
                    // A delta without a start still has a name to show until the call.
                    if !self.tool_inputs.contains_key(id) {
                        self.tool_inputs
                            .insert(id.to_owned(), ("tool".into(), String::new()));
                    }
                    let (name, partial) = self.tool_inputs.get_mut(id).expect("just inserted");
                    partial.push_str(delta);
                    let name = name.clone();
                    let partial = partial.clone();
                    let mut set = map_of("tool", json!(name));
                    set.insert("title".into(), json!(name));
                    set.insert("inputText".into(), json!(clip(&partial)));
                    set.insert("status".into(), json!("running"));
                    set.insert("caller".into(), json!("model"));
                    out.push(self.item(ItemPatch {
                        id: id.to_owned(),
                        kind: Some(Open::Known(ItemKind::Tool)),
                        turn: self.turn_key(),
                        source: Some(ItemSource::Native),
                        set: Some(set),
                        streaming: Some(true),
                        ..Default::default()
                    }));
                }
            }
            "session.tool.input.ended" => {
                if let Some(id) = data.get("id").and_then(Value::as_str) {
                    self.tool_inputs.remove(id);
                }
            }
            "session.tool.called" => {
                out.extend(self.ensure_turn(now));
                out.push(self.tool_called(data));
            }
            "session.tool.progress" => {
                if let Some(id) = data.get("id").and_then(Value::as_str) {
                    let mut set = Map::new();
                    if let Some(metadata) = data.get("metadata").filter(|m| !m.is_null()) {
                        set.insert("output".into(), json!(clip(&metadata.to_string())));
                    }
                    if !set.is_empty() {
                        out.push(self.item(ItemPatch {
                            id: id.to_owned(),
                            set: Some(set),
                            ..Default::default()
                        }));
                    }
                }
            }
            "session.tool.success" | "session.tool.failed" => {
                out.push(self.tool_end(data, event.kind == "session.tool.failed"));
            }
            "session.step.ended" => {
                if let Some(tokens) = data.get("tokens") {
                    // The runtime adds every usage update, so a report is the step's own tokens.
                    let delta = self.add_usage(tokens, data.get("cost"));
                    out.push(self.update(DriverUpdateKind::Usage { usage: delta }));
                }
            }
            "session.usage.updated" => {
                // The session totals opencode reports are absolute; only the context is not
                // already known from the steps.
                if let Some(context) = self.set_usage(data.get("tokens")) {
                    out.push(self.update(DriverUpdateKind::Usage {
                        usage: Usage {
                            context: Some(context),
                            ..Default::default()
                        },
                    }));
                }
            }
            "session.execution.succeeded" => out.extend(self.settled(None, now)),
            "session.execution.failed" => {
                let message = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("opencode failed the turn")
                    .to_owned();
                self.last_error = Some(message);
                out.extend(self.settled(None, now));
            }
            "session.execution.interrupted" => out.extend(self.settled(Some(TurnStop::Cancelled), now)),
            "session.idle" => {
                if self.turn.is_some() {
                    out.extend(self.settled(None, now));
                } else {
                    out.push(self.state(State::Idle));
                }
            }
            "session.model.selected" => {
                if let Some(model) = data.get("model")
                    && let (Some(provider), Some(id)) = (
                        model.get("providerID").and_then(Value::as_str),
                        model.get("id").and_then(Value::as_str),
                    )
                {
                    self.model = Some(format!("{provider}/{id}"));
                    out.push(self.options_update());
                }
            }
            "permission.asked" => out.extend(self.permission(data)),
            "permission.replied" => {
                if let Some(id) = data.get("requestID").and_then(Value::as_str)
                    && let Some(pending) = self.requests.remove(id)
                {
                    let mut request = pending.request;
                    request.status = RequestStatus::Answered;
                    out.push(self.update(DriverUpdateKind::Request {
                        request: Box::new(request),
                    }));
                }
                if self.requests.is_empty() && self.turn.is_some() {
                    out.push(self.state(State::Running));
                }
            }
            "session.compaction.started" => {
                self.unnamed += 1;
                out.push(self.item(ItemPatch {
                    id: format!("compaction-{}", self.unnamed),
                    kind: Some(Open::Known(ItemKind::Compaction)),
                    turn: self.turn_key(),
                    source: Some(ItemSource::Native),
                    set: Some(map_of("trigger", json!("auto"))),
                    streaming: Some(true),
                    ..Default::default()
                }));
            }
            "session.compaction.ended" => {
                let mut set = map_of("trigger", json!("auto"));
                if let Some(summary) = data.get("summary") {
                    set.insert("summary".into(), summary.clone());
                }
                out.push(self.item(ItemPatch {
                    id: format!("compaction-{}", self.unnamed),
                    set: Some(set),
                    streaming: Some(false),
                    ..Default::default()
                }));
            }
            _ => {}
        }
        out
    }

    /// One event that belongs to a child session: its own mapping runs, and every update says
    /// which subagent it belongs to (spec 30.3).
    pub fn step_child(&mut self, child: &str, event: &Event, now: &str) -> Vec<DriverUpdate> {
        let Some(state) = self.children.get_mut(child) else {
            return vec![];
        };
        let mut updates = state.step(event, now);
        for update in &mut updates {
            update.subagent = Some(child.to_owned());
        }
        updates
    }

    /// Whether `session` is a child session of this binding.
    pub fn known_child(&self, session: &str) -> bool {
        self.children.contains_key(session)
    }

    /// The assistant message a fork at `at` starts from: that turn's last message, else the
    /// newest one.
    pub fn fork_message(&self, at: Option<&str>) -> Option<String> {
        match at {
            Some(turn) => self.turn_messages.get(turn).cloned(),
            None => self.turn_messages.values().last().cloned(),
        }
    }

    fn block_key(&self, data: &Value) -> String {
        let message = data
            .get("assistantMessageID")
            .and_then(Value::as_str)
            .unwrap_or("message");
        let ordinal = data.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
        format!("{message}:{ordinal}")
    }

    fn block_kind(kind: &str) -> ItemKind {
        if kind.starts_with("session.reasoning") {
            ItemKind::Reasoning
        } else {
            ItemKind::Assistant
        }
    }

    fn block_start(&mut self, data: &Value, kind: &str) -> DriverUpdate {
        self.item(ItemPatch {
            id: self.block_key(data),
            kind: Some(Open::Known(Self::block_kind(kind))),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            set: Some(map_of("text", json!(""))),
            streaming: Some(true),
            ..Default::default()
        })
    }

    fn block_delta(&mut self, data: &Value, kind: &str) -> DriverUpdate {
        let mut append = BTreeMap::new();
        append.insert(
            "text".to_owned(),
            data.get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
        self.item(ItemPatch {
            id: self.block_key(data),
            kind: Some(Open::Known(Self::block_kind(kind))),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            append: Some(append),
            streaming: Some(true),
            ..Default::default()
        })
    }

    fn block_end(&mut self, data: &Value, kind: &str) -> DriverUpdate {
        let text = data.get("text").and_then(Value::as_str).unwrap_or_default();
        self.item(ItemPatch {
            id: self.block_key(data),
            kind: Some(Open::Known(Self::block_kind(kind))),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            set: Some(map_of("text", json!(text))),
            streaming: Some(false),
            ..Default::default()
        })
    }

    fn tool_called(&mut self, data: &Value) -> DriverUpdate {
        let id = data
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_owned();
        let input = data.get("input").cloned().unwrap_or(Value::Null);
        let name = data
            .get("native")
            .and_then(|n| n.get("tool"))
            .and_then(Value::as_str)
            .or_else(|| input.get("tool").and_then(Value::as_str))
            .unwrap_or("tool")
            .to_owned();
        let mut set = map_of("tool", json!(name));
        set.insert("title".into(), json!(describe(&name, &input)));
        set.insert("input".into(), input);
        set.insert("inputText".into(), Value::Null);
        set.insert("status".into(), json!("running"));
        set.insert("caller".into(), json!("model"));
        self.tool_inputs.remove(&id);
        self.item(ItemPatch {
            id,
            kind: Some(Open::Known(ItemKind::Tool)),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            set: Some(set),
            ..Default::default()
        })
    }

    fn tool_end(&mut self, data: &Value, failed: bool) -> DriverUpdate {
        let id = data
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_owned();
        let mut set = map_of("status", json!(if failed { "error" } else { "ok" }));
        let text = data
            .get("content")
            .map(wire::content_text)
            .filter(|t| !t.is_empty())
            .or_else(|| {
                data.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        if let Some(text) = text {
            set.insert("output".into(), json!(clip(&text)));
        }
        self.item(ItemPatch {
            id,
            set: Some(set),
            streaming: Some(false),
            ..Default::default()
        })
    }

    fn permission(&mut self, data: &Value) -> Vec<DriverUpdate> {
        let Some(id) = data.get("id").and_then(Value::as_str) else {
            return vec![];
        };
        let session = data
            .get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let action = data
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_owned();
        let resources: Vec<String> = data
            .get("resources")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let title = data
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| match resources.first() {
                Some(first) => format!("{action}: {first}"),
                None => format!("Run {action}?"),
            });
        let request = Request {
            id: id.to_owned(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: self.turn_key(),
            item: None,
            kind: Open::Known(RequestKind::Approval),
            title,
            description: data.get("message").and_then(Value::as_str).map(str::to_owned),
            subject: Some(Open::Known(Subject::Tool {
                tool: action.clone(),
                input: json!({"resources": resources}),
                editable: None,
            })),
            level: self.data.tool_level(&action),
            network: None,
            choices: Some(vec![
                choice("allow", "Allow once", ChoiceIntent::Allow, ChoiceScope::Once),
                choice(
                    "allow-session",
                    "Allow for this session",
                    ChoiceIntent::Allow,
                    ChoiceScope::Session,
                ),
                choice("deny", "Deny", ChoiceIntent::Deny, ChoiceScope::Once),
            ]),
            questions: None,
            input: None,
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
        };
        self.requests.insert(
            id.to_owned(),
            PendingPermission {
                request: request.clone(),
                session,
            },
        );
        vec![
            self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            }),
            self.state(State::RequiresAction),
        ]
    }

    /// Adds one step's tokens and cost to the session and turn totals, and returns the step's own
    /// usage, which is what the runtime is told.
    fn add_usage(&mut self, tokens: &Value, cost: Option<&Value>) -> Usage {
        let delta = Usage {
            tokens: Some(token_usage(tokens)),
            cost: cost
                .and_then(Value::as_f64)
                .filter(|c| *c > 0.0)
                .map(|amount| Cost {
                    amount,
                    currency: "USD".into(),
                }),
            ..Default::default()
        };
        accumulate(&mut self.usage, &delta);
        accumulate(&mut self.turn_usage, &delta);
        let mut delta = delta;
        delta.context = self.context();
        delta
    }

    /// Takes the session totals opencode reports, and returns the context they imply.
    fn set_usage(&mut self, tokens: Option<&Value>) -> Option<ContextUsage> {
        let tokens = tokens?;
        self.usage.tokens = Some(token_usage(tokens));
        self.context()
    }

    fn context(&self) -> Option<ContextUsage> {
        let tokens = &self.usage.tokens.as_ref()?;
        let used =
            tokens.input + tokens.output + tokens.cache_read.unwrap_or(0) + tokens.cache_write.unwrap_or(0);
        if used == 0 {
            return None;
        }
        Some(ContextUsage {
            used,
            window: self.window()?,
        })
    }

    fn window(&self) -> Option<u64> {
        let key = self.model.as_ref()?;
        self.models.iter().find(|m| &m.key == key).and_then(|m| m.context)
    }

    /// Ends the open turn, with `cancelled` overriding what the stream says.
    fn settled(&mut self, cancelled: Option<TurnStop>, now: &str) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        let Some(turn) = self.turn.take() else {
            return out;
        };
        for pending in std::mem::take(&mut self.requests).into_values() {
            let mut request = pending.request;
            request.status = RequestStatus::Cancelled;
            out.push(self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            }));
        }
        let error = self.last_error.take().filter(|_| cancelled.is_none());
        let (status, stop, error) = if turn.cancelled || cancelled == Some(TurnStop::Cancelled) {
            (TurnStatus::Interrupted, TurnStop::Cancelled, None)
        } else if let Some(message) = error {
            (
                TurnStatus::Failed,
                TurnStop::Error,
                Some(TurnError {
                    code: "opencode".into(),
                    message,
                    retryable: true,
                }),
            )
        } else {
            (TurnStatus::Completed, TurnStop::End, None)
        };
        out.push(self.update(DriverUpdateKind::Turn {
            turn: Turn {
                id: turn.key,
                binding: String::new(),
                status,
                stop: Some(Open::Known(stop)),
                continuable: matches!(stop, TurnStop::Cancelled | TurnStop::Error),
                input: turn.input,
                started: turn.started,
                ended: Some(now.to_owned()),
                error,
                usage: Some(self.turn_usage.clone()),
                meta: None,
                extra: Map::new(),
            },
        }));
        out.push(self.state(State::Idle));
        out
    }

    /// opencode took a prompt without running a turn: the user item is handled and nothing follows.
    pub fn handled(&mut self, message: &MessageId, now: &str) -> Vec<DriverUpdate> {
        let mut out = vec![self.item(ItemPatch {
            id: message.clone(),
            set: Some(map_of("status", json!("handled"))),
            ..Default::default()
        })];
        let ours = self
            .turn
            .as_ref()
            .map(|t| t.input.as_ref() == Some(message))
            .unwrap_or(false);
        if ours {
            out.extend(self.settled(None, now));
        }
        out
    }

    /// A prompt opencode refused: the turn it started fails and the session is idle again.
    pub fn fail(&mut self, message: String, now: &str) -> Vec<DriverUpdate> {
        self.last_error = Some(message);
        self.settled(None, now)
    }

    /// The turn ends because the server went away; an open turn is interrupted.
    pub fn exited(&mut self, now: &str) -> Vec<DriverUpdate> {
        self.settled(Some(TurnStop::Cancelled), now)
    }
}

/// The actions opencode's commands become, plus the compaction endpoint.
fn command_actions(commands: &[Command]) -> Vec<Action> {
    let mut actions: Vec<Action> = commands.iter().map(command_action).collect();
    actions.push(Action {
        id: "compact".into(),
        name: Some("/compact".into()),
        label: "Compact".into(),
        description: Some("Compact the context".into()),
        input: None,
        hint: None,
        scope: ActionScope::Session,
        when: vec![ActionWhen::Idle, ActionWhen::Running],
        effects: vec![Effect::Turn],
        requires: None,
        source: Open::Known(ActionSource::Harness),
        meta: None,
        extra: Map::new(),
    });
    actions
}

fn command_action(command: &Command) -> Action {
    Action {
        id: command.name.clone(),
        name: Some(format!("/{}", command.name)),
        label: command.name.clone(),
        description: command.description.clone(),
        input: None,
        hint: None,
        scope: ActionScope::Session,
        when: vec![ActionWhen::Idle, ActionWhen::Running],
        effects: vec![Effect::Turn],
        requires: None,
        source: Open::Known(ActionSource::Harness),
        meta: None,
        extra: Map::new(),
    }
}

/// The pure form of [`MapState::step`]: the updates one event causes and the state after it.
pub fn map(state: &MapState, event: &Event, now: &str) -> (Vec<DriverUpdate>, MapState) {
    let mut next = state.clone();
    let updates = next.step(event, now);
    (updates, next)
}

/// opencode's token counts as the protocol's.
fn token_usage(tokens: &Value) -> parrot_proto::usage::TokenUsage {
    let cache = tokens.get("cache");
    let read = cache
        .and_then(|c| c.get("read"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let write = cache
        .and_then(|c| c.get("write"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    parrot_proto::usage::TokenUsage {
        input: tokens.get("input").and_then(Value::as_u64).unwrap_or(0),
        output: tokens.get("output").and_then(Value::as_u64).unwrap_or(0),
        cache_read: (read > 0).then_some(read),
        cache_write: (write > 0).then_some(write),
        reasoning: tokens.get("reasoning").and_then(Value::as_u64).filter(|r| *r > 0),
    }
}

/// Adds a usage report to a running total.
fn accumulate(into: &mut Usage, delta: &Usage) {
    if let Some(tokens) = &delta.tokens {
        let mut totals = into.tokens.clone().unwrap_or_default();
        totals.input += tokens.input;
        totals.output += tokens.output;
        totals.reasoning = match (totals.reasoning, tokens.reasoning) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        totals.cache_read = match (totals.cache_read, tokens.cache_read) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        totals.cache_write = match (totals.cache_write, tokens.cache_write) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        into.tokens = Some(totals);
    }
    if let Some(cost) = &delta.cost {
        let amount = into.cost.as_ref().map(|c| c.amount).unwrap_or(0.0) + cost.amount;
        into.cost = Some(Cost {
            amount,
            currency: cost.currency.clone(),
        });
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() > OUTPUT_MAX {
        let mut cut: String = text.chars().take(OUTPUT_MAX).collect();
        cut.push('…');
        cut
    } else {
        text.to_owned()
    }
}

fn describe(tool: &str, args: &Value) -> String {
    let detail = [
        "command",
        "path",
        "filePath",
        "file_path",
        "pattern",
        "query",
        "url",
        "description",
    ]
    .iter()
    .find_map(|k| args.get(*k).and_then(Value::as_str));
    match detail {
        Some(d) if !d.is_empty() => format!("{tool}: {d}"),
        _ => tool.to_owned(),
    }
}

fn map_of(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

fn option(id: &str, role: OptionRole, label: &str, schema: Value, value: Option<String>) -> ConfigOption {
    ConfigOption {
        id: id.into(),
        role: Some(Open::Known(role)),
        label: label.into(),
        description: None,
        schema,
        value: value.map_or(Value::Null, Value::String),
        default: None,
        applies: Applies::Now,
        depends_on: None,
        complete: None,
        meta: None,
        extra: Map::new(),
    }
}

fn choice(id: &str, label: &str, intent: ChoiceIntent, scope: ChoiceScope) -> Choice {
    Choice {
        id: id.into(),
        label: label.into(),
        intent: Some(intent),
        scope: Some(scope),
        amends: None,
        description: None,
    }
}
