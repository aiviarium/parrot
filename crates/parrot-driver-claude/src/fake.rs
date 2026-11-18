//! A fake `claude` for tests: it replays a scenario over stream-json instead of calling a model.
//!
//! A scenario is JSONL, one step per line (blank lines and lines starting with `//` are skipped):
//!
//! | Step | What the fake does |
//! | --- | --- |
//! | `{"expect": <pattern>}` | reads the next input line and fails unless the pattern matches it |
//! | `{"emit": <message>}` | writes the message |
//! | `{"await_control_response": "<id>", "match": <pattern>}` | reads until the response to `<id>`; the optional pattern is checked against the inner `response` |
//! | `{"exit": <code>}` | exits with the code |
//!
//! A pattern matches when every key it names matches, recursively; arrays match element by element.
//! In an emitted message the string `"{{request_id}}"` stands for the `request_id` of the last
//! input that had one, so a scenario can answer the host's control requests.
//!
//! The fake answers `initialize` by itself (unless the current `expect` asks for it) and skips
//! `keep_alive`. When the steps run out it reads until stdin closes and exits with 0.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

/// The variable that names a scenario when `--scenario` is not given.
pub const SCENARIO_ENV: &str = "PARROT_FAKE_CLAUDE_SCENARIO";

/// The exit code for a scenario that did not match what the host sent.
pub const MISMATCH_EXIT: i32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Expect(Value),
    Emit(Value),
    AwaitControlResponse {
        request_id: String,
        pattern: Option<Value>,
    },
    Exit(i32),
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("reading the scenario: {0}")]
    Io(#[from] std::io::Error),
    #[error("scenario line {line}: {message}")]
    Bad { line: usize, message: String },
}

/// Parses a scenario's text.
pub fn parse_scenario(text: &str) -> Result<Vec<Step>, ScenarioError> {
    let mut steps = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let bad = |message: String| ScenarioError::Bad { line: i + 1, message };
        let v: Value = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
        let step = if let Some(p) = v.get("expect") {
            Step::Expect(p.clone())
        } else if let Some(m) = v.get("emit") {
            Step::Emit(m.clone())
        } else if let Some(id) = v.get("await_control_response") {
            Step::AwaitControlResponse {
                request_id: id
                    .as_str()
                    .ok_or_else(|| bad("await_control_response takes a string".into()))?
                    .to_owned(),
                pattern: v.get("match").cloned(),
            }
        } else if let Some(c) = v.get("exit") {
            Step::Exit(c.as_i64().ok_or_else(|| bad("exit takes an integer".into()))? as i32)
        } else {
            return Err(bad(format!("unknown step {v}")));
        };
        steps.push(step);
    }
    Ok(steps)
}

/// Reads a scenario, or a recorded trace (`{"dir", "at", "msg"}` lines, see [`parse_trace`]).
pub fn load_scenario(path: &Path) -> Result<Vec<Step>, ScenarioError> {
    let text = std::fs::read_to_string(path)?;
    if is_trace(&text) {
        parse_trace(&text)
    } else {
        parse_scenario(&text)
    }
}

/// Whether the text is a recorded trace rather than a scenario.
pub fn is_trace(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//"))
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .is_some_and(|v| v.get("dir").is_some() && v.get("msg").is_some())
}

/// Turns a recorded trace into scenario steps, so the fake replays a real CLI's lines.
///
/// An input line becomes an `expect` on its type and subtype, and an answer to a permission
/// request becomes an `await_control_response` on its behaviour. Output lines are emitted as
/// recorded, except that a control response to the host's own request carries the id the host
/// sent this time.
pub fn parse_trace(text: &str) -> Result<Vec<Step>, ScenarioError> {
    let mut steps = Vec::new();
    let mut host_ids: Vec<String> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let bad = |message: String| ScenarioError::Bad { line: i + 1, message };
        let v: Value = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
        let msg = v.get("msg").cloned().ok_or_else(|| bad("a trace line has msg".into()))?;
        match v.get("dir").and_then(Value::as_str) {
            Some("in") => match msg["type"].as_str() {
                Some("user") => steps.push(Step::Expect(json!({"type": "user"}))),
                Some("control_request") => {
                    if let Some(id) = msg["request_id"].as_str() {
                        host_ids.push(id.to_owned());
                    }
                    steps.push(Step::Expect(json!({
                        "type": "control_request",
                        "request": {"subtype": msg["request"]["subtype"]},
                    })));
                }
                Some("control_response") => {
                    let r = &msg["response"];
                    steps.push(Step::AwaitControlResponse {
                        request_id: r["request_id"].as_str().unwrap_or_default().to_owned(),
                        pattern: r["response"]
                            .get("behavior")
                            .map(|b| json!({"behavior": b})),
                    });
                }
                _ => {}
            },
            Some("out") => {
                let mut msg = msg;
                if msg["type"] == "control_response"
                    && let Some(id) = msg["response"]["request_id"].as_str()
                    && host_ids.iter().any(|h| h == id)
                {
                    msg["response"]["request_id"] = json!("{{request_id}}");
                }
                steps.push(Step::Emit(msg));
            }
            _ => return Err(bad("dir is in or out".into())),
        }
    }
    Ok(steps)
}

/// Whether `value` has everything `pattern` names.
pub fn matches(pattern: &Value, value: &Value) -> bool {
    match (pattern, value) {
        (Value::Object(p), Value::Object(v)) => {
            p.iter().all(|(k, pv)| v.get(k).is_some_and(|vv| matches(pv, vv)))
        }
        (Value::Array(p), Value::Array(v)) => {
            p.len() == v.len() && p.iter().zip(v).all(|(a, b)| matches(a, b))
        }
        _ => pattern == value,
    }
}

/// The scenario names in `args` (`--scenario <path>` or `--scenario=<path>`); other flags are ignored.
pub fn scenario_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--scenario" {
            return it.next().cloned();
        }
        if let Some(p) = a.strip_prefix("--scenario=") {
            return Some(p.to_owned());
        }
    }
    None
}

fn substitute(v: &Value, request_id: Option<&str>) -> Value {
    match v {
        Value::String(s) if s == "{{request_id}}" => {
            request_id.map_or_else(|| v.clone(), |id| Value::String(id.to_owned()))
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, request_id)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), substitute(x, request_id)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

fn initialize_reply(request_id: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "commands": [{"name": "review", "description": "Review the changes on this branch", "argumentHint": ""}],
                "models": [{"value": "default", "displayName": "Default", "description": "The fake default"}],
                "current_permission_mode": "default",
                "claude_code_version": "fake"
            }
        }
    })
}

fn is_initialize(v: &Value) -> bool {
    v["type"] == "control_request" && v["request"]["subtype"] == "initialize"
}

struct Run<R, W> {
    input: R,
    output: W,
    last_request_id: Option<String>,
}

impl<R: BufRead, W: Write> Run<R, W> {
    fn write(&mut self, v: &Value) -> std::io::Result<()> {
        writeln!(self.output, "{v}")?;
        self.output.flush()
    }

    /// The next input message the scenario has to look at, or `None` at end of input. Answers
    /// `initialize` unless `wants` accepts it, and skips keep-alives.
    fn next(&mut self, wants: impl Fn(&Value) -> bool) -> std::io::Result<Option<Value>> {
        loop {
            let mut line = String::new();
            if self.input.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("fake-claude: input is not JSON: {e}");
                    continue;
                }
            };
            if let Some(id) = v.get("request_id").and_then(Value::as_str) {
                self.last_request_id = Some(id.to_owned());
            }
            if v["type"] == "keep_alive" && !wants(&v) {
                continue;
            }
            if is_initialize(&v) && !wants(&v) {
                let id = v["request_id"].as_str().unwrap_or_default().to_owned();
                self.write(&initialize_reply(&id))?;
                continue;
            }
            return Ok(Some(v));
        }
    }
}

/// Plays `steps` against `input` and `output` and returns the exit code.
pub fn run<R: BufRead, W: Write>(steps: &[Step], input: R, output: W) -> i32 {
    let mut run = Run {
        input,
        output,
        last_request_id: None,
    };
    match play(steps, &mut run) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fake-claude: {e}");
            1
        }
    }
}

fn play<R: BufRead, W: Write>(steps: &[Step], run: &mut Run<R, W>) -> std::io::Result<i32> {
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Expect(pattern) => match run.next(|v| matches(pattern, v))? {
                Some(v) if matches(pattern, &v) => {}
                Some(v) => {
                    eprintln!("fake-claude: step {}: expected {pattern}, got {v}", i + 1);
                    return Ok(MISMATCH_EXIT);
                }
                None => {
                    eprintln!(
                        "fake-claude: step {}: input closed while expecting {pattern}",
                        i + 1
                    );
                    return Ok(MISMATCH_EXIT);
                }
            },
            Step::Emit(m) => {
                let m = substitute(m, run.last_request_id.as_deref());
                run.write(&m)?;
            }
            Step::AwaitControlResponse { request_id, pattern } => loop {
                let Some(v) = run.next(|_| false)? else {
                    eprintln!(
                        "fake-claude: step {}: input closed while awaiting {request_id}",
                        i + 1
                    );
                    return Ok(MISMATCH_EXIT);
                };
                if v["type"] != "control_response" || v["response"]["request_id"] != request_id.as_str() {
                    eprintln!("fake-claude: step {}: skipping {v}", i + 1);
                    continue;
                }
                if let Some(p) = pattern
                    && !matches(p, &v["response"]["response"])
                {
                    eprintln!(
                        "fake-claude: step {}: response to {request_id} is {v}, wanted {p}",
                        i + 1
                    );
                    return Ok(MISMATCH_EXIT);
                }
                break;
            },
            Step::Exit(code) => return Ok(*code),
        }
    }
    // After the scenario, option changes and interrupts still get an answer, as the CLI gives.
    while let Some(v) = run.next(|_| false)? {
        if v["type"] == "control_request"
            && let Some(id) = v["request_id"].as_str()
        {
            let reply = json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {}}});
            run.write(&reply)?;
        }
    }
    Ok(0)
}
