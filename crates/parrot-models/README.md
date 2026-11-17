# parrot-models

Clients for OpenAI-compatible model APIs. parrot-models has chat completions, streamed and whole,
structured output checked against a JSON Schema, audio transcription and model lists. It doesn't make
HTTP requests itself. Requests go through the `Transport` trait, and tests replay fixtures without a network.

| Part | What it does |
| --- | --- |
| `Transport` | Sends a `TransportRequest` and returns the response with a streamed body. You write it over the HTTP client you use |
| `OpenAi::chat_stream` | A chat completion as server-sent events, calling back with each piece of text. It asks for usage with `stream_options` and asks again without it if the server refuses |
| `OpenAi::chat_structured` | A chat completion whose answer must match a schema. It tries `json_schema`, then `json_object` with the schema in the prompt, then the prompt alone. Each server's refusals are kept for the client's lifetime |
| `OpenAi::transcribe` | `POST /audio/transcriptions` as multipart, asking for `verbose_json` and falling back to `json` |
| `OpenAi::models` | The server's model list, with a guess at what each model does |
| `until` | Runs a request against a deadline and a `CancellationToken`. Dropping it closes the connection |
| `ModelError`, `ErrorCode` | Errors as `INVALID`, `NOT_FOUND`, `TIMEOUT`, `UNAVAILABLE` or `INTERNAL`, with the server's own message |

## Example

```rust
use parrot_models::{Chat, Endpoint, Message, OpenAi, Role};

let client = OpenAi::new(my_transport);
let ep = Endpoint { base: "http://localhost:11434/v1".into(), key: None, headers: vec![] };
let chat = Chat {
    model: "llama3.2".into(),
    messages: vec![Message { role: Role::User, content: "Say hello.".into() }],
    ..Chat::default()
};
let out = client.chat_stream(&ep, &chat, |piece| print!("{piece}")).await?;
println!("\n{:?} tokens in", out.usage.input_tokens);
```

## Fixtures

`fixtures/` holds answers in the formats OpenAI-compatible servers send: completions, streams, a stream
cut short, error bodies and the refusals that make the client fall back. `fixtures/recorded/` holds what
OpenAI's API answered a request with no key and one with a made-up key. The tests in `tests/models/`
replay both through a fixture transport.
