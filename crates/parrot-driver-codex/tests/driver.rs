//! The driver end to end: JSON-RPC in, `fake-codex` as the app-server, updates out.

use std::path::PathBuf;
use std::time::Duration;

use parrot_driver_codex::driver::{Config, serve};
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
        Client::start_shadow(scenario, None)
    }

    fn start_shadow(scenario: &str, sessions: Option<PathBuf>) -> Client {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = if scenario.contains('/') {
            root.join(scenario)
        } else {
            root.join("fixtures/scenarios").join(format!("{scenario}.jsonl"))
        };
        let config = Config {
            codex_bin: env!("CARGO_BIN_EXE_fake-codex").into(),
            args: vec!["app-server".into()],
            env: vec![("PARROT_FAKE_CODEX_SCENARIO".into(), path.into())],
            guard: false,
            shadow: sessions.is_some(),
            sessions,
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

    async fn bind(&mut self, options: Value) -> Value {
        self.bind_with(json!({"options": options})).await
    }

    async fn bind_with(&mut self, extra: Value) -> Value {
        let init = self
            .call(
                "initialize",
                json!({"protocol": [1], "peer": {"kind": "runtime", "name": "test", "version": "0"}, "features": {}}),
            )
            .await;
        assert_eq!(init["peer"]["kind"], "driver");
        let agents = self.call("agent/list", json!({})).await;
        assert_eq!(agents["agents"][0]["id"], "codex");
        let cwd = std::env::temp_dir();
        let mut params = json!({"session": "s1", "binding": "b1", "agent": "codex", "workspace": cwd, "cwd": cwd,
            "options": {}, "control": "native", "deny": []});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        self.call("binding/start", params).await
    }

    async fn prompt(&mut self, message: &str, text: &str) -> Value {
        self.call(
            "binding/prompt",
            json!({"binding": "b1", "message": message, "text": text, "steer": true}),
        )
        .await
    }
}

fn is_state(state: &'static str) -> impl Fn(&Value) -> bool {
    move |u| u["update"]["type"] == "state" && u["update"]["state"] == state
}

fn turn_ended(u: &Value) -> bool {
    u["update"]["type"] == "turn" && u["update"]["turn"]["status"] != "running"
}

fn option<'a>(start: &'a Value, id: &str) -> &'a Value {
    start["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == id)
        .unwrap_or_else(|| panic!("no option {id} in {start}"))
}

#[tokio::test]
async fn start_lists_models_into_options() {
    let mut c = Client::start("idle");
    let start = c.bind(json!({})).await;
    assert_eq!(start["ref"]["id"], "thr-fake-0001");
    assert_eq!(start["level"], "sandboxed");
    assert_eq!(start["network"], false);
    assert_eq!(start["isolation"], "sandbox");
    let model = option(&start, "model");
    assert_eq!(model["role"], "model");
    assert_eq!(model["applies"], "next_turn");
    let ids: Vec<&Value> = model["schema"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["const"])
        .collect();
    assert_eq!(ids, [&json!("gpt-fake-large"), &json!("gpt-fake-mini")]);
    assert_eq!(model["value"], "gpt-fake-large");
    let effort = option(&start, "effort");
    assert_eq!(effort["role"], "effort");
    assert_eq!(effort["dependsOn"]["option"], "model");
    assert_eq!(
        effort["dependsOn"]["schemas"]["gpt-fake-mini"]["oneOf"][0]["const"],
        "low"
    );
    assert_eq!(effort["value"], "medium");
    assert_eq!(option(&start, "approvalPolicy")["role"], "mode");
    assert_eq!(option(&start, "sandbox")["role"], "sandbox");
    let actions: Vec<&Value> = start["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["id"])
        .collect();
    assert_eq!(actions, [&json!("compact"), &json!("review")]);
    c.wait(is_state("idle")).await;

    let r = c
        .call(
            "binding/options",
            json!({"binding": "b1", "values": {"sandbox": "danger-full-access", "approvalPolicy": "never"}}),
        )
        .await;
    assert_eq!(r["level"], "host");
    assert_eq!(r["network"], true);
    let bad = c
        .try_call(
            "binding/options",
            json!({"binding": "b1", "values": {"sandbox": "everything"}}),
        )
        .await;
    assert_eq!(bad["error"]["code"], -32602);
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn a_turn_streams_text_and_a_command() {
    let mut c = Client::start("text-command");
    c.bind(json!({})).await;
    let r = c.prompt("m1", "List the files").await;
    assert_eq!(r["disposition"], "started");
    c.wait(is_state("running")).await;
    let turn = c.wait(|u| u["update"]["type"] == "turn").await;
    assert_eq!(
        turn["update"]["turn"]["id"],
        "01a1de18-b809-7723-900b-c585bea1163d"
    );
    assert_eq!(turn["update"]["turn"]["input"], "m1");
    let user = c.wait(|u| u["update"]["item"]["id"] == "m1").await;
    assert_eq!(user["update"]["item"]["set"]["message"], "m1");
    assert_eq!(user["update"]["item"]["kind"], "user");
    let echo = c
        .wait(|u| u["update"]["item"]["id"] == "m1" && u["update"]["item"]["ref"].is_object())
        .await;
    assert_eq!(echo["update"]["item"]["ref"]["id"], "u1");
    let a = c
        .wait(|u| u["update"]["item"]["append"]["text"].is_string())
        .await;
    assert_eq!(a["update"]["item"]["id"], "msg_1");
    assert_eq!(a["update"]["item"]["append"]["text"], "I’ll");
    let cmd = c
        .wait(|u| u["update"]["item"]["id"] == "exec-1" && u["update"]["item"]["set"]["status"] == "ok")
        .await;
    assert_eq!(cmd["update"]["item"]["kind"], "command");
    assert_eq!(cmd["update"]["item"]["set"]["command"], "ls");
    assert_eq!(cmd["update"]["item"]["set"]["output"], "README.md\nsrc\n");
    assert_eq!(cmd["update"]["item"]["set"]["exit"], 0);
    let usage = c.wait(|u| u["update"]["type"] == "usage").await;
    assert_eq!(usage["update"]["usage"]["tokens"]["input"], 2460);
    assert_eq!(usage["update"]["usage"]["context"]["window"], 258400);
    let limits = c.wait(|u| u["update"]["usage"]["rateLimits"].is_array()).await;
    assert_eq!(limits["update"]["usage"]["rateLimits"][0]["used"], 12.0);
    let t = c.wait(turn_ended).await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    assert_eq!(t["update"]["turn"]["usage"]["tokens"]["output"], 223);
    c.wait(is_state("idle")).await;
    // Neither Codex's echo of the prompt nor the empty reasoning item became items.
    assert!(!c.updates.iter().any(|u| u["update"]["item"]["id"] == "u1"));
    assert!(!c.updates.iter().any(|u| u["update"]["item"]["id"] == "rs_1"));
    c.call("binding/stop", json!({"binding": "b1"})).await;
}

#[tokio::test]
async fn an_approval_is_answered() {
    let mut c = Client::start("command-approval");
    c.bind(json!({})).await;
    c.prompt("m1", "Write live.txt").await;
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    let request = &req["update"]["request"];
    assert_eq!(request["id"], "req-0");
    assert_eq!(request["kind"], "approval");
    assert_eq!(request["item"], "exec-5ff3");
    assert_eq!(request["subject"]["type"], "command");
    assert_eq!(request["subject"]["command"], "printf 'live' > live.txt");
    let choices: Vec<&Value> = request["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["id"])
        .collect();
    assert_eq!(
        choices,
        [&json!("accept"), &json!("execpolicy"), &json!("cancel")]
    );
    assert_eq!(request["choices"][1]["scope"], "always");
    assert_eq!(
        request["choices"][1]["amends"]["execpolicy_amendment"],
        json!(["printf"])
    );
    c.wait(is_state("requires_action")).await;
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "req-0", "answer": {"choice": "accept"}}),
    )
    .await;
    let answered = c
        .wait(|u| u["update"]["type"] == "request" && u["update"]["request"]["status"] == "answered")
        .await;
    assert_eq!(answered["update"]["request"]["answer"]["choice"], "accept");
    c.wait(|u| u["update"]["item"]["id"] == "exec-5ff3" && u["update"]["item"]["set"]["status"] == "ok")
        .await;
    let t = c.wait(turn_ended).await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn cancel_interrupts_the_turn() {
    let mut c = Client::start("interrupt");
    c.bind(json!({})).await;
    c.prompt("m1", "Count").await;
    c.wait(|u| u["update"]["item"]["append"]["text"] == "One, two, ")
        .await;
    let r = c.call("binding/cancel", json!({"binding": "b1"})).await;
    assert_eq!(r["interrupted"], true);
    let closed = c
        .wait(|u| u["update"]["item"]["id"] == "msg_c" && u["update"]["item"]["streaming"] == false)
        .await;
    assert!(closed["update"]["item"]["set"].is_null());
    let t = c.wait(turn_ended).await;
    assert_eq!(t["update"]["turn"]["status"], "interrupted");
    assert_eq!(t["update"]["turn"]["stop"], "cancelled");
    assert_eq!(t["update"]["turn"]["continuable"], true);
    c.wait(is_state("idle")).await;
}

#[tokio::test]
async fn a_prompt_steers_and_review_is_an_action() {
    let mut c = Client::start("steer-and-review");
    c.bind(json!({})).await;
    assert_eq!(c.prompt("m1", "List the files").await["disposition"], "started");
    assert_eq!(c.prompt("m2", "Also count them").await["disposition"], "steered");
    let user = c.wait(|u| u["update"]["item"]["id"] == "m2").await;
    assert_eq!(user["update"]["item"]["turn"], "turn-s-1");
    c.wait(turn_ended).await;
    c.wait(is_state("idle")).await;
    let r = c
        .call(
            "binding/action",
            json!({"binding": "b1", "action": "review", "input": {"target": "baseBranch", "branch": "main"}}),
        )
        .await;
    assert_eq!(r["status"], "started");
    let review = c.wait(|u| u["update"]["item"]["id"] == "rv-2").await;
    assert_eq!(review["update"]["item"]["kind"], "review");
    assert_eq!(review["update"]["item"]["set"]["findings"], "No findings.");
    let t = c.wait(turn_ended).await;
    assert_eq!(t["update"]["turn"]["id"], "turn-r-1");
    assert!(!c.updates.iter().any(|u| u["update"]["item"]["id"] == "u2"));
}

#[tokio::test]
async fn start_returns_on_a_start_time_request() {
    let mut c = Client::start("start-request");
    let start = c
        .bind_with(json!({"mcp": {"url": "http://127.0.0.1:9/mcp", "token": "tok-made-up"}}))
        .await;
    assert!(start["ref"].is_null());
    let req = c.wait(|u| u["update"]["type"] == "request").await;
    assert_eq!(req["update"]["request"]["kind"], "input");
    assert_eq!(req["update"]["request"]["title"], "Sign in to the notes server");
    c.call(
        "binding/answer",
        json!({"binding": "b1", "request": "req-7", "answer": {"choice": "accept"}}),
    )
    .await;
    let r = c.wait(|u| u["update"]["type"] == "ref").await;
    assert_eq!(r["update"]["ref"]["id"], "thr-late-0002");
    c.wait(is_state("idle")).await;
    // The token goes to Codex in its environment, so no update carries it.
    assert!(!c.updates.iter().any(|u| u.to_string().contains("tok-made-up")));
}

#[tokio::test]
async fn attachments_and_context_reach_codex() {
    let mut c = Client::start("attachments");
    c.bind(json!({})).await;
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(
        agents["agents"][0]["features"]["attachments"]["kinds"],
        json!(["image", "audio", "file"])
    );
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m1", "text": "List the files", "steer": true,
            "context": [{"uri": "file:///work/notes.md", "text": "Notes"}],
            "attachments": [
                {"name": "dot.png", "type": "image/png", "data": "iVBORw0KGgo="},
                {"name": "shot.png", "type": "image/png", "uri": "file:///work/shot.png"},
                {"name": "far.pdf", "type": "application/pdf", "uri": "https://example.test/far.pdf"}
            ]}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    let n = c.wait(|u| u["update"]["type"] == "notice").await;
    assert!(n["update"]["text"].as_str().unwrap().contains("far.pdf"), "{n}");
    let t = c.wait(turn_ended).await;
    assert_eq!(t["update"]["turn"]["status"], "completed");
    let user = c
        .updates
        .iter()
        .find(|u| u["update"]["item"]["id"] == "m1")
        .unwrap();
    assert_eq!(
        user["update"]["item"]["set"]["attachments"][1]["uri"],
        "file:///work/shot.png"
    );
}

#[tokio::test]
async fn frames_rebuild_the_same_items() {
    for scenario in ["text-command", "command-approval"] {
        let mut c = Client::start(scenario);
        c.bind(json!({})).await;
        let text = if scenario == "text-command" {
            "List the files"
        } else {
            "Write live.txt"
        };
        c.prompt("m1", text).await;
        if scenario == "command-approval" {
            c.wait(|u| u["update"]["type"] == "request").await;
            c.call(
                "binding/answer",
                json!({"binding": "b1", "request": "req-0", "answer": {"choice": "accept"}}),
            )
            .await;
        }
        c.wait(turn_ended).await;
        c.wait(is_state("idle")).await;
        let frames: Vec<Value> = c
            .updates
            .iter()
            .filter(|u| u["update"]["type"] == "frame")
            .map(|u| u["update"]["data"].clone())
            .collect();
        assert!(frames.iter().all(|f| f["v"] == "1"));
        let rebuilt: Vec<Value> = parrot_driver_codex::frame::rebuild("b1", &frames)
            .iter()
            .map(|u| serde_json::to_value(u).unwrap())
            .collect();
        let live: Vec<Value> = c
            .updates
            .iter()
            .filter(|u| ["item", "turn", "request"].contains(&u["update"]["type"].as_str().unwrap_or("")))
            .cloned()
            .collect();
        assert_eq!(rebuilt, live, "{scenario}");
    }
}

#[tokio::test]
async fn recorded_trace_plays_end_to_end() {
    let mut c = Client::start("../parrot-conform/fixtures/traces/codex-0.167.1.jsonl");
    let start = c.bind(json!({})).await;
    let agents = c.call("agent/list", json!({})).await;
    assert_eq!(agents["agents"][0]["harness"]["verified"], true, "{agents}");
    assert!(start["ref"]["id"].is_string());
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m1", "text": "Name the color", "steer": false,
                "attachments": [{"name": "red.png", "type": "image/png", "data": "iVBORw0KGgo="}]}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    for _ in 0..2 {
        let req = c
            .wait(|u| u["update"]["type"] == "request" && u["update"]["request"]["status"] == "open")
            .await;
        let id = req["update"]["request"]["id"].as_str().unwrap().to_owned();
        c.call(
            "binding/answer",
            json!({"binding": "b1", "request": id, "answer": {"choice": "accept"}}),
        )
        .await;
    }
    let done = c.wait(turn_ended).await;
    assert_eq!(done["update"]["turn"]["status"], "completed");
    let usage = c
        .updates
        .iter()
        .filter(|u| u["update"]["type"] == "usage")
        .count();
    assert!(usage > 0, "token usage arrives");
    let r = c
        .call(
            "binding/prompt",
            json!({"binding": "b1", "message": "m2", "text": "Count", "steer": false}),
        )
        .await;
    assert_eq!(r["disposition"], "started");
    c.wait(|u| u["update"]["item"]["append"].is_object()).await;
    c.call("binding/cancel", json!({"binding": "b1"})).await;
    let done = c.wait(turn_ended).await;
    assert_eq!(done["update"]["turn"]["status"], "interrupted");
    c.wait(is_state("idle")).await;
    let users: Vec<_> = c
        .updates
        .iter()
        .filter(|u| u["update"]["item"]["kind"] == "user")
        .map(|u| u["update"]["item"]["id"].clone())
        .collect();
    assert_eq!(
        users,
        [json!("m1"), json!("m2")],
        "Codex's echoes fold into the prompts"
    );
    let commands: Vec<_> = c
        .updates
        .iter()
        .filter(|u| u["update"]["item"]["set"]["exit"] == 0)
        .collect();
    assert_eq!(commands.len(), 2, "both approved commands ran");
    assert!(
        c.updates
            .iter()
            .any(|u| u["update"]["usage"]["rateLimits"].is_array()),
        "rate limits arrive"
    );
}

#[tokio::test]
async fn shadow_mode_reports_where_the_rollout_differs() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join("2026/11/18");
    std::fs::create_dir_all(&day).unwrap();
    let name = "rollout-2026-11-18T08-23-03-01a1edab-5a86-75f0-b42a-57127da3693a.jsonl";
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rollouts")
            .join(name),
    )
    .unwrap();
    // A deliberate difference: the rollout says `ls -la` exited 2.
    let changed: Vec<String> = text
        .lines()
        .map(|l| {
            if l.contains("ls -la") && l.contains("CommandExecution") {
                l.replace(r#""exit_code":0"#, r#""exit_code":2"#)
            } else {
                l.to_owned()
            }
        })
        .collect();
    std::fs::write(day.join(name), changed.join("\n")).unwrap();
    let mut c = Client::start_shadow(
        "../parrot-conform/fixtures/traces/codex-0.167.1.jsonl",
        Some(root.path().to_owned()),
    );
    c.bind(json!({})).await;
    c.call(
        "binding/prompt",
        json!({"binding": "b1", "message": "m1", "text": "Name the color of this image in one word. Then run the shell command `ls -la` and report how many entries it printed.", "steer": false}),
    )
    .await;
    for _ in 0..2 {
        let req = c
            .wait(|u| u["update"]["type"] == "request" && u["update"]["request"]["status"] == "open")
            .await;
        let id = req["update"]["request"]["id"].as_str().unwrap().to_owned();
        c.call(
            "binding/answer",
            json!({"binding": "b1", "request": id, "answer": {"choice": "accept"}}),
        )
        .await;
    }
    c.wait(turn_ended).await;
    let n = c
        .wait(|u| u["update"]["type"] == "notice" && u["update"]["code"] == "x.parrot/shadow")
        .await;
    let text = n["update"]["text"].as_str().unwrap();
    assert!(
        text.contains("exit is 0 in the native and 2 in the transcript"),
        "{text}"
    );
    let more: Vec<_> = c
        .updates
        .iter()
        .filter(|u| u["update"]["code"] == "x.parrot/shadow")
        .map(|u| u["update"]["text"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        more.iter().all(|t| t.contains("exec-b6b0d1dd")),
        "only the changed command differs: {more:?}"
    );
}

#[tokio::test]
async fn history_reads_rollouts_and_fork_asks_codex() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join("2026/11/18");
    std::fs::create_dir_all(&day).unwrap();
    let name = "rollout-2026-11-18T08-23-03-01a1edab-5a86-75f0-b42a-57127da3693a.jsonl";
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rollouts")
            .join(name),
        day.join(name),
    )
    .unwrap();
    let mut c = Client::start_shadow("fork", Some(root.path().to_owned()));
    let agents = c.call("agent/list", json!({})).await;
    assert!(agents["agents"][0]["features"]["session.history"].is_object());
    let found = c.call("history/discover", json!({"agent": "codex"})).await;
    assert_eq!(
        found["found"][0]["ref"]["id"],
        "01a1edab-5a86-75f0-b42a-57127da3693a"
    );
    let read = c
        .call(
            "history/read",
            json!({"agent": "codex", "ref": found["found"][0]["ref"]}),
        )
        .await;
    assert!(
        read["updates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["update"]["item"]["kind"] == "command")
    );
    c.bind(json!({})).await;
    let fork = c.call("binding/fork", json!({"binding": "b1"})).await;
    assert_eq!(fork["ref"]["id"], "thr-fake-fork");
}
