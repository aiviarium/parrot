//! Human-readable output of the client commands.

use std::collections::HashMap;

use serde_json::{Map, Value};

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn table(rows: Vec<Vec<String>>) -> String {
    let mut widths = vec![0; rows.iter().map(Vec::len).max().unwrap_or(0)];
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
                .collect();
            cells.join("  ").trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v[key].as_array().into_iter().flatten()
}

pub fn agents(r: &Value) -> String {
    let mut rows = vec![vec!["ID".into(), "NAME".into(), "USABLE".into(), "AUTH".into()]];
    for a in list(r, "agents") {
        rows.push(vec![
            s(&a["id"]).into(),
            s(&a["name"]).into(),
            if a["usable"] == true { "yes" } else { "no" }.into(),
            s(&a["auth"]).into(),
        ]);
    }
    table(rows)
}

pub fn sessions(r: &Value) -> String {
    let mut rows = vec![vec![
        "ID".into(),
        "STATE".into(),
        "AGENT".into(),
        "TITLE".into(),
        "UPDATED".into(),
    ]];
    for x in list(r, "sessions") {
        rows.push(vec![
            s(&x["id"]).into(),
            s(&x["state"]).into(),
            s(&x["agent"]).into(),
            s(&x["title"]).into(),
            s(&x["updated"]).into(),
        ]);
    }
    if rows.len() == 1 {
        return "no sessions".into();
    }
    table(rows)
}

fn choices(req: &Value) -> String {
    list(req, "choices")
        .map(|c| s(&c["id"]).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn requests(r: &Value) -> String {
    let mut rows = vec![vec![
        "ID".into(),
        "SESSION".into(),
        "KIND".into(),
        "TITLE".into(),
        "CHOICES".into(),
    ]];
    for q in list(r, "requests") {
        rows.push(vec![
            s(&q["id"]).into(),
            s(&q["session"]).into(),
            s(&q["kind"]).into(),
            s(&q["title"]).into(),
            choices(q),
        ]);
    }
    if rows.len() == 1 {
        return "no open requests".into();
    }
    table(rows)
}

pub fn options(r: &Value) -> String {
    let mut rows = vec![vec![
        "ID".into(),
        "VALUE".into(),
        "APPLIES".into(),
        "LABEL".into(),
    ]];
    for o in list(r, "options") {
        rows.push(vec![
            s(&o["id"]).into(),
            o["value"].to_string(),
            s(&o["applies"]).into(),
            s(&o["label"]).into(),
        ]);
    }
    let mut out = table(rows);
    if let Some(level) = r["level"].as_str() {
        out.push_str(&format!("\nlevel: {level}"));
    }
    out
}

/// One line for an item, or `None` while there is nothing worth showing yet.
fn item_line(item: &Map<String, Value>) -> String {
    let kind = item.get("kind").and_then(Value::as_str).unwrap_or("item");
    let field = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("");
    let body = match kind {
        "tool" => format!("{} {} [{}]", field("tool"), field("title"), field("status")),
        "command" => format!("$ {} [{}]", field("command"), field("status")),
        _ if !field("text").is_empty() => field("text").to_string(),
        _ => field("title").to_string(),
    };
    format!("{kind}: {body}")
}

/// Keeps the items a watch has seen, so patches print as whole items.
#[derive(Default)]
pub struct Watch {
    items: HashMap<String, Map<String, Value>>,
}

impl Watch {
    pub fn snapshot(&mut self, snap: &Value) -> Vec<String> {
        let mut out = vec![format!(
            "session {} ({})",
            s(&snap["session"]["id"]),
            s(&snap["session"]["state"])
        )];
        for item in list(snap, "items") {
            if let (Some(id), Some(obj)) = (item["id"].as_str(), item.as_object()) {
                self.items.insert(id.into(), obj.clone());
                out.push(item_line(obj));
            }
        }
        for q in list(snap, "requests") {
            out.push(request_line(q));
        }
        out
    }

    pub fn update(&mut self, u: &Value) -> Option<String> {
        match u["type"].as_str()? {
            "item" => {
                let patch = &u["item"];
                let id = patch["id"].as_str()?;
                let item = self.items.entry(id.into()).or_default();
                parrot_proto::patch::apply(item, patch).ok()?;
                // Text streams print once, when they end.
                if item.get("streaming") == Some(&Value::Bool(true)) || item.get("removed").is_some() {
                    return None;
                }
                let only_append = patch.get("append").is_some() && patch.get("streaming").is_none();
                (!only_append).then(|| item_line(item))
            }
            "state" => Some(format!("state: {}", s(&u["state"]))),
            "turn" => Some(format!(
                "turn {}: {}",
                s(&u["turn"]["id"]),
                s(&u["turn"]["status"])
            )),
            "request" => Some(request_line(&u["request"])),
            "queue" => Some(format!(
                "queue: {} waiting{}",
                u["queue"]["entries"].as_array().map_or(0, Vec::len),
                if u["queue"]["paused"] == true {
                    ", paused"
                } else {
                    ""
                }
            )),
            "notice" => Some(format!("{}: {}", s(&u["level"]), s(&u["text"]))),
            "options" | "actions" | "usage" | "session" => None,
            other => Some(format!("{other}: {u}")),
        }
    }
}

fn request_line(q: &Value) -> String {
    format!(
        "request {} ({}, {}): {} [{}]",
        s(&q["id"]),
        s(&q["kind"]),
        s(&q["status"]),
        s(&q["title"]),
        choices(q)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streamed_text_prints_once() {
        let mut w = Watch::default();
        let start = json!({"type": "item", "item": {"id": "a", "kind": "assistant", "set": {"text": ""}, "streaming": true}});
        assert_eq!(w.update(&start), None);
        assert_eq!(
            w.update(&json!({"type": "item", "item": {"id": "a", "append": {"text": "Hi"}}})),
            None
        );
        let end = json!({"type": "item", "item": {"id": "a", "streaming": false}});
        assert_eq!(w.update(&end).as_deref(), Some("assistant: Hi"));
    }

    #[test]
    fn tables_align() {
        let out = agents(
            &json!({"agents": [{"id": "claude", "name": "Claude Code", "usable": true, "auth": "unknown"}]}),
        );
        assert_eq!(out.lines().nth(1), Some("claude  Claude Code  yes     unknown"));
    }
}
