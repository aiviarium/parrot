//! `parrot driver tui` over stdio (spec 30.5), for a data-only egg on the TUI core: the egg
//! holds the profile and the harness command, and the driver answers JSON-RPC lines on its
//! standard input and output.

mod common;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

const WAIT: Duration = Duration::from_secs(20);

fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parrot-driver-tui/tests/fake")
        .join(name)
}

/// A driver on stdio, and the lines it has sent that were not answers.
struct Driver {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    seen: Vec<Value>,
    next_id: u64,
}

impl Driver {
    fn start(data: &Path, env: &[(String, String)]) -> Self {
        let mut cmd = tokio::process::Command::new(common::workspace_bin("parrot"));
        cmd.args(["driver", "tui"])
            .env("PARROT_DRIVER_DATA", data)
            .env("PARROT_DRIVER_AGENT", "stdio-tui")
            .env("PARROT_DRIVER_AGENT_NAME", "Stdio TUI")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("the driver starts");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            seen: Vec::new(),
            next_id: 1,
        }
    }

    async fn send(&mut self, msg: Value) {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    /// Sends a request and returns its result, keeping every update seen meanwhile.
    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        loop {
            let msg = self.next_line(method).await;
            if msg.get("method").is_some() {
                self.seen.push(msg);
                continue;
            }
            if msg["id"] != id {
                continue;
            }
            match msg.get("result") {
                Some(result) => return result.clone(),
                None => panic!("{method} failed: {msg}"),
            }
        }
    }

    async fn next_line(&mut self, what: &str) -> Value {
        let line = tokio::time::timeout(WAIT, self.lines.next_line())
            .await
            .unwrap_or_else(|_| panic!("no {what} within {WAIT:?}"))
            .unwrap_or_else(|e| panic!("reading {what}: {e}"))
            .unwrap_or_else(|| panic!("the driver closed its output waiting for {what}"));
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{what}: {e}: {line}"))
    }

    /// The first update matching `pred`, from what was seen and then from the driver.
    async fn update(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let is_update = |msg: &Value| msg["method"] == "update" && pred(&msg["params"]["update"]);
        if let Some(u) = self.seen.iter().find(|u| is_update(u)) {
            return u.clone();
        }
        loop {
            let msg = self.next_line(what).await;
            let hit = is_update(&msg);
            self.seen.push(msg.clone());
            if hit {
                return msg;
            }
        }
    }
}

#[tokio::test]
async fn a_data_only_egg_drives_the_tui_core_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let transcripts = dir.path().join("transcripts");
    std::fs::create_dir_all(&transcripts).unwrap();
    let fake_tui = common::workspace_bin("fake-tui");
    // KDL reads a backslash as an escape, so a Windows path goes in with forward slashes.
    let command = fake_tui.to_string_lossy().replace('\\', "/");
    let transcript = transcripts.to_string_lossy().replace('\\', "/");
    std::fs::copy(fake("fake-tui.kdl"), dir.path().join("profile.kdl")).unwrap();
    std::fs::write(
        dir.path().join("driver.kdl"),
        format!(
            "driver \"builtin\" name=\"tui\"\ncommand \"{command}\"\ntranscript \"{transcript}\"\ndata-version \"1\"\n"
        ),
    )
    .unwrap();

    let env = vec![(
        "FAKE_TUI_TRANSCRIPT".to_string(),
        transcripts.to_string_lossy().into_owned(),
    )];
    let mut driver = Driver::start(dir.path(), &env);

    let init = driver
        .ok(
            "initialize",
            json!({
                "protocol": [1],
                "peer": { "kind": "client", "name": "stdio-test", "version": "0" },
                "features": {},
            }),
        )
        .await;
    assert_eq!(init["protocol"], 1, "{init}");
    assert_eq!(init["peer"]["kind"], "driver", "{init}");

    let agents = driver.ok("agent/list", json!({})).await;
    let listed = agents["agents"].as_array().expect("agents");
    assert_eq!(listed.len(), 1, "{agents}");
    assert_eq!(listed[0]["id"], "stdio-tui");
    assert_eq!(listed[0]["name"], "Stdio TUI");
    assert_eq!(listed[0]["controls"], json!(["tui"]));

    let started = driver
        .ok(
            "binding/start",
            json!({
                "session": "ses_stdio",
                "binding": "b1",
                "agent": "stdio-tui",
                "workspace": transcripts.to_string_lossy(),
                "cwd": transcripts.to_string_lossy(),
                "options": {},
                "control": "tui",
                "deny": [],
            }),
        )
        .await;
    assert_eq!(started["level"], "host", "{started}");
    assert_eq!(started["isolation"], "none", "{started}");
    assert!(started["actions"].is_array(), "{started}");
    driver
        .update("the idle screen", |u| {
            u["type"] == "state" && u["state"] == "idle"
        })
        .await;

    let prompt = driver
        .ok(
            "binding/prompt",
            json!({ "binding": "b1", "message": "msg_1", "text": "hello over stdio", "steer": false }),
        )
        .await;
    assert_eq!(prompt["disposition"], "started", "{prompt}");

    driver
        .update("the end of the turn", |u| {
            u["type"] == "turn" && u["turn"]["id"] == "turn-1" && u["turn"]["status"] == "completed"
        })
        .await;
    // The reply is read from the transcript the egg's driver.kdl points at.
    let reply = driver
        .update("the assistant item", |u| {
            u["type"] == "item" && u["item"]["kind"] == "assistant"
        })
        .await;
    assert_eq!(
        reply["params"]["update"]["item"]["set"]["text"],
        "Hi from the fake."
    );
    assert_eq!(reply["params"]["update"]["item"]["source"], "transcript");

    assert_eq!(
        driver.ok("binding/stop", json!({ "binding": "b1" })).await,
        json!({})
    );
    drop(driver.stdin);
    let status = tokio::time::timeout(WAIT, driver.child.wait())
        .await
        .expect("the driver exits when stdin closes")
        .unwrap();
    assert!(status.success(), "{status}");
}
