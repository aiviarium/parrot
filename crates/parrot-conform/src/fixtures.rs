//! The fixtures built into the binary, so `parrot conform` runs without a checkout.

/// `(name, text)` of the driver fixtures (spec 30).
pub const DRIVER: &[(&str, &str)] = &[
    ("initialize", include_str!("../fixtures/driver/initialize.jsonl")),
    ("text-turn", include_str!("../fixtures/driver/text-turn.jsonl")),
    (
        "approval-allowed",
        include_str!("../fixtures/driver/approval-allowed.jsonl"),
    ),
    (
        "approval-denied",
        include_str!("../fixtures/driver/approval-denied.jsonl"),
    ),
    ("cancel", include_str!("../fixtures/driver/cancel.jsonl")),
    ("options", include_str!("../fixtures/driver/options.jsonl")),
];

/// `(name, text)` of the client fixtures run against a runtime (spec 4–17).
pub const RUNTIME: &[(&str, &str)] = &[
    ("initialize", include_str!("../fixtures/runtime/initialize.jsonl")),
    ("session", include_str!("../fixtures/runtime/session.jsonl")),
    ("prompt", include_str!("../fixtures/runtime/prompt.jsonl")),
    ("request", include_str!("../fixtures/runtime/request.jsonl")),
    ("queue", include_str!("../fixtures/runtime/queue.jsonl")),
    ("cancel", include_str!("../fixtures/runtime/cancel.jsonl")),
    ("options", include_str!("../fixtures/runtime/options.jsonl")),
    ("lists", include_str!("../fixtures/runtime/lists.jsonl")),
    ("snapshot", include_str!("../fixtures/runtime/snapshot.jsonl")),
    ("read", include_str!("../fixtures/runtime/read.jsonl")),
];
