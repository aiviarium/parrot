//! Frames (spec 30.2, 35) under TUI control: each transcript line with the turn it arrived in.
//! The screen is the control source but carries no content, so items come from these frames
//! alone, and [`rebuild`] gives them again from a fresh transcript reader.

use parrot_proto::driver::{DriverUpdate, DriverUpdateKind};
use parrot_proto::driver_data::frame_data;
use parrot_proto::open::Open;
use serde_json::{Map, Value, json};

use crate::transcript::ClaudeTranscript;

/// The source name of transcript frames.
pub const SOURCE: &str = "transcript";
/// The data version of the TUI driver's frames.
pub const VERSION: &str = "1";

pub fn transcript_frame(line: Value, turn: Option<&str>) -> DriverUpdateKind {
    DriverUpdateKind::Frame {
        source: SOURCE.into(),
        data: frame_data(VERSION, &crate::now(), json!({"line": line, "turn": turn})),
    }
}

/// The item updates the transcript frames give. Echoes of prompts the runtime sent are dropped
/// live; pass their texts as `echoes` to drop them here too.
pub fn rebuild(binding: &str, frames: &[Value], echoes: &[String]) -> Vec<DriverUpdate> {
    let mut t = ClaudeTranscript::new();
    for e in echoes {
        t.expect_echo(e);
    }
    let mut out = Vec::new();
    let mut last_turn: Option<String> = None;
    for f in frames {
        let r = &f["record"];
        if let Some(turn) = r["turn"].as_str() {
            last_turn = Some(turn.to_owned());
        }
        let ev = t.line(&r["line"].to_string());
        for mut item in ev.items {
            if item.kind.is_some() {
                item.turn = last_turn.clone();
            }
            out.push(DriverUpdate {
                binding: binding.to_owned(),
                subagent: None,
                update: Open::Known(DriverUpdateKind::Item { item }),
                extra: Map::new(),
            });
        }
    }
    out
}
