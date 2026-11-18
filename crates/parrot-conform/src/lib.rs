//! Golden fixtures (spec 38): JSONL of messages in and out, with placeholders, and projection checks.
//!
//! A fixture line is `{ "dir": "in" | "out", "msg": … }` or
//! `{ "check": { "projection": …, "expect": … } }`. Strings `"$seq"`, `"$id:<name>"` and `"$time"`
//! stand for values the target picks; the matcher binds each `$id` name once and holds it to that.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Line {
    Msg { dir: Dir, msg: Value },
    Check { check: Check },
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

/// Parse a fixture; blank lines are skipped.
pub fn parse(text: &str) -> Result<Vec<Step>, ParseError> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l)
                .map(|step| Step { line: i + 1, step })
                .map_err(|source| ParseError { line: i + 1, source })
        })
        .collect()
}

/// The time sent in place of `"$time"` in incoming messages.
pub const FIXED_TIME: &str = "2026-01-01T00:00:00Z";

/// Values bound to placeholders so far.
#[derive(Debug, Default, Clone)]
pub struct Bindings {
    ids: HashMap<String, Value>,
    last_seq: Option<f64>,
}

impl Bindings {
    pub fn id(&self, name: &str) -> Option<&Value> {
        self.ids.get(name)
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

    fn match_at(&mut self, p: &Value, a: &Value, path: &str) -> Result<(), (String, String)> {
        let fail = |why: String| Err((path.to_string(), why));
        match p {
            Value::String(s) if s == "$seq" => {
                let Some(n) = a.as_f64() else {
                    return fail(format!("expected a seq number, got {a}"));
                };
                if let Some(last) = self.last_seq
                    && n <= last
                {
                    return fail(format!("seq {n} does not follow {last}"));
                }
                self.last_seq = Some(n);
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
                    return fail(format!("expected {} entries, got {}", pa.len(), aa.len()));
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
    /// The next message from the target, or `None` when it has nothing more to say.
    fn recv(&mut self) -> Option<Value>;
    /// The target's current projection of `what` (items, requests, options, queue, state, tasks).
    fn projection(&mut self, what: &Value) -> Value;
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
}

impl Report {
    pub fn passed(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Run the steps in order against a target and collect every mismatch.
pub fn run(target: &mut dyn Target, steps: &[Step]) -> Report {
    let mut b = Bindings::default();
    let mut report = Report::default();
    for s in steps {
        report.steps += 1;
        let result = match &s.step {
            Line::Msg { dir: Dir::In, msg } => {
                let m = b.substitute(msg);
                target.send(m);
                Ok(())
            }
            Line::Msg { dir: Dir::Out, msg } => match target.recv() {
                Some(actual) => b.matches(msg, &actual),
                None => Err(("$".into(), "no message".into())),
            },
            Line::Check { check } => {
                let what = b.substitute(&check.projection);
                let actual = target.projection(&what);
                b.matches(&check.expect, &actual)
            }
        };
        if let Err((path, reason)) = result {
            report.mismatches.push(Mismatch {
                line: s.line,
                path,
                reason,
            });
        }
    }
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
        assert!(b.matches(&json!("$time"), &json!(FIXED_TIME)).is_ok());
        assert_eq!(b.substitute(&json!({"s": "$id:a"})), json!({"s": "x"}));
    }

    #[test]
    fn parse_reports_line() {
        let e = parse("{\"dir\":\"in\",\"msg\":1}\n\nnot json").unwrap_err();
        assert_eq!(e.line, 3);
    }
}
