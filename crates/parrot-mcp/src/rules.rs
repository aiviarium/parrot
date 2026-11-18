//! The rules of spec 27 for calls from harness code: JSON results as text, fixed error codes,
//! idempotency keys, size limits, and calls that never outlast the harness's tool timeout.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parrot_proto::rpc::RpcError;
use serde_json::{Map, Value, json};

/// No call blocks longer than this, whatever the harness allows (spec 27.2).
pub const MAX_CALL: Duration = Duration::from_secs(180);
/// A list or read result over this is cut at an entry boundary (spec 27.1).
pub const MAX_RESULT: usize = 256 * 1024;

/// How long a call may block: the harness's tool-call timeout minus a margin, at most 3 minutes.
pub fn call_limit(tool_timeout: Option<Duration>) -> Duration {
    let Some(timeout) = tool_timeout else {
        return MAX_CALL;
    };
    let margin = (timeout / 10).max(Duration::from_secs(5));
    timeout
        .saturating_sub(margin)
        .clamp(Duration::from_secs(1), MAX_CALL)
}

/// A fresh idempotency key, so a call cut off by the time limit can be collected later.
pub fn new_key() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    let a = h.finish();
    h.write_u64(a);
    format!("k_{a:016x}{:016x}", h.finish())
}

/// A successful MCP tool result: the JSON as the only text content and as `structuredContent`.
pub fn ok(output: Value) -> Value {
    let text = serde_json::to_string(&output).unwrap_or_else(|_| "null".into());
    let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    if output.is_object() {
        result["structuredContent"] = output;
    }
    result
}

/// An error MCP tool result: `{ "error": { "code", "message", ... } }` with `isError: true`.
pub fn error(code: &str, message: impl Into<String>, extra: Map<String, Value>) -> Value {
    let mut err = Map::new();
    err.insert("code".into(), json!(code));
    err.insert("message".into(), json!(message.into()));
    err.extend(extra);
    let body = json!({ "error": err });
    let text = serde_json::to_string(&body).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": body, "isError": true })
}

pub fn invalid_input(message: impl Into<String>) -> Value {
    error("invalid_input", message, Map::new())
}

/// The fixed error code of a protocol error, so code branches on `code`, never on prose.
pub fn code_of(err: &RpcError) -> &'static str {
    match err.code {
        -32602 => "invalid_input",
        -32601 | -32001 => "unsupported",
        -32002 => "not_found",
        -32003 => "forbidden",
        -32004 => "wrong_state",
        -32020 => "harness_failed",
        -32030 => "offline",
        -32032 => "budget",
        -32040 => "link_failed",
        -32050 => "moved",
        _ => "internal",
    }
}

/// An MCP error result for a protocol error; its `data` is kept as `data`.
pub fn from_rpc(err: &RpcError) -> Value {
    let mut extra = Map::new();
    if let Some(data) = &err.data {
        extra.insert("data".into(), data.clone());
    }
    error(code_of(err), err.message.clone(), extra)
}

/// The MCP result of a `FunctionResult` (spec 25). Content blocks are kept in the JSON, never
/// returned as image blocks.
pub fn from_function_result(result: Value, key: Option<&str>) -> Value {
    if let Some(err) = result.get("error").filter(|e| e.is_object()) {
        let code = err["code"].as_str().unwrap_or("internal");
        let message = err["message"].as_str().unwrap_or_default();
        let mut extra: Map<String, Value> = err
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| *k != "code" && *k != "message")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if code == "pending_approval"
            && let Some(key) = key
        {
            extra.insert("key".into(), json!(key));
        }
        return error(code, message, extra);
    }
    let output = match (result.get("output"), result.get("content")) {
        (Some(output), _) => output.clone(),
        (None, Some(content)) => json!({ "content": content }),
        (None, None) => json!({}),
    };
    ok(output)
}

/// The result of a call cut off by the time limit while it waits for approval or for the runtime.
pub fn pending_approval(key: &str, request: Option<&Value>) -> Value {
    let mut extra = Map::new();
    extra.insert("key".into(), json!(key));
    if let Some(request) = request {
        extra.insert("request".into(), request.clone());
    }
    error(
        "pending_approval",
        "the call waits for approval; call again with the same key to get its result",
        extra,
    )
}

/// Keeps only the named top-level fields of an object.
pub fn select_fields(value: &mut Value, fields: Option<&[String]>) {
    let (Some(fields), Some(obj)) = (fields, value.as_object_mut()) else {
        return;
    };
    obj.retain(|k, _| fields.iter().any(|f| f == k));
}

/// Cuts a list at an entry boundary so its JSON stays under `max` bytes. Returns how many entries
/// were kept; the rest belong to the next page.
pub fn cut(entries: &mut Vec<Value>, max: usize) -> usize {
    let mut size = 2;
    for (i, entry) in entries.iter().enumerate() {
        size += serde_json::to_string(entry).map(|s| s.len()).unwrap_or(0) + 1;
        if size > max && i > 0 {
            entries.truncate(i);
            return i;
        }
    }
    entries.len()
}

/// The strings of an optional array field.
pub fn strings(input: &Value, field: &str) -> Option<Vec<String>> {
    input
        .get(field)?
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_keeps_a_margin_and_a_cap() {
        assert_eq!(call_limit(None), MAX_CALL);
        assert_eq!(call_limit(Some(Duration::from_secs(600))), MAX_CALL);
        assert_eq!(call_limit(Some(Duration::from_secs(60))), Duration::from_secs(54));
        assert_eq!(call_limit(Some(Duration::from_secs(20))), Duration::from_secs(15));
        assert_eq!(call_limit(Some(Duration::from_secs(2))), Duration::from_secs(1));
    }

    #[test]
    fn keys_differ() {
        assert_ne!(new_key(), new_key());
    }

    #[test]
    fn results_are_json_text() {
        let r = ok(json!({ "a": 1 }));
        assert_eq!(r["content"][0]["text"], "{\"a\":1}");
        assert_eq!(r["structuredContent"]["a"], 1);
        let e = from_rpc(&RpcError::new(-32003, "no"));
        assert_eq!(e["isError"], true);
        let body: Value = serde_json::from_str(e["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "forbidden");
    }

    #[test]
    fn cut_at_entry_boundary() {
        let mut entries: Vec<Value> = (0..10)
            .map(|i| json!({ "i": i, "pad": "x".repeat(100) }))
            .collect();
        let kept = cut(&mut entries, 400);
        assert_eq!(kept, 3);
        assert_eq!(entries.len(), 3);
    }
}
