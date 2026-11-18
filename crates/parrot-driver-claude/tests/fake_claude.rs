//! Drives the fake-claude binary as a child process through each scenario.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use parrot_driver_claude::fake::{SCENARIO_ENV, matches, parse_scenario};
use parrot_driver_claude::wire::*;
use parrot_proto::open::Open;
use serde_json::json;

fn scenario(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/scenarios")
        .join(name)
}

struct Fake {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Fake {
    fn spawn(cmd: &mut Command) -> Fake {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Fake { child, stdin, lines }
    }

    /// Started the way a driver starts the real CLI, with the scenario added.
    fn start(name: &str) -> Fake {
        Fake::spawn(
            Command::new(env!("CARGO_BIN_EXE_fake-claude"))
                .args([
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--permission-prompt-tool",
                    "stdio",
                    "--scenario",
                ])
                .arg(scenario(name)),
        )
    }

    fn send(&mut self, msg: ClaudeInMsg) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", to_line(&Open::Known(msg))).unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> ClaudeOut {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("fake-claude wrote nothing");
        parse_line(&line).unwrap()
    }

    /// Reads up to and including the first message `stop` accepts.
    fn until(&self, stop: impl Fn(&ClaudeOutMsg) -> bool) -> Vec<ClaudeOutMsg> {
        let mut out = Vec::new();
        loop {
            let Open::Known(m) = self.recv() else {
                panic!("unknown message from the fake")
            };
            let done = stop(&m);
            out.push(m);
            if done {
                return out;
            }
        }
    }

    fn finish(mut self) -> i32 {
        drop(self.stdin.take());
        self.child.wait().unwrap().code().unwrap()
    }

    fn initialize(&mut self) {
        self.send(ClaudeInMsg::ControlRequest(ControlRequest::new(
            "init-1",
            ControlRequestBody::Initialize { extra: Extra::new() },
        )));
        let Open::Known(ClaudeOutMsg::ControlResponse(r)) = self.recv() else {
            panic!("no answer to initialize")
        };
        assert_eq!(r.request_id(), Some("init-1"));
        let Open::Known(ControlResponseBody::Success {
            response: Some(body), ..
        }) = &r.response
        else {
            panic!()
        };
        assert!(body["models"].is_array());
    }
}

fn is_result(m: &ClaudeOutMsg) -> bool {
    matches!(m, ClaudeOutMsg::Result(_))
}

fn streamed_text(msgs: &[ClaudeOutMsg]) -> String {
    msgs.iter()
        .filter_map(|m| match m {
            ClaudeOutMsg::StreamEvent(StreamEvent {
                event:
                    Open::Known(Event::ContentBlockDelta {
                        delta: Open::Known(Delta::TextDelta { text }),
                        ..
                    }),
                ..
            }) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn text_turn_streams_deltas_then_a_result() {
    let mut fake = Fake::start("text-turn.jsonl");
    fake.initialize();
    fake.send(ClaudeInMsg::User(User::prompt("Say hello")));
    let msgs = fake.until(is_result);
    assert!(matches!(
        &msgs[0],
        ClaudeOutMsg::System(Open::Known(System::Init(i))) if i.session_id.as_deref() == Some("sess-fake-0001")
    ));
    assert_eq!(streamed_text(&msgs), "Hello there.");
    let ClaudeOutMsg::Result(r) = msgs.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(r.subtype, Open::Known(ResultSubtype::Success));
    assert_eq!(r.usage.as_ref().unwrap().output_tokens, Some(4));
    assert_eq!(fake.finish(), 0);
}

fn bash_turn(name: &str, answer: PermissionResult) -> (Vec<ClaudeOutMsg>, i32) {
    let mut fake = Fake::start(name);
    fake.initialize();
    fake.send(ClaudeInMsg::User(User::prompt("List the files")));
    let msgs = fake.until(|m| matches!(m, ClaudeOutMsg::ControlRequest(_)));
    let Some(ClaudeOutMsg::ControlRequest(req)) = msgs.last() else {
        unreachable!()
    };
    let Open::Known(ControlRequestBody::CanUseTool(ask)) = &req.request else {
        panic!("not can_use_tool")
    };
    assert_eq!(ask.tool_name, "Bash");
    assert_eq!(ask.input["command"], "ls");
    fake.send(ClaudeInMsg::ControlResponse(ControlResponse::success(
        req.request_id.clone(),
        answer.to_value(),
    )));
    let rest = fake.until(is_result);
    (rest, fake.finish())
}

fn tool_result(msgs: &[ClaudeOutMsg]) -> ToolResultBlock {
    msgs.iter()
        .find_map(|m| match m {
            ClaudeOutMsg::User(u) => match &u.message.content {
                Content::Blocks(b) => b.iter().find_map(|b| match b {
                    Open::Known(ContentBlock::ToolResult(t)) => Some(t.clone()),
                    _ => None,
                }),
                _ => None,
            },
            _ => None,
        })
        .expect("no tool result")
}

#[test]
fn bash_allowed() {
    let (msgs, code) = bash_turn(
        "bash-allowed.jsonl",
        PermissionResult::Allow {
            updated_input: Some(json!({"command":"ls","description":"List files"})),
            updated_permissions: None,
            extra: Extra::new(),
        },
    );
    assert_eq!(tool_result(&msgs).is_error, Some(false));
    assert_eq!(streamed_text(&msgs), "There are two entries.");
    assert_eq!(code, 0);
}

#[test]
fn bash_denied() {
    let (msgs, code) = bash_turn(
        "bash-denied.jsonl",
        PermissionResult::Deny {
            message: "Not now".into(),
            interrupt: None,
            extra: Extra::new(),
        },
    );
    assert_eq!(tool_result(&msgs).is_error, Some(true));
    let ClaudeOutMsg::Result(r) = msgs.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(r.permission_denials.as_ref().unwrap().len(), 1);
    assert_eq!(code, 0);
}

#[test]
fn the_wrong_answer_fails_the_scenario() {
    let mut fake = Fake::start("bash-allowed.jsonl");
    fake.send(ClaudeInMsg::User(User::prompt("List the files")));
    fake.until(|m| matches!(m, ClaudeOutMsg::ControlRequest(_)));
    fake.send(ClaudeInMsg::ControlResponse(ControlResponse::success(
        "perm-1",
        json!({"behavior":"deny","message":"no"}),
    )));
    assert_eq!(fake.finish(), 2);
}

#[test]
fn interrupt_mid_turn() {
    let mut fake = Fake::start("interrupt.jsonl");
    fake.initialize();
    fake.send(ClaudeInMsg::User(User::prompt("Count to ten slowly")));
    let msgs = fake.until(|m| {
        matches!(
            m,
            ClaudeOutMsg::StreamEvent(StreamEvent {
                event: Open::Known(Event::ContentBlockDelta {
                    delta: Open::Known(Delta::TextDelta { .. }),
                    ..
                }),
                ..
            })
        )
    });
    assert!(msgs.iter().any(|m| matches!(
        m,
        ClaudeOutMsg::StreamEvent(StreamEvent {
            event: Open::Known(Event::ContentBlockDelta {
                delta: Open::Known(Delta::ThinkingDelta { .. }),
                ..
            }),
            ..
        })
    )));
    fake.send(ClaudeInMsg::ControlRequest(ControlRequest::new(
        "int-7",
        ControlRequestBody::Interrupt { extra: Extra::new() },
    )));
    let Open::Known(ClaudeOutMsg::ControlResponse(r)) = fake.recv() else {
        panic!("no answer to interrupt")
    };
    assert_eq!(r.request_id(), Some("int-7"));
    let msgs = fake.until(is_result);
    let ClaudeOutMsg::Result(r) = msgs.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(r.subtype, Open::Known(ResultSubtype::ErrorDuringExecution));
    assert_eq!(r.terminal_reason.as_deref(), Some("interrupted"));
    assert_eq!(fake.finish(), 0);
}

#[test]
fn error_results_then_exit() {
    // Through the variable this time, without initialize: the real CLI also runs without one.
    let mut fake = Fake::spawn(
        Command::new(env!("CARGO_BIN_EXE_fake-claude"))
            .args(["-p", "--output-format", "stream-json", "--model", "x"])
            .env(SCENARIO_ENV, scenario("error-result.jsonl")),
    );
    fake.send(ClaudeInMsg::User(User::prompt("Hello?")));
    let msgs = fake.until(is_result);
    assert!(
        msgs.iter()
            .any(|m| matches!(m, ClaudeOutMsg::System(Open::Unknown(_))))
    );
    let ClaudeOutMsg::Result(r) = msgs.last().unwrap() else {
        unreachable!()
    };
    assert!(r.is_error);
    assert_eq!(r.result.as_deref(), Some("API Error: 529 Overloaded"));
    fake.send(ClaudeInMsg::User(User::prompt("Again")));
    let msgs = fake.until(is_result);
    let ClaudeOutMsg::Result(r) = msgs.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(r.subtype, Open::Known(ResultSubtype::ErrorMaxTurns));
    assert_eq!(fake.child.wait().unwrap().code(), Some(1));
}

#[test]
fn missing_scenario_is_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_fake-claude"))
        .arg("-p")
        .env_remove(SCENARIO_ENV)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn patterns_match_partially() {
    assert!(matches(
        &json!({"type":"user"}),
        &json!({"type":"user","message":{}})
    ));
    assert!(!matches(&json!({"type":"user"}), &json!({"type":"keep_alive"})));
    assert!(matches(&json!([{"a":1}]), &json!([{"a":1,"b":2}])));
    assert!(!matches(&json!([1]), &json!([1, 2])));
    let steps = parse_scenario("// c\n{\"exit\":3}\n").unwrap();
    assert_eq!(steps.len(), 1);
    assert!(parse_scenario("{\"dance\":1}").is_err());
}
