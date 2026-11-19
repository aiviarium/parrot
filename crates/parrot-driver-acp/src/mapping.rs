//! ACP session updates as driver updates (spec 30.2, Appendix C).
//!
//! [`MapState`] holds what the mapping remembers between messages. Driver keys are stable: the
//! agent's `toolCallId` for tools, `<messageId>:t` and `<messageId>:r` for assistant text and
//! reasoning, the runtime's message id for prompts, and `acp-perm-<id>` for permission requests.

use std::collections::{BTreeMap, HashMap};

use parrot_proto::action::{Action, ActionScope, ActionSource, ActionWhen, Effect};
use parrot_proto::driver::{BindingDisposition, BindingPromptParams, DriverUpdate, DriverUpdateKind};
use parrot_proto::ids::MessageId;
use parrot_proto::item::ItemKind;
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
use parrot_proto::usage::{ContextUsage, Cost, TokenUsage, Usage};
use serde_json::{Map, Value, json};

use crate::data::Data;
use crate::wire::{
    self, ContentChunk, PermissionRequest, PromptResult, SessionResult, SessionUpdate, SessionUpdateBody,
    ToolCall,
};

/// The cap on a tool output line the agent sends as content.
const OUTPUT_MAX: usize = 20_000;

/// What the agent advertises in `initialize`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    pub image: bool,
    pub embedded_context: bool,
    pub mcp_http: bool,
    pub resume: bool,
    pub load: bool,
}

impl Capabilities {
    pub fn from_initialize(init: &wire::InitializeResult) -> Self {
        let caps = &init.agent_capabilities;
        Capabilities {
            image: caps.prompt_capabilities.image,
            embedded_context: caps.prompt_capabilities.embedded_context,
            mcp_http: caps.mcp_capabilities.http,
            resume: caps.session_capabilities.resume.is_some(),
            load: caps.load_session,
        }
    }
}

/// The turn in progress.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnState {
    pub key: String,
    pub input: Option<MessageId>,
    pub started: String,
    pub cancelled: bool,
}

/// A permission request waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRequest {
    pub request: Request,
    /// The agent's raw JSON-RPC id, echoed back in the response.
    pub raw: Value,
    pub options: Vec<wire::PermissionOption>,
}

/// One choice of a select option.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

/// What the mapping remembers between messages.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapState {
    pub binding: String,
    pub turn: Option<TurnState>,
    pub turns: u64,
    pub session_id: Option<String>,
    /// The ACP version negotiated in `initialize`: 2, or 1 for an agent that answered 1.
    pub protocol: u32,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub modes: Vec<ChoiceItem>,
    pub models: Vec<ChoiceItem>,
    pub efforts: Vec<ChoiceItem>,
    /// The config option id that carries the model or the effort.
    model_config: Option<String>,
    effort_config: Option<String>,
    /// Select config options this driver does not map to a role.
    extra_options: Vec<ConfigOption>,
    pub actions: Vec<Action>,
    pub requests: BTreeMap<String, PendingRequest>,
    pub capabilities: Capabilities,
    context: Option<(u64, u64)>,
    cost: Option<f64>,
    /// The item streaming now, by `assistant` or `reasoning`.
    current: HashMap<&'static str, String>,
    tool_titles: HashMap<String, String>,
    /// The agent's `sessionId` from a permission request that came first.
    session_hint: Option<String>,
    /// While `session/load` replays history: its updates become items, not turns.
    pub replaying: bool,
    /// The item each replay chunk streams into, by `user`, `assistant` or `reasoning`.
    replay_current: HashMap<&'static str, String>,
    /// The text replayed so far, by item id.
    replay_text: HashMap<String, String>,
    unnamed: u64,
    data: Data,
}

impl MapState {
    pub fn new(binding: impl Into<String>, data: Data) -> Self {
        MapState {
            binding: binding.into(),
            data,
            ..Default::default()
        }
    }

    pub fn update(&self, kind: DriverUpdateKind) -> DriverUpdate {
        DriverUpdate {
            binding: self.binding.clone(),
            subagent: None,
            update: Open::Known(kind),
            extra: Map::new(),
        }
    }

    fn item(&self, patch: parrot_proto::item::ItemPatch) -> DriverUpdate {
        self.update(DriverUpdateKind::Item { item: patch })
    }

    fn state(&self, state: State) -> DriverUpdate {
        self.update(DriverUpdateKind::State {
            state,
            background: vec![],
            detail: None,
        })
    }

    fn notice(&self, level: NoticeLevel, text: impl Into<String>, code: &str) -> DriverUpdate {
        self.update(DriverUpdateKind::Notice {
            level,
            text: text.into(),
            code: Some(code.into()),
        })
    }

    /// The level the agent's current mode gives (spec 15.2).
    pub fn level(&self) -> PermissionLevel {
        self.data.level_of(self.mode.as_deref()).0
    }

    pub fn network(&self) -> bool {
        self.data.level_of(self.mode.as_deref()).1
    }

    /// The `mode`, `model`, `effort` and other select options.
    pub fn options(&self) -> Vec<ConfigOption> {
        let mut out = vec![];
        if !self.modes.is_empty() {
            out.push(select_option(
                "mode",
                Some(OptionRole::Mode),
                "Mode",
                &self.modes,
                self.mode.clone(),
            ));
        }
        if !self.models.is_empty() || self.model.is_some() {
            out.push(select_option(
                "model",
                Some(OptionRole::Model),
                "Model",
                &self.models,
                self.model.clone(),
            ));
        }
        if !self.efforts.is_empty() {
            out.push(select_option(
                "effort",
                Some(OptionRole::Effort),
                "Reasoning level",
                &self.efforts,
                self.effort.clone(),
            ));
        }
        out.extend(self.extra_options.iter().cloned());
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

    /// Reads what `session/new`, `session/load` or `session/resume` returned.
    pub fn opened(&mut self, result: &SessionResult) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if let Some(id) = &result.session_id
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
        if let Some(modes) = &result.modes {
            self.modes = modes
                .available_modes
                .iter()
                .map(|m| ChoiceItem {
                    id: m.id.clone(),
                    label: m.name.clone().unwrap_or_else(|| m.id.clone()),
                    description: m.description.clone(),
                })
                .collect();
            if let Some(current) = &modes.current_mode_id {
                self.mode = Some(current.clone());
            }
        }
        if let Some(models) = &result.models {
            self.models = models
                .available_models
                .iter()
                .map(|m| {
                    let id = m.model_id.clone().or_else(|| m.id.clone()).unwrap_or_default();
                    ChoiceItem {
                        label: m.name.clone().unwrap_or_else(|| id.clone()),
                        id,
                        description: m.description.clone(),
                    }
                })
                .collect();
            if let Some(current) = &models.current_model_id {
                self.model = Some(current.clone());
            }
        }
        if let Some(config) = &result.config_options {
            out.extend(self.apply_config(config));
        }
        out.push(self.options_update());
        out
    }

    fn apply_config(&mut self, config: &[wire::ConfigOption]) -> Vec<DriverUpdate> {
        for option in config {
            if option.kind.as_deref().is_some_and(|k| k != "select") {
                continue;
            }
            let choices: Vec<ChoiceItem> = option
                .flat_choices()
                .iter()
                .filter_map(|c| {
                    let id = c.value.clone()?;
                    Some(ChoiceItem {
                        label: c.name.clone().unwrap_or_else(|| id.clone()),
                        id,
                        description: c.description.clone(),
                    })
                })
                .collect();
            let current = option.current_value.as_str().map(str::to_owned);
            let category = option.category.as_deref();
            if category == Some("model") || option.id == "model" {
                self.model_config = Some(option.id.clone());
                if !choices.is_empty() {
                    self.models = choices;
                }
                if current.is_some() {
                    self.model = current;
                }
            } else if category == Some("thought_level")
                || option.id == "effort"
                || option.id == "thought_level"
            {
                self.effort_config = Some(option.id.clone());
                if !choices.is_empty() {
                    self.efforts = choices;
                }
                if current.is_some() {
                    self.effort = current;
                }
            } else {
                let mut o = select_option(
                    &option.id,
                    None,
                    option.name.as_deref().unwrap_or(&option.id),
                    &choices,
                    current,
                );
                o.description = option.description.clone();
                self.extra_options.retain(|e| e.id != o.id);
                self.extra_options.push(o);
            }
        }
        vec![]
    }

    /// A prompt: its ACP content blocks, the user item and a new turn unless one runs.
    pub fn prompt(
        &mut self,
        p: &BindingPromptParams,
        now: &str,
    ) -> (Vec<DriverUpdate>, BindingDisposition, Vec<Value>) {
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
        out.push(self.item(parrot_proto::item::ItemPatch {
            id: p.message.clone(),
            kind: Some(Open::Known(ItemKind::User)),
            turn: self.turn_key(),
            set: Some(set),
            ..Default::default()
        }));
        let blocks = self.content_blocks(p, &mut out);
        (out, disposition, blocks)
    }

    /// The prompt's content: the text last, after context and attachments (spec 13.1).
    fn content_blocks(&self, p: &BindingPromptParams, out: &mut Vec<DriverUpdate>) -> Vec<Value> {
        let mut blocks = Vec::new();
        for context in p.context.iter().flatten() {
            if self.capabilities.embedded_context {
                blocks.push(json!({
                    "type": "resource",
                    "resource": {
                        "uri": context.uri,
                        "text": context.text,
                        "mimeType": "text/markdown",
                    },
                }));
            } else {
                blocks.push(json!({
                    "type": "text",
                    "text": format!("<context uri=\"{}\">\n{}\n</context>", context.uri, context.text),
                }));
            }
        }
        for attachment in p.attachments.iter().flatten() {
            self.attachment(attachment, out, &mut blocks);
        }
        blocks.push(json!({ "type": "text", "text": p.text }));
        blocks
    }

    fn attachment(&self, attachment: &Attachment, out: &mut Vec<DriverUpdate>, blocks: &mut Vec<Value>) {
        let image = attachment.media_type.starts_with("image/");
        if image
            && self.capabilities.image
            && let Some(data) = &attachment.data
        {
            blocks.push(json!({
                "type": "image",
                "data": data,
                "mimeType": attachment.media_type,
            }));
            return;
        }
        if self.capabilities.embedded_context {
            if let Some(data) = &attachment.data {
                // Text goes in as text; bytes go in as the base64 blob the agent expects.
                let textual =
                    attachment.media_type.starts_with("text/") || attachment.media_type == "application/json";
                let mut resource = json!({
                    "uri": attachment.uri.clone().unwrap_or_else(|| format!("file:///{}", attachment.name)),
                    "mimeType": attachment.media_type,
                });
                if textual {
                    resource["text"] = json!(wire::decode_base64(data));
                } else {
                    resource["blob"] = json!(data);
                }
                blocks.push(json!({ "type": "resource", "resource": resource }));
                return;
            }
            if let Some(uri) = &attachment.uri {
                blocks.push(json!({
                    "type": "resource_link",
                    "uri": uri,
                    "name": attachment.name,
                    "mimeType": attachment.media_type,
                }));
                return;
            }
        }
        let what = if image { "an image" } else { "a file" };
        out.push(self.notice(
            NoticeLevel::Warning,
            format!(
                "The agent cannot take {what} in a prompt, so {} was left out.",
                attachment.name
            ),
            "x.parrot/attachment",
        ));
        blocks.push(json!({
            "type": "text",
            "text": format!("[attachment {} ({})]", attachment.name, attachment.media_type),
        }));
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

    fn turn_key(&self) -> Option<String> {
        self.turn.as_ref().map(|t| t.key.clone())
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.unnamed += 1;
        format!("{prefix}-{}", self.unnamed)
    }

    /// One `session/update` the agent sends while `session/load` replays history: the stored
    /// conversation as items, with no turn (E4). Chunked text accumulates under the item id the
    /// agent gave the message.
    pub fn replay(&mut self, update: &SessionUpdate) -> Vec<DriverUpdate> {
        let Open::Known(update) = update else {
            return vec![];
        };
        match update {
            SessionUpdateBody::UserMessageChunk(chunk) => self.replay_chunk("user", ItemKind::User, chunk),
            SessionUpdateBody::AgentMessageChunk(chunk) => {
                self.replay_chunk("assistant", ItemKind::Assistant, chunk)
            }
            SessionUpdateBody::AgentThoughtChunk(chunk) => {
                self.replay_chunk("reasoning", ItemKind::Reasoning, chunk)
            }
            SessionUpdateBody::ToolCall(call) => self.tool_call(call),
            SessionUpdateBody::ToolCallUpdate(call) => self.tool_call_update(call),
            SessionUpdateBody::Plan(plan) => {
                let entries: Vec<Value> = plan
                    .entries
                    .iter()
                    .map(|e| {
                        json!({
                            "text": e.content,
                            "status": plan_status(e.status.as_deref()),
                        })
                    })
                    .collect();
                let text = plan
                    .entries
                    .iter()
                    .map(|e| {
                        let mark = if plan_status(e.status.as_deref()) == "done" {
                            "x"
                        } else {
                            " "
                        };
                        format!("- [{mark}] {}", e.content)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut set = Map::new();
                set.insert("entries".into(), Value::Array(entries));
                set.insert("text".into(), json!(text));
                vec![self.item(parrot_proto::item::ItemPatch {
                    id: "acp-history-plan".into(),
                    kind: Some(Open::Known(ItemKind::Plan)),
                    set: Some(set),
                    ..Default::default()
                })]
            }
            _ => vec![],
        }
    }

    /// Ends a `session/load` replay: the item each message was streaming into is finished.
    pub fn replay_finish(&mut self) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        for id in self.replay_text.keys().cloned().collect::<Vec<_>>() {
            out.push(self.item(parrot_proto::item::ItemPatch {
                id,
                streaming: Some(false),
                ..Default::default()
            }));
        }
        self.replay_text.clear();
        self.replay_current.clear();
        self.replaying = false;
        out
    }

    fn replay_chunk(
        &mut self,
        kind: &'static str,
        item_kind: ItemKind,
        chunk: &ContentChunk,
    ) -> Vec<DriverUpdate> {
        let Some(text) = wire::block_text(&chunk.content) else {
            return vec![];
        };
        let id = match &chunk.message_id {
            Some(message) => format!("{message}:{kind}"),
            None => {
                if let Some(id) = self.replay_current.get(kind) {
                    id.clone()
                } else {
                    self.unnamed += 1;
                    let id = format!("acp-history-{kind}-{}", self.unnamed);
                    self.replay_current.insert(kind, id.clone());
                    id
                }
            }
        };
        self.replay_current.insert(kind, id.clone());
        let stored = self.replay_text.entry(id.clone()).or_default();
        stored.push_str(&text);
        let text = stored.clone();
        vec![self.item(parrot_proto::item::ItemPatch {
            id,
            kind: Some(Open::Known(item_kind)),
            set: Some(map_of("text", json!(text))),
            streaming: Some(true),
            ..Default::default()
        })]
    }

    /// One `session/update` notification.
    pub fn step(&mut self, update: &SessionUpdate, now: &str) -> Vec<DriverUpdate> {
        let Open::Known(update) = update else {
            return vec![];
        };
        match update {
            SessionUpdateBody::AgentMessageChunk(chunk) => self.text("assistant", chunk, ItemKind::Assistant),
            SessionUpdateBody::AgentThoughtChunk(chunk) => self.text("reasoning", chunk, ItemKind::Reasoning),
            SessionUpdateBody::UserMessageChunk(_) => vec![],
            SessionUpdateBody::ToolCall(call) => {
                let mut out = self.ensure_turn(now);
                out.extend(self.tool_call(call));
                out
            }
            SessionUpdateBody::ToolCallUpdate(call) => self.tool_call_update(call),
            SessionUpdateBody::Plan(plan) => {
                let mut out = self.ensure_turn(now);
                let entries: Vec<Value> = plan
                    .entries
                    .iter()
                    .map(|e| {
                        json!({
                            "text": e.content,
                            "status": plan_status(e.status.as_deref()),
                        })
                    })
                    .collect();
                let text = plan
                    .entries
                    .iter()
                    .map(|e| {
                        let mark = if plan_status(e.status.as_deref()) == "done" {
                            "x"
                        } else {
                            " "
                        };
                        format!("- [{mark}] {}", e.content)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let key = self
                    .turn_key()
                    .map(|t| format!("plan-{t}"))
                    .unwrap_or_else(|| "plan".into());
                let mut set = Map::new();
                set.insert("entries".into(), Value::Array(entries));
                set.insert("text".into(), json!(text));
                out.push(self.item(parrot_proto::item::ItemPatch {
                    id: key,
                    kind: Some(Open::Known(ItemKind::Plan)),
                    turn: self.turn_key(),
                    set: Some(set),
                    ..Default::default()
                }));
                out
            }
            SessionUpdateBody::AvailableCommandsUpdate(update) => {
                let actions: Vec<Action> = update
                    .available_commands
                    .iter()
                    .map(|c| Action {
                        id: c.name.clone(),
                        name: Some(format!("/{}", c.name)),
                        label: c.name.clone(),
                        description: c.description.clone(),
                        input: None,
                        hint: c
                            .input
                            .as_ref()
                            .and_then(|i| i.hint.clone())
                            .filter(|h| !h.is_empty()),
                        scope: ActionScope::Session,
                        when: vec![ActionWhen::Idle],
                        effects: vec![Effect::Turn],
                        requires: None,
                        source: Open::Known(ActionSource::Harness),
                        meta: None,
                        extra: Map::new(),
                    })
                    .collect();
                if actions == self.actions {
                    return vec![];
                }
                self.actions = actions.clone();
                vec![self.update(DriverUpdateKind::Actions { actions })]
            }
            SessionUpdateBody::CurrentModeUpdate(update) => {
                let Some(mode) = &update.current_mode_id else {
                    return vec![];
                };
                if self.mode.as_ref() == Some(mode) {
                    return vec![];
                }
                self.mode = Some(mode.clone());
                vec![self.options_update()]
            }
            SessionUpdateBody::ConfigOptionUpdate(update) => {
                let Some(config) = &update.config_options else {
                    return vec![];
                };
                self.apply_config(config);
                vec![self.options_update()]
            }
            SessionUpdateBody::UsageUpdate(update) => {
                self.context = (update.size > 0).then_some((update.used, update.size));
                if let Some(cost) = &update.cost
                    && cost.get("currency").and_then(Value::as_str) == Some("USD")
                {
                    self.cost = cost.get("amount").and_then(Value::as_f64);
                }
                vec![self.update(DriverUpdateKind::Usage {
                    usage: self.usage(None),
                })]
            }
            SessionUpdateBody::Notice(notice) => {
                let text = notice
                    .message
                    .clone()
                    .or_else(|| notice.text.clone())
                    .unwrap_or_default();
                if text.is_empty() {
                    return vec![];
                }
                let level = match notice.level.as_deref() {
                    Some("error") => NoticeLevel::Error,
                    Some("warning") => NoticeLevel::Warning,
                    _ => NoticeLevel::Info,
                };
                vec![self.notice(level, text, "x.parrot/acp_notice")]
            }
        }
    }

    fn text(&mut self, kind: &'static str, chunk: &ContentChunk, item_kind: ItemKind) -> Vec<DriverUpdate> {
        let Some(text) = wire::block_text(&chunk.content) else {
            return vec![];
        };
        let suffix = if kind == "assistant" { "t" } else { "r" };
        let id = chunk
            .message_id
            .as_ref()
            .map(|m| format!("{m}:{suffix}"))
            .or_else(|| self.current.get(kind).cloned());
        let id = match id {
            Some(id) => id,
            None => {
                self.unnamed += 1;
                format!("acp-{kind}-{}-{}", self.turns, self.unnamed)
            }
        };
        if self.current.get(kind) == Some(&id) {
            let mut append = BTreeMap::new();
            append.insert("text".to_owned(), text);
            return vec![self.item(parrot_proto::item::ItemPatch {
                id,
                append: Some(append),
                ..Default::default()
            })];
        }
        // Another message started: the one before is done.
        let mut out = Vec::new();
        if let Some(previous) = self.current.get(kind).cloned() {
            out.push(self.item(parrot_proto::item::ItemPatch {
                id: previous,
                streaming: Some(false),
                ..Default::default()
            }));
        }
        self.current.insert(kind, id.clone());
        let mut set = Map::new();
        set.insert("text".into(), json!(text));
        out.push(self.item(parrot_proto::item::ItemPatch {
            id,
            kind: Some(Open::Known(item_kind)),
            turn: self.turn_key(),
            set: Some(set),
            streaming: Some(true),
            ..Default::default()
        }));
        out
    }

    fn tool_call(&mut self, call: &ToolCall) -> Vec<DriverUpdate> {
        let id = match &call.tool_call_id {
            Some(id) => id.clone(),
            None => self.fresh("tool"),
        };
        let title = call
            .title
            .clone()
            .unwrap_or_else(|| call.kind.clone().unwrap_or_else(|| "tool".into()));
        self.tool_titles.insert(id.clone(), title.clone());
        let mut set = Map::new();
        set.insert(
            "tool".into(),
            json!(call.kind.clone().unwrap_or_else(|| "tool".into())),
        );
        set.insert("title".into(), json!(title));
        set.insert(
            "input".into(),
            call.raw_input.clone().unwrap_or_else(|| json!({})),
        );
        set.insert("status".into(), json!(tool_status(call.status.as_deref())));
        set.insert("caller".into(), json!("model"));
        if let Some(output) = output_of(call) {
            set.insert("output".into(), json!(output));
        }
        vec![self.item(parrot_proto::item::ItemPatch {
            id,
            kind: Some(Open::Known(ItemKind::Tool)),
            turn: self.turn_key(),
            set: Some(set),
            ..Default::default()
        })]
    }

    fn tool_call_update(&mut self, call: &ToolCall) -> Vec<DriverUpdate> {
        let Some(id) = call.tool_call_id.clone() else {
            return vec![];
        };
        let mut set = Map::new();
        if let Some(status) = &call.status {
            set.insert("status".into(), json!(tool_status(Some(status))));
        }
        if let Some(title) = &call.title {
            self.tool_titles.insert(id.clone(), title.clone());
            set.insert("title".into(), json!(title));
        }
        if let Some(input) = &call.raw_input {
            set.insert("input".into(), input.clone());
        }
        if let Some(output) = output_of(call) {
            set.insert("output".into(), json!(output));
        }
        if set.is_empty() {
            return vec![];
        }
        let terminal = call
            .status
            .as_deref()
            .is_some_and(|s| matches!(s, "completed" | "failed" | "cancelled"));
        vec![self.item(parrot_proto::item::ItemPatch {
            id,
            set: Some(set),
            streaming: terminal.then_some(false),
            ..Default::default()
        })]
    }

    /// A `session/request_permission`: a request with the agent's options as choices.
    pub fn permission(&mut self, raw: &Value, p: &PermissionRequest, now: &str) -> Vec<DriverUpdate> {
        let mut out = self.ensure_turn(now);
        if self.session_id.is_none() {
            self.session_hint = p.session_id.clone();
        }
        let key = format!("acp-perm-{}", raw_key(raw));
        let call = p.tool_call.clone().unwrap_or_default();
        let title = call
            .title
            .clone()
            .or_else(|| call.kind.clone())
            .unwrap_or_else(|| "The agent asks to go ahead".into());
        let tool = call
            .kind
            .clone()
            .or_else(|| call.title.clone())
            .unwrap_or_else(|| "tool".into());
        let choices: Vec<Choice> = p
            .options
            .iter()
            .map(|o| {
                let intent = if o.kind.starts_with("allow") {
                    ChoiceIntent::Allow
                } else if o.kind.starts_with("reject") || o.kind.starts_with("deny") {
                    ChoiceIntent::Deny
                } else {
                    ChoiceIntent::Neutral
                };
                let scope = if o.kind.ends_with("always") {
                    Some(ChoiceScope::Session)
                } else {
                    Some(ChoiceScope::Once)
                };
                Choice {
                    id: o.option_id.clone(),
                    label: o.name.clone(),
                    intent: Some(intent),
                    scope,
                    amends: None,
                    description: None,
                }
            })
            .collect();
        let item = call
            .tool_call_id
            .clone()
            .filter(|id| self.tool_titles.contains_key(id));
        let request = Request {
            id: key.clone(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: self.turn_key(),
            item,
            kind: Open::Known(RequestKind::Approval),
            title,
            description: None,
            subject: Some(Open::Known(Subject::Tool {
                tool: tool.clone(),
                input: call.raw_input.clone().unwrap_or_else(|| json!({})),
                editable: Some(false),
            })),
            level: self.data.tool_level(&tool, call.title.as_deref().unwrap_or("")),
            network: None,
            choices: Some(choices),
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
            key,
            PendingRequest {
                request: request.clone(),
                raw: raw.clone(),
                options: p.options.clone(),
            },
        );
        out.push(self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        }));
        out.push(self.state(State::RequiresAction));
        out
    }

    /// The ACP result for the answer to `key`, and the updates it causes.
    pub fn answer(&mut self, key: &str, answer: &Answer) -> Result<(Value, Vec<DriverUpdate>), String> {
        let pending = self
            .requests
            .remove(key)
            .ok_or_else(|| format!("no open request {key}"))?;
        let option = pick_option(&pending, answer);
        let result = match &option {
            Some(option) => wire::selected(&option.option_id),
            None => wire::cancelled_outcome(),
        };
        let mut request = pending.request;
        request.status = RequestStatus::Answered;
        request.answer = Some(answer.clone());
        let mut out = vec![self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        })];
        if self.requests.is_empty() && self.turn.is_some() {
            out.push(self.state(State::Running));
        }
        Ok((result, out))
    }

    /// A prompt request ended: its result, or an error.
    pub fn turn_end(
        &mut self,
        result: Option<&PromptResult>,
        error: Option<String>,
        now: &str,
    ) -> Vec<DriverUpdate> {
        let mut out = self.ensure_turn(now);
        let turn = self.turn.take().expect("a turn was ensured");
        let stop_reason = result.and_then(|r| r.stop_reason.clone());
        let (status, stop, continuable) = match (&error, stop_reason.as_deref()) {
            _ if turn.cancelled => (TurnStatus::Interrupted, TurnStop::Cancelled, true),
            (Some(_), _) => (TurnStatus::Failed, TurnStop::Error, false),
            (None, Some("cancelled")) => (TurnStatus::Interrupted, TurnStop::Cancelled, true),
            (None, Some("refusal")) => (TurnStatus::Completed, TurnStop::Refusal, false),
            (None, Some("max_tokens")) => (TurnStatus::Completed, TurnStop::MaxTokens, true),
            (None, Some("max_turn_requests")) => (TurnStatus::Completed, TurnStop::TurnLimit, true),
            _ => (TurnStatus::Completed, TurnStop::End, false),
        };
        let usage = self.usage(result.and_then(|r| r.usage.clone()));
        // Blocks cut off by an interrupt never get their final chunk.
        let open: Vec<String> = self.current.values().cloned().collect();
        for id in open {
            out.push(self.item(parrot_proto::item::ItemPatch {
                id,
                streaming: Some(false),
                ..Default::default()
            }));
        }
        self.current.clear();
        // Any request still open is moot once the turn ends.
        let open: Vec<_> = std::mem::take(&mut self.requests).into_values().collect();
        for pending in open {
            let mut request = pending.request;
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
                continuable,
                input: turn.input,
                started: turn.started,
                ended: Some(now.to_owned()),
                error: error.map(|message| TurnError {
                    code: "harness_error".into(),
                    message,
                    retryable: true,
                }),
                usage: Some(usage.clone()),
                meta: None,
                extra: Map::new(),
            },
        }));
        out.push(self.update(DriverUpdateKind::Usage { usage }));
        out.push(self.state(State::Idle));
        out
    }

    fn usage(&self, tokens: Option<wire::PromptUsage>) -> Usage {
        let tokens = tokens.map(|u| TokenUsage {
            input: u.input_tokens.unwrap_or(0),
            output: u.output_tokens.unwrap_or(0),
            cache_read: u.cached_read_tokens,
            cache_write: u.cached_write_tokens,
            reasoning: None,
        });
        Usage {
            tokens,
            cost: self.cost.map(|amount| Cost {
                amount,
                currency: "USD".into(),
            }),
            context: self.context.map(|(used, window)| ContextUsage { used, window }),
            ..Default::default()
        }
    }

    /// Every open permission request, answered `cancelled` before `session/cancel` goes out.
    pub fn cancel_requests(&mut self) -> (Vec<Value>, Vec<DriverUpdate>) {
        let open: Vec<_> = std::mem::take(&mut self.requests).into_values().collect();
        let mut answers = Vec::new();
        let mut updates = Vec::new();
        for pending in open {
            answers.push(json!({ "id": pending.raw, "result": wire::cancelled_outcome() }));
            let mut request = pending.request;
            request.status = RequestStatus::Cancelled;
            updates.push(self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            }));
        }
        (answers, updates)
    }

    /// The session id a permission request named before the session result arrived.
    pub fn session_hint(&self) -> Option<String> {
        self.session_hint.clone()
    }

    /// The config option id that carries the model, when the agent reports one.
    pub fn model_config_id(&self) -> Option<String> {
        self.model_config.clone()
    }

    /// The config option id that carries the reasoning level, when the agent reports one.
    pub fn effort_config_id(&self) -> Option<String> {
        self.effort_config.clone()
    }
}

/// A JSON-RPC id as a driver key: a string as it is, anything else as its JSON text.
fn raw_key(raw: &Value) -> String {
    match raw {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn map_of(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

fn select_option(
    id: &str,
    role: Option<OptionRole>,
    label: &str,
    choices: &[ChoiceItem],
    value: Option<String>,
) -> ConfigOption {
    let schema = if choices.is_empty() {
        json!({ "type": "string" })
    } else {
        json!({
            "type": "string",
            "oneOf": choices.iter().map(|c| {
                let mut o = json!({ "const": c.id, "title": c.label });
                if let Some(d) = &c.description {
                    o["description"] = json!(d);
                }
                o
            }).collect::<Vec<_>>(),
        })
    };
    ConfigOption {
        id: id.into(),
        role: role.map(Open::Known),
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

fn pick_option(pending: &PendingRequest, answer: &Answer) -> Option<wire::PermissionOption> {
    if let Some(id) = answer.choice.as_deref() {
        if let Some(option) = pending.options.iter().find(|o| o.option_id == id) {
            return Some(option.clone());
        }
        let want_allow = !id.contains("deny") && !id.contains("reject");
        let allow = want_allow
            || answer
                .value
                .as_ref()
                .and_then(|v| v.get("intent"))
                .and_then(Value::as_str)
                == Some("allow");
        let kinds: &[&str] = if allow {
            &["allow_once", "allow_always", "allow"]
        } else {
            &["reject_once", "reject_always", "reject", "deny"]
        };
        return kinds
            .iter()
            .find_map(|kind| pending.options.iter().find(|o| o.kind == *kind).cloned());
    }
    // A free answer picks the first option of the matching intent.
    let allow = answer.input.is_some() || answer.message.is_none();
    let kinds: &[&str] = if allow {
        &["allow_once", "allow_always", "allow"]
    } else {
        &["reject_once", "reject_always", "reject", "deny"]
    };
    kinds
        .iter()
        .find_map(|kind| pending.options.iter().find(|o| o.kind == *kind).cloned())
}

fn tool_status(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") => "ok",
        Some("failed") => "error",
        Some("cancelled") => "cancelled",
        Some("in_progress") | Some("running") => "running",
        _ => "pending",
    }
}

fn plan_status(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") | Some("done") => "done",
        Some("in_progress") | Some("active") => "active",
        _ => "pending",
    }
}

/// The text of a tool call's content blocks, capped.
fn output_of(call: &ToolCall) -> Option<String> {
    let mut parts = Vec::new();
    for block in call.content.iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("content") => {
                let inner = block.get("content").unwrap_or(block);
                if let Some(text) = wire::block_text(inner) {
                    parts.push(text);
                }
            }
            Some("diff") => {
                let path = block.get("path").and_then(Value::as_str).unwrap_or("(file)");
                let text = block.get("newText").and_then(Value::as_str).unwrap_or("");
                parts.push(format!("{path}\n{text}"));
            }
            _ => {
                if let Some(text) = wire::block_text(block) {
                    parts.push(text);
                }
            }
        }
    }
    if parts.is_empty() {
        if let Some(Value::String(text)) = &call.raw_output {
            parts.push(text.clone());
        } else if let Some(text) = call
            .raw_output
            .as_ref()
            .and_then(|o| o.get("output"))
            .and_then(Value::as_str)
        {
            parts.push(text.to_owned());
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut text = parts.join("\n");
    if text.len() > OUTPUT_MAX {
        text.truncate(OUTPUT_MAX);
        text.push_str("\n…");
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::InitializeResult;

    fn state() -> MapState {
        let mut state = MapState::new("b1", Data::default());
        state.session_id = Some("s1".into());
        state
    }

    fn prompt(text: &str) -> BindingPromptParams {
        BindingPromptParams {
            binding: "b1".into(),
            subagent: None,
            message: "m1".into(),
            text: text.into(),
            context: None,
            attachments: None,
            steer: false,
        }
    }

    fn update(value: Value) -> SessionUpdate {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_turn_streams_text_and_ends_idle() {
        let mut state = state();
        let (updates, disposition, blocks) = state.prompt(&prompt("go"), "2026-01-01T00:00:00Z");
        assert_eq!(disposition, BindingDisposition::Started);
        assert_eq!(blocks[0]["text"], "go");
        assert!(
            updates
                .iter()
                .any(|u| matches!(u.update, Open::Known(DriverUpdateKind::Turn { .. })))
        );
        let chunk = update(json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg_1",
            "content": { "type": "text", "text": "Hel" },
        }));
        let first = state.step(&chunk, "2026-01-01T00:00:01Z");
        assert_eq!(first[0].binding, "b1");
        state.step(&chunk, "2026-01-01T00:00:01Z");
        let end = state.turn_end(
            Some(
                &serde_json::from_value(json!({
                    "stopReason": "end_turn",
                    "usage": { "inputTokens": 2, "outputTokens": 3 },
                }))
                .unwrap(),
            ),
            None,
            "2026-01-01T00:00:02Z",
        );
        let turn = end
            .iter()
            .find_map(|u| match &u.update {
                Open::Known(DriverUpdateKind::Turn { turn }) if turn.status != TurnStatus::Running => {
                    Some(turn.clone())
                }
                _ => None,
            })
            .expect("the turn ended");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert_eq!(turn.stop, Some(Open::Known(TurnStop::End)));
        assert_eq!(turn.usage.as_ref().unwrap().tokens.as_ref().unwrap().output, 3);
    }

    #[test]
    fn a_permission_request_becomes_choices_and_back() {
        let mut state = state();
        state.prompt(&prompt("go"), "t0");
        let raw = json!(7);
        let request: PermissionRequest = serde_json::from_value(json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "t1", "title": "`ls`", "kind": "execute", "rawInput": { "command": "ls" } },
            "options": [
                { "optionId": "allow_always", "name": "Always allow", "kind": "allow_always" },
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
            ],
        }))
        .unwrap();
        let updates = state.permission(&raw, &request, "t1");
        let request = updates
            .iter()
            .find_map(|u| match &u.update {
                Open::Known(DriverUpdateKind::Request { request }) => Some(request.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(request.id, "acp-perm-7");
        assert_eq!(request.kind, Open::Known(RequestKind::Approval));
        assert_eq!(request.level, PermissionLevel::Host);
        assert_eq!(request.choices.as_ref().unwrap().len(), 3);
        let (result, updates) = state
            .answer(
                "acp-perm-7",
                &Answer {
                    choice: Some("allow".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(result["outcome"]["optionId"], "allow");
        assert!(updates.iter().any(|u| matches!(
            u.update,
            Open::Known(DriverUpdateKind::State {
                state: State::Running,
                ..
            })
        )));
    }

    #[test]
    fn attachments_respect_capabilities_and_never_go_silent() {
        let mut state = state();
        let mut p = prompt("look");
        p.attachments = Some(vec![Attachment {
            name: "shot.png".into(),
            media_type: "image/png".into(),
            data: Some("aGVsbG8=".into()),
            uri: None,
        }]);
        let (updates, _, blocks) = state.prompt(&p, "t0");
        assert_eq!(blocks[0]["type"], "text");
        assert!(
            updates.iter().any(|u| matches!(
                &u.update,
                Open::Known(DriverUpdateKind::Notice { code: Some(code), .. }) if code == "x.parrot/attachment"
            )),
            "no notice for an attachment the agent cannot take"
        );
        state.capabilities = Capabilities {
            image: true,
            embedded_context: true,
            ..Default::default()
        };
        let (_, _, blocks) = state.prompt(&p, "t1");
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["data"], "aGVsbG8=");
    }

    #[test]
    fn capabilities_come_from_initialize() {
        let init: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": { "image": true, "embeddedContext": true },
                "mcpCapabilities": { "http": true },
                "sessionCapabilities": { "resume": {} },
            },
        }))
        .unwrap();
        let caps = Capabilities::from_initialize(&init);
        assert!(caps.image && caps.embedded_context && caps.mcp_http && caps.resume && caps.load);
    }

    #[test]
    fn a_stop_reason_maps_to_a_turn_stop() {
        let mut state = state();
        state.prompt(&prompt("go"), "t0");
        state.cancel();
        let end = state.turn_end(None, None, "t1");
        let turn = end.iter().find_map(|u| match &u.update {
            Open::Known(DriverUpdateKind::Turn { turn }) if turn.status != TurnStatus::Running => {
                Some(turn.clone())
            }
            _ => None,
        });
        let turn = turn.unwrap();
        assert_eq!(turn.status, TurnStatus::Interrupted);
        assert_eq!(turn.stop, Some(Open::Known(TurnStop::Cancelled)));
        assert!(turn.continuable);
    }
}
