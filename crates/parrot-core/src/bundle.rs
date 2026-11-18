//! Context bundles (spec 23.2, Appendix D): a session's history, cut and trimmed, rendered as
//! Markdown for the first prompt of another binding.

use crate::State;
use serde_json::{Value, json};

/// The history kept when the target's context window is unknown: 100,000 tokens with half left
/// for work, at about four characters a token.
pub const DEFAULT_CHARS: usize = 100_000 / 2 * 4;

/// Items of a session oldest first, cut after the last item of turn `at`.
fn items_until(state: &State, session: &str, at: Option<&str>) -> Vec<Value> {
    let Some(items) = state.items.by_session.get(session) else {
        return Vec::new();
    };
    let all: Vec<Value> = items
        .order
        .iter()
        .filter_map(|id| items.by_id.get(id).map(|m| Value::Object(m.clone())))
        .filter(|v| v.get("removed").and_then(Value::as_bool) != Some(true))
        .collect();
    let Some(at) = at else {
        return all;
    };
    let turns = state.seeds.turns.get(session).cloned().unwrap_or_default();
    let keep: Vec<&String> = match turns.iter().position(|t| t == at) {
        Some(i) => turns[..=i].iter().collect(),
        None => return all,
    };
    let last = all
        .iter()
        .rposition(|v| v.get("turn").and_then(Value::as_str) == Some(at))
        .unwrap_or(0);
    all.into_iter()
        .enumerate()
        .filter(|(i, v)| {
            *i <= last
                && v.get("turn")
                    .and_then(Value::as_str)
                    .is_none_or(|t| keep.iter().any(|k| *k == t))
        })
        .map(|(_, v)| v)
        .collect()
}

/// The rendered size of one item, to trim by.
fn size(v: &Value) -> usize {
    render_item(v).map_or(0, |s| s.len() + 1)
}

/// Keep the first user message and the newest items that fit in `chars` (spec 23.2).
fn trim(items: Vec<Value>, chars: usize) -> Vec<Value> {
    let total: usize = items.iter().map(size).sum();
    if total <= chars {
        return items;
    }
    let first = items
        .iter()
        .position(|v| v.get("kind").and_then(Value::as_str) == Some("user"));
    let mut budget = chars.saturating_sub(first.map_or(0, |i| size(&items[i])));
    let mut keep = vec![false; items.len()];
    if let Some(i) = first {
        keep[i] = true;
    }
    for i in (0..items.len()).rev() {
        if keep[i] {
            continue;
        }
        let s = size(&items[i]);
        if s > budget {
            break;
        }
        budget -= s;
        keep[i] = true;
    }
    items
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(v, _)| v)
        .collect()
}

/// Build the `ContextBundle` of a session up to turn `at`.
pub fn build(state: &State, session: &str, at: Option<&str>, chars: usize) -> Value {
    let binding = crate::directory::binding_of(state, session).unwrap_or_default();
    let agent = state
        .sessions
        .by_id
        .get(session)
        .map(|r| r.agent.clone())
        .unwrap_or_default();
    let all = items_until(state, session, at);
    let plan = all
        .iter()
        .rev()
        .find(|v| v.get("kind").and_then(Value::as_str) == Some("plan"))
        .cloned();
    let items: Vec<Value> = all
        .into_iter()
        .filter(|v| !matches!(v.get("kind").and_then(Value::as_str), Some("reasoning" | "plan")))
        .collect();
    let items = trim(items, chars);
    let mut from =
        json!({ "session": session, "binding": binding, "agent": agent, "env": state.principals.env });
    if let Some(at) = at {
        from["at"] = json!(at);
    }
    let mut b = json!({ "version": 1, "from": from, "items": items });
    if let Some(info) = state.sessions.info.get(session)
        && let Some(t) = &info.title
    {
        b["title"] = json!(t);
    }
    if let Some(p) = plan {
        b["plan"] = p;
    }
    if let Some(info) = state.sessions.info.get(session)
        && let Some(meta) = state.tree.workspaces.get(&info.workspace)
        && meta.kind == "worktree"
    {
        b["workspace"] = json!({ "repo": meta.repo, "branch": meta.branch });
    }
    if let Some(l) = crate::directory::binding_of(state, session)
        .and_then(|b| state.bindings.info.get(&b))
        .and_then(|b| b.level)
    {
        b["level"] = json!(l);
    }
    b
}

fn text_of(v: &Value) -> Option<&str> {
    v.get("text").and_then(Value::as_str).filter(|t| !t.is_empty())
}

/// One item as a history line, or `None` for items the history leaves out.
fn render_item(v: &Value) -> Option<String> {
    let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "user" => text_of(v).map(|t| format!("**User:** {t}")),
        "assistant" => text_of(v).map(|t| format!("**Assistant:** {t}")),
        "reasoning" | "plan" | "subagent" | "handoff" => None,
        "file_change" => {
            let paths: Vec<&str> = v
                .get("changes")
                .and_then(Value::as_array)
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| c.get("path").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            (!paths.is_empty()).then(|| format!("- Changed files: {}", paths.join(", ")))
        }
        _ => {
            let title = v
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| v.get("command").and_then(Value::as_str))
                .or_else(|| v.get("tool").and_then(Value::as_str))
                .or_else(|| text_of(v))?;
            let status = v.get("status").and_then(Value::as_str).unwrap_or("ok");
            let outcome = match status {
                "error" | "failed" => {
                    let msg = v
                        .get("error")
                        .and_then(|e| e.as_str().or_else(|| e.get("message").and_then(Value::as_str)))
                        .or_else(|| v.get("message").and_then(Value::as_str))
                        .unwrap_or("failed");
                    format!("error: {msg}")
                }
                other => other.to_string(),
            };
            Some(format!("- Tool {title}: {outcome}"))
        }
    }
}

/// Render a bundle as Appendix D, with `next` as the brief or prompt that follows.
pub fn render(bundle: &Value, next: Option<&str>) -> String {
    let mut out = String::from(
        "# Continuing an earlier session\n\nThis continues work from another agent session. Its history follows.\n",
    );
    if let Some(s) = bundle
        .get("summary")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("\n## Summary\n{s}\n"));
    }
    let lines: Vec<String> = bundle
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(render_item).collect())
        .unwrap_or_default();
    if !lines.is_empty() {
        out.push_str(&format!("\n## History\n{}\n", lines.join("\n")));
    }
    let entries: Vec<String> = bundle
        .pointer("/plan/entries")
        .and_then(Value::as_array)
        .map(|es| {
            es.iter()
                .filter_map(|e| {
                    let text = e.get("text").and_then(Value::as_str)?;
                    let done = e.get("status").and_then(Value::as_str) == Some("done");
                    Some(format!("- [{}] {text}", if done { "x" } else { " " }))
                })
                .collect()
        })
        .unwrap_or_default();
    if !entries.is_empty() {
        out.push_str(&format!("\n## Open plan\n{}\n", entries.join("\n")));
    }
    if let Some(w) = bundle.get("workspace") {
        let mut parts = Vec::new();
        if let Some(r) = w.get("repo").and_then(Value::as_str) {
            parts.push(format!("Repository {r}"));
        }
        if let Some(b) = w.get("branch").and_then(Value::as_str) {
            parts.push(format!("branch {b}"));
        }
        if let Some(c) = w.get("commit").and_then(Value::as_str) {
            parts.push(format!("at {c}"));
        }
        if !parts.is_empty() {
            let mut line = format!("{}.", parts.join(", "));
            if let Some(ch) = w
                .get("changed")
                .and_then(Value::as_array)
                .filter(|c| !c.is_empty())
            {
                let ch: Vec<&str> = ch.iter().filter_map(Value::as_str).collect();
                line.push_str(&format!(" Changed since the history started: {}.", ch.join(", ")));
            }
            out.push_str(&format!("\n## Workspace\n{line}\n"));
        }
    }
    if let Some(n) = next.filter(|n| !n.is_empty()) {
        out.push_str(&format!("\n## Next\n{n}\n"));
    }
    out
}
