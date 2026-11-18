//! The pi driver end to end: JSON-RPC in, `fake-pi` as the CLI, updates out.

use std::path::PathBuf;
use std::time::Duration;

use parrot_driver_pi::driver::{Config, serve};
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
    /// Starts the driver with a fake pi whose sessions live in a scratch folder.
    fn start(home: PathBuf) -> Client {
        let config = Config {
            pi_bin: env!("CARGO_BIN_EXE_fake-pi").into(),
            args: vec!["--mode".into(), "rpc".into()],
            env: vec![("FAKE_PI_HOME".into(), home.into_os_string())],
            guard: false,
            data: parrot_driver_pi::driver::default_data(),
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

    fn temp() -> Client {
        let dir = tempfile::tempdir().expect("a scratch folder");
        // The fake keeps its sessions here for as long as the process runs.
        let home = dir.keep();
        Client::start(home)
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("the driver answers in time")
            .unwrap()
            .expect("the driver is still running");
        serde_json::from_str(&line).unwrap()
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
        self.write.write_all(line.as_bytes()).await.unwrap();
        loop {
            let msg = self.read().await;
            if msg["method"] == "update" {
                self.updates.push(msg["params"].clone());
            } else if msg["id"] == id {
                assert!(msg["error"].is_null(), "{method} failed: {msg}");
                return msg["result"].clone();
            }
        }
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
        let init = self
            .call(
                "initialize",
                json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
            )
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "pi");
        assert_eq!(agents["agents"][0]["levels"], json!(["host"]));
        assert!(agents["agents"][0]["features"]["session.steer"].is_object());
        let cwd = std::env::temp_dir();
        self.call(
            "binding/start",
            json!({"session": "s1", "binding": "b1", "agent": "pi", "workspace": cwd, "cwd": cwd,
                "options": {}, "control": "native", "deny": []}),
        )
        .await
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

fn ended(status: &'static str) -> impl Fn(&Value) -> bool {
    move |u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] == status
}

#[tokio::test]
async fn text_turn_streams_and_goes_idle() {
    let mut c = Client::temp();
    let start = c.bind().await;
    assert_eq!(start["level"], "host");
    assert!(
        start["ref"]["id"].as_str().unwrap().starts_with("fake-"),
        "the session id comes from pi: {}",
        start["ref"]["id"]
    );
    assert_eq!(start["actions"][0]["id"], "review");
    c.prompt("[[text:Hello there.]]").await;
    c.wait(is_state("running")).await;
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    let turn = c
        .updates
        .iter()
        .find(|u| u["update"]["type"] == "turn" && u["update"]["turn"]["status"] == "running")
        .expect("the turn started")
        .clone();
    assert_eq!(turn["update"]["turn"]["input"], "m1");
    c.wait(|u| u["update"]["item"]["append"]["text"] == "Hello ")
        .await;
    // A usage update is the message's own tokens; the runtime adds them up.
    let usage = c.wait(|u| u["update"]["type"] == "usage").await;
    assert_eq!(usage["update"]["usage"]["tokens"]["input"], 10);
    let done = c.wait(ended("completed")).await;
    assert_eq!(done["update"]["turn"]["stop"], "end");
    c.wait(is_state("idle")).await;
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn an_extension_confirm_is_answered() {
    let mut c = Client::temp();
    c.bind().await;
    c.prompt("[[confirm:Run the migration?]]").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["title"], "Run the migration?");
    assert_eq!(request["choices"][0]["id"], "allow");
    let id = request["id"].as_str().unwrap().to_owned();
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": id, "answer": {"choice": "allow"}}),
    )
    .await;
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Confirmed.")
        .await;
    c.wait(ended("completed")).await;
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::temp();
    c.bind().await;
    c.prompt("[[stream-until-abort]]").await;
    c.wait(|u| u["update"]["item"]["append"]["text"] == "One, ").await;
    let r = c.call("binding/cancel", json!({"binding": "b1"})).await;
    assert_eq!(r["interrupted"], true);
    let t = c.wait(ended("interrupted")).await;
    assert_eq!(t["update"]["turn"]["stop"], "cancelled");
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn an_image_attachment_reaches_pi_and_a_file_gets_a_notice() {
    let mut c = Client::temp();
    c.bind().await;
    let r = c
        .call(
            "binding/prompt",
            json!({
                "binding": "b1", "message": "m1", "text": "What is this?", "steer": false,
                "attachments": [
                    {"name": "shot.png", "type": "image/png", "data": "iVBORw0KGgo="},
                    {"name": "notes.txt", "type": "text/plain", "data": "aGVsbG8="},
                ],
            }),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let notice = c.wait(|u| u["update"]["type"] == "notice").await;
    assert_eq!(notice["update"]["code"], "attachment_unsupported");
    assert!(
        notice["update"]["text"].as_str().unwrap().contains("notes.txt"),
        "the notice names the file: {}",
        notice["update"]["text"]
    );
    c.wait(|u| u["update"]["item"]["set"]["text"] == "Received 1 image.")
        .await;
    c.wait(ended("completed")).await;
}

#[tokio::test]
async fn options_come_from_pi_and_apply() {
    let mut c = Client::temp();
    let start = c.bind().await;
    let options = start["options"].as_array().unwrap();
    let model = options.iter().find(|o| o["id"] == "model").unwrap();
    assert_eq!(model["schema"]["enum"], json!(["fake/fake-1", "fake/fake-2"]));
    let effort = options.iter().find(|o| o["id"] == "effort").unwrap();
    assert_eq!(
        effort["schema"]["enum"],
        json!(["minimal", "low", "medium", "high"])
    );
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"model": "fake/fake-1", "effort": "high"}}),
        )
        .await;
    let options = r["options"].as_array().unwrap();
    assert_eq!(
        options.iter().find(|o| o["id"] == "model").unwrap()["value"],
        "fake/fake-1"
    );
    assert_eq!(
        options.iter().find(|o| o["id"] == "effort").unwrap()["value"],
        "high"
    );
    assert_eq!(r["level"], "host");
    let update = c.wait(|u| u["update"]["type"] == "options").await;
    assert_eq!(update["update"]["level"], "host");
}

#[tokio::test]
async fn the_thinking_levels_follow_the_model() {
    let mut c = Client::temp();
    c.bind().await;
    // fake-2 does not reason, so pi offers no level but `off`.
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"model": "fake/fake-2"}}),
        )
        .await;
    let effort = r["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "effort")
        .unwrap()
        .clone();
    assert!(effort["schema"].get("enum").is_none(), "no levels: {effort}");
    assert!(effort["value"].is_null());
    // Back on fake-1 the levels return.
    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"model": "fake/fake-1"}}),
        )
        .await;
    let effort = r["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "effort")
        .unwrap();
    assert_eq!(
        effort["schema"]["enum"],
        json!(["minimal", "low", "medium", "high"])
    );
}

#[tokio::test]
async fn compact_is_an_action_and_becomes_a_compaction_item() {
    let mut c = Client::temp();
    let start = c.bind().await;
    assert!(
        start["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "compact"),
        "the compact action is offered: {}",
        start["actions"]
    );
    let r = c
        .call(
            "binding/action",
            json!({"binding": "b1", "action": "compact", "args": "Focus on the notes"}),
        )
        .await;
    assert_eq!(r["status"], "done");
    assert_eq!(r["result"]["summary"], "The context was compacted.");
    let item = c.wait(|u| u["update"]["item"]["kind"] == "compaction").await;
    assert_eq!(item["update"]["item"]["set"]["trigger"], "manual");
    assert_eq!(
        item["update"]["item"]["set"]["summary"],
        "The context was compacted."
    );
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn nested_tool_calls_hang_under_the_tool_that_made_them() {
    let mut c = Client::temp();
    c.bind().await;
    c.prompt("[[nested]]").await;
    let outer = c.wait(|u| u["update"]["item"]["set"]["tool"] == "task").await;
    let outer_id = outer["update"]["item"]["id"].as_str().unwrap().to_owned();
    c.wait(|u| u["update"]["item"]["id"] == outer_id && u["update"]["item"]["set"]["status"] == "ok")
        .await;
    let inner = c.wait(|u| u["update"]["item"]["set"]["tool"] == "grep").await;
    assert_eq!(inner["update"]["item"]["parent"], outer_id);
    assert_eq!(inner["update"]["item"]["set"]["status"], "ok");
    let failed = c.wait(|u| u["update"]["item"]["set"]["status"] == "error").await;
    assert_eq!(failed["update"]["item"]["set"]["tool"], "read");
    assert_eq!(failed["update"]["item"]["parent"], outer_id);
    assert_eq!(failed["update"]["item"]["set"]["output"], "no such file");
    c.wait(|u| u["update"]["item"]["set"]["text"] == "The task looked.")
        .await;
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn a_fork_returns_the_new_session_and_a_start_resumes_it() {
    let mut c = Client::temp();
    c.bind().await;
    c.prompt("[[text:First.]]").await;
    c.wait(|u| u["update"]["item"]["set"]["text"] == "First.").await;
    c.wait(is_state("idle")).await;
    let forked = c.call("binding/fork", json!({"binding": "b1"})).await;
    let id = forked["ref"]["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("forked-"), "the fork names its session: {id}");
    assert_eq!(forked["ref"]["strength"], "strong");
    let start = c
        .call(
            "binding/start",
            json!({"session": "s1", "binding": "b2", "agent": "pi", "workspace": std::env::temp_dir(),
                "cwd": std::env::temp_dir(), "options": {}, "control": "native", "deny": [],
                "resume": {"id": id, "strength": "strong"}}),
        )
        .await;
    assert_eq!(start["ref"]["id"], id);
    c.call("binding/stop", json!({"binding": "b2"})).await;
}

#[tokio::test]
async fn the_agent_reports_the_version_pi_prints() {
    let mut c = Client::temp();
    c.call(
        "initialize",
        json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
    )
    .await;
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(agents["agents"][0]["harness"]["version"], "1.5.1");
    assert!(agents["agents"][0]["features"]["session.fork"].is_object());
    assert!(agents["agents"][0]["features"]["session.history"].is_object());
}

#[tokio::test]
async fn history_reads_a_session_file() {
    let dir = tempfile::tempdir().expect("a scratch folder");
    let sessions = dir.path().join("sessions/--work-demo--");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("2026_01_02_sess-made-up.jsonl"),
        concat!(
            r#"{"type":"session","version":3,"id":"sess-made-up","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/work/demo"}"#,
            "\n",
            r#"{"type":"message","id":"e1","parentId":null,"timestamp":"2026-01-02T03:04:05.100Z","message":{"role":"user","content":"List the notes"}}"#,
            "\n",
            r#"{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-01-02T03:04:06.000Z","message":{"role":"assistant","content":[{"type":"text","text":"One note."}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let mut c = Client::temp();
    c.call(
        "initialize",
        json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
    )
    .await;
    // The driver reads the root at call time, so the environment set here applies.
    unsafe { std::env::set_var("PI_CODING_AGENT_SESSION_DIR", dir.path().join("sessions")) };
    let found = c
        .call("history/discover", json!({"agent": "pi", "cwd": "/work/demo"}))
        .await;
    assert_eq!(found["found"][0]["ref"]["id"], "sess-made-up");
    assert_eq!(found["found"][0]["title"], "List the notes");
    let read = c
        .call(
            "history/read",
            json!({"agent": "pi", "ref": {"id": "sess-made-up", "strength": "strong"}}),
        )
        .await;
    unsafe { std::env::remove_var("PI_CODING_AGENT_SESSION_DIR") };
    let items = read["updates"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|u| u["update"]["item"]["set"]["text"] == "One note.")
    );
    assert!(items.iter().all(|u| u["binding"] == ""));
}

#[tokio::test]
async fn mcp_reaches_pi_with_the_token_in_the_environment() {
    let mut c = Client::temp();
    let cwd = std::env::temp_dir();
    c.call(
        "initialize",
        json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
    )
    .await;
    c.call(
        "binding/start",
        json!({"session": "s1", "binding": "b1", "agent": "pi", "workspace": cwd, "cwd": cwd,
            "options": {}, "control": "native", "deny": [],
            "mcp": {"url": "http://127.0.0.1:9/mcp", "token": "test-token-123"}}),
    )
    .await;
    c.prompt("[[mcp]]").await;
    let tool = c.wait(|u| u["update"]["item"]["set"]["status"] == "ok").await;
    let output = tool["update"]["item"]["set"]["output"].as_str().unwrap();
    let output: Value = serde_json::from_str(output).expect("the fake reports JSON");
    assert_eq!(output["url"], "http://127.0.0.1:9/mcp");
    assert_eq!(output["authorization"], "Bearer test-token-123");
}
