//! A fake `codex app-server` for tests: it replays a scenario over JSON-RPC instead of calling a
//! model.
//!
//! A scenario is JSONL, one step per line (blank lines and lines starting with `//` are skipped):
//!
//! | Step | What the fake does |
//! | --- | --- |
//! | `{"expect": <pattern>}` | reads the next message and fails unless the pattern matches it |
//! | `{"emit": <message>}` | writes the message |
//! | `{"await_response": <id>, "match": <pattern>}` | reads until the answer to its own request `<id>`; the optional pattern is checked against `result` |
//! | `{"exit": <code>}` | exits with the code |
//! | `{"args": [<string>, ...]}` | fails unless the fake's command line holds these arguments in this order |
//!
//! A pattern matches when every key it names matches, recursively; arrays match element by element.
//! In an emitted message the string `"{{id}}"` stands for the id of the last request read, so a
//! scenario can answer the driver's requests.
//!
//! Unless the current `expect` asks for them, the fake answers `initialize`, `model/list`,
//! `skills/list`, `thread/start` and `thread/resume` by itself and skips the `initialized`
//! notification. Its skill list holds the one skill [`FAKE_SKILL`]. When the
//! steps run out it reads until stdin closes and exits with 0.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

/// The variable that names a scenario when `--scenario` is not given.
pub const SCENARIO_ENV: &str = "PARROT_FAKE_CODEX_SCENARIO";

/// The skill the fake lists.
pub const FAKE_SKILL: &str = "fake-lint";

/// The variable that makes `fake-codex login status` report signed out when set to `out`.
pub const LOGIN_ENV: &str = "PARROT_FAKE_CODEX_LOGIN";

/// The thread id the fake gives a new thread.
pub const FAKE_THREAD: &str = "thr-fake-0001";

/// The exit code for a scenario that did not match what the driver sent.
pub const MISMATCH_EXIT: i32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Expect(Value),
    Emit(Value),
    AwaitResponse { id: Value, pattern: Option<Value> },
    Exit(i32),
    Args(Vec<String>),
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
        } else if let Some(id) = v.get("await_response") {
            Step::AwaitResponse {
                id: id.clone(),
                pattern: v.get("match").cloned(),
            }
        } else if let Some(a) = v.get("args") {
            let args = serde_json::from_value(a.clone()).map_err(|e| bad(format!("args: {e}")))?;
            Step::Args(args)
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

/// Turns a recorded trace into scenario steps, so the fake replays a real app-server's lines.
///
/// A request from the client becomes an `expect` on its method, and an answer to a server
/// request becomes an `await_response` on its decision. Notifications from the client are
/// skipped. Output lines are emitted as recorded, except that the answer to a client request
/// carries the id the client sent this time.
pub fn parse_trace(text: &str) -> Result<Vec<Step>, ScenarioError> {
    let mut steps = Vec::new();
    let mut client_ids: Vec<Value> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let bad = |message: String| ScenarioError::Bad { line: i + 1, message };
        let v: Value = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
        let mut msg = v
            .get("msg")
            .cloned()
            .ok_or_else(|| bad("a trace line has msg".into()))?;
        match v.get("dir").and_then(Value::as_str) {
            Some("in") => match (msg.get("method"), msg.get("id")) {
                (Some(m), Some(id)) => {
                    client_ids.push(id.clone());
                    steps.push(Step::Expect(json!({"method": m})));
                }
                (None, Some(id)) => steps.push(Step::AwaitResponse {
                    id: id.clone(),
                    pattern: msg.pointer("/result/decision").map(|d| json!({"decision": d})),
                }),
                _ => {}
            },
            Some("out") => {
                if msg.get("method").is_none()
                    && let Some(id) = msg.get("id")
                    && client_ids.contains(id)
                {
                    msg["id"] = json!("{{id}}");
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

/// The scenario `args` name (`--scenario <path>` or `--scenario=<path>`); other arguments, such as
/// `app-server`, are ignored.
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

fn substitute(v: &Value, id: Option<&Value>) -> Value {
    match v {
        Value::String(s) if s == "{{id}}" => id.cloned().unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, id)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), substitute(x, id))).collect()),
        _ => v.clone(),
    }
}

/// The models the fake lists unless a scenario answers `model/list` itself.
pub fn fake_models() -> Value {
    json!({
        "data": [
            {
                "id": "gpt-fake-large", "model": "gpt-fake-large", "displayName": "Fake large",
                "description": "The big fake", "hidden": false, "isDefault": true,
                "defaultReasoningEffort": "medium",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low", "description": "Quick"},
                    {"reasoningEffort": "medium", "description": "Balanced"},
                    {"reasoningEffort": "high", "description": "Thorough"}
                ]
            },
            {
                "id": "gpt-fake-mini", "model": "gpt-fake-mini", "displayName": "Fake mini",
                "hidden": false, "isDefault": false, "defaultReasoningEffort": "low",
                "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": "Quick"}]
            },
            {"id": "gpt-fake-hidden", "hidden": true, "supportedReasoningEfforts": []}
        ],
        "nextCursor": null
    })
}

fn auto_answer(v: &Value) -> Option<Vec<Value>> {
    let id = v.get("id")?.clone();
    let reply = |result: Value| json!({"id": id, "result": result});
    match v["method"].as_str()? {
        "initialize" => Some(vec![reply(
            json!({"userAgent": "fake-codex/0.167.1 (Fake OS; x86_64)"}),
        )]),
        "model/list" => Some(vec![reply(fake_models())]),
        "skills/list" => {
            let cwd = v["params"]["cwds"][0].clone();
            Some(vec![reply(json!({"data": [{
                "cwd": cwd,
                "skills": [{
                    "name": FAKE_SKILL,
                    "description": "Lints the fake project",
                    "shortDescription": "Lint",
                    "path": "/fake/skills/fake-lint/SKILL.md",
                    "scope": "user",
                    "enabled": true,
                }],
                "errors": [],
            }]}))])
        }
        m @ ("thread/start" | "thread/resume") => {
            let thread = if m == "thread/resume" {
                v["params"]["threadId"].as_str().unwrap_or(FAKE_THREAD).to_owned()
            } else {
                FAKE_THREAD.to_owned()
            };
            let model = v["params"]["model"]
                .as_str()
                .unwrap_or("gpt-fake-large")
                .to_owned();
            Some(vec![
                reply(json!({
                    "thread": {"id": thread, "status": {"type": "idle"}},
                    "model": model,
                    "reasoningEffort": "medium",
                    "approvalPolicy": v["params"]["approvalPolicy"],
                    "sandbox": v["params"]["sandbox"],
                    "cwd": v["params"]["cwd"],
                })),
                json!({"method": "thread/started", "params": {"thread": {"id": thread}}}),
            ])
        }
        _ => None,
    }
}

struct Run<R, W> {
    input: R,
    output: W,
    last_id: Option<Value>,
}

impl<R: BufRead, W: Write> Run<R, W> {
    fn write(&mut self, v: &Value) -> std::io::Result<()> {
        writeln!(self.output, "{v}")?;
        self.output.flush()
    }

    /// The next message the scenario has to look at, or `None` at end of input. Answers the start-up
    /// requests unless `wants` accepts them.
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
                    eprintln!("fake-codex: input is not JSON: {e}");
                    continue;
                }
            };
            if v.get("method").is_some()
                && let Some(id) = v.get("id")
            {
                self.last_id = Some(id.clone());
            }
            if wants(&v) {
                return Ok(Some(v));
            }
            if v["method"] == "initialized" && v.get("id").is_none() {
                continue;
            }
            if let Some(replies) = auto_answer(&v) {
                for r in replies {
                    self.write(&r)?;
                }
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
        last_id: None,
    };
    match play(steps, &mut run) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fake-codex: {e}");
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
                    eprintln!("fake-codex: step {}: expected {pattern}, got {v}", i + 1);
                    return Ok(MISMATCH_EXIT);
                }
                None => {
                    eprintln!(
                        "fake-codex: step {}: input closed while expecting {pattern}",
                        i + 1
                    );
                    return Ok(MISMATCH_EXIT);
                }
            },
            Step::Emit(m) => {
                let m = substitute(m, run.last_id.as_ref());
                run.write(&m)?;
            }
            Step::AwaitResponse { id, pattern } => loop {
                let Some(v) = run.next(|_| false)? else {
                    eprintln!("fake-codex: step {}: input closed while awaiting {id}", i + 1);
                    return Ok(MISMATCH_EXIT);
                };
                if v.get("method").is_some() || v.get("id") != Some(id) {
                    eprintln!("fake-codex: step {}: skipping {v}", i + 1);
                    continue;
                }
                if let Some(p) = pattern
                    && !matches(p, &v["result"])
                {
                    eprintln!("fake-codex: step {}: answer to {id} is {v}, wanted {p}", i + 1);
                    return Ok(MISMATCH_EXIT);
                }
                break;
            },
            Step::Exit(code) => return Ok(*code),
            Step::Args(want) => {
                let have: Vec<String> = std::env::args().skip(1).collect();
                if !have.windows(want.len()).any(|w| w == want.as_slice()) {
                    eprintln!("fake-codex: step {}: arguments {have:?} lack {want:?}", i + 1);
                    return Ok(MISMATCH_EXIT);
                }
            }
        }
    }
    while run.next(|_| false)?.is_some() {}
    Ok(0)
}
