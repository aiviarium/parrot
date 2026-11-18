//! The runtime's configuration file, `config.json` in the state directory.
//!
//! | Field | Shape | What it sets |
//! | --- | --- | --- |
//! | `tui` | `{ <agent>: TuiAgent }` | Agents driven through their terminal (spec 32.2) |
//! | `notify` | `string[]` | Push targets: `webhook:<url>` or `ntfy:<server>/<topic>` (spec 36) |
//! | `notifyUrl` | `string` | A link put in push bodies, with the session appended |
//!
//! A missing file is the empty configuration. Configuration holds no secrets.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file name inside the state directory.
pub const FILE: &str = "config.json";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub tui: BTreeMap<String, TuiAgent>,
    #[serde(default)]
    pub notify: Vec<String>,
    #[serde(default)]
    pub notify_url: Option<String>,
}

/// One agent under TUI control.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiAgent {
    /// The harness command and its arguments.
    pub command: Vec<String>,
    /// The TUI profile (TOML).
    pub profile: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// A folder whose newest `.jsonl` is the transcript; `"claude"` for Claude Code's own folder.
    #[serde(default)]
    pub transcript: Option<String>,
    /// Milliseconds an unmatched screen is held before it is `unrecognised`.
    #[serde(default)]
    pub unrecognised_after_ms: Option<u64>,
    /// Rows and columns of the terminal.
    #[serde(default)]
    pub size: Option<(u16, u16)>,
    /// Start the harness without `parrot guard`.
    #[serde(default)]
    pub no_guard: bool,
}

impl Config {
    /// Reads `config.json` from `dir`; a missing file is the default.
    pub fn load(dir: &Path) -> Result<Config, String> {
        let path = dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_config() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Config::load(dir.path()).unwrap().tui.is_empty());
        std::fs::write(
            dir.path().join(FILE),
            r#"{"tui":{"fake":{"command":["sh","x.sh"],"profile":"p.toml","size":[12,60]}},
                "notify":["webhook:https://hooks.example.test/a"]}"#,
        )
        .unwrap();
        let c = Config::load(dir.path()).unwrap();
        assert_eq!(c.tui["fake"].command, ["sh", "x.sh"]);
        assert_eq!(c.tui["fake"].size, Some((12, 60)));
        assert_eq!(c.notify.len(), 1);
    }
}
