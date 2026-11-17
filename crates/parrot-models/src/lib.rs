//! Model API clients and provider protocol logic: OpenAI-compatible chat completions (streamed and
//! unary), structured output with JSON Schema validation and format fallback, multipart audio
//! transcriptions, model discovery, and request/output helpers.
//!
//! - [`Transport`] abstracts HTTP requests with streamed response bodies so tests need neither a
//!   network nor a running server.
//! - **Streaming** is server-sent events (`stream: true`), with `stream_options.include_usage` for the
//!   token counts; a server that refuses `stream_options` is asked again without it.
//! - **Structured output** asks for `response_format: json_schema` first. A server that refuses it (an
//!   error that names the parameter, or an answer that is not JSON matching the schema) is asked again
//!   with `json_object` and the schema in the system prompt, then with the prompt alone. Servers that
//!   refused are remembered for the client's lifetime, so the next request starts with what works.
//! - **Transcription** is `POST /audio/transcriptions` (multipart), asking for `verbose_json` so the
//!   answer says how long the audio was and which language it found, and for `json` from servers that
//!   do not have it.
//! - **Cancel**: [`until`] runs a request future against a deadline and a [`CancellationToken`]; dropping
//!   the future drops the transport stream, which closes the connection to the server.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

pub use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
pub use tokio_util::sync::CancellationToken;

/// The id the built-in OpenAI-compatible provider goes by.
pub const BUILTIN: &str = "openai-compatible";
/// How long a client's generation may take.
pub const GENERATE_TIMEOUT: Duration = Duration::from_secs(180);
/// How long one transcription request to the provider may take.
pub const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);
/// Audio per request (OpenAI's limit for one file).
pub const MAX_AUDIO: usize = 25 << 20;
/// How long a dictation may record.
pub const MAX_RECORDING: Duration = Duration::from_secs(15 * 60);
/// How often live transcription asks for the text so far.
pub const LIVE_EVERY: Duration = Duration::from_millis(2500);
/// New audio needed before another live request (about a second of Opus).
pub const LIVE_MIN_BYTES: usize = 4 * 1024;
/// How long `GET /models` waits for an answer.
pub const MODELS_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a provider's host half may take to notice a cancel before the caller stops waiting.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);

// ── shared protocol types ────────────────────────────────────────────────────────────────────────────

/// What a model request is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Task {
    /// Inline AI and drafts.
    #[default]
    Text,
    /// Titles, short summaries, labels: small requests that should be cheap and quick.
    Fast,
    /// Speech to text.
    Transcription,
}

impl Task {
    pub const ALL: [Task; 3] = [Task::Text, Task::Fast, Task::Transcription];

    pub fn as_str(self) -> &'static str {
        match self {
            Task::Text => "text",
            Task::Fast => "fast",
            Task::Transcription => "transcription",
        }
    }

    pub fn parse(s: &str) -> Option<Task> {
        Task::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// A message author role in a chat request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    User,
    Assistant,
}

/// One message in a chat request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// Tokens, or seconds of audio, a request used.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_seconds: Option<f64>,
}

/// A model a provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The tasks it suits, when the provider knows. Any task when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<Task>>,
}

/// Protocol error category for model calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Invalid,
    NotFound,
    Timeout,
    Unavailable,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Invalid => "INVALID",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::Unavailable => "UNAVAILABLE",
            ErrorCode::Internal => "INTERNAL",
        }
    }
}

/// An error returned by a model provider or protocol check.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ModelError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Value>,
}

impl ModelError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Timeout, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// The error a cancelled request ends with.
    pub fn cancelled() -> Self {
        Self::unavailable("the request was cancelled").with_details(json!({ "cancelled": true }))
    }

    pub fn is_cancelled(&self) -> bool {
        self.details
            .as_ref()
            .and_then(|d| d.get("cancelled"))
            .and_then(Value::as_bool)
            == Some(true)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

// ── HTTP transport ───────────────────────────────────────────────────────────────────────────────────

/// HTTP method for a [`TransportRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
}

/// An outgoing HTTP request sent through a [`Transport`].
#[derive(Debug, Clone)]
pub struct TransportRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Error returned by a [`Transport`] when sending a request or reading its response stream.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("timed out")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

impl TransportError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }
}

/// Stream of response body byte chunks.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send>>;

/// An HTTP response returned by a [`Transport`].
pub struct TransportResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ByteStream,
}

impl TransportResponse {
    /// Constructs a response from a status code, headers, and a single byte buffer.
    pub fn from_bytes(status: u16, headers: Vec<(String, String)>, body: impl Into<Bytes>) -> Self {
        let bytes: Bytes = body.into();
        Self {
            status,
            headers,
            body: Box::pin(futures::stream::once(async move { Ok(bytes) })),
        }
    }

    /// Constructs a response from a status code, headers, and a list of body chunks.
    pub fn from_chunks<I, B>(status: u16, headers: Vec<(String, String)>, chunks: I) -> Self
    where
        I: IntoIterator<Item = Result<B, TransportError>>,
        B: Into<Bytes>,
    {
        let items: Vec<Result<Bytes, TransportError>> =
            chunks.into_iter().map(|r| r.map(Into::into)).collect();
        Self {
            status,
            headers,
            body: Box::pin(futures::stream::iter(items)),
        }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_event_stream(&self) -> bool {
        self.header("content-type")
            .is_some_and(|c| c.to_ascii_lowercase().contains("text/event-stream"))
    }

    /// Collects the response body into a UTF-8 string (replacing invalid sequences), ignoring stream errors
    /// after partial bytes the same way `reqwest::Response::text().unwrap_or_default()` does on error bodies.
    pub async fn text(mut self) -> Result<String, TransportError> {
        let mut buf = Vec::new();
        while let Some(chunk) = self.body.next().await {
            buf.extend_from_slice(&chunk?);
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    pub async fn text_lossy(self) -> String {
        self.text().await.unwrap_or_default()
    }
}

/// HTTP transport for model API requests, supporting streamed response bodies.
pub trait Transport: Send + Sync + 'static {
    fn send<'a>(
        &'a self,
        request: TransportRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>> + Send + 'a>>;
}

// ── endpoint and headers ─────────────────────────────────────────────────────────────────────────────

/// Extra headers written one per line as `Name: value`.
pub fn header_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// A connection's values: base URL, key, extra headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub base: String,
    pub key: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl Endpoint {
    pub fn from_values(values: &Map<String, Value>) -> Result<Endpoint, ModelError> {
        let base = values
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ModelError::unavailable("the connection has no base URL"))?
            .trim()
            .trim_end_matches('/')
            .to_string();
        let key = values
            .get("apiKey")
            .and_then(|v| v.as_str())
            .filter(|k| !k.is_empty())
            .map(str::to_string);
        let headers = header_lines(values.get("headers").and_then(|v| v.as_str()).unwrap_or(""));
        Ok(Endpoint { base, key, headers })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn request(
        &self,
        method: HttpMethod,
        path: &str,
        extra_headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> TransportRequest {
        let mut headers = Vec::with_capacity(self.headers.len() + extra_headers.len() + 1);
        if let Some(k) = &self.key {
            headers.push(("authorization".into(), format!("Bearer {k}")));
        }
        headers.extend(self.headers.iter().cloned());
        headers.extend(extra_headers);
        TransportRequest {
            method,
            url: self.url(path),
            headers,
            body,
        }
    }

    /// The host requests go to, for the privacy view.
    pub fn host(&self) -> String {
        url::Url::parse(&self.base)
            .ok()
            .and_then(|u| {
                u.host_str().map(|h| match u.port() {
                    Some(p) => format!("{h}:{p}"),
                    None => h.to_string(),
                })
            })
            .unwrap_or_else(|| self.base.clone())
    }

    /// Whether the host is this machine or a private network.
    pub fn is_local(&self) -> bool {
        let Ok(u) = url::Url::parse(&self.base) else {
            return false;
        };
        match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
            Some(url::Host::Domain(d)) => {
                d == "localhost"
                    || d.ends_with(".localhost")
                    || d.ends_with(".local")
                    || d.ends_with(".internal")
                    || d.ends_with(".ts.net")
                    || !d.contains('.')
            }
            None => false,
        }
    }
}

/// Where requests to a connection go, and whether that is this machine or a private network.
/// `fallback_title` is used when the connection has no `baseUrl` (`None` for the built-in OpenAI-compatible
/// provider, `Some(provider_title)` for a provider the host adds).
pub fn destination_for_config(config: &Map<String, Value>, fallback_title: Option<&str>) -> (String, bool) {
    let base = config
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .filter(|b| !b.is_empty());
    match (base, fallback_title) {
        (Some(b), _) => {
            let ep = Endpoint {
                base: b.trim_end_matches('/').to_string(),
                key: None,
                headers: vec![],
            };
            (ep.host(), ep.is_local())
        }
        (None, Some(title)) => (title.to_string(), false),
        (None, None) => ("(no base URL)".into(), false),
    }
}

// ── OpenAI-compatible client ─────────────────────────────────────────────────────────────────────────

/// What a structured request sends for `response_format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Format {
    JsonSchema,
    JsonObject,
    PromptOnly,
}

/// What a chat request asks for.
#[derive(Debug, Clone, Default)]
pub struct Chat {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatOut {
    pub text: String,
    pub usage: Usage,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
    pub seconds: Option<f64>,
}

/// An error answer of the server as a protocol error.
pub fn upstream_error(status: u16, body: &str, what: &str) -> ModelError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error").filter(|e| e.is_string()))
                .or_else(|| v.get("detail"))
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.chars().take(300).collect::<String>());
    let code = match status {
        400 | 413 | 415 | 422 => ErrorCode::Invalid,
        404 => ErrorCode::NotFound,
        408 | 504 => ErrorCode::Timeout,
        _ => ErrorCode::Unavailable,
    };
    let lead = match status {
        401 | 403 => "the provider refused the connection's key".to_string(),
        429 => "the provider is rate limiting requests".to_string(),
        404 => format!("the provider has no such {what}"),
        s if s >= 500 => "the provider failed".to_string(),
        _ => "the provider refused the request".to_string(),
    };
    let status_label = status_display(status);
    ModelError::new(code, format!("{lead} ({status_label}): {message}"))
        .with_details(json!({ "status": status, "provider": message }))
}

fn status_display(status: u16) -> String {
    let reason = match status {
        400 => Some("Bad Request"),
        401 => Some("Unauthorized"),
        403 => Some("Forbidden"),
        404 => Some("Not Found"),
        408 => Some("Request Timeout"),
        413 => Some("Payload Too Large"),
        415 => Some("Unsupported Media Type"),
        422 => Some("Unprocessable Entity"),
        429 => Some("Too Many Requests"),
        500 => Some("Internal Server Error"),
        502 => Some("Bad Gateway"),
        503 => Some("Service Unavailable"),
        504 => Some("Gateway Timeout"),
        _ => None,
    };
    match reason {
        Some(r) => format!("{status} {r}"),
        None => status.to_string(),
    }
}

fn send_error(e: TransportError, url: &str) -> ModelError {
    if e.is_timeout() {
        ModelError::timeout(format!("{url} did not answer in time"))
    } else {
        ModelError::unavailable(format!("cannot reach {url}: {e}"))
    }
}

fn messages(system: Option<&str>, msgs: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(s) = system.filter(|s| !s.is_empty()) {
        out.push(json!({ "role": "system", "content": s }));
    }
    for m in msgs {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        out.push(json!({ "role": role, "content": m.content }));
    }
    out
}

fn usage_of(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("prompt_tokens").and_then(|t| t.as_u64()),
        output_tokens: v.get("completion_tokens").and_then(|t| t.as_u64()),
        audio_seconds: None,
    }
}

/// Whether an error answer is about a parameter the server does not have.
pub fn refuses(err: &ModelError, words: &[&str]) -> bool {
    if !matches!(
        err.code,
        ErrorCode::Invalid | ErrorCode::Unavailable | ErrorCode::NotFound
    ) {
        return false;
    }
    let status = err
        .details
        .as_ref()
        .and_then(|d| d.get("status"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    if !(400..500).contains(&status) || status == 401 || status == 403 || status == 429 {
        return false;
    }
    let m = err.message.to_lowercase();
    words.iter().any(|w| m.contains(w))
}

/// The OpenAI-compatible protocol client and what it has learned about servers' supported parameters.
pub struct OpenAi<T> {
    transport: T,
    /// `(base URL, format)` pairs a server refused.
    refused: Mutex<HashSet<(String, Format)>>,
    /// Base URLs that refused `stream_options`.
    no_stream_options: Mutex<HashSet<String>>,
    /// Base URLs that refused `verbose_json`.
    no_verbose: Mutex<HashSet<String>>,
}

impl<T: Default> Default for OpenAi<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> OpenAi<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            refused: Default::default(),
            no_stream_options: Default::default(),
            no_verbose: Default::default(),
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: Transport> OpenAi<T> {
    /// Streams a chat completion, calling `delta` with each piece of text.
    pub async fn chat_stream(
        &self,
        ep: &Endpoint,
        chat: &Chat,
        mut delta: impl FnMut(&str) + Send,
    ) -> Result<ChatOut, ModelError> {
        let with_options = !self.no_stream_options.lock().unwrap().contains(&ep.base);
        match self.chat_stream_once(ep, chat, with_options, &mut delta).await {
            Err(e) if with_options && refuses(&e, &["stream_options", "include_usage"]) => {
                self.no_stream_options.lock().unwrap().insert(ep.base.clone());
                self.chat_stream_once(ep, chat, false, &mut delta).await
            }
            r => r,
        }
    }

    async fn chat_stream_once(
        &self,
        ep: &Endpoint,
        chat: &Chat,
        stream_options: bool,
        delta: &mut (impl FnMut(&str) + Send),
    ) -> Result<ChatOut, ModelError> {
        let mut body = json!({
            "model": chat.model,
            "messages": messages(chat.system.as_deref(), &chat.messages),
            "stream": true,
        });
        if stream_options {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if let Some(m) = chat.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if let Some(t) = chat.temperature {
            body["temperature"] = json!(t);
        }
        let url = ep.url("/chat/completions");
        let req = ep.request(
            HttpMethod::Post,
            "/chat/completions",
            vec![
                ("accept".into(), "text/event-stream".into()),
                ("content-type".into(), "application/json".into()),
            ],
            serde_json::to_vec(&body).expect("serializes"),
        );
        let resp = self.transport.send(req).await.map_err(|e| send_error(e, &url))?;
        let status = resp.status;
        if !resp.is_success() {
            let text = resp.text_lossy().await;
            return Err(upstream_error(status, &text, "model"));
        }
        if !resp.is_event_stream() {
            // A server that ignores `stream` answers with one JSON document.
            let text_body = resp
                .text()
                .await
                .map_err(|e| ModelError::unavailable(format!("the stream from {url} broke off: {e}")))?;
            let v: Value = serde_json::from_str(&text_body).map_err(|e| {
                ModelError::unavailable(format!("{url} answered with something other than JSON: {e}"))
            })?;
            let text = v
                .pointer("/choices/0/message/content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            if !text.is_empty() {
                delta(&text);
            }
            return Ok(ChatOut {
                text,
                usage: v.get("usage").map(usage_of).unwrap_or_default(),
                model: v.get("model").and_then(|m| m.as_str()).map(str::to_string),
            });
        }
        let mut out = ChatOut::default();
        let mut buf: Vec<u8> = Vec::new();
        let mut bytes = resp.body;
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk
                .map_err(|e| ModelError::unavailable(format!("the stream from {url} broke off: {e}")))?;
            buf.extend_from_slice(&chunk);
            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(out);
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(e) = v.get("error") {
                    let message = e
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown error");
                    return Err(ModelError::unavailable(format!(
                        "the provider failed mid-stream: {message}"
                    )));
                }
                if out.model.is_none() {
                    out.model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                }
                if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                    out.usage = usage_of(u);
                }
                if let Some(t) = v.pointer("/choices/0/delta/content").and_then(|c| c.as_str())
                    && !t.is_empty()
                {
                    out.text.push_str(t);
                    delta(t);
                }
            }
        }
        Ok(out)
    }

    async fn chat_once(&self, ep: &Endpoint, body: Value) -> Result<ChatOut, ModelError> {
        let url = ep.url("/chat/completions");
        let req = ep.request(
            HttpMethod::Post,
            "/chat/completions",
            vec![("content-type".into(), "application/json".into())],
            serde_json::to_vec(&body).expect("serializes"),
        );
        let resp = self.transport.send(req).await.map_err(|e| send_error(e, &url))?;
        let status = resp.status;
        if !resp.is_success() {
            let text = resp.text_lossy().await;
            return Err(upstream_error(status, &text, "model"));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ModelError::unavailable(format!("the stream from {url} broke off: {e}")))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| {
            ModelError::unavailable(format!("{url} answered with something other than JSON: {e}"))
        })?;
        Ok(ChatOut {
            text: v
                .pointer("/choices/0/message/content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string(),
            usage: v.get("usage").map(usage_of).unwrap_or_default(),
            model: v.get("model").and_then(|m| m.as_str()).map(str::to_string),
        })
    }

    /// A chat completion whose answer must match `schema`. Returns the text and the parsed object.
    pub async fn chat_structured(
        &self,
        ep: &Endpoint,
        chat: &Chat,
        schema: &Value,
        name: &str,
    ) -> Result<(ChatOut, Value), ModelError> {
        let validator = jsonschema::validator_for(schema)
            .map_err(|e| ModelError::invalid(format!("schema is not a valid JSON Schema: {e}")))?;
        let mut last: Option<ModelError> = None;
        let mut used = Usage::default();
        for format in [Format::JsonSchema, Format::JsonObject, Format::PromptOnly] {
            if format != Format::PromptOnly
                && self.refused.lock().unwrap().contains(&(ep.base.clone(), format))
            {
                continue;
            }
            let instruction = format!(
                "Answer with one JSON value and nothing else: no prose, no code fences. It must match this JSON Schema:\n{}",
                serde_json::to_string(schema).unwrap_or_default()
            );
            let system = match (format, &chat.system) {
                (Format::JsonSchema, s) => s.clone(),
                (_, Some(s)) => Some(format!("{s}\n\n{instruction}")),
                (_, None) => Some(instruction),
            };
            let mut body = json!({
                "model": chat.model,
                "messages": messages(system.as_deref(), &chat.messages),
                "stream": false,
            });
            if let Some(m) = chat.max_tokens {
                body["max_tokens"] = json!(m);
            }
            if let Some(t) = chat.temperature {
                body["temperature"] = json!(t);
            }
            match format {
                Format::JsonSchema => {
                    let strict = schema.get("additionalProperties") == Some(&Value::Bool(false));
                    body["response_format"] = json!({
                        "type": "json_schema",
                        "json_schema": { "name": name, "schema": schema, "strict": strict },
                    });
                }
                Format::JsonObject => body["response_format"] = json!({ "type": "json_object" }),
                Format::PromptOnly => {}
            }
            let out = match self.chat_once(ep, body).await {
                Ok(o) => o,
                Err(e)
                    if format != Format::PromptOnly
                        && refuses(&e, &["response_format", "json_schema", "json_object", "schema"]) =>
                {
                    self.refused.lock().unwrap().insert((ep.base.clone(), format));
                    last = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            };
            add_usage(&mut used, &out.usage);
            match parse_json(&out.text) {
                Some(v) if validator.is_valid(&v) => {
                    return Ok((ChatOut { usage: used, ..out }, v));
                }
                parsed => {
                    let problems: Vec<String> = match &parsed {
                        Some(v) => validator.iter_errors(v).map(|e| e.to_string()).take(5).collect(),
                        None => vec!["the answer is not JSON".into()],
                    };
                    // A server that takes `json_schema` but ignores it: try the next format, and remember.
                    if format != Format::PromptOnly {
                        self.refused.lock().unwrap().insert((ep.base.clone(), format));
                    }
                    last = Some(
                        ModelError::unavailable(format!(
                            "the model's answer does not match the schema: {}",
                            problems.join("; ")
                        ))
                        .with_details(json!({ "text": out.text, "problems": problems })),
                    );
                }
            }
        }
        Err(last.unwrap_or_else(|| ModelError::unavailable("the provider gave no structured answer")))
    }

    /// Transcribes a whole clip.
    pub async fn transcribe(
        &self,
        ep: &Endpoint,
        model: &str,
        audio: &[u8],
        mime: &str,
        language: Option<&str>,
    ) -> Result<Transcript, ModelError> {
        let verbose = !self.no_verbose.lock().unwrap().contains(&ep.base);
        match self
            .transcribe_once(ep, model, audio, mime, language, verbose)
            .await
        {
            Err(e) if verbose && refuses(&e, &["verbose_json", "response_format"]) => {
                self.no_verbose.lock().unwrap().insert(ep.base.clone());
                self.transcribe_once(ep, model, audio, mime, language, false)
                    .await
            }
            r => r,
        }
    }

    async fn transcribe_once(
        &self,
        ep: &Endpoint,
        model: &str,
        audio: &[u8],
        mime: &str,
        language: Option<&str>,
        verbose: bool,
    ) -> Result<Transcript, ModelError> {
        let boundary = format!("parrot-{}", uuid::Uuid::now_v7().simple());
        let mut body: Vec<u8> = Vec::with_capacity(audio.len() + 1024);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        };
        field("model", model);
        field("response_format", if verbose { "verbose_json" } else { "json" });
        if let Some(l) = language {
            field("language", l);
        }
        let base_mime = mime.split(';').next().unwrap_or(mime).trim();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.{}\"\r\nContent-Type: {base_mime}\r\n\r\n",
                extension(base_mime)
            )
            .as_bytes(),
        );
        body.extend_from_slice(audio);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let url = ep.url("/audio/transcriptions");
        let req = ep.request(
            HttpMethod::Post,
            "/audio/transcriptions",
            vec![(
                "content-type".into(),
                format!("multipart/form-data; boundary={boundary}"),
            )],
            body,
        );
        let resp = self.transport.send(req).await.map_err(|e| send_error(e, &url))?;
        let status = resp.status;
        if !resp.is_success() {
            let text = resp.text_lossy().await;
            return Err(upstream_error(status, &text, "model"));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ModelError::unavailable(format!("the stream from {url} broke off: {e}")))?;
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            // `text` response formats from servers that ignore ours.
            Err(_) => json!({ "text": text }),
        };
        Ok(Transcript {
            text: v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
            language: v.get("language").and_then(|l| l.as_str()).map(str::to_string),
            seconds: v.get("duration").and_then(|d| d.as_f64()),
        })
    }

    /// `GET /models`.
    pub async fn models(&self, ep: &Endpoint) -> Result<Vec<ModelInfo>, ModelError> {
        let url = ep.url("/models");
        let req = ep.request(
            HttpMethod::Get,
            "/models",
            vec![("accept".into(), "application/json".into())],
            Vec::new(),
        );
        let resp = tokio::time::timeout(MODELS_TIMEOUT, self.transport.send(req))
            .await
            .map_err(|_| ModelError::timeout(format!("{url} did not answer in time")))?
            .map_err(|e| send_error(e, &url))?;
        let status = resp.status;
        if !resp.is_success() {
            let text = resp.text_lossy().await;
            return Err(upstream_error(status, &text, "endpoint"));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ModelError::unavailable(format!("the stream from {url} broke off: {e}")))?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let list = v
            .get("data")
            .or_else(|| v.get("models"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        let mut models: Vec<ModelInfo> = list
            .iter()
            .filter_map(|m| {
                m.get("id")
                    .or_else(|| m.get("name"))
                    .and_then(|i| i.as_str())
                    .map(|id| ModelInfo {
                        id: id.to_string(),
                        title: None,
                        tasks: guess_tasks(id),
                    })
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }
}

fn add_usage(total: &mut Usage, u: &Usage) {
    let add = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    total.input_tokens = add(total.input_tokens, u.input_tokens);
    total.output_tokens = add(total.output_tokens, u.output_tokens);
}

/// Speech models are told apart by name, which is all `/models` gives.
pub fn guess_tasks(id: &str) -> Option<Vec<Task>> {
    let l = id.to_lowercase();
    if l.contains("whisper") || l.contains("transcribe") || l.contains("parakeet") || l.contains("stt") {
        Some(vec![Task::Transcription])
    } else if l.contains("embed") || l.contains("tts") || l.contains("dall-e") || l.contains("image") {
        Some(vec![])
    } else {
        None
    }
}

/// The file extension servers pick a decoder by.
pub fn extension(mime: &str) -> &'static str {
    match mime {
        "audio/webm" | "video/webm" => "webm",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" | "video/mp4" => "m4a",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        _ => "webm",
    }
}

/// The JSON value in a model's answer: the whole text, or inside code fences, or the outermost braces.
pub fn parse_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    if let Some(start) = t.find("```") {
        let rest = &t[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```")
            && let Ok(v) = serde_json::from_str(rest[..end].trim())
        {
            return Some(v);
        }
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(a), Some(b)) = (t.find(open), t.rfind(close))
            && a < b
            && let Ok(v) = serde_json::from_str(&t[a..=b])
        {
            return Some(v);
        }
    }
    None
}

// ── provider and request helpers ─────────────────────────────────────────────────────────────────────

/// A generation request as a provider sees it.
#[derive(Debug, Clone, Default)]
pub struct GenerateIn {
    pub request: String,
    pub task: Task,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub schema: Option<Value>,
    pub schema_name: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// A generation result from a provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenerateOut {
    pub text: String,
    pub object: Option<Value>,
    pub usage: Usage,
    pub model: String,
}

/// A transcription request as a provider sees it.
#[derive(Debug, Clone, Default)]
pub struct TranscribeIn {
    pub request: String,
    pub model: String,
    pub mime: String,
    pub language: Option<String>,
}

/// A transcription result from a provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscribeOut {
    pub text: String,
    pub language: Option<String>,
    pub seconds: Option<f64>,
    pub model: String,
}

/// One resolved context item for [`with_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    pub uri: String,
    pub title: Option<String>,
    pub text: String,
}

/// Runs an OpenAI-compatible generation (`chat_stream` or `chat_structured` when `input.schema` is set),
/// emitting text deltas and stopping when `cancel` fires or `timeout` elapses.
pub async fn generate_openai<T: Transport>(
    openai: &OpenAi<T>,
    ep: &Endpoint,
    input: &GenerateIn,
    timeout: Duration,
    cancel: &CancellationToken,
    delta: impl FnMut(&str) + Send,
) -> Result<GenerateOut, ModelError> {
    let chat = Chat {
        model: input.model.clone(),
        system: input.system.clone(),
        messages: input.messages.clone(),
        max_tokens: input.max_tokens,
        temperature: input.temperature,
    };
    let work = async {
        match &input.schema {
            Some(schema) => {
                let name = input.schema_name.as_deref().unwrap_or("answer");
                let (out, object) = openai
                    .chat_structured(ep, &chat, schema, &sanitize_schema_name(name))
                    .await?;
                let mut delta = delta;
                delta(&out.text);
                Ok(GenerateOut {
                    text: out.text,
                    object: Some(object),
                    usage: out.usage,
                    model: out.model.unwrap_or_else(|| input.model.clone()),
                })
            }
            None => {
                let out = openai.chat_stream(ep, &chat, delta).await?;
                Ok(GenerateOut {
                    text: out.text,
                    object: None,
                    usage: out.usage,
                    model: out.model.unwrap_or_else(|| input.model.clone()),
                })
            }
        }
    };
    until(work, timeout, cancel).await
}

/// Runs an OpenAI-compatible transcription, stopping when `cancel` fires or `timeout` elapses.
pub async fn transcribe_openai<T: Transport>(
    openai: &OpenAi<T>,
    ep: &Endpoint,
    input: &TranscribeIn,
    audio: &[u8],
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<TranscribeOut, ModelError> {
    let work = async {
        let t = openai
            .transcribe(ep, &input.model, audio, &input.mime, input.language.as_deref())
            .await?;
        Ok(TranscribeOut {
            text: t.text,
            language: t.language,
            seconds: t.seconds,
            model: input.model.clone(),
        })
    };
    until(work, timeout, cancel).await
}

/// Checks an answer against the request's JSON Schema.
pub fn check_schema(schema: &Value, v: &Value, text: &str) -> Result<(), ModelError> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| ModelError::invalid(format!("schema is not a valid JSON Schema: {e}")))?;
    let problems: Vec<String> = validator.iter_errors(v).map(|e| e.to_string()).take(5).collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(ModelError::unavailable(format!(
            "the model's answer does not match the schema: {}",
            problems.join("; ")
        ))
        .with_details(json!({ "text": text, "problems": problems })))
    }
}

/// Validates and extracts a structured object from an added provider's answer when the request has a schema.
pub fn validate_provider_object(
    schema: Option<&Value>,
    object: Option<Value>,
    text: &str,
) -> Result<Option<Value>, ModelError> {
    let Some(schema) = schema else {
        return Ok(object);
    };
    let v = object.or_else(|| parse_json(text)).ok_or_else(|| {
        ModelError::unavailable("the provider's answer is not JSON").with_details(json!({ "text": text }))
    })?;
    check_schema(schema, &v, text)?;
    Ok(Some(v))
}

/// `response_format.json_schema.name` allows letters, digits, `_` and `-`.
pub fn sanitize_schema_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if s.is_empty() { "answer".into() } else { s }
}

/// The key a host registers an added provider's handler under.
pub fn provider_key(id: &str) -> String {
    format!("ai.provider:{id}")
}

/// The system prompt with the context entities' text after it.
pub fn with_context(system: Option<String>, items: &[ContextItem]) -> Option<String> {
    if items.is_empty() {
        return system;
    }
    let mut s = system.unwrap_or_default();
    if !s.is_empty() {
        s.push_str("\n\n");
    }
    s.push_str("Context the user has given (each item is delimited by <context> tags):\n");
    for t in items {
        s.push_str(&format!(
            "\n<context uri=\"{}\" title=\"{}\">\n{}\n</context>\n",
            t.uri,
            t.title.as_deref().unwrap_or("").replace('"', "'"),
            t.text
        ));
    }
    Some(s)
}

/// Normalizes a transcription language hint: `auto` and empty mean "detect it".
pub fn normalize_language(l: Option<&str>) -> Option<String> {
    l.map(str::trim)
        .filter(|l| !l.is_empty() && *l != "auto")
        .map(|l| l.to_lowercase())
}

/// Validates a caller-supplied request id, or generates a new UUIDv7 when missing.
pub fn request_id(given: Option<String>) -> Result<String, ModelError> {
    match given {
        Some(id)
            if id.is_empty()
                || id.len() > 64
                || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') =>
        {
            Err(ModelError::invalid(
                "request ids are 1 to 64 letters, digits and dashes",
            ))
        }
        Some(id) => Ok(id),
        None => Ok(uuid::Uuid::now_v7().to_string()),
    }
}

/// Runs `work` until it ends, `cancel` fires (the future is dropped, which drops the HTTP request) or
/// `timeout` passes.
pub async fn until<T>(
    work: impl Future<Output = Result<T, ModelError>>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<T, ModelError> {
    tokio::select! {
        r = work => r,
        _ = cancel.cancelled() => Err(ModelError::cancelled()),
        _ = tokio::time::sleep(timeout) => Err(ModelError::timeout(format!(
            "the model did not answer within {} s", timeout.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_in_answers() {
        assert_eq!(parse_json(r#"{"a":1}"#), Some(json!({"a": 1})));
        assert_eq!(
            parse_json("Sure!\n```json\n{\"a\": 2}\n```"),
            Some(json!({"a": 2}))
        );
        assert_eq!(
            parse_json("Here: {\"title\": \"x\"} done"),
            Some(json!({"title": "x"}))
        );
        assert_eq!(parse_json("no json"), None);
    }

    #[test]
    fn errors_map_to_codes() {
        let e = upstream_error(401, r#"{"error":{"message":"bad key"}}"#, "model");
        assert_eq!(e.code, ErrorCode::Unavailable);
        assert!(e.message.contains("bad key"), "{}", e.message);
        assert_eq!(upstream_error(404, "{}", "model").code, ErrorCode::NotFound);
        assert_eq!(upstream_error(400, "nope", "model").code, ErrorCode::Invalid);
        assert_eq!(upstream_error(504, "", "model").code, ErrorCode::Timeout);
        let refused = upstream_error(
            400,
            r#"{"error":{"message":"response_format json_schema is not supported"}}"#,
            "model",
        );
        assert!(refuses(&refused, &["response_format"]));
        assert!(!refuses(
            &upstream_error(401, "response_format", "model"),
            &["response_format"]
        ));
    }

    #[test]
    fn hosts_and_locality() {
        let ep = |b: &str| Endpoint {
            base: b.into(),
            key: None,
            headers: vec![],
        };
        assert_eq!(ep("http://127.0.0.1:4841/v1").host(), "127.0.0.1:4841");
        assert!(ep("http://127.0.0.1:4841/v1").is_local());
        assert!(ep("http://ollama:11434/v1").is_local());
        assert!(ep("http://10.0.0.5/v1").is_local());
        assert!(!ep("https://api.openai.com/v1").is_local());
        assert_eq!(ep("https://api.openai.com/v1").host(), "api.openai.com");
    }

    #[test]
    fn headers_context_and_request_helpers() {
        assert_eq!(
            header_lines("X-A: 1\n\nbad line\nX-B:two: parts "),
            vec![
                ("X-A".to_string(), "1".to_string()),
                ("X-B".to_string(), "two: parts".to_string())
            ]
        );
        assert_eq!(sanitize_schema_name("note title!"), "note_title_");
        assert_eq!(sanitize_schema_name(""), "answer");
        assert_eq!(normalize_language(Some("auto")), None);
        assert_eq!(normalize_language(Some("  ")), None);
        assert_eq!(normalize_language(Some("DE")), Some("de".into()));
        assert!(request_id(Some("req-123".into())).is_ok());
        assert!(request_id(Some("bad id!".into())).is_err());

        let sys = with_context(
            Some("Be brief.".into()),
            &[ContextItem {
                uri: "note://1".into(),
                title: Some("Plan \"Q4\"".into()),
                text: "Ship parrot-models.".into(),
            }],
        )
        .unwrap();
        assert!(sys.starts_with("Be brief.\n\nContext the user has given"));
        assert!(
            sys.contains("<context uri=\"note://1\" title=\"Plan 'Q4'\">\nShip parrot-models.\n</context>")
        );
    }
}
