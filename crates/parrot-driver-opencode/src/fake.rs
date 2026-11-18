//! A fake `opencode serve` for tests: an HTTP server with the routes the driver calls and a
//! `/api/event` stream that plays a scripted reply instead of calling a model.
//!
//! A prompt is a script of tags, like the fake pi's:
//!
//! | Tag | What the fake does |
//! | --- | --- |
//! | `[[text:...]]` | streams the text as one assistant message |
//! | `[[slow:40]]` | sets the delay between streamed words, in milliseconds |
//! | `[[think:...]]` | streams a finished reasoning block |
//! | `[[tool:bash {"command":"ls"}]]` | reports a tool call and its result |
//! | `[[permission:bash|ls -la]]` | asks for permission and waits for the reply |
//! | `[[image]]` | says it received the image |
//! | `[[sleep:500]]` | waits, ending early on an interrupt |
//! | `[[stream-until-abort]]` | streams words until an interrupt arrives |
//! | `[[bash-approval]]` | the shared fixture's permission, tool and text |
//! | `[[mcp]]` | reports the MCP server the driver configured, with the token expanded |
//! | `[[fail:...]]` | ends the turn with a failure event |
//!
//! `PARROT_CONFORM_SCENARIO` replaces the script with one the shared driver fixtures expect.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot};

/// The variable that names a scenario the shared fixtures run.
pub const SCENARIO_ENV: &str = "PARROT_CONFORM_SCENARIO";
/// The variable the driver sets to the inline config, where the MCP server goes.
pub const CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";
/// The variable the fake checks the server password against.
pub const PASSWORD_ENV: &str = "OPENCODE_PASSWORD";

/// What a run of the fake remembers between requests.
struct Fake {
    password: String,
    events: broadcast::Sender<Value>,
    permissions: Mutex<HashMap<String, oneshot::Sender<String>>>,
    abort: Arc<AtomicBool>,
    model: Mutex<String>,
    counter: AtomicU64,
    /// Every request path the fake answered, for the tests.
    seen: Mutex<Vec<(String, String)>>,
    /// The prompts it was sent, in order.
    prompts: Mutex<Vec<String>>,
}

impl Fake {
    fn emit(&self, kind: &str, data: Value) {
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let event = json!({
            "id": format!("evt_{id}"),
            "created": 1_700_000_000_000u64 + id,
            "type": kind,
            "data": data,
        });
        let _ = self.events.send(event);
    }

    fn record(&self, method: &str, path: &str) {
        self.seen
            .lock()
            .unwrap()
            .push((method.to_owned(), path.to_owned()));
    }

    fn next_id(&self, prefix: &str) -> String {
        source_id(prefix, self.counter.fetch_add(1, Ordering::Relaxed))
    }

    /// The MCP server the driver embedded in the inline config, with `{env:...}` expanded as
    /// opencode does it.
    fn mcp_server(&self) -> Option<Value> {
        let text = std::env::var(CONFIG_ENV).ok()?;
        let config: Value = serde_json::from_str(&text).ok()?;
        let server = config
            .get("mcp")?
            .get("servers")?
            .as_object()?
            .values()
            .next()?
            .clone();
        let url = server.get("url").and_then(Value::as_str).map(str::to_owned);
        let header = server
            .get("headers")
            .and_then(|h| h.get("Authorization"))
            .and_then(Value::as_str)
            .map(expand)
            .unwrap_or_default();
        match url {
            Some(url) => Some(json!({"url": url, "authorization": header})),
            None => Some(json!({"command": server.get("command"), "authorization": header})),
        }
    }

    /// Sends an event and remembers it was the assistant's text so far.
    fn text(&self, session: &str, message: &str, text: &str, delay: u64, abort: &AtomicBool) {
        self.emit(
            "session.text.started",
            json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0}),
        );
        for word in words(text) {
            if abort.load(Ordering::Relaxed) {
                return;
            }
            self.emit(
                "session.text.delta",
                json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0, "delta": word}),
            );
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
        self.emit(
            "session.text.ended",
            json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0, "text": text}),
        );
    }

    /// Reports a tool call that ran without asking.
    fn tool(&self, session: &str, message: &str, name: &str, input: Value, output: &str) {
        let id = self.tool_called(session, message, name, input);
        self.tool_end(session, message, &id, true, output);
    }

    fn tool_called(&self, session: &str, message: &str, name: &str, input: Value) -> String {
        let id = self.next_id("call");
        self.emit(
            "session.tool.called",
            json!({"sessionID": session, "assistantMessageID": message, "id": id, "input": input,
                "executed": true, "native": {"tool": name}}),
        );
        id
    }

    fn tool_end(&self, session: &str, message: &str, id: &str, ok: bool, output: &str) {
        let mut data = json!({"sessionID": session, "assistantMessageID": message, "id": id,
            "content": [{"type": "text", "text": output}], "executed": true});
        if !ok {
            data["error"] = json!({"type": "PermissionDenied", "message": "denied"});
        }
        self.emit(
            if ok {
                "session.tool.success"
            } else {
                "session.tool.failed"
            },
            data,
        );
    }

    /// Raises a permission and waits for the reply; `None` when the turn is interrupted first.
    fn permission(&self, session: &str, action: &str, resource: &str, abort: &AtomicBool) -> Option<String> {
        let id = self.next_id("per");
        let (tx, mut rx) = oneshot::channel();
        self.permissions.lock().unwrap().insert(id.clone(), tx);
        self.emit(
            "permission.asked",
            json!({"id": id, "sessionID": session, "action": action,
                "resources": [resource], "save": [], "message": format!("Run {action}?")}),
        );
        loop {
            match rx.try_recv() {
                Ok(decision) => {
                    self.emit(
                        "permission.replied",
                        json!({"sessionID": session, "requestID": id, "reply": decision}),
                    );
                    return Some(decision);
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => return None,
            }
            if abort.load(Ordering::Relaxed) {
                self.permissions.lock().unwrap().remove(&id);
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn begin(&self, session: &str) -> String {
        let message = self.next_id("msg");
        self.emit("session.execution.started", json!({"sessionID": session}));
        self.emit(
            "session.step.started",
            json!({"sessionID": session, "assistantMessageID": message, "agent": "build",
                "model": {"providerID": "fake", "id": "fake-1"}, "started": 0}),
        );
        message
    }

    fn end(&self, session: &str, message: &str, failed: Option<String>) {
        match failed {
            Some(text) => {
                self.emit(
                    "session.execution.failed",
                    json!({"sessionID": session, "error": {"type": "UnknownError", "message": text}}),
                );
            }
            None => {
                self.emit(
                    "session.step.ended",
                    json!({"sessionID": session, "assistantMessageID": message, "finish": "stop",
                        "cost": 0.0001, "tokens": {"input": 10, "output": 5, "reasoning": 0,
                            "cache": {"read": 2, "write": 0}}}),
                );
                self.emit("session.execution.succeeded", json!({"sessionID": session}));
            }
        }
        self.emit("session.idle", json!({"sessionID": session}));
    }

    /// Plays a prompt and ends the turn.
    fn play(&self, session: String, text: String, session_files: usize) {
        let abort = self.abort.clone();
        abort.store(false, Ordering::Relaxed);
        let script = match std::env::var(SCENARIO_ENV).ok() {
            Some(scenario) => scenario_script(&scenario).to_owned(),
            None => text.clone(),
        };
        let message = self.begin(&session);
        let tags = tags(&script);
        let mut delay = 0u64;
        if tags.is_empty() && session_files > 0 {
            let word = if session_files == 1 { "image" } else { "images" };
            self.text(
                &session,
                &message,
                &format!("Received {session_files} {word}."),
                0,
                &abort,
            );
        }
        if tags.is_empty() {
            self.text(&session, &message, &format!("You said: {text}"), delay, &abort);
        }
        let mut failed = None;
        for (op, arg) in tags {
            if abort.load(Ordering::Relaxed) {
                break;
            }
            match op.as_str() {
                "text" => self.text(&session, &message, &arg, delay, &abort),
                "slow" => delay = arg.trim().parse().unwrap_or(0),
                "think" => {
                    self.emit(
                        "session.reasoning.started",
                        json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0}),
                    );
                    self.emit(
                        "session.reasoning.ended",
                        json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0, "text": arg}),
                    );
                }
                "tool" => {
                    let (name, input) = match arg.split_once(' ') {
                        Some((name, input)) => (
                            name.to_owned(),
                            serde_json::from_str::<Value>(input).unwrap_or_else(|_| json!({})),
                        ),
                        None => (arg.clone(), json!({})),
                    };
                    let output = format!("ran {name}");
                    self.tool(&session, &message, &name, input, &output);
                }
                "permission" => {
                    let (action, resource) = arg.split_once('|').unwrap_or((arg.as_str(), ""));
                    match self.permission(&session, action, resource, &abort) {
                        Some(decision) if decision != "reject" => {
                            self.tool(
                                &session,
                                &message,
                                action,
                                json!({"command": resource}),
                                "allowed",
                            );
                            self.text(&session, &message, "It ran.", delay, &abort);
                        }
                        Some(_) => {
                            let id =
                                self.tool_called(&session, &message, action, json!({"command": resource}));
                            self.tool_end(&session, &message, &id, false, "denied");
                            self.text(&session, &message, "It did not run.", delay, &abort);
                        }
                        None => break,
                    }
                }
                "bash-approval" => {
                    // The tool call is pending when the permission is asked, as opencode reports it.
                    let id = self.tool_called(&session, &message, "bash", json!({"command": "ls"}));
                    match self.permission(&session, "bash", "ls", &abort) {
                        Some(decision) if decision != "reject" => {
                            self.tool_end(&session, &message, &id, true, "README.md\nsrc");
                            self.text(&session, &message, "There are two entries.", delay, &abort);
                        }
                        Some(_) => {
                            self.tool_end(&session, &message, &id, false, "denied");
                            self.text(&session, &message, "I did not run it.", delay, &abort);
                        }
                        None => break,
                    }
                }
                "image" => self.text(&session, &message, "I see it.", delay, &abort),
                "mcp" => {
                    let output = match self.mcp_server() {
                        Some(server) => server.to_string(),
                        None => "no MCP server in the config".to_owned(),
                    };
                    self.tool(&session, &message, "mcp_parrot_echo", json!({}), &output);
                }
                "sleep" => {
                    let until =
                        std::time::Instant::now() + Duration::from_millis(arg.parse().unwrap_or(1000));
                    while std::time::Instant::now() < until && !abort.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                "stream-until-abort" => {
                    self.emit(
                        "session.text.started",
                        json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0}),
                    );
                    let words = ["One, ", "two, ", "three, ", "four, ", "five, "];
                    let mut n = 0usize;
                    while !abort.load(Ordering::Relaxed) {
                        self.emit(
                            "session.text.delta",
                            json!({"sessionID": session, "assistantMessageID": message, "ordinal": 0,
                                "delta": words[n % words.len()]}),
                        );
                        n += 1;
                        std::thread::sleep(Duration::from_millis(40));
                    }
                }
                "fail" => {
                    failed = Some(arg.clone());
                    break;
                }
                _ => {}
            }
        }
        if abort.load(Ordering::Relaxed) {
            self.emit(
                "session.execution.interrupted",
                json!({"sessionID": session, "reason": "user"}),
            );
            self.emit("session.idle", json!({"sessionID": session}));
            return;
        }
        self.end(&session, &message, failed);
    }
}

fn source_id(prefix: &str, n: u64) -> String {
    format!("{prefix}_{}_{n}", std::process::id())
}

/// Expands `{env:NAME}` from the environment, as opencode does for its config.
fn expand(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{env:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 5..];
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

/// Serves the fake until it is killed.
pub async fn run() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let host = flag("--hostname").unwrap_or_else(|| "127.0.0.1".into());
    let port = flag("--port").unwrap_or_else(|| "0".into());
    let password = std::env::var(PASSWORD_ENV).unwrap_or_default();
    let listener = TcpListener::bind(format!("{host}:{port}")).await?;
    let bound = listener.local_addr()?;
    let (events, _) = broadcast::channel::<Value>(1024);
    let fake = Arc::new(Fake {
        password,
        events,
        permissions: Mutex::new(HashMap::new()),
        abort: Arc::new(AtomicBool::new(false)),
        model: Mutex::new("fake/fake-1".into()),
        counter: AtomicU64::new(1),
        seen: Mutex::new(Vec::new()),
        prompts: Mutex::new(Vec::new()),
    });
    println!("server listening on http://{}", bound);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    loop {
        let (socket, _) = listener.accept().await?;
        let fake = fake.clone();
        tokio::spawn(async move {
            let _ = serve_one(fake, socket).await;
        });
    }
}

/// One request over one connection.
async fn serve_one(fake: Arc<Fake>, socket: TcpStream) -> std::io::Result<()> {
    let (read, mut socket) = socket.into_split();
    let mut reader = BufReader::new(read);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await? == 0 {
        return Ok(());
    }
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let length: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await?;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    fake.record(&method, &path);
    let authorized = !fake.password.is_empty()
        && headers
            .get("authorization")
            .map(|a| !a.is_empty() && a == &basic_auth(&fake.password))
            .unwrap_or(false);
    if !authorized {
        return reply(&mut socket, 401, &json!({"error": "unauthorized"})).await;
    }
    if method == "GET" && path == "/api/event" {
        return stream(fake, socket).await;
    }
    let answer = route(&fake, &method, &path, &body);
    match answer {
        Some(value) => reply(&mut socket, 200, &value).await,
        None => reply(&mut socket, 404, &json!({"error": "not found"})).await,
    }
}

fn basic_auth(password: &str) -> String {
    format!("Basic {}", base64(format!("opencode:{password}").as_bytes()))
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The answer for a route, or `None` when there is none.
fn route(fake: &Arc<Fake>, method: &str, path: &str, body: &Value) -> Option<Value> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, segments.as_slice()) {
        ("GET", ["api", "model"]) => Some(json!({"data": [
            {"modelID": "fake-1", "providerID": "fake", "name": "Fake 1", "enabled": true,
             "capabilities": {"tools": true, "input": ["text", "image"], "output": ["text"]},
             "variants": [], "time": {"released": 0}, "cost": [], "status": "active",
             "limit": {"context": 100000, "output": 4096}},
            {"modelID": "fake-2", "providerID": "fake", "name": "Fake 2", "enabled": true,
             "capabilities": {"tools": true, "input": ["text"], "output": ["text"]},
             "variants": [], "time": {"released": 0}, "cost": [], "status": "active",
             "limit": {"context": 50000, "output": 4096}},
        ]})),
        ("GET", ["api", "provider"]) => Some(json!({"data": [
            {"id": "fake", "name": "Fake", "activation": "enabled", "package": "test"},
            {"id": "other", "name": "Other", "activation": "enabled", "package": "test"},
        ]})),
        ("GET", ["api", "command"]) => Some(json!({"data": [
            {"name": "review", "description": "Review the changes"},
            {"name": "test", "description": "Run the tests"},
        ]})),
        ("POST", ["api", "session"]) => {
            if let Some(model) = body.get("model") {
                *fake.model.lock().unwrap() = format!(
                    "{}/{}",
                    model.get("providerID").and_then(Value::as_str).unwrap_or("fake"),
                    model.get("id").and_then(Value::as_str).unwrap_or("fake-1")
                );
            }
            let id = source_id("ses", fake.counter.fetch_add(1, Ordering::Relaxed));
            Some(json!({"data": {"id": id, "title": "test", "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 0, "updated": 0}, "location": {"directory": "/"}}}))
        }
        ("GET", ["api", "session", _]) => {
            let id = segments[2];
            Some(json!({"data": {"id": id, "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 0, "updated": 0}, "location": {"directory": "/"}}}))
        }
        ("POST", ["api", "session", id, "prompt"]) => {
            let id = (*id).to_owned();
            let text = body
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let files = body
                .get("files")
                .and_then(Value::as_array)
                .map(|f| f.len())
                .unwrap_or(0);
            fake.prompts.lock().unwrap().push(text.clone());
            let fake = fake.clone();
            let session = id.clone();
            tokio::task::spawn_blocking(move || fake.play(session, text, files));
            let steer = body.get("delivery").and_then(Value::as_str) == Some("steer");
            Some(
                json!({"data": {"id": source_id("msg", 0), "sessionID": id, "type": "user",
                "time": {"created": 0}, "payload": {"text": ""},
                "delivery": if steer { "steer" } else { "queue" }}}),
            )
        }
        ("POST", ["api", "session", _, "interrupt"]) => {
            fake.abort.store(true, Ordering::Relaxed);
            for (_, tx) in fake.permissions.lock().unwrap().drain() {
                let _ = tx.send("reject".into());
            }
            Some(json!({"interrupted": true}))
        }
        ("POST", ["api", "session", _, "permission", request, "reply"]) => {
            let decision = body
                .get("decision")
                .and_then(Value::as_str)
                .unwrap_or("reject")
                .to_owned();
            if let Some(tx) = fake.permissions.lock().unwrap().remove(*request) {
                let _ = tx.send(decision);
            }
            Some(json!({}))
        }
        ("POST", ["api", "session", _, "model"]) => {
            if let Some(model) = body.get("model") {
                *fake.model.lock().unwrap() = format!(
                    "{}/{}",
                    model.get("providerID").and_then(Value::as_str).unwrap_or("fake"),
                    model.get("id").and_then(Value::as_str).unwrap_or("fake-1")
                );
            }
            Some(json!({}))
        }
        ("POST", ["api", "session", id, "command"]) => {
            let id = (*id).to_owned();
            let name = body
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let fake = fake.clone();
            tokio::task::spawn_blocking(move || {
                fake.play(id, format!("[[text:Ran /{name}.]]"), 0);
            });
            Some(json!({"data": {"id": source_id("msg", 0), "delivery": "queue"}}))
        }
        ("POST", ["api", "session", id, "compact"]) => {
            let id = (*id).to_owned();
            let fake = fake.clone();
            tokio::task::spawn_blocking(move || {
                fake.play(id, "[[text:Compacted the context.]]".to_owned(), 0);
            });
            Some(json!({"data": {"id": source_id("msg", 0), "type": "compaction"}}))
        }
        _ => None,
    }
}

async fn reply(socket: &mut OwnedWriteHalf, status: u16, value: &Value) -> std::io::Result<()> {
    let body = value.to_string();
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await
}

/// The event stream: one chunked SSE frame per event.
async fn stream(fake: Arc<Fake>, mut socket: OwnedWriteHalf) -> std::io::Result<()> {
    let mut events = fake.events.subscribe();
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
        )
        .await?;
    loop {
        let Ok(event) = events.recv().await else { break };
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
            .to_owned();
        let frame = format!("event: {kind}\ndata: {event}\n\n");
        let chunk = format!("{:x}\r\n{frame}\r\n", frame.len());
        if socket.write_all(chunk.as_bytes()).await.is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags() {
        assert_eq!(
            tags("a [[text:hi there]] b [[slow:40]]"),
            vec![
                ("text".to_owned(), "hi there".to_owned()),
                ("slow".to_owned(), "40".to_owned())
            ]
        );
    }

    #[test]
    fn expands_environment() {
        unsafe { std::env::set_var("FAKE_OPENCODE_EXPAND_TEST", "v1") };
        assert_eq!(expand("Bearer {env:FAKE_OPENCODE_EXPAND_TEST}"), "Bearer v1");
        assert_eq!(expand("plain"), "plain");
    }
}
