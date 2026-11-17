use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parrot_models::{
    Bytes, CancellationToken, Chat, ContextItem, Endpoint, ErrorCode, GenerateIn, HttpMethod, Message,
    ModelError, OpenAi, Role, Task, TranscribeIn, Transport, TransportError, TransportRequest,
    TransportResponse, check_schema, destination_for_config, generate_openai, normalize_language,
    provider_key, request_id, sanitize_schema_name, transcribe_openai, until, validate_provider_object,
    with_context,
};
use serde_json::{Value, json};

mod recorded;

#[derive(Clone)]
enum MockReply {
    Status {
        status: u16,
        headers: Vec<(String, String)>,
        chunks: Vec<Result<Bytes, TransportError>>,
    },
    Error(TransportError),
}

impl MockReply {
    fn json(status: u16, body: &str) -> Self {
        Self::Status {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            chunks: vec![Ok(Bytes::from(body.to_owned()))],
        }
    }

    fn sse(chunks: Vec<&str>) -> Self {
        Self::Status {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream; charset=utf-8".into())],
            chunks: chunks
                .into_iter()
                .map(|c| Ok(Bytes::from(c.to_owned())))
                .collect(),
        }
    }
}

#[derive(Clone, Default)]
struct FixtureTransport {
    replies: Arc<Mutex<VecDeque<MockReply>>>,
    seen: Arc<Mutex<Vec<TransportRequest>>>,
}

impl FixtureTransport {
    fn new(replies: Vec<MockReply>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into())),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<TransportRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Transport for FixtureTransport {
    fn send<'a>(
        &'a self,
        req: TransportRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>> + Send + 'a>> {
        self.seen.lock().unwrap().push(req);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected transport call");
        Box::pin(async move {
            match reply {
                MockReply::Status {
                    status,
                    headers,
                    chunks,
                } => Ok(TransportResponse::from_chunks(status, headers, chunks)),
                MockReply::Error(e) => Err(e),
            }
        })
    }
}

fn test_endpoint() -> Endpoint {
    Endpoint {
        base: "https://api.example.test/v1".into(),
        key: Some("sk-test-token".into()),
        headers: vec![("X-Custom-Header".into(), "custom-val".into())],
    }
}

#[tokio::test]
async fn plain_completion_replays_json_fixture_and_sends_headers() {
    let fixture = include_str!("../../fixtures/chat_completion.json");
    let transport = FixtureTransport::new(vec![MockReply::json(200, fixture)]);
    let client = OpenAi::new(transport.clone());

    let chat = Chat {
        model: "gpt-4o-mini".into(),
        system: Some("Be concise.".into()),
        messages: vec![Message {
            role: Role::User,
            content: "Say hello".into(),
        }],
        max_tokens: Some(100),
        temperature: Some(0.2),
    };

    let mut deltas = Vec::new();
    let out = client
        .chat_stream(&test_endpoint(), &chat, |d| deltas.push(d.to_string()))
        .await
        .expect("plain completion should succeed");

    assert_eq!(out.text, "Hello from non-stream completion.");
    assert_eq!(out.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(out.usage.input_tokens, Some(12));
    assert_eq!(out.usage.output_tokens, Some(6));
    assert_eq!(deltas, vec!["Hello from non-stream completion."]);

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, HttpMethod::Post);
    assert_eq!(reqs[0].url, "https://api.example.test/v1/chat/completions");
    assert!(
        reqs[0]
            .headers
            .contains(&("authorization".into(), "Bearer sk-test-token".into()))
    );
    assert!(
        reqs[0]
            .headers
            .contains(&("X-Custom-Header".into(), "custom-val".into()))
    );
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["model"], "gpt-4o-mini");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "Be concise.");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "Say hello");
    assert_eq!(body["max_tokens"], 100);
}

#[tokio::test]
async fn streamed_completion_replays_sse_fixture_across_chunk_boundaries() {
    let fixture = include_str!("../../fixtures/chat_stream.sse");
    // Split mid-line to exercise the SSE line buffer across chunks.
    let (first, second) = fixture.split_at(fixture.len() / 2);
    let transport = FixtureTransport::new(vec![MockReply::sse(vec![first, second])]);
    let client = OpenAi::new(transport);

    let chat = Chat {
        model: "gpt-4o-mini".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "Hi".into(),
        }],
        max_tokens: None,
        temperature: None,
    };

    let mut deltas = Vec::new();
    let out = client
        .chat_stream(&test_endpoint(), &chat, |d| deltas.push(d.to_string()))
        .await
        .expect("SSE stream should succeed");

    assert_eq!(out.text, "Hello world");
    assert_eq!(out.usage.input_tokens, Some(10));
    assert_eq!(out.usage.output_tokens, Some(2));
    assert_eq!(deltas, vec!["Hello", " world"]);
}

#[tokio::test]
async fn streamed_completion_falls_back_when_stream_options_is_refused() {
    let refuse = include_str!("../../fixtures/error_refuse_stream_options.json");
    let sse = include_str!("../../fixtures/chat_stream.sse");
    let transport = FixtureTransport::new(vec![
        MockReply::json(400, refuse),
        MockReply::sse(vec![sse]),
        MockReply::sse(vec![sse]),
    ]);
    let client = OpenAi::new(transport.clone());
    let ep = test_endpoint();
    let chat = Chat {
        model: "gpt-4o-mini".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "Hi".into(),
        }],
        max_tokens: None,
        temperature: None,
    };

    let out = client
        .chat_stream(&ep, &chat, |_| {})
        .await
        .expect("fallback without stream_options should succeed");
    assert_eq!(out.text, "Hello world");

    // Second call to the same base URL should remember stream_options refusal without retrying.
    client
        .chat_stream(&ep, &chat, |_| {})
        .await
        .expect("second call should succeed on first try");

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 3);
    let first_body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let second_body: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let third_body: Value = serde_json::from_slice(&reqs[2].body).unwrap();
    assert!(first_body.get("stream_options").is_some());
    assert!(second_body.get("stream_options").is_none());
    assert!(third_body.get("stream_options").is_none());
}

#[tokio::test]
async fn stream_cut_midway_handles_transport_drop_sse_error_and_eof_without_done() {
    let cut_fixture = include_str!("../../fixtures/chat_stream_cut.sse");
    let chat = Chat {
        model: "gpt-4o-mini".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "Stream please".into(),
        }],
        max_tokens: None,
        temperature: None,
    };

    // Case 1: Stream cut mid-way with transport error after emitting deltas.
    let transport_cut = FixtureTransport::new(vec![MockReply::Status {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![
            Ok(Bytes::from(cut_fixture.to_owned())),
            Err(TransportError::other("connection reset by peer")),
        ],
    }]);
    let client = OpenAi::new(transport_cut);
    let mut deltas1 = Vec::new();
    let err = client
        .chat_stream(&test_endpoint(), &chat, |d| deltas1.push(d.to_string()))
        .await
        .expect_err("transport cut mid-stream must fail");
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(err.message.contains("broke off"), "{}", err.message);
    assert!(
        err.message.contains("connection reset by peer"),
        "{}",
        err.message
    );
    assert_eq!(deltas1, vec!["Partial ", "answer"]);

    // Case 2: Mid-stream SSE error payload.
    let err_fixture = include_str!("../../fixtures/chat_stream_error.sse");
    let transport_sse_err = FixtureTransport::new(vec![MockReply::sse(vec![err_fixture])]);
    let client_sse_err = OpenAi::new(transport_sse_err);
    let mut deltas2 = Vec::new();
    let err2 = client_sse_err
        .chat_stream(&test_endpoint(), &chat, |d| deltas2.push(d.to_string()))
        .await
        .expect_err("SSE error event mid-stream must fail");
    assert_eq!(err2.code, ErrorCode::Unavailable);
    assert!(
        err2.message
            .contains("the provider failed mid-stream: Upstream stream interrupted by rate limit"),
        "{}",
        err2.message
    );
    assert_eq!(deltas2, vec!["Starting..."]);

    // Case 3: Clean EOF without [DONE] returns accumulated deltas.
    let transport_eof = FixtureTransport::new(vec![MockReply::sse(vec![cut_fixture])]);
    let client_eof = OpenAi::new(transport_eof);
    let mut deltas3 = Vec::new();
    let out = client_eof
        .chat_stream(&test_endpoint(), &chat, |d| deltas3.push(d.to_string()))
        .await
        .expect("EOF without [DONE] returns accumulated deltas");
    assert_eq!(out.text, "Partial answer");
    assert_eq!(deltas3, vec!["Partial ", "answer"]);
}

#[tokio::test]
async fn error_statuses_with_provider_bodies_map_to_error_codes() {
    let e400 = include_str!("../../fixtures/error_refuse_response_format.json");
    let e401 = include_str!("../../fixtures/error_401.json");
    let e404 = include_str!("../../fixtures/error_404.json");
    let e429 = include_str!("../../fixtures/error_429.json");
    let e500 = include_str!("../../fixtures/error_500.json");

    let transport = FixtureTransport::new(vec![
        MockReply::json(400, e400),
        MockReply::json(401, e401),
        MockReply::json(404, e404),
        MockReply::json(429, e429),
        MockReply::json(500, e500),
        MockReply::Error(TransportError::Timeout),
        MockReply::Error(TransportError::other("dns lookup failed")),
    ]);
    let client = OpenAi::new(transport);
    let ep = test_endpoint();
    let chat = Chat {
        model: "gpt-4o-mini".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "test".into(),
        }],
        max_tokens: None,
        temperature: None,
    };

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(
        err.message
            .contains("the provider refused the request (400 Bad Request)"),
        "{}",
        err.message
    );

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(
        err.message
            .contains("the provider refused the connection's key (401 Unauthorized): Incorrect API key provided: sk-invalid."),
        "{}",
        err.message
    );

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(
        err.message
            .contains("the provider has no such model (404 Not Found)"),
        "{}",
        err.message
    );
    assert!(err.message.contains("missing-model"));

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(
        err.message
            .contains("the provider is rate limiting requests (429 Too Many Requests)"),
        "{}",
        err.message
    );

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(
        err.message
            .contains("the provider failed (500 Internal Server Error)"),
        "{}",
        err.message
    );

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Timeout);
    assert!(err.message.contains("did not answer in time"));

    let err = client.chat_stream(&ep, &chat, |_| {}).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(err.message.contains("cannot reach"));
    assert!(err.message.contains("dns lookup failed"));
}

#[tokio::test]
async fn structured_completion_validates_schema_and_cascades_fallbacks() {
    let refuse_rf = include_str!("../../fixtures/error_refuse_response_format.json");
    let valid_struct = include_str!("../../fixtures/chat_structured.json");
    let fenced_struct = r#"{
        "choices": [{"message": {"content": "Here is the JSON:\n```json\n{\"title\":\"FromFence\",\"score\":7}\n```"}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 5}
    }"#;

    let transport = FixtureTransport::new(vec![
        // 1. json_schema refused -> falls back to json_object -> succeeds and remembers json_schema refusal
        MockReply::json(400, refuse_rf),
        MockReply::json(200, valid_struct),
        // 2. Subsequent call starts at json_object; if it returns invalid schema JSON, falls back to PromptOnly
        MockReply::json(
            200,
            r#"{"choices":[{"message":{"content":"{\"wrong\":true}"}}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
        ),
        MockReply::json(200, fenced_struct),
    ]);
    let client = OpenAi::new(transport.clone());
    let ep = test_endpoint();
    let schema = json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "score": { "type": "integer" }
        },
        "required": ["title", "score"],
        "additionalProperties": false
    });
    let chat = Chat {
        model: "gpt-4o-mini".into(),
        system: Some("You are helpful.".into()),
        messages: vec![Message {
            role: Role::User,
            content: "Summarize".into(),
        }],
        max_tokens: None,
        temperature: None,
    };

    let (out, data) = client
        .chat_structured(&ep, &chat, &schema, &sanitize_schema_name("Report Summary!"))
        .await
        .expect("structured fallback to json_object should succeed");
    assert_eq!(data, json!({"title": "Report", "score": 42}));
    assert_eq!(out.text, "{\"title\":\"Report\",\"score\":42}");
    assert_eq!(out.usage.input_tokens, Some(20));
    assert_eq!(out.usage.output_tokens, Some(8));

    let (out2, data2) = client
        .chat_structured(&ep, &chat, &schema, &sanitize_schema_name("Report Summary!"))
        .await
        .expect("structured fallback to PromptOnly should succeed");
    assert_eq!(data2, json!({"title": "FromFence", "score": 7}));
    // Usage is accumulated across the two attempts (3+5 input, 2+5 output).
    assert_eq!(out2.usage.input_tokens, Some(8));
    assert_eq!(out2.usage.output_tokens, Some(7));

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 4);
    let r0: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let r1: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let r2: Value = serde_json::from_slice(&reqs[2].body).unwrap();
    let r3: Value = serde_json::from_slice(&reqs[3].body).unwrap();
    assert_eq!(r0["response_format"]["type"], "json_schema");
    assert_eq!(r0["response_format"]["json_schema"]["name"], "Report_Summary_");
    assert_eq!(r0["response_format"]["json_schema"]["strict"], true);
    assert_eq!(r1["response_format"]["type"], "json_object");
    assert_eq!(r2["response_format"]["type"], "json_object");
    assert!(r3.get("response_format").is_none());
}

#[tokio::test]
async fn transcription_replays_verbose_and_falls_back_to_json() {
    let verbose = include_str!("../../fixtures/transcription_verbose.json");
    let refuse_verbose = include_str!("../../fixtures/error_refuse_verbose_json.json");
    let plain_json = include_str!("../../fixtures/transcription_json.json");

    let transport = FixtureTransport::new(vec![
        MockReply::json(200, verbose),
        MockReply::json(400, refuse_verbose),
        MockReply::json(200, plain_json),
    ]);
    let client = OpenAi::new(transport.clone());
    let ep = test_endpoint();

    let tr = client
        .transcribe(
            &ep,
            "whisper-1",
            b"fake-audio-bytes",
            "audio/webm;codecs=opus",
            Some("en"),
        )
        .await
        .expect("verbose_json transcription should succeed");
    assert_eq!(tr.text, "dictated words");
    assert_eq!(tr.language.as_deref(), Some("en"));
    assert_eq!(tr.seconds, Some(3.45));

    // Different base URL triggers verbose_json refusal and falls back to json.
    let ep2 = Endpoint {
        base: "https://other.example.test/v1".into(),
        key: None,
        headers: vec![],
    };
    let tr2 = client
        .transcribe(&ep2, "whisper-1", b"fake-wav", "audio/wav", None)
        .await
        .expect("fallback to json transcription should succeed");
    assert_eq!(tr2.text, "fallback json transcription");
    assert_eq!(tr2.language, None);
    assert_eq!(tr2.seconds, None);

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].url, "https://api.example.test/v1/audio/transcriptions");
    assert!(
        reqs[0]
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("multipart/form-data; boundary="))
    );
    let body0 = String::from_utf8_lossy(&reqs[0].body);
    assert!(body0.contains("filename=\"audio.webm\""));
    assert!(body0.contains("verbose_json"));
    assert!(body0.contains("name=\"language\"\r\n\r\nen"));

    let body2 = String::from_utf8_lossy(&reqs[2].body);
    assert!(body2.contains("filename=\"audio.wav\""));
    assert!(!body2.contains("verbose_json"));
}

#[tokio::test]
async fn models_replays_fixture_sorts_and_guesses_tasks() {
    let fixture = include_str!("../../fixtures/models.json");
    let transport = FixtureTransport::new(vec![MockReply::json(200, fixture)]);
    let client = OpenAi::new(transport.clone());

    let list = client
        .models(&test_endpoint())
        .await
        .expect("models listing should succeed");

    let ids: Vec<&str> = list.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "gpt-4o-mini",
            "text-embedding-3-small",
            "tts-1",
            "whisper-large-v3"
        ]
    );
    assert_eq!(list[0].tasks, None);
    assert_eq!(list[1].tasks, Some(vec![]));
    assert_eq!(list[2].tasks, Some(vec![]));
    assert_eq!(list[3].tasks, Some(vec![Task::Transcription]));

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, HttpMethod::Get);
    assert_eq!(reqs[0].url, "https://api.example.test/v1/models");
}

#[tokio::test]
async fn provider_wrappers_and_helpers_enforce_protocol_invariants() {
    let fixture = include_str!("../../fixtures/chat_completion.json");
    let verbose = include_str!("../../fixtures/transcription_verbose.json");
    let transport = FixtureTransport::new(vec![MockReply::json(200, fixture), MockReply::json(200, verbose)]);
    let client = OpenAi::new(transport);
    let ep = test_endpoint();
    let cancel = CancellationToken::new();

    let gen_in = GenerateIn {
        request: "req-1".into(),
        task: Task::Text,
        model: "gpt-4o-mini".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "hi".into(),
        }],
        schema: None,
        schema_name: None,
        max_tokens: None,
        temperature: None,
    };
    let mut deltas = Vec::new();
    let out = generate_openai(&client, &ep, &gen_in, Duration::from_secs(5), &cancel, |d| {
        deltas.push(d.to_string())
    })
    .await
    .expect("generate_openai should succeed");
    assert_eq!(out.text, "Hello from non-stream completion.");
    assert_eq!(deltas, vec!["Hello from non-stream completion."]);

    let tr_in = TranscribeIn {
        request: "req-2".into(),
        model: "whisper-1".into(),
        mime: "audio/webm".into(),
        language: Some("en".into()),
    };
    let tr_out = transcribe_openai(
        &client,
        &ep,
        &tr_in,
        b"audio-bytes",
        Duration::from_secs(5),
        &cancel,
    )
    .await
    .expect("transcribe_openai should succeed");
    assert_eq!(tr_out.text, "dictated words");
    assert_eq!(tr_out.model, "whisper-1");

    // Helper checks.
    assert_eq!(sanitize_schema_name("My Schema!"), "My_Schema_");
    assert_eq!(sanitize_schema_name(""), "answer");
    assert_eq!(provider_key("custom"), "ai.provider:custom");
    assert!(request_id(None).unwrap().len() >= 32);
    assert!(request_id(Some("bad id!".into())).is_err());
    assert_eq!(normalize_language(Some(" EN ")), Some("en".into()));
    assert_eq!(normalize_language(Some("auto")), None);

    let schema = json!({
        "type": "object",
        "properties": { "k": { "type": "string" } },
        "required": ["k"]
    });
    assert!(check_schema(&schema, &json!({"k": "v"}), r#"{"k":"v"}"#).is_ok());
    assert!(check_schema(&schema, &json!({"k": 1}), r#"{"k":1}"#).is_err());
    let validated = validate_provider_object(Some(&schema), None, "```json\n{\"k\":\"ok\"}\n```").unwrap();
    assert_eq!(validated, Some(json!({"k": "ok"})));

    let sys = with_context(
        Some("Base prompt.".into()),
        &[ContextItem {
            uri: "note://1".into(),
            title: Some("Note".into()),
            text: "ctx body".into(),
        }],
    )
    .unwrap();
    assert!(sys.starts_with("Base prompt."));
    assert!(sys.contains("<context uri=\"note://1\" title=\"Note\">"));
    assert!(sys.contains("ctx body"));

    let cfg = json!({"baseUrl": "http://localhost:11434/v1"})
        .as_object()
        .unwrap()
        .clone();
    let (host, local) = destination_for_config(&cfg, None);
    assert_eq!(host, "localhost:11434");
    assert!(local);

    // Cancellation token triggers cancelled ModelError via `until`.
    let token = CancellationToken::new();
    token.cancel();
    let res: Result<(), ModelError> = until(std::future::pending(), Duration::from_secs(5), &token).await;
    let err = res.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(err.is_cancelled());
}
