//! The driver end to end: JSON-RPC in, `fake-claude` as the CLI, updates out.

use std::path::PathBuf;
use std::time::Duration;

use parrot_driver_claude::driver::{Config, serve};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

struct Client {
    write: WriteHalf<DuplexStream>,
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next: u64,
    updates: Vec<Value>,
    seen: usize,
}

impl Client {
    fn start(scenario: &str) -> Client {
        Client::start_with(scenario, vec![])
    }

    fn start_with(scenario: &str, extra: Vec<(std::ffi::OsString, std::ffi::OsString)>) -> Client {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = if scenario.contains('/') {
            root.join(scenario)
        } else {
            root.join("fixtures/scenarios").join(format!("{scenario}.jsonl"))
        };
        let config = Config {
            claude_bin: env!("CARGO_BIN_EXE_fake-claude").into(),
            env: [("PARROT_FAKE_CLAUDE_SCENARIO".into(), path.into())]
                .into_iter()
                .chain(extra)
                .collect(),
            guard: false,
        };
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(theirs);
        tokio::spawn(serve(config, r, w));
        let (r, write) = tokio::io::split(ours);
        Client {
            write,
            lines: BufReader::new(r).lines(),
            next: 1,
            updates: vec![],
            seen: 0,
        }
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("the driver answers in time")
            .unwrap()
            .expect("the driver is still running");
        serde_json::from_str(&line).unwrap()
    }

    async fn try_call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
        self.write.write_all(line.as_bytes()).await.unwrap();
        loop {
            let msg = self.read().await;
            if msg["method"] == "update" {
                self.updates.push(msg["params"].clone());
            } else if msg["id"] == id {
                return msg;
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let msg = self.try_call(method, params).await;
        assert!(msg["error"].is_null(), "{method} failed: {msg}");
        msg["result"].clone()
    }

    /// Waits for an update that `pred` accepts, past the ones already waited for.
    async fn wait(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            if let Some(i) = self.updates[self.seen..].iter().position(&pred) {
                self.seen += i + 1;
                return self.updates[self.seen - 1].clone();
            }
            let msg = self.read().await;
            if msg["method"] == "update" {
                self.updates.push(msg["params"].clone());
            }
        }
    }

    async fn bind(&mut self) -> Value {
        self.bind_with(json!({})).await
    }

    async fn bind_with(&mut self, more: Value) -> Value {
        let init = self
            .call("initialize", json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}))
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "claude");
        assert!(agents["agents"][0]["features"]["session.steer"].is_object());
        let cwd = std::env::temp_dir();
        let mut params = json!({"session": "s1", "binding": "b1", "agent": "claude", "workspace": cwd, "cwd": cwd,
            "options": {}, "control": "native", "deny": []});
        for (k, v) in more.as_object().unwrap() {
            params[k] = v.clone();
        }
        self.call("binding/start", params).await
    }

    async fn prompt(&mut self, text: &str) {
        let r = self
            .call(
                "binding/prompt",
                json!({"binding": "b1", "message": "m1", "text": text, "steer": false}),
            )
            .await;
        assert_eq!(r["disposition"], "started");
    }
}

fn is_state(state: &'static str) -> impl Fn(&Value) -> bool {
    move |u| u["update"]["type"] == "state" && u["update"]["state"] == state
}

#[tokio::test]
async fn text_turn_streams_and_goes_idle() {
    let mut c = Client::start("text-turn");
    let start = c.bind().await;
    assert_eq!(start["level"], "read");
    assert_eq!(start["actions"][0]["id"], "review");
    assert_eq!(start["options"][0]["schema"]["oneOf"][0]["const"], "default");
    assert_eq!(
        start["options"][1]["schema"]["oneOf"][2]["x-tags"],
        json!(["plan", "read-only"])
    );
    c.prompt("Say hello").await;
    c.wait(is_state("running")).await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    let r = c.wait(|u| u["update"]["type"] == "ref").await;
    assert_eq!(r["update"]["ref"]["id"], "sess-fake-0001");
    let a = c.wait(|u| u["update"]["item"]["append"].is_object()).await;
    assert_eq!(a["update"]["item"]["append"]["text"], "Hello ");
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    c.wait(is_state("idle")).await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn bash_approval_then_completes() {
    let mut c = Client::start("bash-allowed");
    c.bind().await;
    c.prompt("List the files").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["id"], "perm-1");
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "perm-1", "answer": {"choice": "allow"}}),
    )
    .await;
    let done = c
        .wait(|u| {
            u["update"]["item"]["id"] == "toolu_fake_01" && u["update"]["item"]["set"]["status"] == "ok"
        })
        .await;
    assert_eq!(done["update"]["item"]["set"]["output"], "README.md\nsrc");
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::start("interrupt");
    c.bind().await;
    c.prompt("Count").await;
    c.wait(|u| u["update"]["item"]["append"]["text"] == "One, two, ")
        .await;
    let r = c.call("binding/cancel", json!({"binding": "b1"})).await;
    assert_eq!(r["interrupted"], true);
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "interrupted");
    assert_eq!(t["update"]["turn"]["stop"], "cancelled");
    c.wait(is_state("idle")).await;
}

fn is_kind(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |u| u["update"]["type"] == kind
}

/// The item, turn and request updates a run sent, without frames.
fn projected(updates: &[Value]) -> Vec<Value> {
    updates
        .iter()
        .filter(|u| ["item", "turn", "request"].contains(&u["update"]["type"].as_str().unwrap_or("")))
        .cloned()
        .collect()
}

#[tokio::test]
async fn attachments_and_context_reach_claude() {
    let mut c = Client::start("attachments");
    let start = c.bind().await;
    assert_eq!(start["level"], "read");
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(
        agents["agents"][0]["features"]["attachments"]["kinds"],
        json!(["image", "file"])
    );
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m1", "text": "Look", "steer": false,
            "context": [{"uri": "file:///work/notes.md", "text": "Notes"}],
            "attachments": [
                {"name": "dot.png", "type": "image/png", "data": "iVBORw0KGgo="},
                {"name": "tune.mp3", "type": "audio/mpeg", "data": "AAAA"}
            ]}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let n = c.wait(is_kind("notice")).await;
    assert!(n["update"]["text"].as_str().unwrap().contains("tune.mp3"), "{n}");
    let t = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    let user = c
        .updates
        .iter()
        .find(|u| u["update"]["item"]["id"] == "m1")
        .unwrap();
    assert_eq!(user["update"]["item"]["set"]["attachments"][0]["name"], "dot.png");
    assert!(user["update"]["item"]["set"]["attachments"][0]["data"].is_null());
}

#[tokio::test]
async fn mcp_server_is_injected_with_the_token_in_the_environment() {
    let record = std::env::temp_dir().join(format!("parrot-fake-claude-{}.json", std::process::id()));
    let mut c = Client::start_with(
        "text-turn",
        vec![("PARROT_FAKE_CLAUDE_RECORD".into(), record.clone().into())],
    );
    c.bind_with(json!({"mcp": {"url": "http://127.0.0.1:4711/mcp", "token": "tok-made-up-123"}, "pin": {"level": "read", "network": false}}))
        .await;
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    std::fs::remove_file(&record).ok();
    let args: Vec<&str> = seen["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(!args.iter().any(|a| a.contains("tok-made-up-123")), "{args:?}");
    let i = args.iter().position(|a| *a == "--mcp-config").unwrap();
    let config: Value = serde_json::from_str(args[i + 1]).unwrap();
    assert_eq!(config["mcpServers"]["parrot"]["url"], "http://127.0.0.1:4711/mcp");
    assert_eq!(
        config["mcpServers"]["parrot"]["headers"]["Authorization"],
        "Bearer ${PARROT_MCP_TOKEN}"
    );
    assert_eq!(seen["mcpToken"], "tok-made-up-123");
    let s = args.iter().position(|a| *a == "--setting-sources").unwrap();
    assert_eq!(args[s + 1], "");
}

#[tokio::test]
async fn frames_rebuild_the_same_items() {
    for scenario in ["text-turn", "bash-allowed"] {
        let mut c = Client::start(scenario);
        c.bind().await;
        c.prompt("Go").await;
        if scenario == "bash-allowed" {
            c.wait(is_kind("request")).await;
            c.call(
                "binding/answer",
                json!({"binding": "b1", "request": "perm-1", "answer": {"choice": "allow"}}),
            )
            .await;
        }
        c.wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
            .await;
        c.wait(is_state("idle")).await;
        let frames: Vec<Value> = c
            .updates
            .iter()
            .filter(|u| u["update"]["type"] == "frame")
            .map(|u| {
                assert_eq!(u["update"]["source"], "native");
                assert_eq!(u["update"]["data"]["v"], "1");
                u["update"]["data"].clone()
            })
            .collect();
        let rebuilt: Vec<Value> = parrot_driver_claude::frame::rebuild("b1", &frames)
            .iter()
            .map(|u| serde_json::to_value(u).unwrap())
            .collect();
        assert_eq!(rebuilt, projected(&c.updates), "{scenario}");
    }
}

#[tokio::test]
async fn answer_sends_running_before_the_turn_ends_and_options_still_apply() {
    let mut c = Client::start("bash-allowed");
    c.bind().await;
    c.prompt("List the files").await;
    c.wait(is_kind("request")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "perm-1", "answer": {"choice": "allow"}}),
    )
    .await;
    c.wait(is_state("idle")).await;
    let pos = |pred: &dyn Fn(&Value) -> bool| c.updates.iter().rposition(pred).unwrap();
    let running = pos(&|u| is_state("running")(u));
    let ended = pos(&|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] == "completed");
    assert!(running < ended, "running at {running}, turn ended at {ended}");
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"permissionMode": "acceptEdits"}}),
        )
        .await;
    assert_eq!(r["level"], "write");
}

#[tokio::test]
async fn recorded_trace_plays_end_to_end() {
    let mut c = Client::start("../parrot-conform/fixtures/traces/claude-2.1.327.jsonl");
    c.bind().await;
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(agents["agents"][0]["harness"]["verified"], true, "{agents}");
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m1", "text": "Name the color", "steer": false,
                "attachments": [{"name": "red.png", "type": "image/png", "data": "iVBORw0KGgo="}]}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let req = c
        .wait(|u| u["update"]["type"] == "request" && u["update"]["request"]["status"] == "open")
        .await;
    let id = req["update"]["request"]["id"].as_str().unwrap().to_owned();
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": id, "answer": {"choice": "allow"}}),
    )
    .await;
    let done = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(done["update"]["turn"]["status"], "completed");
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m2", "text": "Count", "steer": false}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    c.wait(|u| u["update"]["item"]["append"].is_object()).await;
    c.call("binding/cancel", json!({"binding": "b1"})).await;
    let done = c
        .wait(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running")
        .await;
    assert_eq!(done["update"]["turn"]["status"], "interrupted");
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn a_subagent_stops_through_its_task() {
    let mut c = Client::start("subagent-stop");
    c.bind().await;
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(
        agents["agents"][0]["features"]["session.subagents"]["cancel"],
        true
    );
    c.prompt("Walk the tree").await;
    c.wait(|u| u["subagent"] == "toolu_task_02").await;
    let r = c
        .call(
            "binding/cancel",
            json!({"binding": "b1", "subagent": "toolu_task_02"}),
        )
        .await;
    assert_eq!(r["interrupted"], true);
    let stopped = c
        .wait(|u| {
            u["update"]["item"]["id"] == "toolu_task_02:subagent"
                && u["update"]["item"]["set"]["status"] != "running"
        })
        .await;
    assert_eq!(stopped["update"]["item"]["set"]["status"], "cancelled");
    c.wait(is_state("idle")).await;
    // A subagent with no task yet is not interrupted.
    let r = c
        .call(
            "binding/cancel",
            json!({"binding": "b1", "subagent": "toolu_none"}),
        )
        .await;
    assert_eq!(r["interrupted"], false);
}

#[tokio::test]
async fn a_fork_at_an_earlier_turn_resumes_at_its_last_message() {
    let mut c = Client::start("two-turns");
    c.bind().await;
    c.prompt("First").await;
    c.wait(is_state("idle")).await;
    c.call(
        "binding/prompt",
        json!({"binding": "b1", "message": "m2", "text": "Second", "steer": false}),
    )
    .await;
    c.wait(|u| {
        u["update"]["type"] == "turn"
            && u["update"]["turn"]["id"] == "turn-2"
            && u["update"]["turn"]["status"] != "running"
    })
    .await;
    let fork = c
        .call("binding/fork", json!({"binding": "b1", "at": "turn-1"}))
        .await;
    assert_eq!(fork["ref"]["id"], "fork:sess-fake-two@a-turn-1");
    let latest = c
        .call("binding/fork", json!({"binding": "b1", "at": "turn-2"}))
        .await;
    assert_eq!(latest["ref"]["id"], "fork:sess-fake-two");
    let bad = c
        .try_call("binding/fork", json!({"binding": "b1", "at": "turn-9"}))
        .await;
    assert_eq!(bad["error"]["code"], -32602);

    let record = std::env::temp_dir().join(format!("parrot-fake-claude-fork-{}.json", std::process::id()));
    let mut d = Client::start_with(
        "two-turns",
        vec![("PARROT_FAKE_CLAUDE_RECORD".into(), record.clone().into())],
    );
    d.bind_with(json!({"resume": fork["ref"]})).await;
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    std::fs::remove_file(&record).ok();
    let args: Vec<&str> = seen["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = |flag: &str| args.iter().position(|a| *a == flag).map(|i| args[i + 1]);
    assert_eq!(at("--resume"), Some("sess-fake-two"));
    assert_eq!(at("--resume-session-at"), Some("a-turn-1"));
    assert!(args.contains(&"--fork-session"));
}

#[tokio::test]
async fn auth_status_judges_sign_in() {
    let bin = std::ffi::OsString::from(env!("CARGO_BIN_EXE_fake-claude"));
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: no other test in this binary reads these variables.
    unsafe {
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path());
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
    let a = parrot_driver_claude::driver::probed(&bin).await;
    assert_eq!(serde_json::to_value(a.auth).unwrap(), "signed_in");
    let text = serde_json::to_string(&a).unwrap();
    assert!(!text.contains("example.test"), "the account stays out: {text}");
    unsafe { std::env::set_var("PARROT_FAKE_CLAUDE_LOGIN", "out") };
    let a = parrot_driver_claude::driver::probed(&bin).await;
    assert_eq!(serde_json::to_value(a.auth).unwrap(), "signed_out");
    unsafe { std::env::remove_var("PARROT_FAKE_CLAUDE_LOGIN") };
}
