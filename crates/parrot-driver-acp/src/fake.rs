//! A fake ACP agent for tests: it plays a script instead of calling a model.
//!
//! The script is the scenario file's text (or the prompt text when there is no scenario). A
//! directive is `[[op]]` or `[[op:argument]]`:
//!
//! | Directive | What the fake does |
//! | --- | --- |
//! | `[[text:Hello there.]]` | streams the text as an assistant message |
//! | `[[slow:40]]` | waits 40 ms between streamed words from now on |
//! | `[[think:...]]` | streams a reasoning chunk |
//! | `[[perm:ls]]` | a tool call that asks for permission; allowed prints `README.md` and `src` and says `There are two entries.`, denied says `I did not run it.` |
//! | `[[loop:Counted]]` | streams the word until cancelled |
//! | `[[plan:One|Two]]` | sends a plan |
//! | `[[sleep:200]]` | waits, unless cancelled |
//! | `[[refuse]]` | ends the turn with `stopReason: "refusal"` |
//! | `[[crash]]` | writes to stderr and exits with code 3 |
//!
//! Without directives the fake answers `You said: <text>`. It advertises images, embedded context,
//! MCP over HTTP, resume and load, and it streams every chunk with a message id.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// The variable that names a scenario when `--scenario` is not given.
pub const SCENARIO_ENV: &str = "PARROT_FAKE_ACP_SCENARIO";

/// The exit code for a scenario that could not run.
pub const FAILED_EXIT: i32 = 2;

/// The scenario in `args` (`--scenario <path>` or `--scenario=<path>`); other flags are ignored.
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

/// A script: its text without the comment lines.
pub fn load_script(path: &std::path::Path) -> std::io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[derive(Debug, Clone, PartialEq)]
struct Directive {
    op: String,
    arg: String,
}

fn directives(script: &str) -> Vec<Directive> {
    let mut out = Vec::new();
    let mut rest = script;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let body = &after[..end];
        let (op, arg) = match body.find(':') {
            Some(i) => (body[..i].to_owned(), body[i + 1..].to_owned()),
            None => (body.to_owned(), String::new()),
        };
        out.push(Directive { op, arg });
        rest = &after[end + 2..];
    }
    out
}

/// The text of a prompt's content blocks, as the fake sees it.
fn prompt_text(params: &Value) -> String {
    params
        .get("prompt")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .map(|b| match b.get("type").and_then(Value::as_str) {
                    Some("text") => b
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    Some("resource") => format!(
                        "[{}]",
                        b.pointer("/resource/uri")
                            .and_then(Value::as_str)
                            .unwrap_or("resource")
                    ),
                    Some(other) => format!("[{other}]"),
                    None => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

struct Session {
    id: String,
    /// Recorded, as the real agent would: the prompt capability tests inspect neither.
    _cwd: String,
    mode: String,
    model: String,
    /// The MCP servers `session/new` carried.
    _mcp: Vec<Value>,
}

struct Shared<W: Write + Send + 'static> {
    out: Mutex<W>,
    session: Mutex<Option<Session>>,
    /// The running turn's cancel flag.
    turn: Mutex<Option<Arc<AtomicBool>>>,
    waiting: Mutex<HashMap<String, std::sync::mpsc::Sender<Value>>>,
    requests: AtomicU64,
    messages: AtomicU64,
    chunks: AtomicU64,
    /// The id of the `session/prompt` request the running turn answers.
    pending_prompt: Mutex<Option<Value>>,
}

impl<W: Write + Send + 'static> Shared<W> {
    fn send(&self, mut value: Value) {
        if value.get("jsonrpc").is_none() {
            value["jsonrpc"] = json!("2.0");
        }
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    }

    fn session_id(&self) -> String {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_default()
    }

    fn update(&self, update: Value) {
        self.send(json!({
            "method": "session/update",
            "params": { "sessionId": self.session_id(), "update": update },
        }));
    }

    fn chunk(&self, kind: &str, text: &str, message_id: Option<&str>) {
        let mut update = json!({
            "sessionUpdate": kind,
            "content": { "type": "text", "text": text },
        });
        if let Some(id) = message_id {
            update["messageId"] = json!(id);
        }
        self.update(update);
    }

    /// Streams `text` word by word, stopping when the turn is cancelled.
    fn stream(&self, text: &str, delay_ms: u64, cancelled: &AtomicBool) {
        let message_id = format!("msg_{}", self.messages.fetch_add(1, Ordering::Relaxed) + 1);
        let mut word = String::new();
        for c in text.chars() {
            word.push(c);
            if c == ' ' {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                self.chunk("agent_message_chunk", &word, Some(&message_id));
                word.clear();
                if delay_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
        }
        if !word.is_empty() && !cancelled.load(Ordering::Relaxed) {
            self.chunk("agent_message_chunk", &word, Some(&message_id));
        }
    }

    /// Asks the client a permission request and waits for its answer.
    fn ask(&self, call: &Value) -> Value {
        let id = format!("agent-{}", self.requests.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = std::sync::mpsc::channel();
        self.waiting.lock().unwrap().insert(id.clone(), tx);
        self.send(json!({
            "id": id,
            "method": "session/request_permission",
            "params": {
                "sessionId": self.session_id(),
                "toolCall": call,
                "options": [
                    { "optionId": "allow_always", "name": "Always allow", "kind": "allow_always" },
                    { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
                ],
            },
        }));
        loop {
            if let Ok(answer) = rx.recv_timeout(std::time::Duration::from_millis(25)) {
                return answer;
            }
            let cancelled = self
                .turn
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed));
            if cancelled {
                self.waiting.lock().unwrap().remove(&id);
                return json!({ "outcome": { "outcome": "cancelled" } });
            }
        }
    }

    fn run_script(&self, script: &str, cancelled: &AtomicBool) {
        let list = directives(script);
        let mut delay = 0u64;
        let mut stop = "end_turn";
        if list.is_empty() {
            let text = script.trim();
            if !text.is_empty() {
                self.stream(text, delay, cancelled);
            }
            self.finish_turn(stop, cancelled);
            return;
        }
        for directive in list {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            match directive.op.as_str() {
                "text" => self.stream(&directive.arg, delay, cancelled),
                "slow" => delay = directive.arg.parse().unwrap_or(0),
                "think" => self.chunk("agent_thought_chunk", &directive.arg, None),
                "perm" => {
                    let command = directive.arg;
                    let call = json!({
                        "toolCallId": format!("toolu_{}", self.chunks.fetch_add(1, Ordering::Relaxed) + 1),
                        "title": format!("`{command}`"),
                        "kind": "execute",
                        "status": "pending",
                        "rawInput": { "command": command },
                    });
                    let mut announced = call.clone();
                    announced["sessionUpdate"] = json!("tool_call");
                    self.update(announced);
                    let answer = self.ask(&call);
                    let outcome = answer.pointer("/outcome/outcome").and_then(Value::as_str);
                    let option = answer.pointer("/outcome/optionId").and_then(Value::as_str);
                    let tool_call_id = call["toolCallId"].clone();
                    if outcome == Some("selected") && option != Some("reject") {
                        self.update(json!({
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": tool_call_id,
                            "status": "completed",
                            "content": [{ "type": "content", "content": { "type": "text", "text": "README.md\nsrc" } }],
                        }));
                        self.stream("There are two entries.", delay, cancelled);
                    } else {
                        self.update(json!({
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": tool_call_id,
                            "status": "failed",
                            "content": [{ "type": "content", "content": { "type": "text", "text": "The user rejected it." } }],
                        }));
                        self.stream("I did not run it.", delay, cancelled);
                    }
                }
                "loop" => {
                    for _ in 0..100 {
                        if cancelled.load(Ordering::Relaxed) {
                            break;
                        }
                        self.chunk("agent_message_chunk", &format!("{} ", directive.arg), None);
                        std::thread::sleep(std::time::Duration::from_millis(40));
                    }
                }
                "plan" => {
                    let entries: Vec<Value> = directive
                        .arg
                        .split('|')
                        .enumerate()
                        .map(|(i, text)| {
                            json!({
                                "content": text,
                                "priority": "medium",
                                "status": if i == 0 { "in_progress" } else { "pending" },
                            })
                        })
                        .collect();
                    self.update(json!({ "sessionUpdate": "plan", "entries": entries }));
                }
                "sleep" => {
                    let until = std::time::Instant::now()
                        + std::time::Duration::from_millis(directive.arg.parse().unwrap_or(1000));
                    while std::time::Instant::now() < until && !cancelled.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                }
                "refuse" => stop = "refusal",
                "crash" => {
                    eprintln!("fake-acp: crashing as asked");
                    std::process::exit(3);
                }
                _ => {}
            }
        }
        self.finish_turn(stop, cancelled);
    }

    fn finish_turn(&self, stop: &str, cancelled: &AtomicBool) {
        self.update(json!({
            "sessionUpdate": "usage_update",
            "used": 1234,
            "size": 200000,
            "cost": { "amount": 0.001, "currency": "USD" },
        }));
        let cancelled = cancelled.load(Ordering::Relaxed);
        *self.turn.lock().unwrap() = None;
        self.send(json!({
            "id": self.pending_prompt.lock().unwrap().take().unwrap_or(Value::Null),
            "result": {
                "stopReason": if cancelled { "cancelled" } else { stop },
                "usage": {
                    "inputTokens": 10,
                    "outputTokens": 20,
                    "cachedReadTokens": 5,
                    "cachedWriteTokens": 0,
                    "totalTokens": 35,
                },
            },
        }));
    }
}

/// Plays ACP over `input` and `output`; the script is used for each prompt.
pub fn run<R: BufRead, W: Write + Send + 'static>(script: Option<String>, input: R, output: W) -> i32 {
    let shared = Arc::new(Shared {
        out: Mutex::new(output),
        session: Mutex::default(),
        turn: Mutex::default(),
        waiting: Mutex::default(),
        requests: AtomicU64::new(0),
        messages: AtomicU64::new(0),
        chunks: AtomicU64::new(0),
        pending_prompt: Mutex::new(None),
    });
    for line in input.lines() {
        let Ok(line) = line else { break };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to the fake's permission request.
            if let Some(id) = msg.get("id").map(|i| i.to_string().trim_matches('"').to_owned())
                && let Some(tx) = shared.waiting.lock().unwrap().remove(&id)
            {
                let _ = tx.send(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let id = msg.get("id").cloned();
        match method {
            "initialize" => respond(
                &shared,
                id,
                json!({
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": true,
                        "promptCapabilities": { "image": true, "embeddedContext": true },
                        "mcpCapabilities": { "http": true, "sse": false },
                        "sessionCapabilities": { "resume": {} },
                    },
                    "agentInfo": { "name": "fake-acp", "title": "Fake ACP agent", "version": "0.0.0" },
                    "authMethods": [],
                }),
            ),
            "session/new" => {
                let id_value = format!(
                    "fake-session-{}",
                    shared.messages.fetch_add(1, Ordering::Relaxed) + 1
                );
                *shared.session.lock().unwrap() = Some(Session {
                    id: id_value.clone(),
                    _cwd: params
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    mode: "default".into(),
                    model: "fake-small".into(),
                    _mcp: params
                        .get("mcpServers")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                });
                respond(
                    &shared,
                    id,
                    json!({
                        "sessionId": id_value,
                        "modes": {
                            "currentModeId": "default",
                            "availableModes": [
                                { "id": "default", "name": "Manual", "description": "Always ask before making changes" },
                                { "id": "bypassPermissions", "name": "Bypass permissions", "description": "Accepts all permissions" },
                            ],
                        },
                        "configOptions": config_options("fake-small"),
                    }),
                );
            }
            "session/load" | "session/resume" => {
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("fake-session")
                    .to_owned();
                *shared.session.lock().unwrap() = Some(Session {
                    id: session_id,
                    _cwd: params
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    mode: "default".into(),
                    model: "fake-small".into(),
                    _mcp: params
                        .get("mcpServers")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                });
                respond(
                    &shared,
                    id,
                    json!({
                        "modes": {
                            "currentModeId": "default",
                            "availableModes": [
                                { "id": "default", "name": "Manual" },
                                { "id": "bypassPermissions", "name": "Bypass permissions" },
                            ],
                        },
                        "configOptions": config_options("fake-small"),
                    }),
                );
            }
            "session/set_mode" => {
                if let Some(session) = shared.session.lock().unwrap().as_mut() {
                    session.mode = params
                        .get("modeId")
                        .and_then(Value::as_str)
                        .unwrap_or("default")
                        .to_owned();
                }
                respond(&shared, id, json!({}));
            }
            "session/set_config_option" => {
                let value = params
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or("fake-small");
                if let Some(session) = shared.session.lock().unwrap().as_mut() {
                    session.model = value.to_owned();
                }
                respond(&shared, id, json!({ "configOptions": config_options(value) }));
            }
            "session/set_model" => respond(&shared, id, json!({})),
            "session/cancel" => {
                if let Some(cancel) = shared.turn.lock().unwrap().as_ref() {
                    cancel.store(true, Ordering::Relaxed);
                }
                // The protocol: every open permission request is answered `cancelled`.
                for (id, _) in shared.waiting.lock().unwrap().drain() {
                    shared.send(json!({
                        "id": id,
                        "result": { "outcome": { "outcome": "cancelled" } },
                    }));
                }
            }
            "session/prompt" => {
                if shared.turn.lock().unwrap().is_some() {
                    respond_after_error(&shared, id, -32600, "a prompt is running".into());
                    continue;
                }
                let text = prompt_text(&params);
                let script = script.clone().unwrap_or(text);
                let cancel = Arc::new(AtomicBool::new(false));
                *shared.turn.lock().unwrap() = Some(cancel.clone());
                *shared.pending_prompt.lock().unwrap() = id;
                let shared2 = shared.clone();
                std::thread::spawn(move || shared2.run_script(&script, &cancel));
            }
            other => {
                if let Some(id) = id {
                    respond_after_error(&shared, Some(id), -32601, format!("Method not found: {other}"));
                }
            }
        }
    }
    0
}

fn respond<W: Write + Send + 'static>(shared: &Shared<W>, id: Option<Value>, result: Value) {
    if let Some(id) = id {
        shared.send(json!({ "id": id, "result": result }));
    }
}

fn respond_after_error<W: Write + Send + 'static>(
    shared: &Shared<W>,
    id: Option<Value>,
    code: i64,
    message: String,
) {
    if let Some(id) = id {
        shared.send(json!({ "id": id, "error": { "code": code, "message": message } }));
    }
}

fn config_options(current: &str) -> Value {
    json!([{
        "id": "model",
        "name": "Model",
        "category": "model",
        "type": "select",
        "currentValue": current,
        "options": [
            { "value": "fake-small", "name": "Fake small", "description": "Quick" },
            { "value": "fake-large", "name": "Fake large" },
        ],
    }])
}
