//! Frames (spec 30.2, 35): what the driver read from and wrote to the app-server, stored so the
//! items can be rebuilt when the mapping changes.
//!
//! Each frame's `data` is `{ v, at, record }`. `{ out }` is a line from Codex; `{ models }`,
//! `{ ready }`, `{ accepted }`, `{ answer }`, `{ cancel }` and `{ options }` are what the driver
//! did. [`rebuild`] replays them through a fresh [`MapState`].

use parrot_proto::driver::{BindingDisposition, BindingPromptParams, DriverUpdate, DriverUpdateKind};
use parrot_proto::driver_data::frame_data;
use parrot_proto::open::Open;
use parrot_proto::request::Answer;
use serde_json::{Map, Value, json};

use crate::data::data;
use crate::mapping::MapState;
use crate::wire::{self, Message, ServerNotification};

/// The source name of frames from the app-server.
pub const SOURCE: &str = "native";

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

/// The record of a prompt Codex accepted, without attachment bytes.
pub fn accepted_record(p: &BindingPromptParams, turn: Option<&str>, d: BindingDisposition) -> Value {
    let mut p = p.clone();
    for a in p.attachments.iter_mut().flatten() {
        a.data = None;
    }
    json!({"accepted": {"prompt": p, "turn": turn, "disposition": d}})
}

/// The items, turns and requests the frames give, as a running binding sent them.
pub fn rebuild(binding: &str, frames: &[Value]) -> Vec<DriverUpdate> {
    let mut state = MapState::new(binding);
    let mut out = Vec::new();
    for f in frames {
        let at = f["at"].as_str().unwrap_or_default();
        let r = &f["record"];
        if let Some(line) = r.get("out") {
            match Message::parse(&line.to_string()) {
                Ok(Message::Notification { method, params }) => {
                    out.extend(state.step(&ServerNotification::parse(&method, &params), at));
                }
                Ok(Message::Request { id, method, params }) => {
                    out.extend(state.server_request(&id, &method, &params).0);
                }
                _ => {}
            }
        } else if let Some(m) = r.get("models") {
            if let Ok(list) = serde_json::from_value::<wire::ModelList>(m.clone()) {
                state.models_listed(&list);
            }
        } else if let Some(ready) = r.get("ready") {
            out.extend(state.thread_ready(ready));
        } else if let Some(a) = r.get("accepted") {
            let p = serde_json::from_value::<BindingPromptParams>(a["prompt"].clone());
            let d = serde_json::from_value::<BindingDisposition>(a["disposition"].clone());
            if let (Ok(p), Ok(d)) = (p, d) {
                out.extend(state.prompt_accepted(&p, a["turn"].as_str(), d, at));
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
            let s = |k: &str| o.get(k).map(|v| v.as_str().map(str::to_owned));
            if let Some(v) = s("model") {
                state.model = v;
            }
            if let Some(v) = s("effort") {
                state.effort = v;
            }
            if let Some(Some(v)) = s("approvalPolicy") {
                state.approval = v;
            }
            if let Some(Some(v)) = s("sandbox") {
                state.sandbox = v;
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
