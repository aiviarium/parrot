//! Agents under TUI control (spec 32.2, E4). The runtime runs [`parrot_driver_tui::TuiDriver`] in
//! process for a binding with `control: "tui"`, and answers `binding/screen` and `binding/keys`
//! through it.

use crate::config::TuiAgent;
use parrot_driver_tui::{Config, TranscriptDir};
use parrot_term::screen::Size;
use parrot_tui::Profile;
use serde_json::{Value, json};
use std::time::Duration;

/// The driver configuration for `agent`, with its profile read from disk.
pub fn driver_config(agent: &str, a: &TuiAgent) -> Result<Config, String> {
    let profile = Profile::load(&a.profile).map_err(|e| format!("{}: {e}", a.profile.display()))?;
    let mut c = Config::new(agent, a.command.clone(), profile);
    if let Some(name) = &a.name {
        c.name = name.clone();
    }
    c.env = a.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    c.transcript = match a.transcript.as_deref() {
        None => None,
        Some("claude") => Some(TranscriptDir::ClaudeProject),
        Some(dir) => Some(TranscriptDir::Dir(dir.into())),
    };
    if let Some(ms) = a.unrecognised_after_ms {
        c.unrecognised_after = Duration::from_millis(ms);
    }
    if let Some((rows, cols)) = a.size {
        c.size = Size::new(rows, cols);
    }
    if a.no_guard {
        c.guard = None;
    }
    Ok(c)
}

/// The agent record the core lists before its driver has answered `agent/list`.
pub fn agent_record(agent: &str, a: &TuiAgent) -> Value {
    json!({
        "id": agent,
        "name": a.name.clone().unwrap_or_else(|| agent.to_string()),
        "driver": { "name": "tui", "version": env!("CARGO_PKG_VERSION"), "kind": "builtin", "data": "1" },
        "installed": true,
        "auth": "unknown",
        "usable": true,
        "controls": ["tui"],
        "features": { "session.stream": {} },
        "levels": ["host"],
        "isolation": ["none"],
        "options": [],
        "actions": [],
    })
}
