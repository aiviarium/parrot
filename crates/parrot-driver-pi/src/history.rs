//! Sessions pi stored outside Parrot (spec 9.5, 30.1): `history/discover` lists the session files
//! under `$PI_CODING_AGENT_DIR/sessions`, and `history/read` replays one as items with
//! `binding: ""` (Appendix E4).
//!
//! A session file is JSONL: a `session` header, then entries that form a tree by `id` and
//! `parentId`. Messages keep the roles the prune stream gives them, so items get the ids a live
//! binding gave their tool calls.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parrot_proto::driver::{
    DriverUpdate, DriverUpdateKind, HistoryDiscoverResult, HistoryReadResult,
};
use parrot_proto::item::{ItemKind, ItemPatch};
use parrot_proto::open::Open;
use parrot_proto::session::{DiscoveredSession, Ref, RefStrength, SessionDiscoverParams};
use parrot_proto::state::{Turn, TurnStatus, TurnStop};
use serde_json::{Map, Value, json};

/// Lines per `history/read` page.
pub const PAGE: usize = 500;

/// `$PI_CODING_AGENT_SESSION_DIR`, else `$PI_CODING_AGENT_DIR/sessions`, else
/// `~/.pi/agent/sessions`.
pub fn root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
        return Some(PathBuf::from(dir).join("sessions"));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".pi/agent/sessions"))
}

/// The session files under `root`, newest first.
fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for dir in entries.flatten() {
        if !dir.path().is_dir() {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        out.extend(
            inner
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl")),
        );
    }
    out
}

/// The header of a session file: its id and working directory.
fn header(text: &str) -> (Option<String>, Option<String>) {
    for line in text.lines().take(4) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["type"] == "session" {
            return (
                v["id"].as_str().map(str::to_owned),
                v["cwd"].as_str().map(str::to_owned),
            );
        }
    }
    (None, None)
}

/// The first user prompt, for a session's title.
fn title(text: &str) -> Option<String> {
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["type"] != "message" || v["message"]["role"] != json!("user") {
            continue;
        }
        let content = &v["message"]["content"];
        let text = content.as_str().map(str::to_owned).or_else(|| {
            content
                .as_array()?
                .iter()
                .find_map(|b| b.get("text").and_then(Value::as_str).map(str::to_owned))
        });
        if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
            return Some(text.chars().take(80).collect());
        }
    }
    None
}

/// `history/discover` under `root`.
pub fn discover(root: &Path, p: &SessionDiscoverParams) -> HistoryDiscoverResult {
    let mut found: Vec<(SystemTime, DiscoveredSession)> = files(root)
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let (id, cwd) = header(&text);
            let id = id.or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
            if let (Some(want), Some(have)) = (&p.cwd, &cwd)
                && want != have
            {
                return None;
            }
            let modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((
                modified,
                DiscoveredSession {
                    reference: Ref {
                        id,
                        strength: RefStrength::Strong,
                    },
                    title: title(&text),
                    cwd: cwd.or_else(|| p.cwd.clone()).unwrap_or_default(),
                    updated: rfc3339(modified),
                },
            ))
        })
        .collect();
    found.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
    let start: usize = p.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
    let limit = p.limit.unwrap_or(50).max(1) as usize;
    let list: Vec<DiscoveredSession> = found
        .iter()
        .skip(start)
        .take(limit)
        .map(|(_, s)| s.clone())
        .collect();
    let next = (start + limit < found.len()).then(|| (start + limit).to_string());
    HistoryDiscoverResult {
        found: list,
        next,
    }
}

/// The session file for `id` under `root`.
pub fn find(root: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return None;
    }
    files(root)
        .into_iter()
        .find(|path| header(&std::fs::read_to_string(path).unwrap_or_default()).0.as_deref() == Some(id))
}

/// `history/read` of a session file's text, from line `cursor`.
pub fn read(text: &str, cursor: Option<&str>) -> HistoryReadResult {
    let lines: Vec<&str> = text.lines().collect();
    let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    let mut replay = Replay::default();
    let end = (start + PAGE).min(lines.len());
    for (i, line) in lines.iter().enumerate().take(end) {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if i >= start {
            replay.entry(&entry);
        } else {
            // The page needs what came before it, such as the turn an item belongs to.
            replay.scan(&entry);
        }
    }
    let next = (end < lines.len()).then(|| end.to_string());
    HistoryReadResult {
        updates: replay.updates,
        next,
    }
}

/// The items one session file produced, in order.
#[derive(Default)]
struct Replay {
    updates: Vec<DriverUpdate>,
    turns: u64,
    turn: Option<String>,
    started: Option<String>,
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

    /// Follows the tree without producing updates.
    fn scan(&mut self, entry: &Value) {
        if entry["type"] == "message" && entry["message"]["role"] == json!("user") {
            self.turns += 1;
            self.turn = Some(format!("h-{}", self.turns));
            self.started = entry["timestamp"].as_str().map(str::to_owned);
        }
    }

    fn entry(&mut self, entry: &Value) {
        match entry["type"].as_str() {
            Some("message") => self.message(entry),
            Some("compaction") => {
                let mut set = Map::new();
                set.insert("trigger".into(), json!("auto"));
                if let Some(summary) = entry.get("summary") {
                    set.insert("summary".into(), summary.clone());
                }
                let id = format!(
                    "compaction-{}",
                    entry["id"].as_str().unwrap_or("entry")
                );
                self.updates.push(self.item(ItemPatch {
                    id,
                    kind: Some(Open::Known(ItemKind::Compaction)),
                    turn: self.turn.clone(),
                    source: Some(parrot_proto::item::ItemSource::Transcript),
                    set: Some(set),
                    ..Default::default()
                }));
            }
            Some("branch_summary") => {
                let mut set = Map::new();
                set.insert("trigger".into(), json!("branch"));
                if let Some(summary) = entry.get("summary") {
                    set.insert("summary".into(), summary.clone());
                }
                let id = format!("branch-{}", entry["id"].as_str().unwrap_or("entry"));
                self.updates.push(self.item(ItemPatch {
                    id,
                    kind: Some(Open::Known(ItemKind::Compaction)),
                    turn: self.turn.clone(),
                    source: Some(parrot_proto::item::ItemSource::Transcript),
                    set: Some(set),
                    ..Default::default()
                }));
            }
            _ => {}
        }
    }

    fn message(&mut self, entry: &Value) {
        let message = &entry["message"];
        let at = entry["timestamp"].as_str().unwrap_or_default().to_owned();
        let entry_id = entry["id"].as_str().unwrap_or("entry");
        match message["role"].as_str() {
            Some("user") => {
                let Some(text) = text_of(&message["content"]) else {
                    return;
                };
                self.turns += 1;
                let turn = format!("h-{}", self.turns);
                self.turn = Some(turn.clone());
                self.started = Some(at.clone());
                let id = format!("{entry_id}:u");
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
                            started: at.clone(),
                            ended: Some(at),
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
                    source: Some(parrot_proto::item::ItemSource::Transcript),
                    set: Some(map_of("text", json!(text))),
                    ..Default::default()
                }));
            }
            Some("assistant") => {
                let blocks = message["content"].as_array().cloned().unwrap_or_default();
                for (index, block) in blocks.iter().enumerate() {
                    match block["type"].as_str() {
                        Some("text") | Some("thinking") => {
                            let Some(text) = block
                                .get("text")
                                .or_else(|| block.get("thinking"))
                                .and_then(Value::as_str)
                            else {
                                continue;
                            };
                            let kind = if block["type"] == "thinking" {
                                ItemKind::Reasoning
                            } else {
                                ItemKind::Assistant
                            };
                            self.updates.push(self.item(ItemPatch {
                                id: format!("{entry_id}:{index}"),
                                kind: Some(Open::Known(kind)),
                                turn: self.turn.clone(),
                                source: Some(parrot_proto::item::ItemSource::Transcript),
                                set: Some(map_of("text", json!(text))),
                                streaming: Some(false),
                                ..Default::default()
                            }));
                        }
                        Some("toolCall") => {
                            let id = block["id"].as_str().unwrap_or(entry_id).to_owned();
                            let name = block["name"].as_str().unwrap_or("tool").to_owned();
                            let mut set = map_of("tool", json!(name));
                            set.insert("title".into(), json!(name));
                            set.insert(
                                "input".into(),
                                block.get("arguments").cloned().unwrap_or_else(|| json!({})),
                            );
                            set.insert("status".into(), json!("running"));
                            set.insert("caller".into(), json!("model"));
                            self.updates.push(self.item(ItemPatch {
                                id,
                                kind: Some(Open::Known(ItemKind::Tool)),
                                turn: self.turn.clone(),
                                source: Some(parrot_proto::item::ItemSource::Transcript),
                                set: Some(set),
                                ..Default::default()
                            }));
                        }
                        _ => {}
                    }
                }
            }
            Some("toolResult") => {
                let id = message["toolCallId"]
                    .as_str()
                    .unwrap_or(entry_id)
                    .to_owned();
                let mut set = map_of(
                    "status",
                    json!(if message["isError"] == true {
                        "error"
                    } else {
                        "ok"
                    }),
                );
                if let Some(text) = text_of(&message["content"]) {
                    set.insert("output".into(), json!(text));
                }
                self.updates.push(self.item(ItemPatch {
                    id: id.clone(),
                    set: Some(set),
                    streaming: Some(false),
                    ..Default::default()
                }));
                if let Some(calls) = message.get("nestedCalls").and_then(|n| n.get("calls")) {
                    for (index, call) in calls.as_array().into_iter().flatten().enumerate() {
                        let key = call["id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("{id}/{index}"));
                        let name = call["name"].as_str().unwrap_or("tool");
                        let mut set = map_of("tool", json!(name));
                        set.insert("title".into(), json!(name));
                        set.insert("input".into(), call.get("arguments").cloned().unwrap_or_else(|| json!({})));
                        set.insert("status".into(), json!(call["status"].as_str().unwrap_or("running")));
                        set.insert("caller".into(), json!("model"));
                        self.updates.push(self.item(ItemPatch {
                            id: key,
                            kind: Some(Open::Known(ItemKind::Tool)),
                            parent: Some(id.clone()),
                            turn: self.turn.clone(),
                            source: Some(parrot_proto::item::ItemSource::Transcript),
                            set: Some(set),
                            ..Default::default()
                        }));
                    }
                }
            }
            _ => {}
        }
    }
}

fn text_of(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let text: String = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn map_of(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

/// A time in RFC 3339, UTC, to the millisecond.
pub fn rfc3339(time: SystemTime) -> String {
    let d = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = d.as_secs() as i64;
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = r#"{"type":"session","version":3,"id":"sess-made-up","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/work/demo"}
{"type":"message","id":"e1","parentId":null,"timestamp":"2026-01-02T03:04:05.100Z","message":{"role":"user","content":"List the notes"}}
{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-01-02T03:04:06.000Z","message":{"role":"assistant","content":[{"type":"text","text":"One note."},{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"notes.md"}}]}}
{"type":"message","id":"e3","parentId":"e2","timestamp":"2026-01-02T03:04:06.500Z","message":{"role":"toolResult","toolCallId":"call-1","isError":false,"content":[{"type":"text","text":"hello"}],"nestedCalls":{"complete":true,"calls":[{"id":"inner-1","name":"grep","status":"ok","arguments":{"pattern":"x"}}]}}}
{"type":"compaction","id":"c1","parentId":"e3","timestamp":"2026-01-02T03:05:00.000Z","summary":"Talked about notes."}
"#;

    #[test]
    fn discovers_and_reads_a_session() {
        let root = std::env::temp_dir().join(format!("parrot-pi-history-{}", std::process::id()));
        let dir = root.join("--work-demo--");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026_01_02_sess-made-up.jsonl"), SESSION).unwrap();
        let found = discover(
            &root,
            &SessionDiscoverParams {
                agent: "pi".into(),
                cwd: Some("/work/demo".into()),
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(found.found.len(), 1);
        assert_eq!(found.found[0].reference.id, "sess-made-up");
        assert_eq!(found.found[0].title.as_deref(), Some("List the notes"));
        assert_eq!(found.found[0].cwd, "/work/demo");
        let path = find(&root, "sess-made-up").expect("the session file");
        let read = read(&std::fs::read_to_string(path).unwrap(), None);
        let items: Vec<&ItemPatch> = read
            .updates
            .iter()
            .filter_map(|u| match &u.update {
                Open::Known(DriverUpdateKind::Item { item }) => Some(item),
                _ => None,
            })
            .collect();
        assert!(items.iter().any(|i| i.kind == Some(Open::Known(ItemKind::User))));
        assert!(
            items
                .iter()
                .any(|i| i.kind == Some(Open::Known(ItemKind::Assistant))
                    && i.set.as_ref().and_then(|s| s.get("text")) == Some(&json!("One note.")))
        );
        assert!(
            items
                .iter()
                .any(|i| i.id == "call-1" && i.set.as_ref().and_then(|s| s.get("output")) == Some(&json!("hello")))
        );
        let inner = items.iter().find(|i| i.id == "inner-1").expect("the nested call");
        assert_eq!(inner.parent.as_deref(), Some("call-1"));
        assert!(
            items
                .iter()
                .any(|i| i.kind == Some(Open::Known(ItemKind::Compaction)))
        );
        assert!(read.updates.iter().all(|u| u.binding.is_empty()));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
