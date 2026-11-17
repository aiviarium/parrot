//! What a real provider (OpenAI's API) answered a caller with no key and one with a made-up key, recorded once
//! into `fixtures/recorded/` and replayed through the transport: the error mapping is held to the provider's own
//! words, not to words made up for the test. No network.

use parrot_models::{Chat, ErrorCode, Message, OpenAi, Role};
use serde_json::Value;

use super::{FixtureTransport, MockReply, test_endpoint};

fn replay(file: &str) -> (u16, String) {
    let e: Value = serde_json::from_str(file).unwrap();
    let status = e["response"]["status"].as_u64().unwrap() as u16;
    let body = serde_json::to_string(&e["response"]["body"]).unwrap();
    (status, body)
}

fn chat() -> Chat {
    Chat {
        model: "gpt-4o-mini".into(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: "Say hello in five words.".into(),
        }],
        max_tokens: Some(40),
        temperature: None,
    }
}

#[tokio::test]
async fn a_recorded_refusal_of_the_key_is_unavailable_and_says_what_the_provider_said() {
    for (file, said) in [
        (
            include_str!("../../fixtures/recorded/unauthorized-chat.json"),
            "You didn't provide an API key",
        ),
        (
            include_str!("../../fixtures/recorded/invalid-key-chat.json"),
            "Incorrect API key provided",
        ),
    ] {
        let (status, body) = replay(file);
        assert_eq!(status, 401);
        let client = OpenAi::new(FixtureTransport::new(vec![MockReply::json(status, &body)]));
        let err = client
            .chat_stream(&test_endpoint(), &chat(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unavailable);
        assert!(err.message.contains("401"), "{}", err.message);
        assert!(err.message.contains(said), "{}", err.message);
    }
}

#[tokio::test]
async fn a_recorded_refusal_of_the_model_list_is_unavailable_too() {
    let (status, body) = replay(include_str!("../../fixtures/recorded/unauthorized-models.json"));
    let client = OpenAi::new(FixtureTransport::new(vec![MockReply::json(status, &body)]));
    let err = client.models(&test_endpoint()).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert!(err.message.contains("401"), "{}", err.message);
}
