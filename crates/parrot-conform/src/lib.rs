//! Golden fixtures (spec 38): JSONL of messages in and out, with placeholders, and projection checks.
//!
//! A fixture line is one of:
//!
//! | Line | Meaning |
//! | --- | --- |
//! | `{ "fixture": { "scenario"?, "pending"?, "about"? } }` | What the fixture needs; `pending` skips it unless asked for |
//! | `{ "dir": "in", "msg": … }` | Send the message to the target |
//! | `{ "dir": "out", "msg": … }` | Wait for a matching message from the target |
//! | `{ "check": { "projection": …, "expect": … } }` | Wait until the projection matches |
//!
//! An `out` message with an `id` and no `method` is the response to that id. Any other `out` message
//! matches the first message received that fits it, so drivers may interleave and chunk updates
//! differently. Every message received feeds the projection: items after their patches, requests,
//! options, queue, state, turns and usage.
//!
//! `{ "$has": … }` matches an array with at least one entry that fits, wherever it is.
//!
//! Strings `"$seq"`, `"$id:<name>"`, `"$time"` and `"$any"` stand for values the target picks; the
//! matcher binds each `$id` name once and holds it to that. Strings `"$<var>"` for a variable the
//! runner was given, such as `"$dir"` and `"$agent"`, are replaced in both directions.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

pub mod fixtures;
pub mod projection;
pub mod target;

pub use projection::Projection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Check {
    pub projection: Value,
    pub expect: Value,
}

/// The header of a fixture.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Meta {
    /// The harness scenario the target's fake should play, by name.
    #[serde(default)]
    pub scenario: Option<String>,
    /// Why the fixture is not run by default yet.
    #[serde(default)]
    pub pending: Option<String>,
    #[serde(default)]
    pub about: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Line {
    Msg { dir: Dir, msg: Value },
    Check { check: Check },
    Meta { fixture: Meta },
}

/// One parsed line with its 1-based line number in the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line: usize,
    pub step: Line,
}

#[derive(Debug, thiserror::Error)]
#[error("fixture line {line}: {source}")]
pub struct ParseError {
    pub line: usize,
    #[source]
    pub source: serde_json::Error,
}

/// Parse a fixture; blank lines and lines starting with `//` are skipped.
pub fn parse(text: &str) -> Result<Vec<Step>, ParseError> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
        .map(|(i, l)| {
            serde_json::from_str(l)
                .map(|step| Step { line: i + 1, step })
                .map_err(|source| ParseError { line: i + 1, source })
        })
        .collect()
}

/// The fixture's header, or an empty one.
pub fn meta(steps: &[Step]) -> Meta {
    steps
        .iter()
        .find_map(|s| match &s.step {
            Line::Meta { fixture } => Some(fixture.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The time sent in place of `"$time"` in incoming messages.
pub const FIXED_TIME: &str = "2026-01-01T00:00:00Z";

/// Values bound to placeholders so far.
#[derive(Debug, Default, Clone)]
pub struct Bindings {
    ids: HashMap<String, Value>,
    vars: HashMap<String, Value>,
    last_seq: Option<f64>,
}

impl Bindings {
    /// Bindings with the runner's variables, such as `dir` and `agent`.
    pub fn with_vars(vars: impl IntoIterator<Item = (String, Value)>) -> Self {
        Bindings {
            vars: vars.into_iter().collect(),
            ..Default::default()
        }
    }

    pub fn id(&self, name: &str) -> Option<&Value> {
        self.ids.get(name)
    }

    fn var(&self, s: &str) -> Option<&Value> {
        s.strip_prefix('$').and_then(|name| self.vars.get(name))
    }

    /// Replace placeholders in a message the fixture sends. An unbound `$id:<name>` becomes the
    /// string `<name>`, and is bound to it.
    pub fn substitute(&mut self, v: &Value) -> Value {
        match v {
            Value::String(s) if s == "$seq" => self.last_seq.map(Value::from).unwrap_or(0.into()),
            Value::String(s) if s == "$time" => Value::String(FIXED_TIME.into()),
            Value::String(s) if s.starts_with("$id:") => {
                let name = &s[4..];
                self.ids
                    .entry(name.into())
                    .or_insert_with(|| Value::String(name.into()))
                    .clone()
            }
            Value::String(s) if self.var(s).is_some() => self.var(s).cloned().unwrap_or_default(),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.substitute(x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), self.substitute(x)))
                    .collect::<Map<_, _>>(),
            ),
            other => other.clone(),
        }
    }

    /// Match `actual` against `pattern`, binding placeholders. Objects may carry fields the pattern
    /// doesn't name, since peers add optional fields. Returns the path and reason of the first
    /// difference. Bindings made before a failure are kept.
    pub fn matches(&mut self, pattern: &Value, actual: &Value) -> Result<(), (String, String)> {
        self.match_at(pattern, actual, "$")
    }

    /// Like [`Bindings::matches`], but keeps no bindings unless the whole message matches.
    pub fn try_matches(&mut self, pattern: &Value, actual: &Value) -> Result<(), (String, String)> {
        let mut trial = self.clone();
        trial.matches(pattern, actual)?;
        *self = trial;
        Ok(())
    }

    fn match_at(&mut self, p: &Value, a: &Value, path: &str) -> Result<(), (String, String)> {
        let fail = |why: String| Err((path.to_string(), why));
        match p {
            Value::String(s) if s == "$any" => Ok(()),
            Value::String(s) if s == "$seq" => {
                // Cursors are opaque strings (E2); numeric ones must increase.
                let n = match a {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.parse::<f64>().ok(),
                    _ => return fail(format!("expected a seq, got {a}")),
                };
                if let Some(n) = n {
                    if let Some(last) = self.last_seq
                        && n <= last
                    {
                        return fail(format!("seq {n} does not follow {last}"));
                    }
                    self.last_seq = Some(n);
                }
                Ok(())
            }
            Value::String(s) if s == "$time" => match a.as_str() {
                Some(t) if t.len() >= 20 && t.as_bytes().get(10) == Some(&b'T') => Ok(()),
                _ => fail(format!("expected a time, got {a}")),
            },
            Value::String(s) if s.starts_with("$id:") => {
                let name = &s[4..];
                if !(a.is_string() || a.is_number()) {
                    return fail(format!("expected an id for `{name}`, got {a}"));
                }
                match self.ids.get(name) {
                    Some(bound) if bound == a => Ok(()),
                    Some(bound) => fail(format!("`$id:{name}` is {bound}, got {a}")),
                    None => {
                        if let Some((other, _)) = self.ids.iter().find(|(_, v)| *v == a) {
                            return fail(format!("{a} is already bound to `$id:{other}`"));
                        }
                        self.ids.insert(name.into(), a.clone());
                        Ok(())
                    }
                }
            }
            Value::String(s) if self.var(s).is_some() => {
                let want = self.var(s).cloned().unwrap_or_default();
                if &want == a {
                    Ok(())
                } else {
                    fail(format!("expected {want}, got {a}"))
                }
            }
            // `{"$has": pattern}`: an array with at least one entry that matches, such as the
            // choice with a given intent wherever it is listed.
            Value::Object(po) if po.len() == 1 && po.contains_key("$has") => {
                let Some(aa) = a.as_array() else {
                    return fail(format!("expected an array, got {a}"));
                };
                for (i, av) in aa.iter().enumerate() {
                    let mut trial = self.clone();
                    if trial.match_at(&po["$has"], av, &format!("{path}[{i}]")).is_ok() {
                        *self = trial;
                        return Ok(());
                    }
                }
                fail(format!("no entry matches {}: {a}", po["$has"]))
            }
            Value::Object(po) => {
                let Some(ao) = a.as_object() else {
                    return fail(format!("expected an object, got {a}"));
                };
                for (k, pv) in po {
                    let sub = format!("{path}.{k}");
                    match ao.get(k) {
                        Some(av) => self.match_at(pv, av, &sub)?,
                        None => return Err((sub, "missing".into())),
                    }
                }
                Ok(())
            }
            Value::Array(pa) => {
                let Some(aa) = a.as_array() else {
                    return fail(format!("expected an array, got {a}"));
                };
                if pa.len() != aa.len() {
                    return fail(format!("expected {} entries, got {}: {a}", pa.len(), aa.len()));
                }
                for (i, (pv, av)) in pa.iter().zip(aa).enumerate() {
                    self.match_at(pv, av, &format!("{path}[{i}]"))?;
                }
                Ok(())
            }
            _ if p == a => Ok(()),
            _ => fail(format!("expected {p}, got {a}")),
        }
    }
}

/// What a fixture runs against: a driver or a runtime behind some transport.
pub trait Target {
    fn send(&mut self, msg: Value);
    /// The next message from the target, or `None` when it has nothing more to say in time.
    fn recv(&mut self) -> Option<Value>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub line: usize,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    pub steps: usize,
    pub mismatches: Vec<Mismatch>,
    /// Every message received, in order, for showing what went wrong.
    pub received: Vec<Value>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Runs fixtures against one target, keeping messages that no step has claimed yet.
struct Runner<'a> {
    target: &'a mut dyn Target,
    bindings: Bindings,
    projection: Projection,
    /// Received and not yet matched by an `out` line.
    unclaimed: Vec<Value>,
    received: Vec<Value>,
}

impl Runner<'_> {
    fn pull(&mut self) -> bool {
        match self.target.recv() {
            Some(msg) => {
                self.projection.observe(&msg);
                self.unclaimed.push(msg.clone());
                self.received.push(msg);
                true
            }
            None => false,
        }
    }

    fn expect_out(&mut self, pattern: &Value) -> Result<(), (String, String)> {
        let is_response = pattern.get("method").is_none() && pattern.get("id").is_some();
        let mut scanned = 0;
        let mut last_err = None;
        loop {
            while scanned < self.unclaimed.len() {
                let msg = &self.unclaimed[scanned];
                if is_response {
                    if msg.get("method").is_none() && msg.get("id") == pattern.get("id") {
                        let msg = self.unclaimed.remove(scanned);
                        return self.bindings.matches(pattern, &msg);
                    }
                } else {
                    match self.bindings.try_matches(pattern, msg) {
                        Ok(()) => {
                            self.unclaimed.remove(scanned);
                            return Ok(());
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                scanned += 1;
            }
            if !self.pull() {
                let what = if is_response {
                    "no response"
                } else {
                    "no matching message"
                };
                let why = match last_err {
                    Some((path, reason)) if !is_response => {
                        format!(
                            "{what} in {} received; last tried: {path}: {reason}",
                            self.received.len()
                        )
                    }
                    _ => format!("{what} in {} received", self.received.len()),
                };
                return Err(("$".into(), why));
            }
        }
    }

    fn check(&mut self, check: &Check) -> Result<(), (String, String)> {
        let what = self.bindings.substitute(&check.projection);
        loop {
            let actual = self.projection.query(&what);
            let result = self.bindings.try_matches(&check.expect, &actual);
            if result.is_ok() || !self.pull() {
                return result;
            }
        }
    }
}

/// Run the steps in order against a target and collect every mismatch.
pub fn run(target: &mut dyn Target, steps: &[Step]) -> Report {
    run_with(target, steps, Bindings::default())
}

/// Like [`run`], with bindings that carry the runner's variables.
pub fn run_with(target: &mut dyn Target, steps: &[Step], bindings: Bindings) -> Report {
    let mut r = Runner {
        target,
        bindings,
        projection: Projection::default(),
        unclaimed: Vec::new(),
        received: Vec::new(),
    };
    let mut report = Report::default();
    for s in steps {
        report.steps += 1;
        let result = match &s.step {
            Line::Meta { .. } => Ok(()),
            Line::Msg { dir: Dir::In, msg } => {
                let m = r.bindings.substitute(msg);
                r.target.send(m);
                Ok(())
            }
            Line::Msg { dir: Dir::Out, msg } => r.expect_out(msg),
            Line::Check { check } => r.check(check),
        };
        if let Err((path, reason)) = result {
            report.mismatches.push(Mismatch {
                line: s.line,
                path,
                reason,
            });
            // Later steps depend on this one; stop instead of reporting a cascade of timeouts.
            break;
        }
    }
    report.received = r.received;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_bind_consistently() {
        let mut b = Bindings::default();
        assert!(b.matches(&json!({"id": "$id:a"}), &json!({"id": "x"})).is_ok());
        assert!(b.matches(&json!("$id:a"), &json!("x")).is_ok());
        assert!(b.matches(&json!("$id:a"), &json!("y")).is_err());
        assert!(b.matches(&json!("$id:b"), &json!("x")).is_err());
        assert!(b.matches(&json!(["$seq", "$seq"]), &json!([1, 2])).is_ok());
        assert!(b.matches(&json!("$seq"), &json!(2)).is_err());
        assert!(b.matches(&json!("$seq"), &json!("3")).is_ok());
        assert!(b.matches(&json!("$time"), &json!(FIXED_TIME)).is_ok());
        assert!(b.matches(&json!("$any"), &json!([1, {"x": 2}])).is_ok());
        assert_eq!(b.substitute(&json!({"s": "$id:a"})), json!({"s": "x"}));
    }

    #[test]
    fn vars_substitute_and_match() {
        let mut b = Bindings::with_vars([("dir".to_string(), json!("/w/demo"))]);
        assert_eq!(b.substitute(&json!({"cwd": "$dir"})), json!({"cwd": "/w/demo"}));
        assert!(b.matches(&json!("$dir"), &json!("/w/demo")).is_ok());
        assert!(b.matches(&json!("$dir"), &json!("/elsewhere")).is_err());
        // An unknown variable is a literal.
        assert!(b.matches(&json!("$nope"), &json!("$nope")).is_ok());
    }

    #[test]
    fn try_matches_keeps_nothing_on_failure() {
        let mut b = Bindings::default();
        assert!(b.try_matches(&json!(["$id:a", 1]), &json!(["x", 2])).is_err());
        assert!(b.id("a").is_none());
    }

    #[test]
    fn has_finds_an_entry_anywhere_and_binds_from_it() {
        let mut b = Bindings::default();
        let choices = json!([{ "id": "c-deny", "intent": "deny" }, { "id": "c-allow", "intent": "allow" }]);
        let pattern = json!({ "$has": { "id": "$id:allow", "intent": "allow" } });
        assert!(b.matches(&pattern, &choices).is_ok());
        assert_eq!(b.id("allow"), Some(&json!("c-allow")));
        let none = json!({ "$has": { "intent": "neutral" } });
        assert!(b.matches(&none, &choices).is_err());
    }

    #[test]
    fn parse_reports_line() {
        let e = parse("{\"dir\":\"in\",\"msg\":1}\n\nnot json").unwrap_err();
        assert_eq!(e.line, 3);
        let steps = parse("// a comment\n{\"fixture\":{\"scenario\":\"x\"}}").unwrap();
        assert_eq!(meta(&steps).scenario.as_deref(), Some("x"));
    }
}
