//! A fake `pi --mode rpc` for tests: it answers the RPC commands and plays a scripted reply
//! instead of calling a model.
//!
//! A prompt is a script of tags, as the TypeScript port's `fake-pi.mjs` has them:
//!
//! | Tag | What the fake does |
//! | --- | --- |
//! | `[[text:...]]` | streams the text as one assistant message |
//! | `[[slow:40]]` | sets the delay between streamed words, in milliseconds |
//! | `[[think:...]]` | streams a finished reasoning block |
//! | `[[tool:name {"q":"x"}]]` | calls an MCP tool and reports its result |
//! | `[[mcp]]` | reports the MCP server the driver configured, and the header it would send |
//! | `[[bash:...]]` | runs a command as a tool |
//! | `[[confirm:title]]` | raises an extension `confirm` dialog |
//! | `[[select:title|A|B]]` | raises an extension `select` dialog |
//! | `[[sleep:500]]` | waits, ending early on an abort |
//! | `[[recall]]` | repeats what earlier prompts said |
//! | `[[fail:...]]` | ends the turn with an error |
//! | `[[crash]]` | exits with code 3 |
//!
//! `PARROT_CONFORM_SCENARIO` replaces the script with one the shared driver fixtures expect.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufWriter};
use tokio::sync::{mpsc, oneshot};

/// The variable that names a scenario the shared fixtures run.
pub const SCENARIO_ENV: &str = "PARROT_CONFORM_SCENARIO";
/// Where the fake keeps its sessions when `FAKE_PI_HOME` is not set.
pub const HOME_ENV: &str = "FAKE_PI_HOME";

/// What a run of the fake knows while a prompt is playing.
struct Run {
    aborted: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Fake {
    out: mpsc::UnboundedSender<String>,
    files: PathBuf,
    session_id: Arc<Mutex<String>>,
    model: Arc<Mutex<String>>,
    thinking: Arc<Mutex<String>>,
    said: Arc<Mutex<Vec<String>>>,
    dialogs: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    /// Set by an abort; the running script watches it.
    abort: Arc<AtomicBool>,
    counter: Arc<AtomicU64>,
}

impl Fake {
    fn send(&self, value: Value) {
        let _ = self.out.send(value.to_string());
    }

    fn reply(&self, id: &Value, command: &str, fields: Value) {
        let mut value = json!({"type": "response", "command": command});
        if !id.is_null() {
            value["id"] = id.clone();
        }
        if let Value::Object(fields) = fields {
            for (k, v) in fields {
                value[k] = v;
            }
        }
        self.send(value);
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(&self.files);
        let said = self.said.lock().unwrap().clone();
        let _ = std::fs::write(
            self.files
                .join(format!("{}.json", self.session_id.lock().unwrap())),
            serde_json::to_string(&said).unwrap_or_default(),
        );
    }

    fn next_id(&self, prefix: &str) -> String {
        format!(
            "{prefix}-{}-{}",
            std::process::id(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// The MCP server the driver configured, with the environment it names expanded as pi does.
    fn mcp_server(&self) -> Option<Value> {
        let dir = std::env::var_os("PI_CODING_AGENT_DIR")?;
        let text = std::fs::read_to_string(PathBuf::from(dir).join("mcp.json")).ok()?;
        let config: Value = serde_json::from_str(&text).ok()?;
        let server = config.get("mcpServers")?.as_object()?.values().next()?.clone();
        let url = server.get("url")?.as_str()?.to_owned();
        let header = server
            .get("headers")
            .and_then(|h| h.get("Authorization"))
            .and_then(Value::as_str)
            .map(expand)
            .unwrap_or_default();
        Some(json!({"url": url, "authorization": header}))
    }

    async fn dialog(&self, method: &str, fields: Value, run: &Run) -> Value {
        let id = self.next_id("ui");
        let (tx, mut rx) = oneshot::channel();
        self.dialogs.lock().unwrap().insert(id.clone(), tx);
        let mut request = json!({"type": "extension_ui_request", "id": id, "method": method});
        if let Value::Object(fields) = fields {
            for (k, v) in fields {
                request[k] = v;
            }
        }
        self.send(request);
        let mut ticker = tokio::time::interval(Duration::from_millis(20));
        loop {
            tokio::select! {
                answer = &mut rx => {
                    return answer.unwrap_or_else(|_| json!({"cancelled": true}));
                }
                _ = ticker.tick() => {
                    if run.aborted.load(Ordering::Relaxed) {
                        self.dialogs.lock().unwrap().remove(&id);
                        return json!({"cancelled": true});
                    }
                }
            }
        }
    }

    async fn text(&self, text: &str, delay_ms: u64, run: &Run) {
        self.send(json!({"type": "message_start", "message": {"role": "assistant", "content": [], "stopReason": "pending"}}));
        self.send(json!({"type": "message_update", "assistantMessageEvent": {"type": "text_start", "contentIndex": 0}}));
        let mut streamed = String::new();
        for word in words(text) {
            if run.aborted.load(Ordering::Relaxed) {
                return;
            }
            streamed.push_str(&word);
            self.send(json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": word},
            }));
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
        let _ = streamed;
        self.send(json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_end", "contentIndex": 0, "content": text},
        }));
        self.send(json!({"type": "message_end", "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stopReason": "stop",
            "usage": {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15, "cost": {"total": 0.0001}},
        }}));
    }

    async fn tool(&self, name: &str, args: Value, output: Result<String, String>) {
        let id = self.next_id("call");
        self.send(json!({"type": "tool_execution_start", "toolCallId": id, "toolName": name, "args": args}));
        self.send(match output {
            Ok(text) => json!({
                "type": "tool_execution_end", "toolCallId": id, "toolName": name,
                "result": {"content": [{"type": "text", "text": text}], "details": {}},
                "isError": false,
            }),
            Err(text) => json!({
                "type": "tool_execution_end", "toolCallId": id, "toolName": name,
                "result": {"content": [{"type": "text", "text": text}], "details": {}},
                "isError": true,
            }),
        });
    }

    /// Plays a prompt and ends the turn.
    async fn play(&self, text: String, images: usize) {
        let run = Arc::new(Run {
            aborted: self.abort.clone(),
        });
        self.send(json!({"type": "agent_start"}));
        self.send(json!({"type": "turn_start"}));
        self.send(json!({"type": "message_start", "message": {"role": "user", "content": text}}));
        self.send(json!({"type": "message_end", "message": {"role": "user", "content": text}}));
        let script = match std::env::var(SCENARIO_ENV).ok() {
            Some(scenario) => scenario_script(&scenario).to_owned(),
            None => text.clone(),
        };
        let tags = tags(&script);
        let mut delay = 0u64;
        let mut failed = false;
        if images > 0 {
            let word = if images == 1 { "image" } else { "images" };
            self.text(&format!("Received {images} {word}."), delay, &run)
                .await;
        }
        if tags.is_empty() {
            self.text(&format!("You said: {text}"), delay, &run).await;
        }
        for (op, arg) in tags {
            if run.aborted.load(Ordering::Relaxed) || failed {
                break;
            }
            match op.as_str() {
                "text" => self.text(&arg, delay, &run).await,
                "slow" => delay = arg.trim().parse().unwrap_or(0),
                "think" => {
                    self.send(json!({"type": "message_update", "assistantMessageEvent": {"type": "thinking_start", "contentIndex": 0}}));
                    self.send(json!({"type": "message_update", "assistantMessageEvent": {"type": "thinking_end", "contentIndex": 0, "content": arg}}));
                    self.send(json!({
                        "type": "message_end",
                        "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": arg}], "stopReason": "stop"},
                    }));
                }
                "tool" => {
                    let (name, input) = split_tool(&arg);
                    let args: Value = serde_json::from_str(&input).unwrap_or_else(|_| json!({}));
                    let result = match self.mcp_server() {
                        Some(server) => Ok(server.to_string()),
                        None => Err("no MCP server in mcp.json".to_owned()),
                    };
                    self.tool(&format!("mcp__parrot__{name}"), args, result).await;
                    self.text("The tool ran.", delay, &run).await;
                }
                "mcp" => {
                    let output = match self.mcp_server() {
                        Some(server) => Ok(server.to_string()),
                        None => Err("no MCP server in mcp.json".to_owned()),
                    };
                    self.tool("mcp__parrot__echo", json!({}), output).await;
                }
                "bash" => {
                    let output = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&arg)
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                        .map_err(|e| e.to_string());
                    self.tool("bash", json!({"command": arg}), output).await;
                    self.text("Ran it.", delay, &run).await;
                }
                "confirm" => {
                    let answer = self
                        .dialog(
                            "confirm",
                            json!({"title": arg, "message": "An extension asks first"}),
                            &run,
                        )
                        .await;
                    if run.aborted.load(Ordering::Relaxed) {
                        break;
                    }
                    match answer.get("confirmed").and_then(Value::as_bool) {
                        Some(true) => self.text("Confirmed.", delay, &run).await,
                        _ => self.text("Not confirmed.", delay, &run).await,
                    }
                }
                "select" => {
                    let mut parts = arg.split('|');
                    let title = parts.next().unwrap_or_default().to_owned();
                    let options: Vec<Value> = parts.map(|o| json!(o)).collect();
                    let answer = self
                        .dialog("select", json!({"title": title, "options": options}), &run)
                        .await;
                    if run.aborted.load(Ordering::Relaxed) {
                        break;
                    }
                    match answer.get("value").and_then(Value::as_str) {
                        Some(picked) => self.text(&format!("You picked {picked}."), delay, &run).await,
                        None => self.text("Nothing picked.", delay, &run).await,
                    }
                }
                "sleep" => {
                    let until =
                        std::time::Instant::now() + Duration::from_millis(arg.parse().unwrap_or(1000));
                    while std::time::Instant::now() < until && !run.aborted.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }
                "recall" => {
                    let before = {
                        let said = self.said.lock().unwrap();
                        said[..said.len().saturating_sub(1)].join(" / ")
                    };
                    let before = if before.is_empty() {
                        "nothing".to_owned()
                    } else {
                        before
                    };
                    self.text(&format!("Before this you said: {before}"), delay, &run)
                        .await;
                }
                "bash-approval" => {
                    let (id, confirmed) = self.bash_approval(&run).await;
                    if run.aborted.load(Ordering::Relaxed) {
                        break;
                    }
                    match confirmed {
                        Some(true) => {
                            self.tool_end(&id, true, "README.md\nsrc");
                            self.text("There are two entries.", delay, &run).await;
                        }
                        _ => {
                            self.tool_end(&id, false, "denied");
                            self.text("I did not run it.", delay, &run).await;
                        }
                    }
                }
                "stream-until-abort" => {
                    let mut n = 0u64;
                    let words = ["One, ", "two, ", "three, ", "four, ", "five, "];
                    self.send(json!({"type": "message_start", "message": {"role": "assistant", "content": [], "stopReason": "pending"}}));
                    self.send(json!({"type": "message_update", "assistantMessageEvent": {"type": "text_start", "contentIndex": 0}}));
                    while !run.aborted.load(Ordering::Relaxed) {
                        let word = words[(n as usize) % words.len()];
                        n += 1;
                        self.send(json!({
                            "type": "message_update",
                            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": word},
                        }));
                        tokio::time::sleep(Duration::from_millis(40)).await;
                    }
                }
                "fail" => {
                    failed = true;
                    self.send(json!({"type": "message_end", "message": {
                        "role": "assistant",
                        "content": [],
                        "stopReason": "error",
                        "errorMessage": arg,
                    }}));
                }
                "nested" => {
                    let id = self.next_id("call");
                    self.send(
                        json!({"type": "tool_execution_start", "toolCallId": id, "toolName": "task",
                        "args": {"description": "Review the notes"}}),
                    );
                    self.send(json!({
                        "type": "tool_execution_end", "toolCallId": id, "toolName": "task",
                        "result": {
                            "content": [{"type": "text", "text": "Two notes."}],
                            "details": {},
                            "nestedCalls": {
                                "complete": true,
                                "calls": [
                                    {"id": "inner-1", "name": "grep", "arguments": {"pattern": "note"}, "status": "ok", "durationMs": 3},
                                    {"id": "inner-2", "name": "read", "arguments": {"path": "notes.md"}, "status": "error", "error": "no such file"},
                                ],
                            },
                        },
                        "isError": false,
                    }));
                    self.text("The task looked.", delay, &run).await;
                }
                "crash" => {
                    std::process::exit(3);
                }
                _ => {}
            }
        }
        self.send(json!({"type": "agent_end", "messages": [], "willRetry": false}));
        self.send(json!({"type": "agent_settled", "aborted": run.aborted.load(Ordering::Relaxed)}));
    }

    async fn bash_approval(&self, run: &Run) -> (String, Option<bool>) {
        let id = self.next_id("call");
        self.send(json!({
            "type": "tool_execution_start", "toolCallId": id, "toolName": "bash",
            "args": {"command": "ls"},
        }));
        let answer = self
            .dialog(
                "confirm",
                json!({"title": "Run ls?", "message": "An extension asks first"}),
                run,
            )
            .await;
        (id, answer.get("confirmed").and_then(Value::as_bool))
    }

    fn tool_end(&self, id: &str, ok: bool, text: &str) {
        self.send(json!({
            "type": "tool_execution_end", "toolCallId": id, "toolName": "bash",
            "result": {"content": [{"type": "text", "text": text}], "details": {}},
            "isError": !ok,
        }));
    }
}

/// Expands `${NAME}` from the environment, as pi does for MCP headers.
fn expand(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                out.push_str(&std::env::var(name).unwrap_or_default());
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if c == ' ' {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The script a shared fixture expects.
fn scenario_script(name: &str) -> &'static str {
    match name {
        "text-turn" => "[[text:Hello there.]]",
        "bash-allowed" | "bash-denied" => "[[bash-approval]]",
        "interrupt" => "[[stream-until-abort]]",
        _ => "",
    }
}

/// The `[[op:arg]]` tags in a script, in order.
fn tags(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        let (op, arg) = match inner.split_once(':') {
            Some((op, arg)) => (op.to_owned(), arg.to_owned()),
            None => (inner.to_owned(), String::new()),
        };
        out.push((op, arg));
        rest = &after[end + 2..];
    }
    out
}

fn split_tool(arg: &str) -> (String, String) {
    match arg.split_once(' ') {
        Some((name, input)) => (name.to_owned(), input.to_owned()),
        None => (arg.to_owned(), "{}".to_owned()),
    }
}

/// Serves the fake on stdin and stdout until stdin closes.
pub async fn run() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("0.73.1 (fake-pi)");
        return Ok(());
    }
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let session_id = flag("--session-id").unwrap_or_else(|| format!("fake-{}", std::process::id()));
    let home = std::env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fake-pi"));
    let files = home.clone();
    let said = std::fs::read_to_string(files.join(format!("{session_id}.json")))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default();
    let (out, mut lines) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = BufWriter::new(tokio::io::stdout());
        while let Some(mut line) = lines.recv().await {
            line.push('\n');
            stdout.write_all(line.as_bytes()).await?;
            stdout.flush().await?;
        }
        std::io::Result::Ok(())
    });
    let fake = Fake {
        out,
        files,
        session_id: Arc::new(Mutex::new(session_id)),
        model: Arc::new(Mutex::new(
            flag("--model").unwrap_or_else(|| "fake/fake-1".into()),
        )),
        thinking: Arc::new(Mutex::new(flag("--thinking").unwrap_or_else(|| "off".into()))),
        said: Arc::new(Mutex::new(said)),
        dialogs: Arc::default(),
        abort: Arc::new(AtomicBool::new(false)),
        counter: Arc::new(AtomicU64::new(1)),
    };
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            fake.reply(
                &Value::Null,
                "parse",
                json!({"success": false, "error": "Failed to parse command"}),
            );
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        match kind.as_str() {
            "extension_ui_response" => {
                let dialog = message.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                if let Some(tx) = fake.dialogs.lock().unwrap().remove(&dialog) {
                    let _ = tx.send(message);
                }
            }
            "prompt" => {
                fake.reply(&id, "prompt", json!({"success": true, "data": {"disposition": "started"}}));
                let text = message.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
                let images = message
                    .get("images")
                    .and_then(Value::as_array)
                    .map(|i| i.len())
                    .unwrap_or(0);
                fake.said.lock().unwrap().push(text.clone());
                fake.save();
                fake.abort.store(false, Ordering::Relaxed);
                let f = fake.clone();
                tokio::spawn(async move { f.play(text, images).await });
            }
            "steer" => {
                fake.reply(&id, "steer", json!({"success": true, "data": {"disposition": "queued"}}));
                let text = message.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
                fake.said.lock().unwrap().push(text);
            }
            "abort" => {
                fake.abort.store(true, Ordering::Relaxed);
                for (_, tx) in fake.dialogs.lock().unwrap().drain() {
                    let _ = tx.send(json!({"cancelled": true}));
                }
                fake.reply(&id, "abort", json!({"success": true}));
                // The play task reads the same flag; it is set before this reply.
            }
            "get_state" => {
                let model = fake.model.lock().unwrap().clone();
                let (provider, name) = match model.split_once('/') {
                    Some((p, n)) => (p.to_owned(), n.to_owned()),
                    None => (model.clone(), model.clone()),
                };
                fake.reply(&id, "get_state", json!({"success": true, "data": {
                    "model": {"id": name, "name": "Fake 1", "provider": provider},
                    "thinkingLevel": fake.thinking.lock().unwrap().clone(),
                    "isStreaming": false,
                    "sessionId": fake.session_id.lock().unwrap().clone(),
                    "messageCount": fake.said.lock().unwrap().len(),
                }}));
            }
            "get_available_models" => fake.reply(&id, "get_available_models", json!({"success": true, "data": {"models": [
                {"id": "fake-1", "name": "Fake 1", "provider": "fake", "reasoning": true, "input": ["text", "image"], "contextWindow": 100000},
                {"id": "fake-2", "name": "Fake 2", "provider": "fake", "reasoning": false, "input": ["text"], "contextWindow": 50000},
            ]}})),
            "get_available_thinking_levels" => fake.reply(
                &id,
                "get_available_thinking_levels",
                json!({"success": true, "data": {"levels": ["off", "low", "medium", "high"]}}),
            ),
            "get_commands" => fake.reply(&id, "get_commands", json!({"success": true, "data": {"commands": [
                {"name": "review", "description": "Review the changes", "source": "prompt"},
                {"name": "skill:fake-skill", "description": "A skill the fake offers", "source": "skill"},
            ]}})),
            "set_model" => {
                let provider = message.get("provider").and_then(Value::as_str).unwrap_or("fake");
                let name = message.get("modelId").and_then(Value::as_str).unwrap_or("fake-1");
                *fake.model.lock().unwrap() = format!("{provider}/{name}");
                fake.reply(&id, "set_model", json!({"success": true, "data": {"id": name, "provider": provider, "name": name}}));
            }
            "set_thinking_level" => {
                let level = message.get("level").and_then(Value::as_str).unwrap_or("off");
                *fake.thinking.lock().unwrap() = level.to_owned();
                fake.reply(&id, "set_thinking_level", json!({"success": true}));
            }
            "compact" => {
                fake.reply(
                    &id,
                    "compact",
                    json!({"success": true, "data": {
                        "summary": "The context was compacted.",
                        "firstKeptEntryId": "entry-1",
                        "tokensBefore": 1200,
                        "estimatedTokensAfter": 300,
                        "usage": {"input": 20, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 25},
                    }}),
                );
            }
            "get_fork_messages" => {
                let messages: Vec<Value> = fake
                    .said
                    .lock()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .map(|(i, text)| json!({"entryId": format!("entry-{}", i + 1), "text": text}))
                    .collect();
                fake.reply(&id, "get_fork_messages", json!({"success": true, "data": {"messages": messages}}));
            }
            "fork" => {
                let entry = message.get("entryId").and_then(Value::as_str).unwrap_or("entry-1");
                let index: usize = entry.trim_start_matches("entry-").parse().unwrap_or(1);
                let text = fake
                    .said
                    .lock()
                    .unwrap()
                    .get(index.saturating_sub(1))
                    .cloned()
                    .unwrap_or_default();
                let forked = fake.next_id("forked");
                fake.reply(&id, "fork", json!({"success": true, "data": {"text": text, "cancelled": false}}));
                // A fork switches to a new session in the same process.
                *fake.session_id.lock().unwrap() = forked.clone();
                fake.save();
            }
            other => fake.reply(
                &id,
                other,
                json!({"success": false, "error": format!("Unknown command: {other}")}),
            ),
        }
    }
    // Let a running script notice the closed stdin and the writer flush.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(fake);
    let _ = writer.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags() {
        let t = tags("a [[text:hi there]] b [[slow:40]]");
        assert_eq!(
            t,
            vec![
                ("text".to_owned(), "hi there".to_owned()),
                ("slow".to_owned(), "40".to_owned())
            ]
        );
    }

    #[test]
    fn expands_environment() {
        unsafe { std::env::set_var("FAKE_PI_EXPAND_TEST", "v1") };
        assert_eq!(expand("Bearer ${FAKE_PI_EXPAND_TEST}"), "Bearer v1");
        assert_eq!(expand("plain"), "plain");
    }
}
