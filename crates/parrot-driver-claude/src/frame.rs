//! Frames (spec 30.2, 35): what the driver read from and wrote to Claude Code, stored so the items
//! can be rebuilt when the mapping changes.
//!
//! Each frame's `data` is `{ v, at, record }` with the data version, the time and one record:
//! `{ out }` for a line from the CLI, and `{ init }`, `{ prompt }`, `{ answer }`, `{ cancel }` or
//! `{ options }` for what the driver did. [`rebuild`] replays them through a fresh [`MapState`],
//! which gives the same items with the same keys, since the mapping depends only on its input.

use parrot_proto::driver::{BindingPromptParams, DriverUpdate, DriverUpdateKind};
use parrot_proto::driver_data::frame_data;
use parrot_proto::open::Open;
use parrot_proto::request::Answer;
use serde_json::{Map, Value, json};

use crate::data::data;
use crate::mapping::MapState;
use crate::wire;

/// The source name of frames from stream-json.
pub const SOURCE: &str = "native";

/// A frame update for `record`.
pub fn frame(binding: &str, at: &str, record: Value) -> DriverUpdate {
    DriverUpdate {
        binding: binding.to_owned(),
        subagent: None,
        update: Open::Known(DriverUpdateKind::Frame {
            source: SOURCE.into(),
            data: frame_data(&data().version, at, record),
        }),
        extra: Map::new(),
    }
}

/// The record of a prompt, without attachment bytes.
pub fn prompt_record(p: &BindingPromptParams) -> Value {
    let mut p = p.clone();
    for a in p.attachments.iter_mut().flatten() {
        a.data = None;
    }
    json!({"prompt": p})
}

pub fn answer_record(request: &str, answer: &Answer) -> Value {
    json!({"answer": {"request": request, "answer": answer}})
}

/// The items, turns and requests the frames give, as a running binding sent them.
pub fn rebuild(binding: &str, frames: &[Value]) -> Vec<DriverUpdate> {
    let mut state = MapState::new(binding);
    let mut out = Vec::new();
    for f in frames {
        let at = f["at"].as_str().unwrap_or_default();
        let r = &f["record"];
        if let Some(line) = r.get("out") {
            if let Ok(msg) = serde_json::from_value::<wire::ClaudeOut>(line.clone()) {
                out.extend(state.step(&msg, at));
            }
        } else if let Some(init) = r.get("init") {
            let mode = state.permission_mode.clone();
            state.initialized(init);
            if mode.is_some() {
                state.permission_mode = mode;
            }
        } else if let Some(p) = r.get("prompt") {
            if let Ok(p) = serde_json::from_value::<BindingPromptParams>(p.clone()) {
                out.extend(state.prompt(&p, at).0);
            }
        } else if let Some(a) = r.get("answer") {
            let answer = serde_json::from_value::<Answer>(a["answer"].clone());
            if let (Some(req), Ok(answer)) = (a["request"].as_str(), answer)
                && let Ok((_, updates)) = state.answer(req, &answer)
            {
                out.extend(updates);
            }
        } else if r.get("cancel").is_some() {
            state.cancel();
        } else if let Some(o) = r.get("options") {
            if let Some(m) = o.get("model") {
                state.model = m.as_str().map(str::to_owned);
            }
            if let Some(m) = o.get("permissionMode") {
                state.permission_mode = m.as_str().map(str::to_owned);
            }
        }
    }
    out.retain(|u| {
        matches!(
            u.update,
            Open::Known(
                DriverUpdateKind::Item { .. }
                    | DriverUpdateKind::Turn { .. }
                    | DriverUpdateKind::Request { .. }
            )
        )
    });
    out
}
