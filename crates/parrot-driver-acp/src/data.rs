//! Driver data (spec 31): the parts of the ACP mapping that change between agents.
//!
//! `driver.kdl` and `levels.kdl` ship with the egg, which is their only source. Without
//! `PARROT_DRIVER_DATA`, or without a file, nothing is mapped: every mode counts as `host` with
//! network and every tool needs `host` (spec 15.2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use parrot_proto::driver_data::{self, LevelsFile};
use parrot_proto::option::PermissionLevel;
use serde_json::json;

/// What the data files say.
#[derive(Debug, Clone, PartialEq)]
pub struct Data {
    /// Tools per level, and the level each agent mode gives (`levels.kdl`).
    pub levels: LevelsFile,
    /// A command that starts the agent, from `driver.kdl`.
    pub command: Option<Vec<String>>,
    /// The agent's display name.
    pub agent_name: String,
    /// The data version, from `driver.kdl`.
    pub version: String,
}

impl Default for Data {
    fn default() -> Self {
        Data {
            levels: LevelsFile::default(),
            command: None,
            agent_name: "ACP agent".into(),
            version: "1".into(),
        }
    }
}

impl Data {
    /// The level a mode id gives; an unmapped mode is `host` with network. Before the agent
    /// reports a mode, the mapping for `default` applies.
    pub fn level_of(&self, mode: Option<&str>) -> (PermissionLevel, bool) {
        let values = BTreeMap::from([("mode".to_owned(), json!(mode.unwrap_or("default")))]);
        self.levels.level_of(&values)
    }

    /// The level a tool call needs: its name first, which is more specific than its ACP kind.
    /// Names match the data without regard to case.
    pub fn tool_level(&self, kind: &str, name: &str) -> PermissionLevel {
        let listed = |candidate: &str| {
            self.levels
                .level
                .iter()
                .flat_map(|l| l.tool.iter())
                .find(|t| !candidate.is_empty() && t.eq_ignore_ascii_case(candidate))
                .cloned()
        };
        match listed(name).or_else(|| listed(kind)) {
            Some(tool) => self.levels.tool_level(&tool),
            None => PermissionLevel::Host,
        }
    }

    /// The levels the mapped modes give, lowest first.
    pub fn mode_levels(&self) -> Vec<PermissionLevel> {
        let mut levels: Vec<PermissionLevel> = self.levels.map.iter().map(|m| m.level).collect();
        levels.sort();
        levels.dedup();
        levels
    }
}

/// The folder the runtime named.
pub fn folder() -> Option<PathBuf> {
    std::env::var_os("PARROT_DRIVER_DATA").map(PathBuf::from)
}

/// Reads the data files, falling back to [`Data::default`] for what is missing or malformed.
pub fn load() -> Data {
    match folder() {
        Some(dir) => load_from(&dir),
        None => Data::default(),
    }
}

/// [`load`] from one folder.
pub fn load_from(dir: &Path) -> Data {
    let mut data = Data::default();
    if let Ok(text) = std::fs::read_to_string(dir.join("driver.kdl")) {
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("command ") {
                let argv = strings(rest);
                if !argv.is_empty() {
                    data.command = Some(argv);
                }
            } else if line.starts_with("data-version ")
                && let Some(v) = strings(line).into_iter().next()
            {
                data.version = v;
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("levels.kdl")) {
        // A malformed file leaves nothing mapped, which holds every agent to `host`.
        if let Ok(levels) = driver_data::parse("levels.kdl", &text) {
            data.levels = levels;
        }
    }
    data
}

/// The quoted strings on a line, in order.
fn strings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else {
            break;
        };
        out.push(after[..end].to_owned());
        rest = &after[end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn egg() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eggs/acp/parrot")
    }

    #[test]
    fn without_data_every_mode_and_tool_is_host() {
        let data = Data::default();
        assert_eq!(data.level_of(Some("plan")), (PermissionLevel::Host, true));
        assert_eq!(data.tool_level("read", "Read"), PermissionLevel::Host);
    }

    #[test]
    fn the_egg_holds_every_agent_to_host_until_a_mode_says_otherwise() {
        let data = load_from(&egg());
        assert_eq!(data.level_of(Some("plan")), (PermissionLevel::Read, false));
        assert_eq!(data.level_of(Some("a-new-mode")), (PermissionLevel::Host, true));
        assert_eq!(data.level_of(None), (PermissionLevel::Read, false));
        assert_eq!(data.tool_level("execute", "Bash"), PermissionLevel::Host);
        assert_eq!(data.tool_level("read", "Read"), PermissionLevel::Read);
    }

    #[test]
    fn levels_kdl_overrides_modes_and_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("levels.kdl"),
            r#"
map level="read" network=#false { when mode="careful"; }
level "write" { tool "Change"; }
            "#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("driver.kdl"),
            r#"
driver "builtin" name="acp"
command "acp-agent" "--acp"
data-version "7"
            "#,
        )
        .unwrap();
        let data = load_from(dir.path());
        assert_eq!(data.level_of(Some("careful")), (PermissionLevel::Read, false));
        assert_eq!(data.tool_level("other", "Change"), PermissionLevel::Write);
        assert_eq!(data.command, Some(vec!["acp-agent".into(), "--acp".into()]));
        assert_eq!(data.version, "7");
    }
}
