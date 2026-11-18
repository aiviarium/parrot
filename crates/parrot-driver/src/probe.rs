//! What a driver can find out about a harness without a session (spec 6): its version from the
//! CLI, and names from its config files. Nothing here reads a secret's value; sign-in is judged
//! by whether a credentials file or variable exists.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

/// The first version-like word (`1.2.3`) in the output of `bin --version`, or `None` when the
/// CLI can't run or answers nothing in time.
pub async fn version(bin: &OsStr) -> Option<String> {
    let run = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    version_in(&String::from_utf8_lossy(&out.stdout))
}

/// What a CLI subcommand printed, such as `claude auth status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `bin args…` with no input and returns what it printed, or `None` when it can't run or
/// takes longer than five seconds. Status commands print no secrets, and nothing here logs them.
pub async fn run(bin: &OsStr, args: &[&str]) -> Option<Output> {
    let run = tokio::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .ok()?
        .ok()?;
    Some(Output {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The first word of `text` that looks like `major.minor.patch`.
pub fn version_in(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '/' | ';'))
        .map(|w| w.trim_start_matches('v'))
        .find(|w| {
            let parts: Vec<&str> = w.split('.').collect();
            parts.len() >= 3
                && parts[..3]
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// Whether `version` is at least `min` and below `below` (both `major.minor.patch`).
pub fn in_range(version: &str, min: &str, below: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.split('.').take(3).map(|p| p.parse().ok()).collect() };
    match (parse(version), parse(min), parse(below)) {
        (Some(v), Some(lo), Some(hi)) => v >= lo && v < hi,
        _ => false,
    }
}

/// The names of the folders in `dir` that hold `marker`, such as skills with a `SKILL.md`.
pub fn folders_with(dir: &Path, marker: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join(marker).is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The keys of the object at `key` in a JSON file, such as the MCP server names in
/// `~/.claude.json`. Values are not returned.
pub fn json_keys(file: &Path, key: &str) -> Vec<String> {
    let Some(v) = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return vec![];
    };
    let mut out: Vec<String> = v
        .get(key)
        .and_then(|o| o.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// The names of the TOML tables `[<prefix>.<name>]` in a file, such as `mcp_servers` in Codex's
/// `config.toml`. Only table headers are read.
pub fn toml_tables(file: &Path, prefix: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return vec![];
    };
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(h) = line.trim().strip_prefix('[').and_then(|l| l.strip_suffix(']')) else {
            continue;
        };
        let Some(rest) = h.trim().strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) else {
            continue;
        };
        let name = match rest.strip_prefix('"') {
            Some(q) => q.split('"').next().unwrap_or_default(),
            None => rest.split('.').next().unwrap_or_default(),
        };
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_ranges() {
        assert_eq!(version_in("2.1.327 (Claude Code)").as_deref(), Some("2.1.327"));
        assert_eq!(version_in("codex-cli 0.167.1").as_deref(), Some("0.167.1"));
        assert_eq!(version_in("nothing here"), None);
        assert_eq!(
            version_in("parrot/0.167.1 (Ubuntu 26.4.0; x86_64)").as_deref(),
            Some("0.167.1")
        );
        assert!(in_range("2.1.327", "2.1.0", "2.2.0"));
        assert!(!in_range("2.2.0", "2.1.0", "2.2.0"));
    }

    #[test]
    fn names_from_config_files() {
        let dir = std::env::temp_dir().join(format!("parrot-probe-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("skills/review")).unwrap();
        std::fs::create_dir_all(dir.join("skills/empty")).unwrap();
        std::fs::write(dir.join("skills/review/SKILL.md"), "# Review").unwrap();
        assert_eq!(folders_with(&dir.join("skills"), "SKILL.md"), ["review"]);
        std::fs::write(
            dir.join("config.toml"),
            "model = \"x\"\n[mcp_servers.docs]\ncommand = \"d\"\n[mcp_servers.docs.env]\nTOKEN = \"made-up\"\n[mcp_servers.\"web search\"]\nurl = \"https://mcp.example.test\"\n",
        )
        .unwrap();
        assert_eq!(
            toml_tables(&dir.join("config.toml"), "mcp_servers"),
            ["docs", "web search"]
        );
        std::fs::write(
            dir.join("c.json"),
            r#"{"mcpServers": {"notes": {"env": {"KEY": "made-up"}}}}"#,
        )
        .unwrap();
        assert_eq!(json_keys(&dir.join("c.json"), "mcpServers"), ["notes"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
