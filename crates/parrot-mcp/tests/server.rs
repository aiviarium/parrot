//! The MCP server against a fake runtime on an in-process duplex stream.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parrot_mcp::server::{self, Auth, Config};
use parrot_mcp::upstream::Upstream;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

const SESSION: &str = "ses_01FAKEPARENT0000000000000";
const CHILD: &str = "ses_01FAKECHILD00000000000000";
const TOKEN: &str = "tok-made-up-secret";

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A fake runtime: answers the methods the server uses and records every call.
fn fake_runtime(stream: DuplexStream, calls: Calls) -> mpsc::UnboundedSender<Value> {
    let (r, mut w) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let line = format!("{msg}\n");
            if w.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let out = tx.clone();
    tokio::spawn(async move {
        // Keys of calls the fake has seen; the first call of `acme.deploy.run` waits for approval.
        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let msg: Value = serde_json::from_str(&line).unwrap();
            let method = msg["method"].as_str().unwrap().to_string();
            let params = msg["params"].clone();
            calls.lock().unwrap().push((method.clone(), params.clone()));
            let result = match method.as_str() {
                "initialize" => {
                    if params["binding"]["token"] != TOKEN {
                        json!({ "error": { "code": -32003, "message": "bad token" } })
                    } else {
                        json!({ "result": {
                            "protocol": 1,
                            "peer": { "kind": "runtime", "name": "fake", "version": "0" },
                            "runtime": { "id": "rt_FAKE", "env": "env_FAKE" },
                            "features": {},
                            "connection": { "id": "con_FAKE", "principal": SESSION, "scopes": ["read", "prompt", "delegate"] },
                        }})
                    }
                }
                "session/get" => json!({ "result": { "session": {
                    "id": params["session"], "workspace": "wsp_01FAKEWORKSPACE00000000000",
                    "task": { "id": "tsk_FAKE", "status": "running" },
                    "state": "running", "kind": "child",
                }}}),
                "session/read" => {
                    json!({ "result": { "items": [{ "id": "itm_1", "kind": "assistant", "text": "hello" }] } })
                }
                "subscribe" => json!({ "result": { "subscription": "sub_FAKE", "head": 1 } }),
                "function/list" => json!({ "result": { "functions": [
                    { "name": "acme.notes.add", "description": "Add a note", "pinned": true, "effects": ["write"], "provider": "con_X",
                      "input": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] },
                      "output": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } },
                    { "name": "acme.deploy.run", "description": "Deploy the site", "effects": ["destructive"], "provider": "con_X",
                      "input": { "type": "object" }, "output": { "type": "object" } },
                ]}}),
                "function/search" => json!({ "result": { "functions": [
                    { "name": "acme.deploy.run", "description": "Deploy the site", "effects": ["destructive"], "provider": "con_X",
                      "input": { "type": "object" } },
                ]}}),
                "session/new" => {
                    json!({ "result": { "session": { "id": CHILD, "task": { "id": "tsk_CHILD", "status": "running" } }, "subscription": "sub_2", "head": 2 } })
                }
                "task/wait" => json!({ "result": { "changes": [], "cursor": 7, "timedOut": true } }),
                "function/call" => {
                    let key = params["key"].as_str().unwrap_or_default().to_string();
                    let n = seen.entry(key).or_default();
                    *n += 1;
                    match (params["function"].as_str(), *n) {
                        (Some("acme.deploy.run"), 1) => continue, // waits for a person
                        (Some("acme.deploy.run"), _) => {
                            json!({ "result": { "output": { "deployed": true } } })
                        }
                        (Some("acme.notes.add"), _) => json!({ "result": { "output": { "id": "note_1" } } }),
                        _ => {
                            json!({ "result": { "error": { "code": "pending_approval", "message": "waits", "request": "req_1" } } })
                        }
                    }
                }
                _ => json!({ "error": { "code": -32601, "message": "method not found" } }),
            };
            let mut reply = result;
            reply["jsonrpc"] = json!("2.0");
            reply["id"] = msg["id"].clone();
            let _ = out.send(reply);
        }
    });
    tx
}

struct Harness {
    to_server: WriteHalf<DuplexStream>,
    from_server: Lines<BufReader<ReadHalf<DuplexStream>>>,
    calls: Calls,
    runtime: mpsc::UnboundedSender<Value>,
    next: u64,
}

impl Harness {
    async fn start(config: Config) -> Harness {
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let calls = Calls::default();
        let runtime = fake_runtime(theirs, calls.clone());
        let (r, w) = tokio::io::split(ours);
        let (up, notes) = Upstream::spawn(r, w);
        let auth = Auth {
            binding: "bnd_01FAKEBINDING000000000000".into(),
            token: Some(TOKEN.into()),
        };
        let identity = server::handshake(&up, &auth).await.expect("handshake");
        assert_eq!(identity.session, SESSION);
        let (client, server_end) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(server_end);
        tokio::spawn(server::serve(sr, sw, up, notes, identity, config));
        let (cr, cw) = tokio::io::split(client);
        Harness {
            to_server: cw,
            from_server: BufReader::new(cr).lines(),
            calls,
            runtime,
            next: 1,
        }
    }

    async fn line(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.from_server.next_line())
            .await
            .expect("an answer in time")
            .unwrap()
            .expect("the server is open");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.to_server
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let msg = self.line().await;
            if msg["id"] == id {
                return msg;
            }
        }
    }

    /// Calls a tool and returns the parsed JSON text and `isError`.
    async fn tool(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        let resp = self
            .request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await;
        let result = &resp["result"];
        let content = result["content"].as_array().expect("content");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        let body: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        (body, result["isError"] == true)
    }

    fn calls_of(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

async fn list_all(h: &mut Harness) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor: Option<Value> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let resp = h.request("tools/list", params).await;
        for t in resp["result"]["tools"].as_array().unwrap() {
            assert!(t["outputSchema"].is_object());
            assert!(t["description"].as_str().unwrap().contains("Returns: "));
            names.push(t["name"].as_str().unwrap().to_string());
        }
        match resp["result"].get("nextCursor") {
            Some(c) => cursor = Some(c.clone()),
            None => return names,
        }
    }
}

#[tokio::test]
async fn initialize_negotiates_the_version() {
    let mut h = Harness::start(Config::default()).await;
    let init = h.calls_of("initialize");
    assert_eq!(init[0]["binding"]["id"], "bnd_01FAKEBINDING000000000000");
    let r = h.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } })).await;
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], true);
    let r = h
        .request("initialize", json!({ "protocolVersion": "1999-01-01" }))
        .await;
    assert_eq!(r["result"]["protocolVersion"], server::PROTOCOL_VERSIONS[0]);
    let r = h.request("ping", json!({})).await;
    assert_eq!(r["result"], json!({}));
    let r = h.request("resources/list", json!({})).await;
    assert_eq!(r["error"]["code"], -32601);
}

#[tokio::test]
async fn tools_list_is_sorted_paged_and_filtered_by_scope() {
    let mut h = Harness::start(Config {
        page_size: 3,
        ..Config::default()
    })
    .await;
    let names = list_all(&mut h).await;
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    for expected in [
        "parrot__sessions__spawn",
        "parrot__sessions__send",
        "parrot__tasks__wait",
        "parrot__task__finish",
        "parrot__task__fail",
        "parrot__functions__search",
        "parrot__functions__describe",
        "parrot__functions__call",
        "acme__notes__add",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} missing from {names:?}"
        );
    }
    // `manage` and `workspace` aren't granted, and unpinned functions are reached by search.
    assert!(!names.contains(&"parrot__sessions__stop".to_string()));
    assert!(!names.contains(&"parrot__workspaces__merge".to_string()));
    assert!(!names.contains(&"acme__deploy__run".to_string()));
    let r = h.request("tools/list", json!({ "cursor": "nope" })).await;
    assert_eq!(r["error"]["code"], -32602);
}

#[tokio::test]
async fn search_returns_compact_infos() {
    let mut h = Harness::start(Config::default()).await;
    let (body, err) = h
        .tool("parrot__functions__search", json!({ "query": "deploy" }))
        .await;
    assert!(!err);
    assert_eq!(body["functions"][0]["name"], "acme.deploy.run");
    assert!(body["functions"][0].get("input").is_none());
    assert_eq!(h.calls_of("function/search")[0]["session"], SESSION);
    let (body, err) = h.tool("parrot__functions__search", json!({})).await;
    assert!(err);
    assert_eq!(body["error"]["code"], "invalid_input");
}

#[tokio::test]
async fn session_function_spawns_a_child_in_the_callers_workspace() {
    let mut h = Harness::start(Config::default()).await;
    let (body, err) = h
        .tool(
            "parrot__sessions__spawn",
            json!({ "agent": "claude", "brief": "Count the files", "key": "spawn-1", "wake": ["done"] }),
        )
        .await;
    assert!(!err, "{body}");
    assert_eq!(body["session"], CHILD);
    assert_eq!(body["task"]["status"], "running");
    let p = &h.calls_of("session/new")[0];
    assert_eq!(p["parent"], SESSION);
    assert_eq!(
        p["workspace"],
        json!({ "kind": "use", "workspace": "wsp_01FAKEWORKSPACE00000000000" })
    );
    assert_eq!(p["task"]["brief"], "Count the files");
    assert_eq!(p["wake"]["on"], json!(["done"]));
    assert_eq!(p["key"], "spawn-1");

    let (body, err) = h
        .tool(
            "parrot__sessions__read",
            json!({ "sessions": [CHILD], "fields": ["id", "state"] }),
        )
        .await;
    assert!(!err);
    assert_eq!(
        body["results"][CHILD]["session"],
        json!({ "id": CHILD, "state": "running" })
    );
    assert_eq!(body["results"][CHILD]["items"][0]["id"], "itm_1");

    let (body, err) = h
        .tool("parrot__sessions__spawn", json!({ "brief": "no agent" }))
        .await;
    assert!(err);
    assert_eq!(body["error"]["code"], "invalid_input");
}

#[tokio::test]
async fn wait_is_capped_by_the_tool_timeout() {
    let mut h = Harness::start(Config {
        tool_timeout: Some(Duration::from_secs(30)),
        ..Config::default()
    })
    .await;
    let (body, err) = h
        .tool(
            "parrot__tasks__wait",
            json!({ "sessions": [CHILD], "timeout": 3600 }),
        )
        .await;
    assert!(!err);
    assert_eq!(body["cursor"], 7);
    // 30 s minus the 5 s margin, minus 2 s so the runtime answers first.
    assert_eq!(h.calls_of("task/wait")[0]["timeout"], 23);
}

#[tokio::test]
async fn task_finish_and_fail_act_on_the_caller() {
    let mut h = Harness::start(Config::default()).await;
    let (_, err) = h
        .tool("parrot__task__fail", json!({ "error": "no network" }))
        .await;
    // The fake has no task/fail: the protocol error comes back with a fixed code.
    assert!(err);
    let p = &h.calls_of("task/fail")[0];
    assert_eq!(p, &json!({ "session": SESSION, "error": "no network" }));
}

#[tokio::test]
async fn pinned_function_runs_through_function_call() {
    let mut h = Harness::start(Config::default()).await;
    let (body, err) = h
        .tool("acme__notes__add", json!({ "text": "hi", "key": "note-key" }))
        .await;
    assert!(!err);
    assert_eq!(body, json!({ "id": "note_1" }));
    let p = &h.calls_of("function/call")[0];
    assert_eq!(p["function"], "acme.notes.add");
    assert_eq!(p["input"], json!({ "text": "hi" }));
    assert_eq!(p["key"], "note-key");
    assert_eq!(p["session"], SESSION);
}

#[tokio::test]
async fn call_needing_approval_returns_pending_and_its_key_collects_the_result() {
    // A 2 s tool timeout leaves a 1 s call limit.
    let mut h = Harness::start(Config {
        tool_timeout: Some(Duration::from_secs(2)),
        ..Config::default()
    })
    .await;
    let started = std::time::Instant::now();
    let (body, err) = h
        .tool(
            "parrot__functions__call",
            json!({ "function": "acme.deploy.run", "input": {} }),
        )
        .await;
    assert!(err);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(body["error"]["code"], "pending_approval");
    let key = body["error"]["key"].as_str().expect("a key").to_string();
    assert!(key.starts_with("k_"));

    let (body, err) = h
        .tool(
            "parrot__functions__call",
            json!({ "function": "acme.deploy.run", "input": {}, "key": key }),
        )
        .await;
    assert!(!err, "{body}");
    assert_eq!(body, json!({ "deployed": true }));
    let keys: Vec<Value> = h
        .calls_of("function/call")
        .iter()
        .map(|p| p["key"].clone())
        .collect();
    assert_eq!(keys, vec![json!(key), json!(key)]);
}

#[tokio::test]
async fn runtime_pending_approval_carries_the_key() {
    let mut h = Harness::start(Config::default()).await;
    let (body, err) = h
        .tool(
            "parrot__functions__call",
            json!({ "function": "acme.other.thing", "key": "mine" }),
        )
        .await;
    assert!(err);
    assert_eq!(body["error"]["code"], "pending_approval");
    assert_eq!(body["error"]["key"], "mine");
    assert_eq!(body["error"]["request"], "req_1");
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error() {
    let mut h = Harness::start(Config::default()).await;
    let r = h
        .request(
            "tools/call",
            json!({ "name": "parrot__sessions__stop", "arguments": {} }),
        )
        .await;
    assert_eq!(r["error"]["code"], -32602);
}

#[tokio::test]
async fn scope_change_sends_list_changed() {
    let mut h = Harness::start(Config::default()).await;
    // Let the server subscribe before the update arrives.
    let _ = h.request("ping", json!({})).await;
    h.runtime
        .send(json!({ "jsonrpc": "2.0", "method": "update", "params": {
            "subscription": "sub_FAKE", "seq": 2, "at": "2026-01-01T00:00:00Z", "session": SESSION,
            "update": { "type": "session", "session": { "id": SESSION, "grant": ["read", "prompt", "delegate", "manage"] } },
        }}))
        .unwrap();
    let note = h.line().await;
    assert_eq!(note["method"], "notifications/tools/list_changed");
    let names = list_all(&mut h).await;
    assert!(names.contains(&"parrot__sessions__stop".to_string()));
    // The update has no task, so the task functions go away.
    assert!(!names.contains(&"parrot__task__finish".to_string()));
}

#[tokio::test]
async fn a_wrong_token_fails_the_handshake() {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    fake_runtime(theirs, Calls::default());
    let (r, w) = tokio::io::split(ours);
    let (up, _notes) = Upstream::spawn(r, w);
    let auth = Auth {
        binding: "bnd_X".into(),
        token: Some("wrong".into()),
    };
    let err = server::handshake(&up, &auth).await.unwrap_err();
    assert!(
        !err.contains("wrong"),
        "the error must not echo the credential: {err}"
    );
}
