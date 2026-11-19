//! pi's RPC events as driver updates (spec 30.2, Appendix C).
//!
//! [`MapState`] holds what the mapping remembers between lines. Driver keys are stable: pi's
//! `toolCallId` for tools, `<message>:<content index>` for text and reasoning, the runtime's message
//! id for prompts, and `pi-ui-<id>` for an extension dialog.

use std::collections::{BTreeMap, HashMap};

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
use parrot_proto::usage::{ContextUsage, Cost, TokenUsage, Usage};
use serde_json::{Map, Value, json};

use crate::data::DriverData;
use crate::wire::{
    self, AssistantEvent, Event, Inbound, Message, Model, RawUsage, SessionState, SlashCommand,
};

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

/// An extension dialog waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingDialog {
    pub request: Request,
    /// pi's own id for the dialog, which its answer must carry.
    pub raw: String,
    pub method: String,
    pub labels: Vec<String>,
}

/// What the mapping remembers between lines.
#[derive(Debug, Clone, PartialEq)]
pub struct MapState {
    pub binding: String,
    pub turn: Option<TurnState>,
    pub turns: u64,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub models: Vec<Model>,
    pub thinking: Vec<String>,
    /// The thinking level pi reports now, or `None` for `off`.
    pub current_thinking: Option<String>,
    pub actions: Vec<Action>,
    pub requests: BTreeMap<String, PendingDialog>,
    /// Each turn's input text, so a fork can name the message it starts from.
    pub turn_inputs: Vec<(String, String)>,
    pub data: DriverData,
    /// The assistant message streaming now, counting from 1.
    message: u64,
    /// Content index → item key, for the message streaming now.
    blocks: HashMap<u64, String>,
    /// Token and cost totals over the whole session.
    usage: Usage,
    /// Token and cost totals of the running turn, which its turn update reports.
    turn_usage: Usage,
    /// Whether pi is running a turn.
    running: bool,
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
            thinking: Vec::new(),
            current_thinking: None,
            actions: Vec::new(),
            requests: BTreeMap::new(),
            turn_inputs: Vec::new(),
            data,
            message: 0,
            blocks: HashMap::new(),
            usage: Usage::default(),
            turn_usage: Usage::default(),
            running: false,
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
        self.notice(level, text, code)
    }

    fn notice(&self, level: NoticeLevel, text: String, code: &str) -> DriverUpdate {
        self.update(DriverUpdateKind::Notice {
            level,
            text,
            code: Some(code.into()),
        })
    }

    /// pi runs tools without asking; its data lists `host` only (spec 15.2).
    pub fn level(&self) -> PermissionLevel {
        PermissionLevel::Host
    }

    pub fn network(&self) -> bool {
        true
    }

    /// The levels this agent can be held to, from its data.
    pub fn levels(&self) -> Vec<PermissionLevel> {
        if self.data.levels.is_empty() {
            vec![PermissionLevel::Host]
        } else {
            self.data.levels.clone()
        }
    }

    /// The `model` and `effort` options with their current values.
    pub fn options(&self) -> Vec<ConfigOption> {
        let model_schema = if self.models.is_empty() {
            json!({"type": "string"})
        } else {
            json!({
                "type": "string",
                "enum": self.models.iter().map(Model::key).collect::<Vec<_>>(),
            })
        };
        let effort_schema = if self.thinking.is_empty() {
            json!({"type": "string"})
        } else {
            json!({"type": "string", "enum": self.thinking})
        };
        vec![
            option(
                "model",
                OptionRole::Model,
                &self.data.label("model", "Model"),
                model_schema,
                self.model.clone(),
            ),
            option(
                "effort",
                OptionRole::Effort,
                &self.data.label("effort", "Thinking"),
                effort_schema,
                self.thinking_value(),
            ),
        ]
    }

    /// The thinking levels the current model offers, as pi names them; `off` is not a level here.
    pub fn set_levels(&mut self, levels: &[String]) {
        self.thinking = levels.iter().filter(|l| *l != "off").cloned().collect();
    }

    fn thinking_value(&self) -> Option<String> {
        self.current_thinking
            .as_ref()
            .filter(|l| self.thinking.contains(l))
            .cloned()
    }

    /// The `options` update for the current values.
    pub fn options_update(&self) -> DriverUpdate {
        self.update(DriverUpdateKind::Options {
            options: self.options(),
            level: self.level(),
            network: self.network(),
        })
    }

    /// What pi reports at open: its session id, model, models, thinking levels and commands.
    pub fn initialized(
        &mut self,
        state: &SessionState,
        models: &[Model],
        levels: &[String],
        commands: &[SlashCommand],
    ) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        self.models = models.to_vec();
        if let Some(key) = state.model_key() {
            self.model = Some(key.clone());
        }
        self.set_levels(levels);
        self.current_thinking = state.thinking_level.clone().filter(|l| l != "off");
        if let Some(id) = &state.session_id
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
        let actions: Vec<Action> = commands
            .iter()
            .filter(|c| c.source.as_deref() != Some("skill"))
            .filter(|c| c.source.as_deref() != Some("extension"))
            .filter(|c| !c.name.is_empty())
            .map(command_action)
            .chain(std::iter::once(compact_action()))
            .collect();
        if actions != self.actions {
            self.actions = actions.clone();
            out.push(self.update(DriverUpdateKind::Actions { actions }));
        }
        out.push(self.options_update());
        out
    }

    /// A prompt written to pi: the user item, and a new turn unless one runs.
    pub fn prompt(
        &mut self,
        p: &BindingPromptParams,
        disposition: BindingDisposition,
        now: &str,
    ) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if self.turn.is_none() && disposition != BindingDisposition::Handled {
            self.running = true;
            out.extend(self.begin_turn(Some(p.message.clone()), now));
            if let Some(turn) = &self.turn {
                self.turn_inputs.push((turn.key.clone(), p.text.clone()));
            }
        }
        let mut set = Map::new();
        set.insert("text".into(), json!(p.text));
        set.insert(
            "status".into(),
            json!(match disposition {
                BindingDisposition::Started => "sent",
                BindingDisposition::Steered => "sent",
                BindingDisposition::Handled => "handled",
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
        self.running = true;
        if self.turn.is_some() {
            vec![]
        } else {
            self.begin_turn(None, now)
        }
    }

    fn turn_key(&self) -> Option<String> {
        self.turn.as_ref().map(|t| t.key.clone())
    }

    /// The answer to the dialog with driver key `key`, as the line to write to pi.
    pub fn answer(&mut self, key: &str, answer: &Answer) -> Result<(Value, Vec<DriverUpdate>), String> {
        let pending = self
            .requests
            .remove(key)
            .ok_or_else(|| format!("no open request {key}"))?;
        let value = ui_response(&pending, answer);
        let mut request = pending.request;
        request.status = RequestStatus::Answered;
        request.answer = Some(answer.clone());
        let mut out = vec![self.update(DriverUpdateKind::Request {
            request: Box::new(request),
        })];
        if self.requests.is_empty() && self.turn.is_some() {
            out.push(self.state(State::Running));
        }
        Ok((value, out))
    }

    /// One event from pi. Response lines are the driver's business and never reach here.
    pub fn step(&mut self, line: &Value, now: &str) -> Vec<DriverUpdate> {
        let mut out = vec![self.update(DriverUpdateKind::Frame {
            source: "pi".into(),
            data: line.clone(),
        })];
        let Some(Open::Known(Inbound::Event(event))) = wire::parse_inbound(&line.to_string()) else {
            return out;
        };
        match event {
            Event::AgentStart { .. } => out.extend(self.ensure_turn(now)),
            Event::TurnStart { .. } => {}
            Event::MessageStart { message, .. } => {
                if message.role == "assistant" {
                    self.message += 1;
                    self.blocks.clear();
                }
            }
            Event::MessageUpdate {
                assistant_message_event,
                ..
            } => {
                if let Some(e) = assistant_message_event {
                    out.extend(self.ensure_turn(now));
                    out.extend(self.delta(&e));
                }
            }
            Event::MessageEnd { message, .. } => out.extend(self.message_end(&message)),
            Event::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                ..
            } => out.push(self.tool_start(&tool_call_id, &tool_name, &args)),
            Event::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                let text = result_text(&partial_result);
                if !text.is_empty() {
                    out.push(self.item(ItemPatch {
                        id: tool_call_id,
                        set: Some(map_of("output", json!(clip(&text)))),
                        ..Default::default()
                    }));
                }
            }
            Event::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                extra,
                ..
            } => {
                let mut set = map_of("status", json!(if is_error { "error" } else { "ok" }));
                let text = result_text(&result);
                if !text.is_empty() {
                    set.insert("output".into(), json!(clip(&text)));
                }
                if let Some(ms) = extra.get("durationMs").and_then(Value::as_u64) {
                    set.insert("durationMs".into(), json!(ms));
                }
                out.push(self.item(ItemPatch {
                    id: tool_call_id.clone(),
                    set: Some(set),
                    streaming: Some(false),
                    ..Default::default()
                }));
                // A tool may have called other tools; they hang under it (Appendix C).
                out.extend(self.nested(&tool_call_id, &result));
            }
            Event::TurnEnd { .. } => {}
            Event::AgentEnd { .. } => {}
            Event::AgentSettled { aborted, .. } => out.extend(self.settled(aborted, now)),
            Event::QueueUpdate { .. }
            | Event::SessionInfoChanged { .. }
            | Event::EntryAppended { .. }
            | Event::BashExecutionUpdate { .. } => {}
            Event::ThinkingLevelChanged { level, .. } => {
                let level = (level != "off").then_some(level);
                if self.current_thinking != level {
                    self.current_thinking = level;
                    out.push(self.options_update());
                }
            }
            Event::ExtensionError {
                extension_path,
                event,
                error,
                ..
            } => out.push(self.notice(
                NoticeLevel::Warning,
                format!(
                    "An extension of pi failed in {event} ({}): {error}",
                    basename(&extension_path)
                ),
                "extension_error",
            )),
            Event::AutoRetryStart {
                attempt,
                max_attempts,
                error_message,
                ..
            } => out.push(self.notice(
                NoticeLevel::Info,
                format!(
                    "pi is retrying after an error (attempt {attempt} of {max_attempts}): {error_message}"
                ),
                "retry",
            )),
            Event::SummarizationRetryScheduled { .. }
            | Event::SummarizationRetryAttemptStart { .. }
            | Event::SummarizationRetryFinished { .. } => {}
            Event::ExtensionUiRequest {
                id,
                method,
                title,
                message,
                options,
                ..
            } => out.extend(self.dialog(&id, &method, title, message, options)),
            Event::AutoRetryEnd {
                success, final_error, ..
            } => {
                if !success {
                    let error = final_error.unwrap_or_else(|| "pi gave up retrying".into());
                    self.last_error = Some(error.clone());
                    out.push(self.notice(
                        NoticeLevel::Warning,
                        format!("pi retried and failed: {error}"),
                        "retry_failed",
                    ));
                }
            }
            Event::CompactionStart { .. } => {}
            Event::CompactionEnd {
                reason,
                result,
                error_message,
                ..
            } => {
                // The end event is the one place both a manual and an automatic compaction report
                // their result, so the item is made here rather than from the `compact` answer.
                match (result, error_message) {
                    (Some(result), _) => {
                        let trigger = if reason.as_deref() == Some("manual") {
                            "manual"
                        } else {
                            "auto"
                        };
                        out.extend(self.compacted(&result, trigger));
                    }
                    (None, Some(message)) => out.push(self.notice(
                        NoticeLevel::Warning,
                        format!("pi could not compact the context: {message}"),
                        "compaction_failed",
                    )),
                    (None, None) => {}
                }
            }
        }
        out
    }

    fn delta(&mut self, event: &AssistantEvent) -> Vec<DriverUpdate> {
        let index = event.index();
        let key = format!("pi-{}-{index}", self.message);
        let kind = if event.is_text() {
            ItemKind::Assistant
        } else {
            ItemKind::Reasoning
        };
        match event {
            AssistantEvent::TextStart { .. } | AssistantEvent::ThinkingStart { .. } => {
                self.blocks.insert(index, key.clone());
                vec![self.item(ItemPatch {
                    id: key,
                    kind: Some(Open::Known(kind)),
                    turn: self.turn_key(),
                    source: Some(ItemSource::Native),
                    set: Some(map_of("text", json!(""))),
                    streaming: Some(true),
                    ..Default::default()
                })]
            }
            AssistantEvent::TextDelta { delta, .. } | AssistantEvent::ThinkingDelta { delta, .. } => {
                let known = self.blocks.contains_key(&index);
                self.blocks.entry(index).or_insert(key.clone());
                let mut append = BTreeMap::new();
                append.insert("text".to_owned(), delta.clone());
                let mut patch = ItemPatch {
                    id: key,
                    append: Some(append),
                    ..Default::default()
                };
                if !known {
                    patch.kind = Some(Open::Known(kind));
                    patch.turn = self.turn_key();
                    patch.source = Some(ItemSource::Native);
                    patch.streaming = Some(true);
                }
                vec![self.item(patch)]
            }
            AssistantEvent::TextEnd { content, .. } | AssistantEvent::ThinkingEnd { content, .. } => {
                self.blocks.entry(index).or_insert(key.clone());
                let text = wire::content_text(content);
                vec![self.item(ItemPatch {
                    id: key,
                    set: Some(map_of("text", json!(text))),
                    streaming: Some(false),
                    ..Default::default()
                })]
            }
            AssistantEvent::Other => vec![],
        }
    }

    fn message_end(&mut self, message: &Message) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        if message.role == "toolResult" {
            let id = match &message.tool_call_id {
                Some(id) => id.clone(),
                None => self.fresh("tool"),
            };
            let mut set = map_of("status", json!(if message.is_error { "error" } else { "ok" }));
            let text = result_text(&message.content);
            if !text.is_empty() {
                set.insert("output".into(), json!(text));
            }
            out.push(self.item(ItemPatch {
                id: id.clone(),
                set: Some(set),
                streaming: Some(false),
                ..Default::default()
            }));
            if let Some(nested) = &message.nested_calls {
                out.extend(self.nested_calls(&id, nested));
            }
            return out;
        }
        if message.role != "assistant" {
            return out;
        }
        if let Some(u) = &message.usage {
            // The runtime adds every usage update, so each report is the message's own tokens.
            let delta = self.add_usage(u);
            out.push(self.update(DriverUpdateKind::Usage { usage: delta }));
        }
        if message.stop_reason.as_deref() == Some("error")
            && let Some(error) = &message.error_message
        {
            self.last_error = Some(error.clone());
        }
        // The final message is authoritative for the text that streamed before it.
        let blocks = match &message.content {
            Value::Array(blocks) => blocks.clone(),
            _ => vec![],
        };
        for (index, block) in blocks.iter().enumerate() {
            let (kind, text) = match block.get("type").and_then(Value::as_str) {
                Some("text") => (
                    ItemKind::Assistant,
                    block.get("text").and_then(Value::as_str).unwrap_or_default(),
                ),
                Some("thinking") => (
                    ItemKind::Reasoning,
                    block.get("thinking").and_then(Value::as_str).unwrap_or_default(),
                ),
                _ => continue,
            };
            let index = index as u64;
            let key = self
                .blocks
                .get(&index)
                .cloned()
                .unwrap_or_else(|| format!("pi-{}-{index}", self.message));
            out.push(self.item(ItemPatch {
                id: key,
                kind: Some(Open::Known(kind)),
                turn: self.turn_key(),
                source: Some(ItemSource::Native),
                set: Some(map_of("text", json!(text))),
                streaming: Some(false),
                ..Default::default()
            }));
        }
        out
    }

    /// Adds one message's tokens and cost to the session and turn totals, and returns the message's
    /// own usage, which is what the runtime is told.
    fn add_usage(&mut self, u: &RawUsage) -> Usage {
        let tokens = TokenUsage {
            input: u.input,
            output: u.output,
            cache_read: Some(u.cache_read),
            cache_write: Some(u.cache_write),
            reasoning: None,
        };
        let mut delta = Usage {
            tokens: Some(tokens),
            ..Default::default()
        };
        if u.cost() > 0.0 {
            delta.cost = Some(Cost {
                amount: u.cost(),
                currency: "USD".into(),
            });
        }
        for into in [&mut self.usage, &mut self.turn_usage] {
            let mut totals = into.tokens.clone().unwrap_or_default();
            totals.input += u.input;
            totals.output += u.output;
            totals.cache_read = Some(totals.cache_read.unwrap_or(0) + u.cache_read);
            totals.cache_write = Some(totals.cache_write.unwrap_or(0) + u.cache_write);
            into.tokens = Some(totals);
            if let Some(cost) = &delta.cost {
                let amount = into.cost.as_ref().map(|c| c.amount).unwrap_or(0.0) + cost.amount;
                into.cost = Some(Cost {
                    amount,
                    currency: "USD".into(),
                });
            }
        }
        if u.total_tokens > 0
            && let Some(window) = self.window()
        {
            delta.context = Some(ContextUsage {
                used: u.total_tokens,
                window,
            });
        }
        delta
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.unnamed += 1;
        format!("{prefix}-{}", self.unnamed)
    }

    /// A compaction item from pi's `compact` answer or its `compaction_end` event.
    pub fn compacted(&mut self, result: &Value, trigger: &str) -> Vec<DriverUpdate> {
        let id = self.fresh("compaction");
        let mut set = map_of("trigger", json!(trigger));
        if let Some(summary) = result.get("summary").filter(|v| !v.is_null()) {
            set.insert("summary".into(), summary.clone());
        }
        if let Some(before) = result.get("tokensBefore").filter(|v| !v.is_null()) {
            set.insert("tokensBefore".into(), before.clone());
        }
        if let Some(after) = result.get("estimatedTokensAfter").filter(|v| !v.is_null()) {
            set.insert("tokensAfter".into(), after.clone());
        }
        vec![self.item(ItemPatch {
            id,
            kind: Some(Open::Known(ItemKind::Compaction)),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            set: Some(set),
            ..Default::default()
        })]
    }

    /// Calls a tool made to other tools, from a tool result's `nestedCalls`. They hang under the
    /// tool that made them and keep an id derived from its own.
    fn nested(&mut self, parent: &str, result: &Value) -> Vec<DriverUpdate> {
        let Some(value) = result.get("nestedCalls").filter(|v| !v.is_null()) else {
            return vec![];
        };
        let Ok(nested) = serde_json::from_value::<wire::NestedCalls>(value.clone()) else {
            return vec![];
        };
        self.nested_calls(parent, &nested)
    }

    fn nested_calls(&mut self, parent: &str, nested: &wire::NestedCalls) -> Vec<DriverUpdate> {
        nested
            .calls
            .iter()
            .map(|call| {
                let key = if call.id.is_empty() {
                    format!("{parent}/{}", call.name)
                } else {
                    format!("{parent}/{}", call.id)
                };
                let args = call.arguments.clone().unwrap_or_else(|| json!({}));
                let mut set = map_of("tool", json!(call.name));
                set.insert("title".into(), json!(describe(&call.name, &args)));
                set.insert("input".into(), args);
                set.insert(
                    "status".into(),
                    json!(match call.status.as_deref() {
                        Some("ok") => "ok",
                        Some("error") => "error",
                        _ => "running",
                    }),
                );
                set.insert("caller".into(), json!("model"));
                if let Some(error) = &call.error {
                    set.insert("output".into(), json!(error));
                }
                if let Some(ms) = call.duration_ms {
                    set.insert("durationMs".into(), json!(ms));
                }
                self.item(ItemPatch {
                    id: key,
                    kind: Some(Open::Known(ItemKind::Tool)),
                    parent: Some(parent.to_owned()),
                    turn: self.turn_key(),
                    source: Some(ItemSource::Native),
                    set: Some(set),
                    streaming: Some(false),
                    ..Default::default()
                })
            })
            .collect()
    }

    /// The order and text of the prompt a turn was started with, for a fork at that turn.
    pub fn turn_input(&self, turn: &str) -> Option<(usize, usize, &str)> {
        let index = self.turn_inputs.iter().position(|(key, _)| key == turn)?;
        let (_, text) = &self.turn_inputs[index];
        Some((index, self.turn_inputs.len(), text.as_str()))
    }

    fn tool_start(&mut self, id: &str, name: &str, args: &Value) -> DriverUpdate {
        let mut set = map_of("tool", json!(name));
        set.insert("title".into(), json!(describe(name, args)));
        set.insert("input".into(), args.clone());
        set.insert("status".into(), json!("running"));
        set.insert("caller".into(), json!("model"));
        // pi runs extension tools themselves; the name says which extension.
        if let Some(server) = name.strip_prefix("mcp__").and_then(|n| n.split("__").next()) {
            set.insert("server".into(), json!(server));
        }
        let _ = self.data.tool_level(name);
        self.item(ItemPatch {
            id: id.to_owned(),
            kind: Some(Open::Known(ItemKind::Tool)),
            turn: self.turn_key(),
            source: Some(ItemSource::Native),
            set: Some(set),
            ..Default::default()
        })
    }

    fn window(&self) -> Option<u64> {
        let key = self.model.as_ref()?;
        self.models
            .iter()
            .find(|m| &m.key() == key)
            .and_then(|m| m.context_window)
    }

    fn settled(&mut self, aborted: bool, now: &str) -> Vec<DriverUpdate> {
        let mut out = Vec::new();
        self.running = false;
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
        let error = if !aborted { self.last_error.take() } else { None };
        let (status, stop, error) = if turn.cancelled || aborted {
            (TurnStatus::Interrupted, TurnStop::Cancelled, None)
        } else if let Some(message) = error {
            (
                TurnStatus::Failed,
                TurnStop::Error,
                Some(TurnError {
                    code: "pi".into(),
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

    /// pi took a prompt without running a turn: the user item is handled and nothing else follows.
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
            self.running = false;
            out.extend(self.settled(false, now));
        }
        out
    }

    /// A prompt pi refused: the turn it started fails and the session is idle again.
    pub fn fail(&mut self, message: String, now: &str) -> Vec<DriverUpdate> {
        self.last_error = Some(message);
        self.running = false;
        self.settled(false, now)
    }

    /// The turn ends because pi exited; an open turn is interrupted.
    pub fn exited(&mut self, now: &str) -> Vec<DriverUpdate> {
        let mut out = self.settled(false, now);
        out.extend(std::mem::take(&mut self.requests).into_values().map(|pending| {
            let mut request = pending.request;
            request.status = RequestStatus::Cancelled;
            self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            })
        }));
        out
    }

    fn dialog(
        &mut self,
        raw_id: &str,
        method: &str,
        title: Option<String>,
        message: Option<String>,
        options: Option<Vec<String>>,
    ) -> Vec<DriverUpdate> {
        if !matches!(method, "confirm" | "select" | "input" | "editor") {
            return vec![];
        }
        let key = format!("pi-ui-{raw_id}");
        let labels = options.clone().unwrap_or_default();
        let title = title
            .clone()
            .or_else(|| message.clone())
            .unwrap_or_else(|| "An extension asks".into());
        let (kind, choices, input) = match method {
            "confirm" => (
                RequestKind::Approval,
                Some(vec![
                    choice("allow", "Allow", ChoiceIntent::Allow),
                    choice("deny", "Deny", ChoiceIntent::Deny),
                ]),
                None,
            ),
            "select" => {
                let mut choices: Vec<Choice> = labels
                    .iter()
                    .enumerate()
                    .map(|(i, label)| choice(&format!("option-{i}"), label, ChoiceIntent::Neutral))
                    .collect();
                choices.push(choice("cancel", "Cancel", ChoiceIntent::Deny));
                (RequestKind::Question, Some(choices), None)
            }
            _ => (
                RequestKind::Input,
                Some(vec![choice("cancel", "Cancel", ChoiceIntent::Deny)]),
                Some(json!({
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"],
                })),
            ),
        };
        let request = Request {
            id: key.clone(),
            session: String::new(),
            path: vec![],
            reference: None,
            turn: self.turn_key(),
            item: None,
            kind: Open::Known(kind),
            title,
            description: message,
            subject: Some(Open::Known(Subject::Tool {
                tool: "Pi extension".into(),
                input: json!({"method": method}),
                editable: None,
            })),
            level: self.data.tool_level("Pi extension"),
            network: None,
            choices,
            questions: None,
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
        };
        self.requests.insert(
            key,
            PendingDialog {
                request: request.clone(),
                raw: raw_id.to_owned(),
                method: method.to_owned(),
                labels,
            },
        );
        vec![
            self.update(DriverUpdateKind::Request {
                request: Box::new(request),
            }),
            self.state(State::RequiresAction),
        ]
    }
}

/// The line that answers a pi extension dialog.
fn ui_response(pending: &PendingDialog, answer: &Answer) -> Value {
    let mut value = json!({"type": "extension_ui_response", "id": pending.raw});
    let allowed = matches!(answer.choice.as_deref(), Some("allow"))
        || matches!(answer.choice.as_deref(), Some("allowSession"));
    if pending.method == "confirm" {
        value["confirmed"] = json!(allowed);
        return value;
    }
    let picked = match answer.choice.as_deref() {
        Some(id) if id.starts_with("option-") => id["option-".len()..]
            .parse::<usize>()
            .ok()
            .and_then(|i| pending.labels.get(i).cloned()),
        _ => None,
    };
    let text = picked
        .or_else(|| answer.value.as_ref().and_then(Value::as_str).map(str::to_owned))
        .or_else(|| {
            answer
                .input
                .as_ref()
                .and_then(|i| i.get("value"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    match text {
        Some(text) if answer.choice.as_deref() != Some("cancel") => value["value"] = json!(text),
        _ => value["cancelled"] = json!(true),
    }
    value
}

/// The pure form of [`MapState::step`]: the updates one event causes and the state after it.
pub fn map(state: &MapState, line: &Value, now: &str) -> (Vec<DriverUpdate>, MapState) {
    let mut next = state.clone();
    let updates = next.step(line, now);
    (updates, next)
}

/// The tool output as text: pi sends a string or content blocks.
pub fn result_text(result: &Value) -> String {
    match result {
        Value::String(s) => s.clone(),
        other => {
            let content = other.get("content").unwrap_or(other);
            wire::content_text(content)
        }
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

/// The last segment of a path, for naming an extension in a notice.
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn describe(tool: &str, args: &Value) -> String {
    let detail = ["command", "path", "file_path", "pattern", "query", "url"]
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

/// The compaction pi runs on demand, which it does not list with its commands.
fn compact_action() -> Action {
    Action {
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
    }
}

fn command_action(command: &SlashCommand) -> Action {
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

fn choice(id: &str, label: &str, intent: ChoiceIntent) -> Choice {
    Choice {
        id: id.into(),
        label: label.into(),
        intent: Some(intent),
        scope: Some(ChoiceScope::Once),
        amends: None,
        description: None,
    }
}
