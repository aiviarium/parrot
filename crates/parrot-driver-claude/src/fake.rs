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
//! | `{"choose": [{"when": <pattern>, "steps": "<file>"}, …]}` | reads the next input and goes on with the steps of the first file whose pattern matches it, relative to the scenario |
//! | `{"mcp_call": {"name", "arguments"}, "save"?: "<name>", "until"?: <pattern>, "attempts"?: n}` | calls a tool of the injected `parrot` MCP server from `--mcp-config`; with `until`, again (once a second) until the structured result matches |
//! | `{"shell": "<command>"}` | runs the command with `sh -c` in the working directory and fails the scenario if it fails |
//!
//! A pattern matches when every key it names matches, recursively; arrays match element by element.
//! In an emitted message the string `"{{request_id}}"` stands for the `request_id` of the last
//! input that had one, so a scenario can answer the host's control requests. In emitted messages
//! and MCP arguments, `"{{cwd}}"` is the working directory and `"{{<name>/<pointer>}}"` a field of
//! a result saved by `mcp_call`; `[<name>/<pointer>]` inside a pointer is replaced by that string
//! first.
//!
//! The fake answers `initialize` by itself (unless the current `expect` asks for it) and skips
//! `keep_alive`. When the steps run out it reads until stdin closes and exits with 0.

use std::io::{BufRead, Write};
use std::path::Path;

/// Writes to stderr, and also to the file `PARROT_FAKE_CLAUDE_LOG` names, since a driver may not
/// pass the CLI's stderr on.
macro_rules! eprintln {
    ($($t:tt)*) => {{
        let m = format!($($t)*);
        std::eprintln!("{m}");
        if let Some(p) = std::env::var_os("PARROT_FAKE_CLAUDE_LOG")
            && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
        {
            let _ = writeln!(f, "{m}");
        }
    }};
}

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
    /// Patterns for the next input, each with the steps that follow it.
    Choose(Vec<(Value, std::path::PathBuf)>),
    McpCall {
        call: Value,
        save: Option<String>,
        until: Option<Value>,
        attempts: u32,
    },
    Shell(String),
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
    parse_scenario_in(text, Path::new("."))
}

/// Parses a scenario's text; files it names are relative to `base`.
pub fn parse_scenario_in(text: &str, base: &Path) -> Result<Vec<Step>, ScenarioError> {
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
        } else if let Some(options) = v.get("choose") {
            let options = options
                .as_array()
                .ok_or_else(|| bad("choose takes an array".into()))?
                .iter()
                .map(|o| {
                    let file = o["steps"]
                        .as_str()
                        .ok_or_else(|| bad("each choice needs steps".into()))?;
                    Ok((o.get("when").cloned().unwrap_or(json!({})), base.join(file)))
                })
                .collect::<Result<Vec<_>, ScenarioError>>()?;
            Step::Choose(options)
        } else if let Some(call) = v.get("mcp_call") {
            Step::McpCall {
                call: call.clone(),
                save: v.get("save").and_then(Value::as_str).map(str::to_owned),
                until: v.get("until").cloned(),
                attempts: v.get("attempts").and_then(Value::as_u64).unwrap_or(30) as u32,
            }
        } else if let Some(cmd) = v.get("shell") {
            Step::Shell(
                cmd.as_str()
                    .ok_or_else(|| bad("shell takes a string".into()))?
                    .to_owned(),
            )
        } else if let Some(c) = v.get("exit") {
            Step::Exit(c.as_i64().ok_or_else(|| bad("exit takes an integer".into()))? as i32)
        } else {
            return Err(bad(format!("unknown step {v}")));
        };
        steps.push(step);
    }
    Ok(steps)
}

pub fn load_scenario(path: &Path) -> Result<Vec<Step>, ScenarioError> {
    let base = path.parent().unwrap_or(Path::new("."));
    parse_scenario_in(&std::fs::read_to_string(path)?, base)
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

/// What `{{…}}` placeholders stand for.
#[derive(Default)]
struct Vars {
    request_id: Option<String>,
    saved: std::collections::BTreeMap<String, Value>,
}

impl Vars {
    fn lookup(&self, name: &str) -> Option<Value> {
        // `[<name>/<pointer>]` inside a name is replaced by the string it stands for first.
        if let (Some(open), Some(close)) = (name.find('['), name.find(']'))
            && open < close
        {
            let inner = self.lookup(&name[open + 1..close])?;
            let inner = inner.as_str()?;
            return self.lookup(&format!("{}{inner}{}", &name[..open], &name[close + 1..]));
        }
        match name {
            "request_id" => self.request_id.clone().map(Value::String),
            "cwd" => std::env::current_dir()
                .ok()
                .map(|d| Value::String(d.display().to_string())),
            _ => {
                let (key, pointer) = name.split_once('/').map_or((name, ""), |(k, p)| (k, p));
                let saved = self.saved.get(key)?;
                if pointer.is_empty() {
                    Some(saved.clone())
                } else {
                    saved.pointer(&format!("/{pointer}")).cloned()
                }
            }
        }
    }

    fn substitute(&self, v: &Value) -> Value {
        match v {
            Value::String(s) if s.starts_with("{{") && s.ends_with("}}") => {
                self.lookup(&s[2..s.len() - 2]).unwrap_or_else(|| v.clone())
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.substitute(x)).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, x)| (k.clone(), self.substitute(x))).collect())
            }
            _ => v.clone(),
        }
    }
}

/// A client of the injected MCP server, started on first use from `--mcp-config`.
struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(args: &[String]) -> std::io::Result<Self> {
        let err = |m: &str| std::io::Error::other(format!("mcp: {m}"));
        let config = args
            .iter()
            .position(|a| a == "--mcp-config")
            .and_then(|i| args.get(i + 1))
            .ok_or_else(|| err("no --mcp-config"))?;
        let config: Value = serde_json::from_str(config).map_err(|e| err(&e.to_string()))?;
        let server = &config["mcpServers"]["parrot"];
        let command = server["command"]
            .as_str()
            .ok_or_else(|| err("the parrot server has no command"))?;
        let mut cmd = std::process::Command::new(command);
        for a in server["args"].as_array().into_iter().flatten() {
            cmd.arg(a.as_str().unwrap_or_default());
        }
        // `${VAR}` in the server's environment names a variable of ours, as Claude Code expands it.
        for (k, v) in server["env"].as_object().into_iter().flatten() {
            let v = v.as_str().unwrap_or_default();
            let v = match v.strip_prefix("${").and_then(|r| r.strip_suffix('}')) {
                Some(var) => std::env::var(var).unwrap_or_default(),
                None => v.to_owned(),
            };
            cmd.env(k, v);
        }
        if let Some(p) = std::env::var_os("PARROT_FAKE_CLAUDE_LOG")
            && let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
        {
            cmd.stderr(f);
        }
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| err("no stdin"))?;
        let stdout = std::io::BufReader::new(child.stdout.take().ok_or_else(|| err("no stdout"))?);
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        mcp.ask(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake-claude", "version": "0"}}),
        )?;
        writeln!(
            mcp.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )?;
        Ok(mcp)
    }

    fn ask(&mut self, method: &str, params: Value) -> std::io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )?;
        self.stdin.flush()?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(std::io::Error::other("mcp: the server closed"));
            }
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if v["id"] == id {
                return Ok(v);
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    vars: Vars,
    args: Vec<String>,
    mcp: Option<Mcp>,
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
                self.vars.request_id = Some(id.to_owned());
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
    run_with_args(steps, input, output, Vec::new())
}

/// Like [`run`], with the CLI's arguments, where `mcp_call` finds `--mcp-config`.
pub fn run_with_args<R: BufRead, W: Write>(steps: &[Step], input: R, output: W, args: Vec<String>) -> i32 {
    let mut run = Run {
        input,
        output,
        vars: Vars::default(),
        args,
        mcp: None,
    };
    let played = play(steps, &mut run).and_then(|code| match code {
        Some(code) => Ok(code),
        None => drain(&mut run),
    });
    match played {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fake-claude: {e}");
            1
        }
    }
}

/// Plays the steps; `Some(code)` when the scenario ends the process.
fn play<R: BufRead, W: Write>(steps: &[Step], run: &mut Run<R, W>) -> std::io::Result<Option<i32>> {
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Expect(pattern) => match run.next(|v| matches(pattern, v))? {
                Some(v) if matches(pattern, &v) => {}
                Some(v) => {
                    eprintln!("fake-claude: step {}: expected {pattern}, got {v}", i + 1);
                    return Ok(Some(MISMATCH_EXIT));
                }
                None => {
                    eprintln!(
                        "fake-claude: step {}: input closed while expecting {pattern}",
                        i + 1
                    );
                    return Ok(Some(MISMATCH_EXIT));
                }
            },
            Step::Emit(m) => {
                let m = run.vars.substitute(m);
                run.write(&m)?;
            }
            Step::AwaitControlResponse { request_id, pattern } => loop {
                let Some(v) = run.next(|_| false)? else {
                    eprintln!(
                        "fake-claude: step {}: input closed while awaiting {request_id}",
                        i + 1
                    );
                    return Ok(Some(MISMATCH_EXIT));
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
                    return Ok(Some(MISMATCH_EXIT));
                }
                break;
            },
            Step::Exit(code) => return Ok(Some(*code)),
            Step::Choose(options) => {
                let next = run.next(|v| options.iter().any(|(p, _)| matches(p, v)))?;
                let chosen = next
                    .as_ref()
                    .and_then(|v| options.iter().find(|(p, _)| matches(p, v)));
                let Some((_, file)) = chosen else {
                    eprintln!("fake-claude: step {}: no choice matches {next:?}", i + 1);
                    return Ok(Some(MISMATCH_EXIT));
                };
                let steps = load_scenario(file).map_err(std::io::Error::other)?;
                if let Some(code) = play(&steps, run)? {
                    return Ok(Some(code));
                }
            }
            Step::McpCall {
                call,
                save,
                until,
                attempts,
            } => {
                if run.mcp.is_none() {
                    run.mcp = Some(Mcp::start(&run.args)?);
                }
                let call = run.vars.substitute(call);
                let mut tries = 0;
                let result = loop {
                    tries += 1;
                    let Some(mcp) = run.mcp.as_mut() else {
                        unreachable!("started above")
                    };
                    let reply = mcp.ask("tools/call", call.clone())?;
                    let result = reply["result"]["structuredContent"].clone();
                    if reply["result"]["isError"] == true || reply.get("error").is_some() {
                        eprintln!("fake-claude: step {}: {call} failed: {reply}", i + 1);
                        return Ok(Some(MISMATCH_EXIT));
                    }
                    match until {
                        Some(p) if !matches(p, &result) => {
                            if tries >= *attempts {
                                eprintln!("fake-claude: step {}: {result} never matched {p}", i + 1);
                                return Ok(Some(MISMATCH_EXIT));
                            }
                            std::thread::sleep(std::time::Duration::from_secs(1));
                        }
                        _ => break result,
                    }
                };
                if let Some(name) = save {
                    run.vars.saved.insert(name.clone(), result);
                }
            }
            Step::Shell(cmd) => {
                let status = std::process::Command::new("sh")
                    .args(["-c", cmd])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .status()?;
                if !status.success() {
                    eprintln!("fake-claude: step {}: `{cmd}` failed: {status}", i + 1);
                    return Ok(Some(MISMATCH_EXIT));
                }
            }
        }
    }
    Ok(None)
}

/// After the scenario, answers what the host still sends until stdin closes.
fn drain<R: BufRead, W: Write>(run: &mut Run<R, W>) -> std::io::Result<i32> {
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
