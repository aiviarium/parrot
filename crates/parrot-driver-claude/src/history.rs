//! Sessions Claude Code stored outside Parrot (spec 9.5, 30.1): `history/discover` lists the
//! transcripts under `~/.claude/projects`, and `history/read` replays one through the same
//! mapping as a live binding, with `binding: ""` (Appendix E4). Transcript lines are the
//! stream-json messages with `uuid`s, so items keep the ids a live binding gave them.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parrot_proto::driver::{DriverUpdate, DriverUpdateKind, HistoryDiscoverResult, HistoryReadResult};
use parrot_proto::open::Open;
use parrot_proto::session::{DiscoveredSession, Ref, RefStrength, SessionDiscoverParams};
use serde_json::Value;

use crate::mapping::MapState;
use crate::wire;

/// Lines per `history/read` page.
pub const PAGE: usize = 500;

/// `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
pub fn projects_root() -> Option<PathBuf> {
    let base = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".claude"),
    };
    Some(base.join("projects"))
}

/// The folder for `cwd`: every character that is not a letter or digit becomes `-`.
pub fn project_dir(root: &Path, cwd: &str) -> PathBuf {
    let folder: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    root.join(folder)
}

fn transcripts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect()
}

/// The first user prompt and the working directory a transcript names.
fn summary(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let (mut title, mut cwd) = (None, None);
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = v["cwd"].as_str().map(str::to_owned);
        }
        if title.is_none() && v["type"] == "user" && v["isMeta"] != true {
            title = v["message"]["content"]
                .as_str()
                .filter(|t| !t.starts_with('<'))
                .map(|t| t.chars().take(80).collect());
        }
        if title.is_some() && cwd.is_some() {
            break;
        }
    }
    (title, cwd)
}

/// `history/discover` under `root`.
pub fn discover(root: &Path, p: &SessionDiscoverParams) -> HistoryDiscoverResult {
    let dirs: Vec<PathBuf> = match &p.cwd {
        Some(cwd) => vec![project_dir(root, cwd)],
        None => std::fs::read_dir(root)
            .map(|es| es.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default(),
    };
    let mut files: Vec<(SystemTime, PathBuf)> = dirs
        .iter()
        .flat_map(|d| transcripts(d))
        .map(|f| {
            let m = f
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (m, f)
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let start: usize = p.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
    let limit = p.limit.unwrap_or(50).max(1) as usize;
    let found = files
        .iter()
        .skip(start)
        .take(limit)
        .map(|(m, f)| {
            let (title, cwd) = summary(f);
            DiscoveredSession {
                reference: Ref {
                    id: f.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                    strength: RefStrength::Strong,
                },
                title,
                cwd: cwd.or_else(|| p.cwd.clone()).unwrap_or_default(),
                updated: crate::driver::rfc3339(*m),
            }
        })
        .collect();
    let next = (start + limit < files.len()).then(|| (start + limit).to_string());
    HistoryDiscoverResult { found, next }
}

/// The transcript of session `id` under `root`.
pub fn find(root: &Path, id: &str) -> Option<PathBuf> {
    if id.contains('/') || id.contains("..") {
        return None;
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .find(|p| p.exists())
}

/// `history/read` of a transcript's text, from line `cursor`.
pub fn read(text: &str, cursor: Option<&str>) -> HistoryReadResult {
    let lines: Vec<&str> = text.lines().collect();
    let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    // The mapping needs what came before the page, so it replays from the start and keeps the
    // page's updates.
    let mut state = MapState::new("");
    let mut updates: Vec<DriverUpdate> = Vec::new();
    let end = (start + PAGE).min(lines.len());
    for (i, line) in lines.iter().enumerate().take(end) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Subagent lines and the CLI's own notes are not part of the conversation.
        if v["isSidechain"] == true || v["isMeta"] == true {
            continue;
        }
        let Ok(msg) = serde_json::from_value::<wire::ClaudeOut>(v.clone()) else {
            continue;
        };
        let at = v["timestamp"].as_str().unwrap_or_default();
        let out = state.step(&msg, at);
        if i >= start {
            updates.extend(out.into_iter().filter(|u| {
                matches!(
                    u.update,
                    Open::Known(
                        DriverUpdateKind::Item { .. }
                            | DriverUpdateKind::Turn { .. }
                            | DriverUpdateKind::Ref { .. }
                    )
                )
            }));
        }
    }
    let next = (end < lines.len()).then(|| end.to_string());
    HistoryReadResult { updates, next }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &str = r#"{"type":"user","uuid":"u-1","sessionId":"sess-made-up","cwd":"/work/demo","timestamp":"2026-01-02T03:04:05.000Z","message":{"role":"user","content":"List the files"}}
{"type":"assistant","uuid":"a-1","sessionId":"sess-made-up","timestamp":"2026-01-02T03:04:06.000Z","message":{"id":"msg_h1","role":"assistant","content":[{"type":"text","text":"Two files."}],"stop_reason":"end_turn"}}
{"type":"user","uuid":"m-1","isMeta":true,"sessionId":"sess-made-up","message":{"role":"user","content":"<local-command-stdout></local-command-stdout>"}}
"#;

    #[test]
    fn discovers_and_reads_a_transcript() {
        let root = std::env::temp_dir().join(format!("parrot-claude-history-{}", std::process::id()));
        let dir = project_dir(&root, "/work/demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sess-made-up.jsonl"), TRANSCRIPT).unwrap();
        let found = discover(
            &root,
            &SessionDiscoverParams {
                agent: "claude".into(),
                cwd: Some("/work/demo".into()),
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(found.found.len(), 1);
        assert_eq!(found.found[0].reference.id, "sess-made-up");
        assert_eq!(found.found[0].title.as_deref(), Some("List the files"));
        assert_eq!(found.found[0].cwd, "/work/demo");
        let path = find(&root, "sess-made-up").unwrap();
        let r = read(&std::fs::read_to_string(path).unwrap(), None);
        let items: Vec<Value> = r
            .updates
            .iter()
            .map(|u| serde_json::to_value(u).unwrap())
            .filter(|u| u["update"]["type"] == "item")
            .collect();
        assert!(r.updates.iter().all(|u| u.binding.is_empty()));
        assert_eq!(items[0]["update"]["item"]["id"], "u-1");
        assert_eq!(items[0]["update"]["item"]["set"]["text"], "List the files");
        assert!(
            items
                .iter()
                .any(|u| u["update"]["item"]["set"]["text"] == "Two files.")
        );
        assert!(r.next.is_none());
        assert!(find(&root, "../x").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
