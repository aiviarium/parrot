//! The client commands against a real `parrot serve` over `fake-claude`: new, prompt, watch until idle.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");

fn fake_claude() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--locked",
                "-p",
                "parrot-driver-claude",
                "--bin",
                "fake-claude",
            ])
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building fake-claude failed");
        Path::new(PARROT).with_file_name(format!("fake-claude{}", std::env::consts::EXE_SUFFIX))
    })
}

struct Serve {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Serve {
    fn start(scenario: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        let scenario = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../parrot-driver-claude/fixtures/scenarios")
            .join(scenario);
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(common::with_eggs(dir.path().join("state")))
            .env("PARROT_SOCKET", &socket)
            .env("PARROT_CLAUDE_BIN", fake_claude())
            .env("PARROT_FAKE_CLAUDE_SCENARIO", scenario)
            .stdin(Stdio::null())
            .spawn()
            .expect("serve starts");
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "serve never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve { child, socket, dir }
    }

    /// Runs a client command and returns its standard output, failing on a non-zero exit.
    fn parrot(&self, args: &[&str]) -> String {
        let out: Output = Command::new(PARROT)
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .output()
            .expect("parrot runs");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "parrot {args:?} failed\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn new_prompt_and_watch_until_idle() {
    let serve = Serve::start("text-turn.jsonl");
    let work = serve.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();

    assert!(serve.parrot(&["agents"]).contains("claude"));
    let session = serve.parrot(&["new", "claude", "--dir", work.to_str().unwrap()]);
    let session = session.trim();
    assert!(session.starts_with("ses_"), "{session}");
    assert!(serve.parrot(&["sessions"]).contains(session));

    let prompted = serve.parrot(&["prompt", session, "Say hello."]);
    // A prompt that arrives before the binding has started is queued and sent once it has.
    assert!(
        prompted.starts_with("started ") || prompted.starts_with("queued "),
        "{prompted}"
    );

    let watched = serve.parrot(&["watch", session, "--until", "idle"]);
    assert!(
        watched.contains("assistant: Hello there."),
        "{prompted}\n{watched}"
    );

    let raw = serve.parrot(&["sessions", "--json"]);
    let list: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(list["sessions"][0]["state"], "idle", "{raw}");

    assert_eq!(serve.parrot(&["stop", session]).trim(), "stopped");
}

#[test]
fn requests_are_listed_and_answered() {
    let serve = Serve::start("bash-allowed.jsonl");
    let work = serve.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let session = serve.parrot(&["new", "claude", "--dir", work.to_str().unwrap()]);
    let session = session.trim();
    serve.parrot(&["prompt", session, "Run ls."]);

    let deadline = Instant::now() + Duration::from_secs(10);
    let request = loop {
        let raw = serve.parrot(&["requests", session, "--json"]);
        let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        if let Some(id) = v["requests"][0]["id"].as_str() {
            break id.to_string();
        }
        assert!(Instant::now() < deadline, "no request appeared: {raw}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(serve.parrot(&["requests"]).contains(&request));
    assert_eq!(serve.parrot(&["answer", &request, "allow"]).trim(), "answered");
    assert_eq!(serve.parrot(&["requests", session]).trim(), "no open requests");
}
