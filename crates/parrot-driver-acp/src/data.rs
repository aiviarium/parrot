//! Driver data (spec 31): the parts of the ACP mapping that change between agents.
//!
//! `driver.kdl` and `levels.kdl` ship with the egg. Without `PARROT_DRIVER_DATA`, or without a
//! file, the built-in fallbacks below apply, so a fresh install works offline. An agent without a
//! mode mapping counts as `host` with network (spec 15.2).
//!
//! The files are read with a small reader for the two shapes this driver uses, so the crate needs
//! no KDL dependency: `mode "<id>" level="<level>" network=<bool>`, `level "<name>":` with
//! `tool "<name>";` children, and top-level `driver`/`command`/`data-version` nodes.

use std::path::{Path, PathBuf};

use parrot_proto::option::PermissionLevel;

/// What the data files say.
#[derive(Debug, Clone, PartialEq)]
pub struct Data {
    /// Mode id to level and network.
    pub modes: Vec<ModeRule>,
    /// Tool names or ACP tool kinds to the lowest level that allows them.
    pub tools: Vec<ToolRule>,
    /// A command that starts the agent, from `driver.kdl`.
    pub command: Option<Vec<String>>,
    /// The agent's display name.
    pub agent_name: String,
    /// The data version, from `driver.kdl`.
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModeRule {
    pub mode: String,
    pub level: PermissionLevel,
    pub network: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRule {
    pub tool: String,
    pub level: PermissionLevel,
}

impl Default for Data {
    fn default() -> Self {
        Data {
            modes: [
                ("bypassPermissions", PermissionLevel::Host, true),
                ("acceptEdits", PermissionLevel::Write, false),
                ("plan", PermissionLevel::Read, false),
                ("default", PermissionLevel::Read, false),
            ]
            .into_iter()
            .map(|(mode, level, network)| ModeRule {
                mode: mode.into(),
                level,
                network,
            })
            .collect(),
            tools: [
                ("read", PermissionLevel::Read),
                ("search", PermissionLevel::Read),
                ("think", PermissionLevel::Read),
                ("edit", PermissionLevel::Write),
                ("write", PermissionLevel::Write),
                ("delete", PermissionLevel::Write),
                ("move", PermissionLevel::Write),
                ("execute", PermissionLevel::Host),
                ("fetch", PermissionLevel::Host),
                ("other", PermissionLevel::Host),
            ]
            .into_iter()
            .map(|(tool, level)| ToolRule {
                tool: tool.into(),
                level,
            })
            .collect(),
            command: None,
            agent_name: "ACP agent".into(),
            version: "1".into(),
        }
    }
}

impl Data {
    /// The level a mode id gives; an unmapped mode is `host` with network.
    pub fn level_of(&self, mode: Option<&str>) -> (PermissionLevel, bool) {
        let Some(mode) = mode else {
            // Before the agent reports a mode, driver data's mapping for `default` applies.
            return self
                .modes
                .iter()
                .find(|r| r.mode == "default")
                .map_or((PermissionLevel::Host, true), |r| (r.level, r.network));
        };
        self.modes
            .iter()
            .find(|r| r.mode == mode)
            .map_or((PermissionLevel::Host, true), |r| (r.level, r.network))
    }

    /// The level a tool call needs: its name first, which is more specific than its ACP kind.
    pub fn tool_level(&self, kind: &str, name: &str) -> PermissionLevel {
        let find = |candidate: &str| {
            self.tools
                .iter()
                .find(|r| r.tool.eq_ignore_ascii_case(candidate))
                .map(|r| r.level)
        };
        find(name).or_else(|| find(kind)).unwrap_or(PermissionLevel::Host)
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
            if let Some(rest) = line.strip_prefix("driver ") {
                if let Some(name) = property(rest, "name") {
                    data.agent_name = name;
                }
            } else if let Some(rest) = line.strip_prefix("command ") {
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
        let mut level: Option<PermissionLevel> = None;
        // Braces and semicolons only group the one level of nesting this file has.
        for raw in text.replace(['{', '}', ';'], "\n").lines() {
            let line = raw.split("//").next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with("mode ") {
                let Some(mode) = strings(line).into_iter().next() else {
                    continue;
                };
                let Some(mode_level) = property(line, "level").and_then(|l| level_of_name(&l)) else {
                    continue;
                };
                let network = property(line, "network")
                    .map(|n| n == "true")
                    .unwrap_or(mode_level == PermissionLevel::Host);
                data.modes.retain(|r| r.mode != mode);
                data.modes.push(ModeRule {
                    mode,
                    level: mode_level,
                    network,
                });
            } else if line.starts_with("level ") {
                level = strings(line).into_iter().next().and_then(|l| level_of_name(&l));
            } else if line.starts_with("tool ")
                && let (Some(level), Some(tool)) = (level, strings(line).into_iter().next())
            {
                data.tools.retain(|r| r.tool != tool);
                data.tools.push(ToolRule { tool, level });
            }
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

/// The value of `key="value"` or `key=value` on a line.
fn property(line: &str, key: &str) -> Option<String> {
    let at = line.find(&format!("{key}="))? + key.len() + 1;
    let rest = line[at..].trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        return Some(stripped[..end].to_owned());
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ';' || c == '}')
        .unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

fn level_of_name(name: &str) -> Option<PermissionLevel> {
    match name {
        "read" => Some(PermissionLevel::Read),
        "write" => Some(PermissionLevel::Write),
        "sandboxed" => Some(PermissionLevel::Sandboxed),
        "host" => Some(PermissionLevel::Host),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_hold_every_agent_to_host_until_a_mode_says_otherwise() {
        let data = Data::default();
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
mode "careful" level="read" network=false
level "write" { tool "Change"; }
            "#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("driver.kdl"),
            r#"
driver "builtin" name="Example ACP"
command "acp-agent" "--acp"
data-version "7"
            "#,
        )
        .unwrap();
        let data = load_from(dir.path());
        assert_eq!(data.level_of(Some("careful")), (PermissionLevel::Read, false));
        assert_eq!(data.tool_level("other", "Change"), PermissionLevel::Write);
        assert_eq!(data.agent_name, "Example ACP");
        assert_eq!(data.command, Some(vec!["acp-agent".into(), "--acp".into()]));
        assert_eq!(data.version, "7");
    }
}
