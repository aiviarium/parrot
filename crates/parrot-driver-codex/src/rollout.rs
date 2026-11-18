//! Codex rollouts: the JSONL Codex writes for every thread, at
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl` (spec 9.5, 32.1).
//!
//! A rollout is the passive source beside the app-server, and the source of `history/discover`
//! and `history/read`. Its `event_msg` lines hold the core's events: `task_started`,
//! `item_completed` (items in the core's shape, PascalCase types and snake_case fields),
//! `token_count` and `task_complete`. [`notifications`] turns each into the app-server
//! notification it corresponds to, so the live mapping reads both and item ids agree.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parrot_proto::driver::{DriverUpdate, DriverUpdateKind, HistoryDiscoverResult, HistoryReadResult};
use parrot_proto::open::Open;
use parrot_proto::session::{DiscoveredSession, Ref, RefStrength, SessionDiscoverParams};
use serde_json::{Map, Value, json};

use crate::mapping::{MapState, rfc3339};
use crate::wire::ServerNotification;

/// Lines per `history/read` page.
pub const PAGE: usize = 500;

/// `$CODEX_HOME/sessions`, else `~/.codex/sessions`.
pub fn sessions_root() -> Option<PathBuf> {
    let base = match std::env::var_os("CODEX_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".codex"),
    };
    Some(base.join("sessions"))
}

fn camel(k: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in k.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn texts(content: &Value, kinds: &[&str]) -> String {
    content
        .as_array()
        .map(|c| {
            c.iter()
                .filter(|x| x["type"].as_str().is_some_and(|t| kinds.contains(&t)))
                .filter_map(|x| x["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// A rollout item (core shape) as the app-server's `ThreadItem`.
pub fn thread_item(it: &Value) -> Value {
    let ty = it["type"].as_str().unwrap_or_default();
    let mut out = Map::new();
    if let Some(o) = it.as_object() {
        for (k, v) in o {
            out.insert(camel(k), v.clone());
        }
    }
    let mut lower = ty.to_owned();
    if let Some(f) = lower.get_mut(0..1) {
        f.make_ascii_lowercase();
    }
    out.insert("type".into(), json!(lower));
    match ty {
        "UserMessage" => {
            let content: Vec<Value> = it["content"]
                .as_array()
                .map(|c| {
                    c.iter()
                        .map(|x| {
                            let mut x = x.clone();
                            if let Some(t) = x["type"].as_str() {
                                x["type"] = json!(t.to_ascii_lowercase());
                            }
                            x
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.insert("content".into(), Value::Array(content));
        }
        "AgentMessage" => {
            out.insert(
                "text".into(),
                json!(texts(&it["content"], &["Text", "text", "output_text"])),
            );
        }
        "Reasoning" => {
            out.insert(
                "summary".into(),
                it.get("summary_text").cloned().unwrap_or(json!([])),
            );
            out.insert(
                "content".into(),
                it.get("raw_content").cloned().unwrap_or(json!([])),
            );
        }
        "CommandExecution" => {
            let command = match &it["command"] {
                Value::Array(c) => {
                    let c: Vec<String> = c
                        .iter()
                        .map(|x| x.as_str().unwrap_or_default().to_owned())
                        .collect();
                    if c.len() == 3 && c[0].ends_with("sh") && c[1] == "-lc" {
                        format!("{} -lc {}", c[0], shell_quote(&c[2]))
                    } else {
                        c.join(" ")
                    }
                }
                v => json!(v).as_str().unwrap_or_default().to_owned(),
            };
            out.insert("command".into(), json!(command));
            out.insert("aggregatedOutput".into(), it["aggregated_output"].clone());
            out.insert("exitCode".into(), it["exit_code"].clone());
        }
        "PatchApply" | "FileChange" => {
            out.insert("type".into(), json!("fileChange"));
            let changes: Vec<Value> = match &it["changes"] {
                Value::Array(c) => c.clone(),
                Value::Object(m) => m
                    .iter()
                    .map(|(path, v)| {
                        json!({"path": path, "kind": v.get("kind").cloned().unwrap_or(json!({"type": "update"})),
                            "diff": v.get("unified_diff").or_else(|| v.get("diff")).cloned().unwrap_or(json!(""))})
                    })
                    .collect(),
                _ => vec![],
            };
            out.insert("changes".into(), Value::Array(changes));
        }
        _ => {}
    }
    Value::Object(out)
}

/// POSIX single quotes, the way the app-server shows a `bash -lc` command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// What a rollout line says, as app-server notifications (method, params), plus the thread id
/// when the line is `session_meta`.
pub fn notifications(line: &Value) -> (Vec<(String, Value)>, Option<String>) {
    let p = &line["payload"];
    if line["type"] == "session_meta" {
        return (vec![], p["id"].as_str().map(str::to_owned));
    }
    if line["type"] != "event_msg" {
        return (vec![], None);
    }
    let turn = p["turn_id"].clone();
    let n = |m: &str, v: Value| vec![(m.to_owned(), v)];
    let out = match p["type"].as_str() {
        Some("task_started") => n(
            "turn/started",
            json!({"turn": {"id": turn, "status": "inProgress", "items": []}}),
        ),
        Some("item_completed") => n(
            "item/completed",
            json!({"item": thread_item(&p["item"]), "turnId": turn, "threadId": p["thread_id"]}),
        ),
        Some("token_count") if p["info"].is_object() => {
            let i = &p["info"];
            let usage = |u: &Value| {
                json!({
                    "totalTokens": u["total_tokens"], "inputTokens": u["input_tokens"],
                    "cachedInputTokens": u["cached_input_tokens"], "outputTokens": u["output_tokens"],
                    "reasoningOutputTokens": u["reasoning_output_tokens"],
                })
            };
            n(
                "thread/tokenUsage/updated",
                json!({"turnId": turn, "tokenUsage": {"total": usage(&i["total_token_usage"]),
                    "last": usage(&i["last_token_usage"]), "modelContextWindow": i["model_context_window"]}}),
            )
        }
        Some("task_complete") => {
            let (status, error) = match p["error"].as_object() {
                Some(e) => (
                    "failed",
                    json!({"message": e.get("message").cloned().unwrap_or(json!("the turn failed")),
                        "codexErrorInfo": e.get("codex_error_info")}),
                ),
                None => ("completed", Value::Null),
            };
            n(
                "turn/completed",
                json!({"turn": {"id": turn, "status": status, "error": error, "items": []}}),
            )
        }
        Some("turn_aborted") => n(
            "turn/completed",
            json!({"turn": {"id": turn, "status": "interrupted", "items": []}}),
        ),
        _ => vec![],
    };
    (out, None)
}

/// The updates a rollout's text gives, through the live mapping, under `binding`.
pub fn updates(binding: &str, text: &str) -> Vec<DriverUpdate> {
    let mut state = MapState::new(binding);
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        out.extend(step(&mut state, &v));
    }
    out
}

fn step(state: &mut MapState, v: &Value) -> Vec<DriverUpdate> {
    let at = v["timestamp"].as_str().unwrap_or_default().to_owned();
    let (ns, thread) = notifications(v);
    let mut out = Vec::new();
    if let Some(id) = thread {
        state.thread_id = Some(id.clone());
        out.push(DriverUpdate {
            binding: state.binding.clone(),
            subagent: None,
            update: Open::Known(DriverUpdateKind::Ref {
                reference: Ref {
                    id,
                    strength: RefStrength::Strong,
                },
            }),
            extra: Map::new(),
        });
    }
    for (method, params) in ns {
        out.extend(state.step(&ServerNotification::parse(&method, &params), &at));
    }
    out
}

/// `history/read` of a rollout's text, from line `cursor`, with `binding: ""` (E4).
pub fn read(text: &str, cursor: Option<&str>) -> HistoryReadResult {
    let lines: Vec<&str> = text.lines().collect();
    let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    let end = (start + PAGE).min(lines.len());
    let mut state = MapState::new("");
    let mut updates = Vec::new();
    for (i, line) in lines.iter().enumerate().take(end) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let out = step(&mut state, &v);
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

fn rollouts(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 3 {
            rollouts(&p, out, depth + 1);
        } else if p.extension().is_some_and(|x| x == "jsonl")
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
        {
            out.push(p);
        }
    }
}

/// The thread id in a rollout's file name: the last 36 characters of its stem.
pub fn thread_of(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    (stem.len() > 36).then(|| stem[stem.len() - 36..].to_owned())
}

/// The working directory, the first prompt and the thread it forked from, from the start of a
/// rollout.
fn summary(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(path) else {
        return (None, None);
    };
    let (mut cwd, mut title) = (None, None);
    for line in std::io::BufReader::new(f).lines().take(400).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"] == "session_meta" {
            cwd = v["payload"]["cwd"].as_str().map(str::to_owned);
        }
        if title.is_none()
            && v["payload"]["type"] == "item_completed"
            && v["payload"]["item"]["type"] == "UserMessage"
        {
            let t = texts(&v["payload"]["item"]["content"], &["text", "Text"]);
            title = Some(t.chars().take(80).collect());
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }
    (cwd, title)
}

/// `history/discover` under `root`, newest first.
pub fn discover(root: &Path, p: &SessionDiscoverParams) -> HistoryDiscoverResult {
    let mut files = Vec::new();
    rollouts(root, &mut files, 0);
    let mut files: Vec<(SystemTime, PathBuf)> = files
        .into_iter()
        .map(|f| {
            let m = f
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (m, f)
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut found: Vec<DiscoveredSession> = Vec::new();
    for (m, f) in &files {
        let Some(id) = thread_of(f) else {
            continue;
        };
        let (cwd, title) = summary(f);
        if p.cwd.as_ref().is_some_and(|want| cwd.as_ref() != Some(want)) {
            continue;
        }
        let secs = m
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        found.push(DiscoveredSession {
            reference: Ref {
                id,
                strength: RefStrength::Strong,
            },
            title,
            cwd: cwd.unwrap_or_default(),
            updated: rfc3339(secs),
        });
    }
    let start: usize = p.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
    let limit = p.limit.unwrap_or(50).max(1) as usize;
    let next = (start + limit < found.len()).then(|| (start + limit).to_string());
    let found = found.into_iter().skip(start).take(limit).collect();
    HistoryDiscoverResult { found, next }
}

/// The rollout of thread `id` under `root`.
pub fn find(root: &Path, id: &str) -> Option<PathBuf> {
    if id.contains('/') || id.contains("..") || id.is_empty() {
        return None;
    }
    let mut files = Vec::new();
    rollouts(root, &mut files, 0);
    files.into_iter().find(|f| thread_of(f).as_deref() == Some(id))
}
