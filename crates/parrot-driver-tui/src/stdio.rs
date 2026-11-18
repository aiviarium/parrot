//! `parrot driver tui`: the TUI core as a process driver on stdio, for a data-only egg on
//! `driver "builtin" name="tui"`. Everything comes from the egg's `parrot` folder:
//!
//! | File or node | Gives |
//! | --- | --- |
//! | `profile.kdl` | The TUI profile |
//! | `command "prog" "arg"…` in `driver.kdl` | The harness command. Without it, the `cli` name |
//! | `transcript "claude"`, `"codex"` or `"jsonl" dir=…` in `driver.kdl` | Where the transcript is, and its format |
//! | `mcp "mcp-config"` or `mcp "config-override"` in `driver.kdl` | How the harness takes the MCP server |
//!
//! The agent id and name come from `PARROT_DRIVER_AGENT` and `PARROT_DRIVER_AGENT_NAME`.

use crate::driver::{Config, McpStyle, TranscriptDir, TuiDriver};
use parrot_driver::traits::DriverContext;
use parrot_proto::driver_data::{self, Children};
use std::path::Path;

/// The driver configuration from an egg's `parrot` folder.
pub fn config_from_data(dir: &Path) -> Result<Config, String> {
    let profile_path = dir.join("profile.kdl");
    let profile =
        parrot_tui::Profile::load(&profile_path).map_err(|e| format!("{}: {e}", profile_path.display()))?;
    let driver = std::fs::read_to_string(dir.join("driver.kdl")).unwrap_or_default();
    let json = driver_data::to_json(&driver, Children::Lists).unwrap_or_default();
    let command = driver_data::driver_command(&driver)
        .or_else(|| json["cli"][0]["name"].as_str().map(|c| vec![c.to_owned()]))
        .or_else(|| json["cli"][0].as_str().map(|c| vec![c.to_owned()]))
        .ok_or_else(|| "driver.kdl names no command and no cli".to_string())?;
    let agent = driver_data::agent_id("tui");
    let mut c = Config::new(&agent, command, profile);
    c.name = driver_data::agent_name(&c.name);
    let t = &json["transcript"][0];
    let format = t.as_str().or_else(|| t["name"].as_str());
    if let Some((dir, format)) = format.and_then(|f| TranscriptDir::from_data(f, t["dir"].as_str())) {
        c.transcript = Some(dir);
        c.transcript_format = format;
    }
    c.mcp = json["mcp"][0].as_str().and_then(McpStyle::from_data);
    Ok(c)
}

/// Serves the TUI core on stdin and stdout until stdin closes.
pub async fn serve_stdio() -> std::io::Result<()> {
    let dir = std::env::var_os(driver_data::DRIVER_DATA_ENV)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| std::io::Error::other("the TUI driver needs PARROT_DRIVER_DATA"))?;
    let config = config_from_data(Path::new(&dir)).map_err(std::io::Error::other)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let driver = TuiDriver::new(config, DriverContext { updates: tx });
    let handle = driver.handle();
    parrot_driver::bin::serve_stdio(move |method, params| handle(method, params), rx).await
}
