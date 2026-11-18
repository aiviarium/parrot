//! Claude Code's stream-json output as driver updates (spec 30.2, Appendix C).
//!
//! [`MapState`] holds what the mapping remembers between lines. [`map`] is a pure function from a
//! state and one [`ClaudeOut`] line to the updates it causes and the next state, so the mapping is
//! tested without processes. Driver keys are stable: Claude's `tool_use` ids for tools,
//! `<message id>:<block index>` for text and thinking, the runtime's message id for prompts and
//! the control `request_id` for requests.

use std::collections::{BTreeMap, HashMap};

use parrot_proto::action::{Action, ActionScope, ActionSource, ActionWhen, Effect};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams, DriverUpdate, DriverUpdateKind};
use parrot_proto::driver_data::ChoiceData;
use parrot_proto::ids::MessageId;
use parrot_proto::item::{ItemKind, ItemPatch};
use parrot_proto::open::Open;
use parrot_proto::option::{ConfigOption, PermissionLevel};
use parrot_proto::request::{
    Answer, Answerable, Choice, ChoiceIntent, ChoiceScope, Notify, Request, RequestKind, RequestStatus,
    Subject,
};
use parrot_proto::session::{Ref, RefStrength};
use parrot_proto::state::{State, Turn, TurnError, TurnStatus, TurnStop};
use parrot_proto::subscribe::NoticeLevel;
use parrot_proto::usage::{ContextUsage, Cost, TokenUsage, Usage};
use serde_json::{Map, Value, json};

use crate::data::data;
use crate::wire::{
    self, CanUseTool, ClaudeIn, ClaudeInMsg, ClaudeOut, ClaudeOutMsg, Content, ContentBlock, ControlResponse,
    Delta, Event, PermissionResult, ResultMsg, ResultSubtype, System,
};

/// The turn in progress.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnState {
    pub key: String,
    pub input: Option<MessageId>,
    pub started: String,
    pub cancelled: bool,
}

/// A content block seen in the stream and not yet replaced by its final message.
#[derive(Debug, Clone, PartialEq)]
struct StreamBlock {
    key: String,
    kind: ItemKind,
    finalized: bool,
}

/// A `can_use_tool` request waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRequest {
    pub request: Request,
    pub tool: String,
    pub input: Value,
    pub suggestions: Vec<Value>,
}

/// What the mapping remembers between lines.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MapState {
    pub binding: String,
    pub turn: Option<TurnState>,
    pub turns: u64,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    /// Model values offered by `initialize`.
    pub models: Vec<String>,
    pub actions: Vec<Action>,
    pub requests: BTreeMap<String, PendingRequest>,
    /// Prompts written whose echo, if the CLI replays it, is already reported.
    pub echoes: Vec<(String, MessageId)>,
    message: Option<String>,
    blocks: HashMap<u64, StreamBlock>,
    /// Tool keys by `tool_use_id`, for results and subagent parents.
    tools: HashMap<String, String>,
    unnamed: u64,
    /// Context tokens of the latest top-level API request, from its assistant message.
    context_used: Option<u64>,
    /// Subagent keys (the `tool_use` id of the Task call that runs each).
    subagents: Vec<String>,
}

impl MapState {
    pub fn new(binding: impl Into<String>) -> Self {
        MapState {
            binding: binding.into(),
            ..Default::default()
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

    /// The option values that decide the level, with defaults filled in.
    fn level_values(&self) -> BTreeMap<String, Value> {
        let options = &data().options;
        let mode = self.permission_mode.clone().or_else(|| {
            options
                .get("permissionMode")
                .and_then(|o| o.default.as_ref())
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        BTreeMap::from([("permissionMode".to_owned(), json!(mode))])
    }

    /// The permission level the current mode gives (`levels.kdl`).
    pub fn level(&self) -> PermissionLevel {
        data().levels.level_of(&self.level_values()).0
    }

    pub fn network(&self) -> bool {
        data().levels.level_of(&self.level_values()).1
    }

    /// The `model` and `permissionMode` options with their current values (`options.kdl`), with
    /// the models `initialize` reported in place of the data's.
    pub fn options(&self) -> Vec<ConfigOption> {
        let options = &data().options;
        let models: Vec<ChoiceData> = self.models.iter().cloned().map(ChoiceData::Bare).collect();
        let mut out = Vec::new();
        if let Some(o) = options.get("model") {
            let choices = (!models.is_empty()).then_some(models.as_slice());
            out.push(o.to_option(self.model.clone().map(Value::String), choices));
        }
        if let Some(o) = options.get("permissionMode") {
            out.push(o.to_option(self.permission_mode.clone().map(Value::String), None));
        }
        out
    }

    /// The `options` update for the current values.
    pub fn options_update(&self) -> DriverUpdate {
        self.update(DriverUpdateKind::Options {
            options: self.options(),
            level: self.level(),
            network: self.network(),
        })
    }

    /// Reads the `initialize` control response: models, commands and the permission mode.
    pub fn initialized(&mut self, response: &Value) {
        if let Some(models) = response.get("models").and_then(Value::as_array) {
            self.models = models
                .iter()
                .filter_map(|m| m.get("value").and_then(Value::as_str).map(str::to_owned))
                .collect();
        }
        if let Some(mode) = response.get("current_permission_mode").and_then(Value::as_str) {
            self.permission_mode = Some(mode.to_owned());
        }
        if let Some(commands) = response.get("commands").and_then(Value::as_array) {
            self.actions = commands
                .iter()
                .filter_map(|c| {
                    let name = c.get("name")?.as_str()?;
                    let mut a = command_action(name);
                    a.description = c.get("description").and_then(Value::as_str).map(str::to_owned);
                    a.hint = c
                        .get("argumentHint")
                        .and_then(Value::as_str)
                        .filter(|h| !h.is_empty())
                        .map(str::to_owned);
                    Some(a)
                })
                .collect();
        }
    }

    /// A prompt written to the CLI: the user item, and a new turn unless one is running.
    pub fn prompt(&mut self, p: &BindingPromptParams, now: &str) -> (Vec<DriverUpdate>, BindingDisposition) {
        let mut out = Vec::new();
        let disposition = if self.turn.is_some() {
            BindingDisposition::Steered
        } else {
            out.extend(self.start_turn(Some(p.message.clone()), now));
            BindingDisposition::Started
        };
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
        (out, disposition)
    }

    /// Marks the running turn as cancelled, so its result reads as an interruption.
    pub fn cancel(&mut self) {
        if let Some(t) = self.turn.as_mut() {
            t.cancelled = true;
        }
    }

    fn start_turn(&mut self, input: Option<MessageId>, now: &str) -> Vec<DriverUpdate> {
        self.turns += 1;
        let turn = TurnState {
            key: format!("turn-{}", self.turns),
            input,
            started: now.to_owned(),
            cancelled: false,
        };
        let t = Turn {
            id: turn.key.clone(),
            binding: String::new(),
            status: TurnStatus::Running,
            stop: None,
            continuable: false,
            input: turn.input.clone(),
            started: turn.started.clone(),
            ended: None,
            error: None,
            usage: None,
            meta: None,
            extra: Map::new(),
        };
        self.turn = Some(turn);
        vec![
            self.state(State::Running),
            self.update(DriverUpdateKind::Turn { turn: t }),
        ]
    }

    fn ensure_turn(&mut self, now: &str) -> Vec<DriverUpdate> {
        if self.turn.is_some() {
            vec![]
        } else {
            self.start_turn(None, now)
        }
    }

    /// The key of the latest turn, running or not.
    pub fn last_turn_key(&self) -> Option<String> {
        (self.turns > 0).then(|| format!("turn-{}", self.turns))
    }

    fn turn_key(&self) -> Option<String> {
        self.turn.as_ref().map(|t| t.key.clone())
    }

    /// The answer to the request with driver key `key`, as the control response to write.
    pub fn answer(&mut self, key: &str, answer: &Answer) -> Result<(ClaudeIn, Vec<DriverUpdate>), String> {
        let pending = self
            .requests
            .remove(key)
            .ok_or_else(|| format!("no open request {key}"))?;
        let result = permission_result(&pending, answer);
        let mut request = pending.request;
        request.status = RequestStatus::Answered;
        request.answer = Some(answer.clone());
        let mut out = vec![self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        })];
        if self.requests.is_empty() && self.turn.is_some() {
            out.push(self.state(State::Running));
        }
        let resp = ControlResponse::success(key, result.to_value());
        Ok((Open::Known(ClaudeInMsg::ControlResponse(resp)), out))
    }

    /// One line from the CLI. Control responses are the driver's business and map to nothing.
    pub fn step(&mut self, msg: &ClaudeOut, now: &str) -> Vec<DriverUpdate> {
        let Open::Known(msg) = msg else {
            return vec![];
        };
        match msg {
            ClaudeOutMsg::System(Open::Known(System::Init(init))) => {
                let mut out = Vec::new();
                if let Some(id) = &init.session_id
                    && self.session_id.as_ref() != Some(id)
                {
                    self.session_id = Some(id.clone());
                    out.push(self.update(DriverUpdateKind::Ref {
                        reference: Ref {
                            id: id.clone(),
                            strength: RefStrength::Strong,
                        },
                    }));
                }
                let mode = init
                    .permission_mode
                    .as_ref()
                    .and_then(|m| serde_json::to_value(m).ok())
                    .and_then(|v| v.as_str().map(str::to_owned));
                if (init.model.is_some() && init.model != self.model)
                    || (mode.is_some() && mode != self.permission_mode)
                {
                    if init.model.is_some() {
                        self.model = init.model.clone();
                    }
                    if mode.is_some() {
                        self.permission_mode = mode;
                    }
                    out.push(self.options_update());
                }
                if let Some(cmds) = &init.slash_commands {
                    let actions: Vec<Action> = cmds
                        .iter()
                        .map(|c| {
                            self.actions
                                .iter()
                                .find(|a| a.id == *c)
                                .cloned()
                                .unwrap_or_else(|| command_action(c))
                        })
                        .collect();
                    if actions != self.actions {
                        self.actions = actions.clone();
                        out.push(self.update(DriverUpdateKind::Actions { actions }));
                    }
                }
                out
            }
            ClaudeOutMsg::System(Open::Known(System::CompactBoundary(c))) => {
                let trigger = c
                    .compact_metadata
                    .as_ref()
                    .and_then(|m| m.get("trigger"))
                    .and_then(Value::as_str)
                    .unwrap_or("auto")
                    .to_owned();
                let key = c.uuid.clone().unwrap_or_else(|| self.fresh("compaction"));
                let mut set = Map::new();
                set.insert("trigger".into(), json!(trigger));
                vec![self.item(ItemPatch {
                    id: key,
                    kind: Some(Open::Known(ItemKind::Compaction)),
                    turn: self.turn_key(),
                    set: Some(set),
                    ..Default::default()
                })]
            }
            ClaudeOutMsg::System(Open::Unknown(v)) => {
                if v.get("subtype").and_then(Value::as_str) == Some("api_retry") {
                    let text = format!(
                        "API retry {} of {}: {}",
                        v["attempt"],
                        v["max_retries"],
                        v["error"].as_str().unwrap_or("error")
                    );
                    vec![self.update(DriverUpdateKind::Notice {
                        level: NoticeLevel::Warning,
                        text,
                        code: Some("api_retry".into()),
                    })]
                } else {
                    vec![]
                }
            }
            ClaudeOutMsg::StreamEvent(e) => {
                let mut out = self.ensure_turn(now);
                let parent = self.parent_of(e.parent_tool_use_id.as_ref());
                let updates = self.event(&e.event, parent.clone());
                out.extend(self.sidechain(parent, updates));
                out
            }
            ClaudeOutMsg::Assistant(a) => {
                let mut out = self.ensure_turn(now);
                let parent = self.parent_of(a.parent_tool_use_id.as_ref());
                if parent.is_none()
                    && let Some(u) = &a.message.usage
                {
                    self.context_used = Some(context_tokens(u));
                }
                let updates = self.assistant(&a.message, parent.clone());
                out.extend(self.sidechain(parent, updates));
                out
            }
            ClaudeOutMsg::User(u) => {
                let parent = self.parent_of(u.parent_tool_use_id.as_ref());
                let updates = self.user(u, parent.clone());
                self.sidechain(parent, updates)
            }
            ClaudeOutMsg::Result(r) => self.result(r, now),
            ClaudeOutMsg::ControlRequest(req) => match &req.request {
                Open::Known(wire::ControlRequestBody::CanUseTool(c)) => {
                    let mut out = self.ensure_turn(now);
                    out.extend(self.can_use_tool(&req.request_id, c));
                    out
                }
                _ => vec![],
            },
            ClaudeOutMsg::ControlCancelRequest(c) => match self.requests.remove(&c.request_id) {
                Some(p) => {
                    let mut request = p.request;
                    request.status = RequestStatus::Cancelled;
                    let mut out = vec![self.update(DriverUpdateKind::Request {
                        request: Box::new(request),
                    })];
                    if self.requests.is_empty() && self.turn.is_some() {
                        out.push(self.state(State::Running));
                    }
                    out
                }
                None => vec![],
            },
            _ => vec![],
        }
    }

    /// Updates from a Task call's sidechain (`parent_tool_use_id`) belong to its subagent
    /// (spec 30.3): they carry `subagent` and no `parent`. The subagent item comes first.
    fn sidechain(&mut self, parent: Option<String>, updates: Vec<DriverUpdate>) -> Vec<DriverUpdate> {
        let Some(key) = parent else {
            return updates;
        };
        let mut out = Vec::new();
        if !self.subagents.contains(&key) {
            self.subagents.push(key.clone());
            out.push(self.subagent_item(&key, None, "running"));
        }
        for mut u in updates {
            if let Open::Known(DriverUpdateKind::Item { item }) = &mut u.update {
                item.parent = None;
            }
            u.subagent = Some(key.clone());
            out.push(u);
        }
        out
    }

    /// The parent's item for the subagent that the Task call `key` runs.
    fn subagent_item(&self, key: &str, title: Option<&str>, status: &str) -> DriverUpdate {
        let mut set = Map::new();
        set.insert("key".into(), json!(key));
        if let Some(t) = title {
            set.insert("title".into(), json!(t));
        }
        set.insert("status".into(), json!(status));
        self.item(ItemPatch {
            id: format!("{key}:subagent"),
            kind: Some(Open::Known(ItemKind::Subagent)),
            turn: self.turn_key(),
            parent: Some(key.to_owned()),
            set: Some(set),
            ..Default::default()
        })
    }

    /// For a Task tool call, the subagent item with its title.
    fn task_started(&mut self, t: &wire::ToolUseBlock) -> Option<DriverUpdate> {
        if !SUBAGENT_TOOLS.contains(&t.name.as_str()) {
            return None;
        }
        let title = t
            .input
            .get("description")
            .or_else(|| t.input.get("subagent_type"))
            .and_then(Value::as_str)
            .unwrap_or("Subagent")
            .to_owned();
        if !self.subagents.contains(&t.id) {
            self.subagents.push(t.id.clone());
        }
        Some(self.subagent_item(&t.id, Some(&title), "running"))
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.unnamed += 1;
        format!("{prefix}-{}", self.unnamed)
    }

    fn parent_of(&self, parent: Option<&Option<String>>) -> Option<String> {
        let id = parent?.as_ref()?;
        Some(self.tools.get(id).cloned().unwrap_or_else(|| id.clone()))
    }

    fn event(&mut self, event: &Open<Event>, parent: Option<String>) -> Vec<DriverUpdate> {
        let Open::Known(event) = event else {
            return vec![];
        };
        match event {
            Event::MessageStart { message, .. } => {
                self.message = message.id.clone();
                self.blocks.clear();
                vec![]
            }
            Event::ContentBlockStart {
                index, content_block, ..
            } => {
                let msg = self.message.clone().unwrap_or_else(|| "msg".into());
                let (key, kind, set) = match content_block {
                    Open::Known(ContentBlock::Text(t)) => {
                        (format!("{msg}:{index}"), ItemKind::Assistant, text_set(&t.text))
                    }
                    Open::Known(ContentBlock::Thinking(t)) => (
                        format!("{msg}:{index}"),
                        ItemKind::Reasoning,
                        text_set(&t.thinking),
                    ),
                    Open::Known(ContentBlock::ToolUse(t)) => {
                        self.tools.insert(t.id.clone(), t.id.clone());
                        (
                            t.id.clone(),
                            ItemKind::Tool,
                            tool_set(&t.name, &t.input, "pending"),
                        )
                    }
                    _ => return vec![],
                };
                let task = match content_block {
                    Open::Known(ContentBlock::ToolUse(t)) if !self.subagents.contains(&t.id) => {
                        self.task_started(t)
                    }
                    _ => None,
                };
                self.blocks.insert(
                    *index,
                    StreamBlock {
                        key: key.clone(),
                        kind,
                        finalized: false,
                    },
                );
                let mut out = vec![self.item(ItemPatch {
                    id: key,
                    kind: Some(Open::Known(kind)),
                    turn: self.turn_key(),
                    parent,
                    set: Some(set),
                    streaming: Some(true),
                    ..Default::default()
                })];
                out.extend(task);
                out
            }
            Event::ContentBlockDelta { index, delta, .. } => {
                let Some(block) = self.blocks.get(index) else {
                    return vec![];
                };
                if block.finalized {
                    return vec![];
                }
                let text = match delta {
                    Open::Known(Delta::TextDelta { text }) => text,
                    Open::Known(Delta::ThinkingDelta { thinking }) => thinking,
                    // Tool input arrives whole with the assistant message.
                    _ => return vec![],
                };
                let mut append = BTreeMap::new();
                append.insert("text".to_owned(), text.clone());
                vec![self.item(ItemPatch {
                    id: block.key.clone(),
                    append: Some(append),
                    ..Default::default()
                })]
            }
            _ => vec![],
        }
    }

    fn assistant(&mut self, message: &wire::ApiMessage, parent: Option<String>) -> Vec<DriverUpdate> {
        let Content::Blocks(blocks) = &message.content else {
            let key = self.fresh("text");
            return vec![self.item(ItemPatch {
                id: key,
                kind: Some(Open::Known(ItemKind::Assistant)),
                turn: self.turn_key(),
                parent,
                set: Some(text_set(&message.content.text())),
                ..Default::default()
            })];
        };
        let same_message = message.id.is_some() && message.id == self.message;
        let mut out = Vec::new();
        let mut tasks = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            let (kind, set) = match block {
                Open::Known(ContentBlock::Text(t)) => (ItemKind::Assistant, text_set(&t.text)),
                Open::Known(ContentBlock::Thinking(t)) => (ItemKind::Reasoning, text_set(&t.thinking)),
                Open::Known(ContentBlock::ToolUse(t)) => {
                    self.tools.insert(t.id.clone(), t.id.clone());
                    // The title is in the whole input, which only the final message has.
                    if SUBAGENT_TOOLS.contains(&t.name.as_str()) {
                        tasks.extend(self.task_started(t));
                    }
                    (ItemKind::Tool, tool_set(&t.name, &t.input, "running"))
                }
                _ => continue,
            };
            let key = match block {
                Open::Known(ContentBlock::ToolUse(t)) => {
                    if let Some(b) = self.blocks.values_mut().find(|b| b.key == t.id) {
                        b.finalized = true;
                    }
                    t.id.clone()
                }
                _ => {
                    // The CLI sends a finished block as its own message; match it to the oldest
                    // streamed block of the same kind.
                    let streamed = if same_message {
                        self.blocks
                            .iter()
                            .filter(|(_, b)| !b.finalized && b.kind == kind)
                            .map(|(i, _)| *i)
                            .min()
                    } else {
                        None
                    };
                    match streamed {
                        Some(n) => {
                            let b = self.blocks.get_mut(&n).expect("index came from the map");
                            b.finalized = true;
                            b.key.clone()
                        }
                        None => match &message.id {
                            Some(id) => format!("{id}:final:{i}"),
                            None => self.fresh("block"),
                        },
                    }
                }
            };
            out.push(self.item(ItemPatch {
                id: key,
                kind: Some(Open::Known(kind)),
                turn: self.turn_key(),
                parent: parent.clone(),
                set: Some(set),
                streaming: Some(false),
                ..Default::default()
            }));
        }
        out.extend(tasks);
        out
    }

    fn user(&mut self, u: &wire::User, parent: Option<String>) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if let Content::Blocks(blocks) = &u.message.content {
            for block in blocks {
                if let Open::Known(ContentBlock::ToolResult(r)) = block {
                    let key = self
                        .tools
                        .get(&r.tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| r.tool_use_id.clone());
                    let mut set = Map::new();
                    let status = if r.is_error == Some(true) { "error" } else { "ok" };
                    set.insert("status".into(), json!(status));
                    if let Some(c) = &r.content {
                        set.insert("output".into(), c.clone());
                    }
                    out.push(self.item(ItemPatch {
                        id: key.clone(),
                        set: Some(set),
                        streaming: Some(false),
                        ..Default::default()
                    }));
                    if self.subagents.contains(&key) {
                        let mut set = Map::new();
                        set.insert("status".into(), json!(status));
                        out.push(self.item(ItemPatch {
                            id: format!("{key}:subagent"),
                            set: Some(set),
                            ..Default::default()
                        }));
                    }
                }
            }
        }
        let text = u.message.content.text();
        // The CLI marks an interrupt with a user message of its own; the turn already says it.
        if text.is_empty() || text.starts_with("[Request interrupted by user") {
            return out;
        }
        // An echo of a prompt this driver wrote is already reported under the runtime's id.
        if let Some(pos) = self.echoes.iter().position(|(t, _)| *t == text) {
            self.echoes.remove(pos);
            return out;
        }
        let key = u.uuid.clone().unwrap_or_else(|| self.fresh("user"));
        let mut set = Map::new();
        set.insert("text".into(), json!(text));
        set.insert("status".into(), json!("handled"));
        out.push(self.item(ItemPatch {
            id: key,
            kind: Some(Open::Known(ItemKind::User)),
            turn: self.turn_key(),
            parent,
            set: Some(set),
            ..Default::default()
        }));
        out
    }

    fn result(&mut self, r: &ResultMsg, now: &str) -> Vec<DriverUpdate> {
        let mut out = self.ensure_turn(now);
        let turn = self.turn.take().expect("a turn was ensured");
        let interrupted = turn.cancelled || r.terminal_reason.as_deref() == Some("interrupted");
        let (status, stop) = match &r.subtype {
            _ if interrupted => (TurnStatus::Interrupted, TurnStop::Cancelled),
            Open::Known(ResultSubtype::Success) if !r.is_error => (TurnStatus::Completed, TurnStop::End),
            Open::Known(ResultSubtype::ErrorMaxTurns) => (TurnStatus::Completed, TurnStop::TurnLimit),
            Open::Known(ResultSubtype::ErrorMaxBudgetUsd) => (TurnStatus::Completed, TurnStop::Budget),
            _ => (TurnStatus::Failed, TurnStop::Error),
        };
        let error = (status == TurnStatus::Failed).then(|| TurnError {
            code: serde_json::to_value(&r.subtype)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".into()),
            message: r
                .result
                .clone()
                .or_else(|| {
                    r.extra
                        .get("errors")
                        .and_then(|e| e.get(0))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "the turn failed".into()),
            retryable: true,
        });
        let usage = usage_of(r, self.context_used.take());
        // Blocks cut off by an interrupt never get their final message.
        let mut open_blocks: Vec<_> = self.blocks.iter().filter(|(_, b)| !b.finalized).collect();
        open_blocks.sort_by_key(|(i, _)| **i);
        let closes: Vec<_> = open_blocks
            .into_iter()
            .map(|(_, b)| ItemPatch {
                id: b.key.clone(),
                streaming: Some(false),
                ..Default::default()
            })
            .collect();
        for patch in closes {
            out.push(self.item(patch));
        }
        // Any request still open is moot once the turn ends.
        let open: Vec<_> = std::mem::take(&mut self.requests).into_values().collect();
        for p in open {
            let mut request = p.request;
            request.status = RequestStatus::Cancelled;
            out.push(self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            }));
        }
        out.push(self.update(DriverUpdateKind::Turn {
            turn: Turn {
                id: turn.key,
                binding: String::new(),
                status,
                stop: Some(Open::Known(stop)),
                continuable: matches!(stop, TurnStop::TurnLimit | TurnStop::Budget | TurnStop::Cancelled),
                input: turn.input,
                started: turn.started,
                ended: Some(now.to_owned()),
                error,
                usage: Some(usage.clone()),
                meta: None,
                extra: Map::new(),
            },
        }));
        out.push(self.update(DriverUpdateKind::Usage { usage }));
        out.push(self.state(State::Idle));
        self.blocks.clear();
        self.message = None;
        self.echoes.clear();
        out
    }

    fn can_use_tool(&mut self, request_id: &str, c: &CanUseTool) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        let tool = c.tool_name.as_str();
        let suggestions = c.permission_suggestions.clone().unwrap_or_default();
        let (kind, title, subject, choices) = match tool {
            "AskUserQuestion" => {
                let first = c.input.pointer("/questions/0");
                let title = first
                    .and_then(|q| q.get("question"))
                    .and_then(Value::as_str)
                    .unwrap_or("Claude has a question")
                    .to_owned();
                let choices = first
                    .and_then(|q| q.get("options"))
                    .and_then(Value::as_array)
                    .map(|opts| {
                        opts.iter()
                            .enumerate()
                            .filter_map(|(i, o)| {
                                Some(Choice {
                                    id: format!("option-{i}"),
                                    label: o.get("label")?.as_str()?.to_owned(),
                                    intent: Some(ChoiceIntent::Neutral),
                                    scope: None,
                                    amends: None,
                                    description: o
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                })
                            })
                            .collect::<Vec<_>>()
                    });
                (RequestKind::Question, title, None, choices)
            }
            "ExitPlanMode" => {
                let text = c
                    .input
                    .get("plan")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let mut set = Map::new();
                set.insert("text".into(), json!(text));
                out.push(self.item(ItemPatch {
                    id: format!("plan-{request_id}"),
                    kind: Some(Open::Known(ItemKind::Plan)),
                    turn: self.turn_key(),
                    set: Some(set),
                    ..Default::default()
                }));
                let choices = vec![
                    choice(
                        "allow",
                        "Start",
                        ChoiceIntent::Allow,
                        Some(ChoiceScope::Once),
                        None,
                    ),
                    choice("deny", "Keep planning", ChoiceIntent::Deny, None, None),
                ];
                (
                    RequestKind::Plan,
                    "Leave plan mode and start on the plan".to_owned(),
                    Some(Subject::Plan { text }),
                    Some(choices),
                )
            }
            _ => {
                let name = c.display_name.as_deref().unwrap_or(tool);
                let detail = c
                    .input
                    .get("command")
                    .or_else(|| c.input.get("file_path"))
                    .or_else(|| c.input.get("url"))
                    .and_then(Value::as_str);
                let title = match detail {
                    Some(d) => format!("{name}: {d}"),
                    None => name.to_owned(),
                };
                let mut choices = vec![choice(
                    "allow",
                    "Allow",
                    ChoiceIntent::Allow,
                    Some(ChoiceScope::Once),
                    None,
                )];
                for (i, s) in suggestions.iter().enumerate() {
                    let scope = match s.get("destination").and_then(Value::as_str) {
                        Some("session") | Some("cliArg") => ChoiceScope::Session,
                        _ => ChoiceScope::Always,
                    };
                    let label = match scope {
                        ChoiceScope::Session => "Allow for this session",
                        _ => "Always allow",
                    };
                    choices.push(choice(
                        &format!("suggestion-{i}"),
                        label,
                        ChoiceIntent::Allow,
                        Some(scope),
                        Some(s.clone()),
                    ));
                }
                choices.push(choice("deny", "Deny", ChoiceIntent::Deny, None, None));
                let subject = Subject::Tool {
                    tool: tool.to_owned(),
                    input: c.input.clone(),
                    editable: Some(true),
                };
                (RequestKind::Approval, title, Some(subject), Some(choices))
            }
        };
        let request = Request {
            id: request_id.to_owned(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: self.turn_key(),
            item: c
                .tool_use_id
                .as_ref()
                .map(|id| self.tools.get(id).cloned().unwrap_or_else(|| id.clone())),
            kind: Open::Known(kind),
            title,
            description: None,
            subject: subject.map(Open::Known),
            level: tool_level(tool),
            network: None,
            choices,
            input: (kind == RequestKind::Question).then(|| c.input.clone()),
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
            request_id.to_owned(),
            PendingRequest {
                request: request.clone(),
                tool: tool.to_owned(),
                input: c.input.clone(),
                suggestions,
            },
        );
        out.push(self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        }));
        out.push(self.state(State::RequiresAction));
        out
    }
}

/// The pure form of [`MapState::step`]: the updates one line causes and the state after it.
pub fn map(state: &MapState, msg: &ClaudeOut, now: &str) -> (Vec<DriverUpdate>, MapState) {
    let mut next = state.clone();
    let updates = next.step(msg, now);
    (updates, next)
}

fn permission_result(p: &PendingRequest, a: &Answer) -> PermissionResult {
    let deny = |message: Option<&str>| PermissionResult::Deny {
        message: message
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("The user denied this. Do not try it again; ask what to do instead.")
            .to_owned(),
        interrupt: None,
        extra: Map::new(),
    };
    // Claude Code wants `updatedInput` on every allow.
    let allow = |input: Value, perms: Option<Vec<Value>>| PermissionResult::Allow {
        updated_input: Some(input),
        updated_permissions: perms,
        extra: Map::new(),
    };
    if p.tool == "AskUserQuestion" {
        if a.choice.as_deref() == Some("deny") {
            return deny(a.message.as_deref());
        }
        let mut answers = match &a.value {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let first = p.input.pointer("/questions/0/question").and_then(Value::as_str);
        if answers.is_empty()
            && let Some(q) = first
        {
            let label = a
                .choice
                .as_ref()
                .and_then(|id| p.request.choices.as_ref()?.iter().find(|c| c.id == *id))
                .map(|c| c.label.clone())
                .or_else(|| a.value.as_ref().and_then(Value::as_str).map(str::to_owned))
                .or_else(|| a.message.clone());
            if let Some(label) = label {
                answers.insert(q.to_owned(), json!(label));
            }
        }
        let mut input = a.input.clone().unwrap_or_else(|| p.input.clone());
        if let Value::Object(m) = &mut input {
            m.insert("answers".into(), Value::Object(answers));
        }
        return allow(input, None);
    }
    let input = a.input.clone().unwrap_or_else(|| p.input.clone());
    match a.choice.as_deref() {
        Some("deny") => deny(a.message.as_deref()),
        Some(id) if id.starts_with("suggestion-") => {
            let perms = id["suggestion-".len()..]
                .parse::<usize>()
                .ok()
                .and_then(|i| p.suggestions.get(i).cloned())
                .map(|s| vec![s]);
            allow(input, perms)
        }
        Some(_) => allow(input, None),
        None if a.input.is_some() => allow(input, None),
        None => deny(a.message.as_deref()),
    }
}

fn context_tokens(u: &wire::Usage) -> u64 {
    u.input_tokens.unwrap_or(0)
        + u.cache_read_input_tokens.unwrap_or(0)
        + u.cache_creation_input_tokens.unwrap_or(0)
        + u.output_tokens.unwrap_or(0)
}

/// The turn's usage. `used` is the context of the last API request: the result's last
/// iteration, else the last assistant message's usage. The result's totals add up every request
/// of the turn, which overstates the context.
fn usage_of(r: &ResultMsg, last: Option<u64>) -> Usage {
    let tokens = r.usage.as_ref().map(|u| TokenUsage {
        input: u.input_tokens.unwrap_or(0),
        output: u.output_tokens.unwrap_or(0),
        cache_read: u.cache_read_input_tokens,
        cache_write: u.cache_creation_input_tokens,
        reasoning: None,
    });
    let window = r.model_usage.as_ref().and_then(|m| {
        m.values()
            .filter_map(|v| v.get("contextWindow").and_then(Value::as_u64))
            .max()
    });
    let iteration = r
        .usage
        .as_ref()
        .and_then(|u| u.extra.get("iterations"))
        .and_then(Value::as_array)
        .and_then(|i| i.last())
        .and_then(|i| serde_json::from_value::<wire::Usage>(i.clone()).ok())
        .map(|u| context_tokens(&u));
    let used = iteration.or(last).or_else(|| {
        tokens
            .as_ref()
            .map(|t| t.input + t.cache_read.unwrap_or(0) + t.cache_write.unwrap_or(0) + t.output)
    });
    let context = match (window, used) {
        (Some(window), Some(used)) => Some(ContextUsage { used, window }),
        _ => None,
    };
    Usage {
        tokens,
        cost: r.cost_usd().map(|amount| Cost {
            amount,
            currency: "USD".into(),
        }),
        context,
        ..Default::default()
    }
}

/// Tools whose calls run a subagent in a sidechain.
const SUBAGENT_TOOLS: &[&str] = &["Task", "Agent"];

fn tool_level(tool: &str) -> PermissionLevel {
    data().levels.tool_level(tool)
}

fn command_action(name: &str) -> Action {
    Action {
        id: name.into(),
        name: Some(format!("/{name}")),
        label: name.into(),
        description: None,
        input: None,
        hint: None,
        scope: ActionScope::Session,
        when: vec![ActionWhen::Idle],
        effects: vec![Effect::Turn],
        requires: None,
        source: Open::Known(ActionSource::Harness),
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

fn text_set(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("text".into(), json!(text));
    m
}

fn tool_set(name: &str, input: &Value, status: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("tool".into(), json!(name));
    m.insert("title".into(), json!(name));
    m.insert("input".into(), input.clone());
    m.insert("status".into(), json!(status));
    m.insert("caller".into(), json!("model"));
    m
}
