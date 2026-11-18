//! Sessions opencode stored outside a binding (spec 9.5, 30.1): `history/discover` lists the
//! session files under the opencode data directory, and `history/read` replays one as items with
//! `binding: ""` (Appendix E4).
//!
//! A session is one JSON file per session under `storage/session/<project>/`, its messages one
//! file each under `storage/message/<session>/`, and their parts under `storage/part/<message>/`.
//! Part ids are stable, so history read again keeps the item ids a live binding gave them.

use std::path::{Path, PathBuf};

use parrot_proto::driver::{DriverUpdate, DriverUpdateKind, HistoryDiscoverResult, HistoryReadResult};
use parrot_proto::item::{ItemKind, ItemPatch, ItemSource};
use parrot_proto::open::Open;
use parrot_proto::session::{DiscoveredSession, Ref, RefStrength, SessionDiscoverParams};
use parrot_proto::state::{Turn, TurnStatus, TurnStop};
use serde_json::{Map, Value, json};

/// Items per `history/read` page.
pub const PAGE: usize = 500;

/// `$XDG_DATA_HOME/opencode/storage`, else `~/.local/share/opencode/storage`.
pub fn storage_root() -> Option<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(data.join("opencode/storage"))
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The project's worktree, from `storage/project/<id>.json`.
fn worktree(root: &Path, project: &str) -> Option<String> {
    let file = root.join("project").join(format!("{project}.json"));
    read_json(&file)?
        .get("worktree")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// `history/discover` under `root`.
pub fn discover(root: &Path, p: &SessionDiscoverParams) -> HistoryDiscoverResult {
    let mut files: Vec<(i64, PathBuf)> = Vec::new();
    let Ok(projects) = std::fs::read_dir(root.join("session")) else {
        return HistoryDiscoverResult {
            found: vec![],
            next: None,
        };
    };
    for project in projects.flatten() {
        if !project.path().is_dir() {
            continue;
        }
        for file in json_files(&project.path()) {
            let updated = read_json(&file)
                .and_then(|v| {
                    v.pointer("/time/updated")
                        .or_else(|| v.pointer("/time/created"))
                        .and_then(Value::as_i64)
                })
                .unwrap_or(0);
            files.push((updated, file));
        }
    }
    files.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
    let start: usize = p.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
    let limit = p.limit.unwrap_or(50).max(1) as usize;
    let mut found = Vec::new();
    for (updated, file) in files.iter().skip(start).take(limit) {
        let Some(session) = read_json(file) else {
            continue;
        };
        let Some(id) = session.get("id").and_then(Value::as_str) else {
            continue;
        };
        let project = session
            .get("projectID")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let cwd = worktree(root, project)
            .or_else(|| p.cwd.clone())
            .unwrap_or_default();
        if let Some(want) = &p.cwd
            && !want.is_empty()
            && &cwd != want
        {
            continue;
        }
        found.push(DiscoveredSession {
            reference: Ref {
                id: id.to_owned(),
                strength: RefStrength::Strong,
            },
            title: session.get("title").and_then(Value::as_str).map(str::to_owned),
            cwd,
            updated: rfc3339(*updated),
        });
    }
    let next = (start + limit < files.len()).then(|| (start + limit).to_string());
    HistoryDiscoverResult { found, next }
}

/// The session file for `id` under `root`.
pub fn find(root: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return None;
    }
    let projects = std::fs::read_dir(root.join("session")).ok()?;
    for project in projects.flatten() {
        let file = project.path().join(format!("{id}.json"));
        if file.is_file() {
            return Some(file);
        }
    }
    None
}

/// `history/read` of session `id`'s stored messages and parts, from `cursor`.
pub fn read(root: &Path, id: &str, cursor: Option<&str>) -> HistoryReadResult {
    let mut messages: Vec<Value> = json_files(&root.join("message").join(id))
        .into_iter()
        .filter_map(|f| read_json(&f))
        .collect();
    messages.sort_by_key(|m| m.pointer("/time/created").and_then(Value::as_i64).unwrap_or(0));
    let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    let end = (start + PAGE).min(messages.len());
    let mut replay = Replay::default();
    for (i, message) in messages.iter().enumerate().take(end) {
        let mut parts: Vec<Value> = json_files(
            &root
                .join("part")
                .join(message.get("id").and_then(Value::as_str).unwrap_or_default()),
        )
        .into_iter()
        .filter_map(|f| read_json(&f))
        .collect();
        parts.sort_by_key(|p| p.get("id").and_then(Value::as_str).map(str::to_owned));
        if i >= start {
            replay.message(message, &parts);
        } else {
            replay.scan(message);
        }
    }
    let next = (end < messages.len()).then(|| end.to_string());
    HistoryReadResult {
        updates: replay.updates,
        next,
    }
}

/// The items one session produced, in order.
#[derive(Default)]
struct Replay {
    updates: Vec<DriverUpdate>,
    turns: u64,
    turn: Option<String>,
}

impl Replay {
    fn item(&self, patch: ItemPatch) -> DriverUpdate {
        DriverUpdate {
            binding: String::new(),
            subagent: None,
            update: Open::Known(DriverUpdateKind::Item { item: patch }),
            extra: Map::new(),
        }
    }

    fn scan(&mut self, message: &Value) {
        if message.get("role").and_then(Value::as_str) == Some("user") {
            self.turns += 1;
            self.turn = Some(format!("h-{}", self.turns));
        }
    }

    fn message(&mut self, message: &Value, parts: &[Value]) {
        let role = message.get("role").and_then(Value::as_str).unwrap_or_default();
        let time = message
            .pointer("/time/created")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let message_id = message.get("id").and_then(Value::as_str).unwrap_or("message");
        if role == "user" {
            let text: String = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            if text.is_empty() {
                return;
            }
            self.turns += 1;
            let turn = format!("h-{}", self.turns);
            self.turn = Some(turn.clone());
            let id = format!("{message_id}:u");
            self.updates.push(DriverUpdate {
                binding: String::new(),
                subagent: None,
                update: Open::Known(DriverUpdateKind::Turn {
                    turn: Turn {
                        id: turn.clone(),
                        binding: String::new(),
                        status: TurnStatus::Completed,
                        stop: Some(Open::Known(TurnStop::End)),
                        continuable: false,
                        input: Some(id.clone()),
                        started: rfc3339(time),
                        ended: Some(rfc3339(time)),
                        error: None,
                        usage: None,
                        meta: None,
                        extra: Map::new(),
                    },
                }),
                extra: Map::new(),
            });
            self.updates.push(self.item(ItemPatch {
                id,
                kind: Some(Open::Known(ItemKind::User)),
                turn: Some(turn),
                source: Some(ItemSource::Transcript),
                set: Some(map_of("text", json!(text))),
                ..Default::default()
            }));
            return;
        }
        for part in parts {
            let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
            let id = part
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{message_id}:{kind}"));
            match kind {
                "text" | "reasoning" => {
                    let Some(text) = part.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    self.updates.push(self.item(ItemPatch {
                        id,
                        kind: Some(Open::Known(if kind == "reasoning" {
                            ItemKind::Reasoning
                        } else {
                            ItemKind::Assistant
                        })),
                        turn: self.turn.clone(),
                        source: Some(ItemSource::Transcript),
                        set: Some(map_of("text", json!(text))),
                        streaming: Some(false),
                        ..Default::default()
                    }));
                }
                "tool" => {
                    let tool = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
                    let state = part.get("state").cloned().unwrap_or_else(|| json!({}));
                    let mut set = map_of("tool", json!(tool));
                    set.insert(
                        "title".into(),
                        json!(state.get("title").and_then(Value::as_str).unwrap_or(tool)),
                    );
                    set.insert(
                        "input".into(),
                        state.get("input").cloned().unwrap_or_else(|| json!({})),
                    );
                    set.insert(
                        "status".into(),
                        json!(match state.get("status").and_then(Value::as_str) {
                            Some("completed") => "ok",
                            Some("error") => "error",
                            _ => "running",
                        }),
                    );
                    set.insert("caller".into(), json!("model"));
                    if let Some(output) = state.get("output").and_then(Value::as_str) {
                        set.insert("output".into(), json!(output));
                    }
                    self.updates.push(
                        self.item(ItemPatch {
                            id: part
                                .get("callID")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or(id),
                            kind: Some(Open::Known(ItemKind::Tool)),
                            turn: self.turn.clone(),
                            source: Some(ItemSource::Transcript),
                            set: Some(set),
                            streaming: Some(false),
                            ..Default::default()
                        }),
                    );
                }
                "compaction" => {
                    self.updates.push(self.item(ItemPatch {
                        id,
                        kind: Some(Open::Known(ItemKind::Compaction)),
                        turn: self.turn.clone(),
                        source: Some(ItemSource::Transcript),
                        set: Some(map_of("trigger", json!("auto"))),
                        ..Default::default()
                    }));
                }
                _ => {}
            }
        }
    }
}

fn map_of(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

/// A time in RFC 3339, UTC, to the millisecond.
pub fn rfc3339(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let sub = millis.rem_euclid(1000) as u64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{sub:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_and_reads_a_session() {
        let root = std::env::temp_dir().join(format!("parrot-opencode-history-{}", std::process::id()));
        let project = root.join("session/proj-1");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(
            root.join("project/proj-1.json"),
            r#"{"id":"proj-1","worktree":"/work/demo"}"#,
        )
        .unwrap();
        std::fs::write(
            project.join("ses-1.json"),
            r#"{"id":"ses-1","projectID":"proj-1","title":"Notes","time":{"created":1000,"updated":2000}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("message/ses-1")).unwrap();
        std::fs::create_dir_all(root.join("part/msg-1")).unwrap();
        std::fs::create_dir_all(root.join("part/msg-2")).unwrap();
        std::fs::write(
            root.join("message/ses-1/msg-1.json"),
            r#"{"id":"msg-1","sessionID":"ses-1","role":"user","time":{"created":1000}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("part/msg-1/part-1.json"),
            r#"{"id":"part-1","messageID":"msg-1","type":"text","text":"List the notes"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("message/ses-1/msg-2.json"),
            r#"{"id":"msg-2","sessionID":"ses-1","role":"assistant","time":{"created":1100}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("part/msg-2/part-2.json"),
            r#"{"id":"part-2","messageID":"msg-2","type":"text","text":"One note."}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("part/msg-2/part-3.json"),
            r#"{"id":"part-3","messageID":"msg-2","type":"tool","tool":"read","callID":"call-1","state":{"status":"completed","input":{"path":"notes.md"},"output":"hello"}}"#,
        )
        .unwrap();
        let found = discover(
            &root,
            &SessionDiscoverParams {
                agent: "opencode".into(),
                cwd: Some("/work/demo".into()),
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(found.found.len(), 1);
        assert_eq!(found.found[0].reference.id, "ses-1");
        assert_eq!(found.found[0].title.as_deref(), Some("Notes"));
        assert_eq!(found.found[0].cwd, "/work/demo");
        let result = read(&root, "ses-1", None);
        let items: Vec<&ItemPatch> = result
            .updates
            .iter()
            .filter_map(|u| match &u.update {
                Open::Known(DriverUpdateKind::Item { item }) => Some(item),
                _ => None,
            })
            .collect();
        assert!(items.iter().any(|i| i.kind == Some(Open::Known(ItemKind::User))));
        assert!(items.iter().any(
            |i| i.id == "call-1" && i.set.as_ref().and_then(|s| s.get("output")) == Some(&json!("hello"))
        ));
        assert!(result.updates.iter().all(|u| u.binding.is_empty()));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
