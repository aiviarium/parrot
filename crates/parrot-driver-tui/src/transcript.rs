//! A harness's own transcript as a passive source (spec 32.1): Claude Code's project
//! transcripts and Codex's rollout files.
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
    /// Token counts of the latest model request, as the turn's `usage`.
    pub usage: Option<Value>,
    /// Prompts the driver typed, as the transcript recorded them. They are not reported again,
    /// but shadow mode compares them with what the driver sent.
    pub echoed: Vec<ItemPatch>,
}

/// A transcript reader for one format.
#[derive(Debug)]
pub enum Reader {
    Claude(ClaudeTranscript),
    Codex(CodexTranscript),
}

impl Default for Reader {
    fn default() -> Self {
        Reader::Claude(ClaudeTranscript::new())
    }
}

impl Reader {
    pub fn expect_echo(&mut self, text: &str) {
        match self {
            Reader::Claude(t) => t.expect_echo(text),
            Reader::Codex(t) => t.expect_echo(text),
        }
    }

    pub fn line(&mut self, text: &str) -> TranscriptEvent {
        match self {
            Reader::Claude(t) => t.line(text),
            Reader::Codex(t) => t.line(text),
        }
    }
}

/// Reads Codex rollout lines (`sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`). Each line is
/// `{timestamp, type, payload}`: `session_meta` names the thread, and `event_msg` lines carry
/// finished items (`item_completed`), token counts and the end of a task.
#[derive(Debug, Default)]
pub struct CodexTranscript {
    echoes: Vec<String>,
    echoed: Vec<ItemPatch>,
    fresh: u64,
}

impl CodexTranscript {
    pub fn new() -> Self {
        Self::default()
    }

    /// A prompt the driver typed; its echo in the rollout is dropped.
    pub fn expect_echo(&mut self, text: &str) {
        self.echoes.push(text.trim().to_owned());
    }

    pub fn line(&mut self, text: &str) -> TranscriptEvent {
        let mut ev = TranscriptEvent::default();
        let Ok(m) = serde_json::from_str::<Value>(text.trim()) else {
            return ev;
        };
        let p = &m["payload"];
        match (str_of(&m, "type"), str_of(p, "type")) {
            ("session_meta", _) => ev.session = p.get("id").and_then(Value::as_str).map(str::to_owned),
            ("event_msg", "item_completed") => {
                ev.items.extend(self.item(&p["item"]));
                ev.echoed.append(&mut self.echoed);
            }
            ("event_msg", "task_complete" | "turn_aborted") => ev.turn_done = true,
            ("event_msg", "token_count") => {
                if let Some(u) = p.pointer("/info/last_token_usage") {
                    let cached = u["cached_input_tokens"].as_u64().unwrap_or(0);
                    let input = u["input_tokens"].as_u64().unwrap_or(0);
                    ev.usage = Some(json!({
                        "input": input.saturating_sub(cached),
                        "output": u["output_tokens"].as_u64().unwrap_or(0),
                        "cacheRead": cached,
                    }));
                }
            }
            _ => {}
        }
        ev
    }

    fn item(&mut self, i: &Value) -> Option<ItemPatch> {
        let id = i.get("id").and_then(Value::as_str).map(str::to_owned);
        let text = || {
            i["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        };
        match str_of(i, "type") {
            "UserMessage" => {
                let t = text();
                let mut set = text_set(&t);
                set.insert("status".into(), json!("handled"));
                let user = item(id.unwrap_or_else(|| self.fresh("user")), ItemKind::User, set);
                if let Some(pos) = self.echoes.iter().position(|e| *e == t.trim()) {
                    self.echoes.remove(pos);
                    self.echoed.push(user);
                    return None;
                }
                Some(user)
            }
            "AgentMessage" => Some(item(
                id.unwrap_or_else(|| self.fresh("assistant")),
                ItemKind::Assistant,
                text_set(&text()),
            )),
            "Reasoning" => {
                let summary = i["summary_text"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                (!summary.is_empty()).then(|| {
                    item(
                        id.unwrap_or_else(|| self.fresh("reasoning")),
                        ItemKind::Reasoning,
                        text_set(&summary),
                    )
                })
            }
            "CommandExecution" => {
                let command = match &i["command"] {
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    v => v.as_str().unwrap_or_default().to_owned(),
                };
                let exit = i["exit_code"].as_i64();
                let mut set = Map::new();
                set.insert("command".into(), json!(command));
                set.insert("output".into(), json!(str_of(i, "aggregated_output")));
                set.insert(
                    "status".into(),
                    json!(if exit.is_some_and(|c| c != 0) {
                        "error"
                    } else {
                        "ok"
                    }),
                );
                if let Some(c) = exit {
                    set.insert("exit".into(), json!(c));
                }
                Some(item(
                    id.unwrap_or_else(|| self.fresh("command")),
                    ItemKind::Command,
                    set,
                ))
            }
            _ => None,
        }
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("transcript-{prefix}-{}", self.fresh)
    }
}

/// Codex's folder of rollouts: `$CODEX_HOME/sessions` or `~/.codex/sessions`.
pub fn codex_sessions_dir() -> Option<PathBuf> {
    let home = match std::env::var_os("CODEX_HOME") {
        Some(d) => PathBuf::from(d),
        None => parrot_driver::paths::home_dir()?.join(".codex"),
    };
    Some(home.join("sessions"))
}

/// The rollout of thread `id` under `root`, whose name ends in `-<id>.jsonl`.
pub fn codex_rollout(root: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    jsonl_files(root, 3).into_iter().find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
    })
}

/// Reads transcript lines into items. It keeps the tool calls it has seen and the prompts the
/// driver typed, so their echoes are not reported a second time.
#[derive(Debug, Default)]
pub struct ClaudeTranscript {
    tools: HashSet<String>,
    echoes: Vec<String>,
    echoed: Vec<ItemPatch>,
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
                if let Some(u) = msg.get("usage").filter(|u| u.is_object()) {
                    ev.usage = Some(json!({
                        "input": u["input_tokens"].as_u64().unwrap_or(0),
                        "output": u["output_tokens"].as_u64().unwrap_or(0),
                        "cacheRead": u["cache_read_input_tokens"].as_u64().unwrap_or(0),
                        "cacheWrite": u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
                    }));
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
                ev.echoed.append(&mut self.echoed);
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
        // Attachments the driver pasted first show up ahead of the prompt's text.
        if let Some(pos) = self
            .echoes
            .iter()
            .position(|e| e == t || t.ends_with(&format!(" {e}")))
        {
            self.echoes.remove(pos);
            let key = uuid.unwrap_or_else(|| self.fresh("user"));
            self.echoed.push(item(key, ItemKind::User, text_set(t)));
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
        None => parrot_driver::paths::home_dir()?.join(".claude"),
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
    /// How many folder levels below `dir` hold transcripts; Codex keeps them by date.
    depth: usize,
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
            depth: 0,
            file,
            since,
            offset: 0,
            partial: String::new(),
        }
    }

    /// Looks for the newest file in folders up to `depth` levels below the tail's folder.
    pub fn nested(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// The complete lines written since the last call.
    pub fn poll(&mut self) -> Vec<String> {
        if self.file.is_none() {
            self.file = newest_jsonl(&self.dir, self.depth, self.since);
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

fn newest_jsonl(dir: &Path, depth: usize, since: SystemTime) -> Option<PathBuf> {
    // Clocks of file systems are coarse; a file written just before the start still counts.
    let since = since.checked_sub(Duration::from_secs(2)).unwrap_or(since);
    jsonl_files(dir, depth)
        .into_iter()
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

/// The `.jsonl` files in `dir` and in folders up to `depth` levels below it.
fn jsonl_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                out.extend(jsonl_files(&p, depth - 1));
            }
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: &str = include_str!("../fixtures/transcript.jsonl");

    const ROLLOUT: &str = r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","payload":{"id":"thr-fake-0001","cwd":"/fake/project","cli_version":"0.167.1"}}
{"timestamp":"2026-01-02T03:04:05.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"timestamp":"2026-01-02T03:04:05.000Z","type":"event_msg","payload":{"type":"item_completed","thread_id":"thr-fake-0001","turn_id":"turn-1","item":{"type":"UserMessage","id":"u-1","content":[{"type":"text","text":"list the files"}]}}}
{"timestamp":"2026-01-02T03:04:06.000Z","type":"event_msg","payload":{"type":"item_completed","thread_id":"thr-fake-0001","turn_id":"turn-1","item":{"type":"CommandExecution","id":"exec-1","command":["/bin/bash","-lc","ls"],"aggregated_output":"a.txt\n","exit_code":0,"status":"completed"}}}
{"timestamp":"2026-01-02T03:04:06.000Z","type":"event_msg","payload":{"type":"token_count","turn_id":"turn-1","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":7}}}}
{"timestamp":"2026-01-02T03:04:07.000Z","type":"event_msg","payload":{"type":"item_completed","thread_id":"thr-fake-0001","turn_id":"turn-1","item":{"type":"AgentMessage","id":"msg-1","content":[{"type":"Text","text":"One file."}]}}}
{"timestamp":"2026-01-02T03:04:07.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","error":null}}
"#;

    #[test]
    fn a_codex_rollout_reads_as_items() {
        let mut t = CodexTranscript::new();
        t.expect_echo("list the files");
        let events: Vec<_> = ROLLOUT.lines().map(|l| t.line(l)).collect();
        assert_eq!(events[0].session.as_deref(), Some("thr-fake-0001"));
        let items: Vec<_> = events.iter().flat_map(|e| e.items.clone()).collect();
        // The echoed prompt is dropped.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "exec-1");
        assert_eq!(items[0].set.as_ref().unwrap()["command"], "/bin/bash -lc ls");
        assert_eq!(items[0].set.as_ref().unwrap()["exit"], 0);
        assert_eq!(items[1].set.as_ref().unwrap()["text"], "One file.");
        assert_eq!(events[4].usage.as_ref().unwrap()["input"], 60);
        assert_eq!(events[4].usage.as_ref().unwrap()["cacheRead"], 40);
        assert!(events.last().unwrap().turn_done);
    }

    #[test]
    fn a_nested_tail_finds_a_rollout_by_date_and_id() {
        let root = tempfile::tempdir().unwrap();
        let day = root.path().join("2026/01/02");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-01-02T03-04-05-thr-fake-0001.jsonl");
        std::fs::write(&file, ROLLOUT).unwrap();
        assert_eq!(codex_rollout(root.path(), "thr-fake-0001"), Some(file.clone()));
        let since = SystemTime::now() - Duration::from_secs(60);
        assert!(Tail::new(root.path().into(), None, since).poll().is_empty());
        let mut tail = Tail::new(root.path().into(), None, since).nested(3);
        assert_eq!(tail.poll().len(), ROLLOUT.lines().count());
    }

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
