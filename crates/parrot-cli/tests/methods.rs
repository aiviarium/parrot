//! Spec 40: every method of the spec answers as implemented or with `-32001`, never `-32601`;
//! methods outside the spec answer `-32601`.
#![cfg(unix)]

mod common;

use common::{Client, Serve, Setup};
use serde_json::json;

/// The methods of the spec's method index (39) and Appendix E8.
const METHODS: &[&str] = &[
    "agent/list",
    "env/list",
    "session/list",
    "session/get",
    "session/tree",
    "session/read",
    "item/get",
    "workspace/list",
    "workspace/status",
    "request/list",
    "link/list",
    "subscribe",
    "unsubscribe",
    "session/discover",
    "session/bundle",
    "task/wait",
    "function/list",
    "function/search",
    "function/describe",
    "session/screen",
    "session/prompt",
    "session/cancel",
    "queue/list",
    "queue/edit",
    "queue/move",
    "queue/remove",
    "queue/clear",
    "queue/pause",
    "queue/resume",
    "action/invoke",
    "action/list",
    "session/keys",
    "request/answer",
    "option/set",
    "option/list",
    "option/complete",
    "agent/refresh",
    "session/new",
    "session/start",
    "session/stop",
    "session/rename",
    "session/archive",
    "session/adopt",
    "session/rebind",
    "session/configure",
    "task/assign",
    "task/fail",
    "task/finish",
    "workspace/create",
    "workspace/merge",
    "workspace/transfer",
    "workspace/remove",
    "session/delete",
    "session/rewind",
    "function/call",
    "function/provide",
    "token/create",
    "token/list",
    "token/revoke",
    "link/invite",
    "link/add",
    "link/update",
    "link/remove",
    "carrier/list",
];

#[tokio::test]
async fn every_spec_method_is_served_or_unsupported() {
    let serve = Serve::start(Setup::default());
    let mut c = Client::connect(&serve, "owner").await;
    let init = c.initialize().await;
    for profile in ["profile.core", "profile.tree", "profile.tui"] {
        assert!(init["features"].get(profile).is_some(), "{init}");
    }
    let mut missing = Vec::new();
    for method in METHODS {
        let r = c.call(method, json!({})).await;
        if r["error"]["code"] == -32601 {
            missing.push(*method);
        }
    }
    assert!(missing.is_empty(), "answered -32601: {missing:?}");
    let r = c.call("x.example/nothing", json!({})).await;
    assert_eq!(r["error"]["code"], -32601, "{r}");
}
