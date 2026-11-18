//! Claude Code's session transcript as a passive source (spec 32.1).
//!
//! Claude Code writes `<config>/projects/<folder>/<session id>.jsonl`, one message per line.
//! `assistant` lines carry one content block each (text, thinking, tool_use), `user` lines the
//! prompt (a string or text blocks) or tool results. A turn ends with an assistant line whose
//! `stop_reason` is `end_turn` or `stop_sequence`, or with the `system` line written after one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use parrot_proto::item::{ItemKind, ItemPatch, ItemSource};
use parrot_proto::open::Open;
use serde_json::{Map, Value, json};

/// What one transcript line said.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TranscriptEvent {
    /// Items without a turn; the driver fills in the turn it belongs to.
    pub items: Vec<ItemPatch>,
    /// The line ended a turn.
    pub turn_done: bool,
    /// The harness's session id, from any line that carries it.
    pub session: Option<String>,
}

/// Reads transcript lines into items. It keeps the tool calls it has seen and the prompts the
/// driver typed, so their echoes are not reported a second time.
#[derive(Debug, Default)]
pub struct ClaudeTranscript {
    tools: HashSet<String>,
    echoes: Vec<String>,
    fresh: u64,
}

impl ClaudeTranscript {
    pub fn new() -> Self {
        Self::default()
    }

    /// A prompt the driver typed; its echo in the transcript is dropped.
    pub fn expect_echo(&mut self, text: &str) {
        self.echoes.push(text.trim().to_owned());
    }

    pub fn line(&mut self, text: &str) -> TranscriptEvent {
        let mut ev = TranscriptEvent::default();
        let Ok(m) = serde_json::from_str::<Value>(text.trim()) else {
            return ev;
        };
        if m.get("isSidechain") == Some(&Value::Bool(true)) || m.get("isMeta") == Some(&Value::Bool(true)) {
            return ev;
        }
        ev.session = m.get("sessionId").and_then(Value::as_str).map(str::to_owned);
        let uuid = m.get("uuid").and_then(Value::as_str).map(str::to_owned);
        match m.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let msg = &m["message"];
                let base = uuid
                    .or_else(|| msg.get("id").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| self.fresh("assistant"));
                match &msg["content"] {
                    Value::String(s) => {
                        ev.items
                            .push(item(format!("{base}:0"), ItemKind::Assistant, text_set(s)))
                    }
                    Value::Array(blocks) => {
                        for (i, b) in blocks.iter().enumerate() {
                            let key = format!("{base}:{i}");
                            match b.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    ev.items
                                        .push(item(key, ItemKind::Assistant, text_set(str_of(b, "text"))))
                                }
                                Some("thinking") => ev.items.push(item(
                                    key,
                                    ItemKind::Reasoning,
                                    text_set(str_of(b, "thinking")),
                                )),
                                Some("tool_use") => {
                                    let id = str_of(b, "id").to_owned();
                                    self.tools.insert(id.clone());
                                    let mut set = Map::new();
                                    set.insert("tool".into(), json!(str_of(b, "name")));
                                    set.insert("input".into(), b.get("input").cloned().unwrap_or(json!({})));
                                    set.insert("status".into(), json!("running"));
                                    ev.items.push(item(id, ItemKind::Tool, set));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                let stop = msg.get("stop_reason").and_then(Value::as_str);
                ev.turn_done = matches!(stop, Some("end_turn" | "stop_sequence"));
            }
            Some("user") => {
                let content = &m["message"]["content"];
                let text = match content {
                    Value::String(s) => s.clone(),
                    Value::Array(blocks) => {
                        for b in blocks {
                            if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                                continue;
                            }
                            let id = str_of(b, "tool_use_id").to_owned();
                            let mut set = Map::new();
                            let error = b.get("is_error") == Some(&Value::Bool(true));
                            set.insert("status".into(), json!(if error { "error" } else { "ok" }));
                            if let Some(c) = b.get("content") {
                                set.insert("output".into(), c.clone());
                            }
                            let mut p = item(id, ItemKind::Tool, set);
                            p.kind = None;
                            ev.items.push(p);
                        }
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                            .map(|b| str_of(b, "text"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    _ => String::new(),
                };
                if let Some(p) = self.user(&text, uuid) {
                    ev.items.push(p);
                }
            }
            Some("system") => {
                let sub = m.get("subtype").and_then(Value::as_str);
                ev.turn_done = matches!(sub, Some("turn_duration" | "stop_hook_summary"));
            }
            _ => {}
        }
        ev
    }

    fn user(&mut self, text: &str, uuid: Option<String>) -> Option<ItemPatch> {
        let t = text.trim();
        // Interruptions, slash command echoes and hook output are the CLI's, not the person's.
        if t.is_empty() || t.starts_with("[Request interrupted") || t.starts_with('<') {
            return None;
        }
        if let Some(pos) = self.echoes.iter().position(|e| e == t) {
            self.echoes.remove(pos);
            return None;
        }
        let key = uuid.unwrap_or_else(|| self.fresh("user"));
        let mut set = Map::new();
        set.insert("text".into(), json!(text));
        set.insert("status".into(), json!("handled"));
        Some(item(key, ItemKind::User, set))
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("transcript-{prefix}-{}", self.fresh)
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn text_set(text: &str) -> Map<String, Value> {
    let mut set = Map::new();
    set.insert("text".into(), json!(text));
    set
}

fn item(id: String, kind: ItemKind, set: Map<String, Value>) -> ItemPatch {
    ItemPatch {
        id,
        kind: Some(Open::Known(kind)),
        source: Some(ItemSource::Transcript),
        set: Some(set),
        streaming: Some(false),
        ..Default::default()
    }
}

/// The folder Claude Code keeps a working directory's transcripts in: every character of the
/// path that is not a letter or digit becomes `-`, under `$CLAUDE_CONFIG_DIR/projects` or
/// `~/.claude/projects`.
pub fn claude_project_dir(cwd: &Path) -> Option<PathBuf> {
    let base = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".claude"),
    };
    let folder: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(base.join("projects").join(folder))
}

/// Follows a transcript file as it grows, a line at a time.
pub struct Tail {
    dir: PathBuf,
    /// The file, once known: given by a resume ref, or the newest one written after `since`.
    file: Option<PathBuf>,
    since: SystemTime,
    offset: u64,
    partial: String,
}

/// How often a tail looks for new lines.
pub const POLL: Duration = Duration::from_millis(200);

impl Tail {
    pub fn new(dir: PathBuf, file: Option<PathBuf>, since: SystemTime) -> Self {
        Tail {
            dir,
            file,
            since,
            offset: 0,
            partial: String::new(),
        }
    }

    /// The complete lines written since the last call.
    pub fn poll(&mut self) -> Vec<String> {
        if self.file.is_none() {
            self.file = newest_jsonl(&self.dir, self.since);
        }
        let Some(file) = &self.file else { return vec![] };
        let Ok(bytes) = std::fs::read(file) else {
            return vec![];
        };
        if (bytes.len() as u64) <= self.offset {
            return vec![];
        }
        self.partial
            .push_str(&String::from_utf8_lossy(&bytes[self.offset as usize..]));
        self.offset = bytes.len() as u64;
        let mut lines: Vec<String> = self.partial.split('\n').map(str::to_owned).collect();
        self.partial = lines.pop().unwrap_or_default();
        lines.retain(|l| !l.trim().is_empty());
        lines
    }
}

fn newest_jsonl(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    // Clocks of file systems are coarse; a file written just before the start still counts.
    let since = since.checked_sub(Duration::from_secs(2)).unwrap_or(since);
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: &str = include_str!("../fixtures/transcript.jsonl");

    #[test]
    fn a_turn_reads_as_items() {
        let mut t = ClaudeTranscript::new();
        t.expect_echo("list the files");
        let events: Vec<_> = LINES.lines().map(|l| t.line(l)).collect();
        let items: Vec<_> = events.iter().flat_map(|e| e.items.clone()).collect();
        let kinds: Vec<_> = items
            .iter()
            .map(|i| match &i.kind {
                Some(Open::Known(k)) => format!("{k:?}"),
                _ => "patch".into(),
            })
            .collect();
        // The echoed prompt is dropped; the meta line and the sidechain line are skipped.
        assert_eq!(kinds, ["Reasoning", "Tool", "patch", "Assistant", "User"]);
        assert_eq!(items[1].id, "toolu_fake_1");
        assert_eq!(items[2].id, "toolu_fake_1");
        assert_eq!(items[2].set.as_ref().unwrap()["status"], "ok");
        assert_eq!(
            items[3].set.as_ref().unwrap()["text"],
            "Two files: a.txt and b.txt."
        );
        assert_eq!(items[4].set.as_ref().unwrap()["status"], "handled");
        assert!(events.iter().any(|e| e.turn_done));
        assert_eq!(
            events[0].session.as_deref(),
            Some("0b0e5b9e-fake-4c1a-9d1e-000000000001")
        );
    }

    #[test]
    fn a_tail_returns_whole_lines_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\"").unwrap();
        let mut tail = Tail::new(dir.path().into(), None, SystemTime::now());
        assert_eq!(tail.poll(), ["{\"a\":1}"]);
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        assert_eq!(tail.poll(), ["{\"b\":2}"]);
        assert!(tail.poll().is_empty());
    }

    #[test]
    fn the_project_folder_replaces_punctuation() {
        let d = claude_project_dir(Path::new("/srv/fake.repo/x_y")).unwrap();
        assert!(d.ends_with("projects/-srv-fake-repo-x-y"), "{d:?}");
    }
}
