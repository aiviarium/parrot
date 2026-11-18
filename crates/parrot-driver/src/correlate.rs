//! Items from two sources of one binding, matched and compared by `correlate.kdl` (spec 32.1).
//!
//! A [`Fold`] collects the item patches of one source into whole items. [`correlate`] pairs the
//! items of a control source with those of a passive one, by harness id (`ref`), by turn and
//! order (`position`) or by text, in the order the kind's `match` names. [`resolve`] picks the
//! winning item of each pair, and [`differences`] lists what shadow mode reports.

use std::collections::{BTreeMap, HashMap};

use parrot_proto::driver_data::CorrelateFile;
use serde_json::{Map, Value, json};

/// The notice code for a difference found in shadow mode.
pub const SHADOW_CODE: &str = "x.parrot/shadow";

/// One item, with every patch applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    /// The ordinal of its turn in the source, from 0.
    pub turn: Option<usize>,
    pub set: Map<String, Value>,
}

/// The items of one source, in the order they first appeared.
#[derive(Debug, Clone, Default)]
pub struct Fold {
    pub items: Vec<Item>,
    index: HashMap<String, usize>,
    turns: Vec<String>,
}

impl Fold {
    pub fn new() -> Self {
        Fold::default()
    }

    /// Applies one driver update (`{ binding, update }` as JSON). Only items and turns count.
    pub fn push(&mut self, update: &Value) {
        let u = update.get("update").unwrap_or(update);
        match u["type"].as_str() {
            Some("turn") => {
                if let Some(id) = u["turn"]["id"].as_str()
                    && !self.turns.iter().any(|t| t == id)
                {
                    self.turns.push(id.to_owned());
                }
            }
            Some("item") => self.patch(&u["item"]),
            _ => {}
        }
    }

    pub fn extend<'a>(&mut self, updates: impl IntoIterator<Item = &'a Value>) {
        for u in updates {
            self.push(u);
        }
    }

    fn patch(&mut self, p: &Value) {
        let Some(id) = p["id"].as_str() else {
            return;
        };
        let turn = p["turn"]
            .as_str()
            .map(|t| match self.turns.iter().position(|x| x == t) {
                Some(i) => i,
                None => {
                    self.turns.push(t.to_owned());
                    self.turns.len() - 1
                }
            });
        let i = match self.index.get(id) {
            Some(i) => *i,
            None => {
                self.items.push(Item {
                    id: id.to_owned(),
                    kind: String::new(),
                    turn: None,
                    set: Map::new(),
                });
                self.index.insert(id.to_owned(), self.items.len() - 1);
                self.items.len() - 1
            }
        };
        let item = &mut self.items[i];
        if let Some(k) = p["kind"].as_str() {
            item.kind = k.to_owned();
        }
        if turn.is_some() {
            item.turn = turn;
        }
        if let Some(set) = p["set"].as_object() {
            for (k, v) in set {
                item.set.insert(k.clone(), v.clone());
            }
        }
        if let Some(append) = p["append"].as_object() {
            for (k, v) in append {
                let add = v.as_str().unwrap_or_default();
                let old = item.set.get(k).and_then(Value::as_str).unwrap_or_default();
                item.set.insert(k.clone(), json!(format!("{old}{add}")));
            }
        }
    }

    /// The number of turns seen.
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// The items of the turn with ordinal `turn`.
    pub fn turn(&self, turn: usize) -> Vec<&Item> {
        self.items.iter().filter(|i| i.turn == Some(turn)).collect()
    }

    /// The ordinal of the turn with id `id`.
    pub fn turn_index(&self, id: &str) -> Option<usize> {
        self.turns.iter().position(|t| t == id)
    }

    /// The id of the last turn.
    pub fn last_turn_id(&self) -> Option<&str> {
        self.turns.last().map(String::as_str)
    }

    /// The items of the turn with id `id`, else of the last turn.
    pub fn turn_or_last(&self, id: Option<&str>) -> Vec<&Item> {
        match id.and_then(|id| self.turn_index(id)) {
            Some(i) => self.turn(i),
            None => self.last_turn(),
        }
    }

    /// The items of the last turn.
    pub fn last_turn(&self) -> Vec<&Item> {
        match self.turns.len() {
            0 => self.items.iter().collect(),
            n => self.turn(n - 1),
        }
    }
}

/// A pair from the two sources; either side may be missing.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair<'a> {
    pub control: Option<&'a Item>,
    pub passive: Option<&'a Item>,
}

fn position(items: &[&Item], item: &Item) -> (Option<usize>, usize) {
    let n = items
        .iter()
        .filter(|i| i.kind == item.kind && i.turn == item.turn)
        .position(|i| i.id == item.id)
        .unwrap_or(0);
    (item.turn, n)
}

fn text(item: &Item) -> Option<&str> {
    item.set.get("text").and_then(Value::as_str).map(str::trim)
}

/// Pairs the items of `control` with those of `passive`. Turn ordinals are compared relative to
/// each list's first turn, so a transcript holding earlier turns still lines up.
pub fn correlate<'a>(data: &CorrelateFile, control: &[&'a Item], passive: &[&'a Item]) -> Vec<Pair<'a>> {
    let base = |items: &[&Item]| items.iter().filter_map(|i| i.turn).min().unwrap_or(0);
    let (cb, pb) = (base(control), base(passive));
    let rel = |t: Option<usize>, b: usize| t.map(|t| t.saturating_sub(b));
    let mut used = vec![false; passive.len()];
    let mut out = Vec::new();
    for c in control {
        let keys = data
            .kind(&c.kind)
            .map(|k| k.keys())
            .unwrap_or_else(|| vec!["ref".into(), "position".into()]);
        let mut found = None;
        for key in &keys {
            found = passive.iter().enumerate().position(|(i, p)| {
                !used[i]
                    && p.kind == c.kind
                    && match key.as_str() {
                        "ref" => p.id == c.id,
                        "position" => {
                            let (ct, cn) = position(control, c);
                            let (pt, pn) = position(passive, p);
                            rel(ct, cb) == rel(pt, pb) && cn == pn
                        }
                        "text" => text(c).is_some() && text(c) == text(p),
                        _ => false,
                    }
            });
            if found.is_some() {
                break;
            }
        }
        if let Some(i) = found {
            used[i] = true;
        }
        out.push(Pair {
            control: Some(c),
            passive: found.map(|i| passive[i]),
        });
    }
    for (i, p) in passive.iter().enumerate() {
        if !used[i] {
            out.push(Pair {
                control: None,
                passive: Some(p),
            });
        }
    }
    out
}

/// The item that wins for each pair: `prefer` while the binding runs (`live`), `history` when
/// history is read again. `control` and `passive` name the two sources, such as `native` and
/// `transcript`.
pub fn resolve<'a>(data: &CorrelateFile, pairs: &[Pair<'a>], live: bool, control: &str) -> Vec<&'a Item> {
    pairs
        .iter()
        .filter_map(|p| match (p.control, p.passive) {
            (Some(c), Some(t)) => Some(if data.winner(&c.kind, live) == control {
                c
            } else {
                t
            }),
            (c, t) => c.or(t),
        })
        .collect()
}

/// One difference between the sources.
#[derive(Debug, Clone, PartialEq)]
pub struct Difference {
    pub kind: String,
    pub item: String,
    pub text: String,
}

fn show(v: Option<&Value>) -> String {
    match v {
        None => "nothing".into(),
        Some(Value::String(s)) => {
            let s: String = s.chars().take(60).collect();
            format!("{s:?}")
        }
        Some(v) => {
            let s = v.to_string();
            s.chars().take(60).collect()
        }
    }
}

/// What differs between the paired items: items only one source has, and the fields the kind's
/// `compare` names (or, without it, the fields both have) that differ.
pub fn differences(data: &CorrelateFile, pairs: &[Pair], control: &str, passive: &str) -> Vec<Difference> {
    let mut out = Vec::new();
    for p in pairs {
        match (p.control, p.passive) {
            (Some(c), None) => out.push(Difference {
                kind: c.kind.clone(),
                item: c.id.clone(),
                text: format!("{} item {} is missing from the {passive}", c.kind, c.id),
            }),
            (None, Some(t)) => out.push(Difference {
                kind: t.kind.clone(),
                item: t.id.clone(),
                text: format!("{} item {} is only in the {passive}", t.kind, t.id),
            }),
            (Some(c), Some(t)) => {
                let named = data.kind(&c.kind).map(|k| k.fields()).unwrap_or_default();
                let fields: Vec<String> = if named.is_empty() {
                    c.set.keys().filter(|k| t.set.contains_key(*k)).cloned().collect()
                } else {
                    named
                };
                let diffs: BTreeMap<&str, (Option<&Value>, Option<&Value>)> = fields
                    .iter()
                    .filter_map(|f| {
                        let (a, b) = (c.set.get(f), t.set.get(f));
                        let same = match (a, b) {
                            (Some(Value::String(x)), Some(Value::String(y))) => x.trim() == y.trim(),
                            _ => a == b,
                        };
                        (!same).then_some((f.as_str(), (a, b)))
                    })
                    .collect();
                for (f, (a, b)) in diffs {
                    out.push(Difference {
                        kind: c.kind.clone(),
                        item: c.id.clone(),
                        text: format!(
                            "{} item {}: {f} is {} in the {control} and {} in the {passive}",
                            c.kind,
                            c.id,
                            show(a),
                            show(b)
                        ),
                    });
                }
            }
            (None, None) => {}
        }
    }
    out
}

/// The item updates that merge the passive source into the live stream: an item only the
/// passive source has is added under its own id, and where the kind prefers the passive source
/// while the binding runs, its fields are set on the control item. `turn` is the live turn the
/// added items belong to.
pub fn merged(
    data: &CorrelateFile,
    pairs: &[Pair],
    binding: &str,
    turn: Option<&str>,
    control: &str,
) -> Vec<Value> {
    let mut out = Vec::new();
    for p in pairs {
        let (id, item) = match (p.control, p.passive) {
            (None, Some(t)) => (&t.id, t),
            (Some(c), Some(t)) if data.winner(&c.kind, true) != control && c.set != t.set => (&c.id, t),
            _ => continue,
        };
        let mut patch = json!({"id": id, "kind": item.kind, "set": item.set});
        if let Some(turn) = turn {
            patch["turn"] = json!(turn);
        }
        out.push(json!({"binding": binding, "update": {"type": "item", "item": patch}}));
    }
    out
}

/// A difference as the `notice` update shadow mode sends.
pub fn notice(binding: &str, d: &Difference) -> Value {
    json!({
        "binding": binding,
        "update": {"type": "notice", "level": "warning", "text": d.text, "code": SHADOW_CODE},
    })
}

/// Whether shadow mode is on: `PARROT_SHADOW` set to anything but `0` or empty.
pub fn shadow_from_env() -> bool {
    std::env::var("PARROT_SHADOW").is_ok_and(|v| !v.is_empty() && v != "0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_proto::driver_data::parse;

    fn data() -> CorrelateFile {
        parse(
            "correlate.kdl",
            r#"
kind "command" {
  match "ref"
  prefer "native"
  history "transcript"
  compare "command" "status" "exit"
}
kind "*" {
  match "ref" "position"
  prefer "native"
  history "transcript"
}
"#,
        )
        .unwrap()
    }

    fn item(id: &str, kind: &str, turn: &str, set: Value) -> Value {
        json!({"binding": "b", "update": {"type": "item", "item": {"id": id, "kind": kind, "turn": turn, "set": set}}})
    }

    #[test]
    fn merging_adds_passive_only_items_and_keeps_preferred_ones() {
        let mut live = Fold::new();
        live.extend(&[item(
            "c1",
            "command",
            "t1",
            json!({"command": "ls", "status": "ok"}),
        )]);
        let mut disk = Fold::new();
        disk.extend(&[
            item("c1", "command", "t1", json!({"command": "ls", "status": "error"})),
            item("n1", "note", "t1", json!({"text": "Only on disk"})),
        ]);
        let d = data();
        let pairs = correlate(&d, &live.last_turn(), &disk.last_turn());
        let out = merged(&d, &pairs, "b", Some("turn-1"), "native");
        // The command prefers the native source, so only the note is added.
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["update"]["item"]["id"], "n1");
        assert_eq!(out[0]["update"]["item"]["turn"], "turn-1");
        assert_eq!(out[0]["update"]["item"]["set"]["text"], "Only on disk");
        let flipped = parse(
            "correlate.kdl",
            "kind \"*\" {\n  match \"ref\" \"position\"\n  prefer \"transcript\"\n  history \"transcript\"\n}\n",
        )
        .unwrap();
        let out = merged(&flipped, &pairs, "b", None, "native");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["update"]["item"]["set"]["status"], "error");
    }

    #[test]
    fn pairs_by_ref_then_position_and_reports_differences() {
        let mut live = Fold::new();
        live.extend(&[
            item("m1", "user", "t1", json!({"text": "hi"})),
            item(
                "c1",
                "command",
                "t1",
                json!({"command": "ls", "status": "ok", "exit": 0}),
            ),
            item("a1", "assistant", "t1", json!({"text": "Two"})),
            json!({"update": {"type": "item", "item": {"id": "a1", "append": {"text": " files"}}}}),
        ]);
        let mut disk = Fold::new();
        disk.extend(&[
            item("u-9", "user", "t9", json!({"text": "hi"})),
            item(
                "c1",
                "command",
                "t9",
                json!({"command": "ls", "status": "ok", "exit": 1}),
            ),
            item("a-9", "assistant", "t9", json!({"text": "Two files"})),
            item("x-9", "reasoning", "t9", json!({"text": "thinking"})),
        ]);
        let (c, p) = (live.last_turn(), disk.last_turn());
        let pairs = correlate(&data(), &c, &p);
        assert_eq!(pairs.len(), 4);
        let diffs = differences(&data(), &pairs, "native", "transcript");
        let texts: Vec<_> = diffs.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "command item c1: exit is 0 in the native and 1 in the transcript",
                "reasoning item x-9 is only in the transcript",
            ]
        );
        let live_win = resolve(&data(), &pairs, true, "native");
        assert_eq!(live_win[1].set["exit"], 0);
        let history_win = resolve(&data(), &pairs, false, "native");
        assert_eq!(history_win[1].set["exit"], 1);
        assert_eq!(notice("b", &diffs[0])["update"]["code"], SHADOW_CODE);
    }
}
