//! An out-of-process driver egg (`driver "command"`), installed and run end to end through
//! `parrot serve`, through `parrot probe`, and with a harness version its data doesn't cover.

mod common;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");
const WAIT: Duration = Duration::from_secs(15);
const GREETING: &str = "hello from the tiny egg";

/// The tiny driver example.
fn tiny_driver() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| common::workspace_example("tiny-driver"))
}

/// A package folder for the agent `tiny`: its driver binary is a Hatchery variant for this OS,
/// and `driver.kdl` picks it by OS.
fn tiny_package(root: &Path) -> PathBuf {
    let os = std::env::consts::OS;
    let exe = std::env::consts::EXE_SUFFIX;
    let dir = root.join("tiny");
    let variant = dir.join("parrot/variants").join(os);
    std::fs::create_dir_all(&variant).unwrap();
    std::fs::copy(tiny_driver(), variant.join(format!("tiny-driver{exe}"))).unwrap();
    std::fs::create_dir_all(dir.join("parrot/probes")).unwrap();
    let files = [
        (
            "package.kdl".to_string(),
            format!(
                "format 1\npackage {{\n  group example\n  id tiny\n  version \"1.0.0\"\n}}\nabout {{\n  name \"Tiny\"\n}}\ntargets {{\n  parrot {{\n    requires {{\n      parrot \"^0.1\"\n    }}\n    variants {{\n      variant os={os}\n    }}\n  }}\n}}\n"
            ),
        ),
        (
            "parrot/driver.kdl".into(),
            format!(
                "driver \"command\" {{\n  variant os=\"{os}\" run=\"variants/{os}/tiny-driver{exe}\"\n}}\ncli \"tiny\" versions=\"^1.0\"\ndata-version \"3\"\n"
            ),
        ),
        ("parrot/greeting.txt".into(), format!("{GREETING}\n")),
        (
            "parrot/probes/hello.kdl".into(),
            "prompt \"hi\"\nwait \"idle\" timeout-secs=10\n".into(),
        ),
    ];
    for (path, text) in files {
        std::fs::write(dir.join(path), text).unwrap();
    }
    dir
}

fn ok(out: Output) -> String {
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "parrot failed:\n{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

/// A temporary state directory with the tiny egg installed.
fn installed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // The working directory is the folder's parent and the spec is `./tiny`, because a Windows
    // drive letter is not read as a path.
    tiny_package(dir.path());
    let out = ok(Command::new(PARROT)
        .args(["eggs", "install", "./tiny"])
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .current_dir(dir.path())
        .output()
        .unwrap());
    assert!(out.contains("installed example/tiny 1.0.0"), "{out}");
    dir
}

/// `parrot serve` on the state directory, killed on drop.
struct Serve {
    child: Child,
    socket: PathBuf,
}

impl Serve {
    fn start(dir: &Path, cli_version: &str) -> Self {
        common::set_roots(&dir.join("state"), dir);
        let socket = common::test_socket(dir);
        let child = Command::new(PARROT)
            .args(["serve", "--state-dir"])
            .arg(dir.join("state"))
            .env("PARROT_SOCKET", &socket)
            .env("TINY_CLI_VERSION", cli_version)
            .stdin(Stdio::null())
            .spawn()
            .expect("serve starts");
        let deadline = Instant::now() + WAIT;
        while common::connect_once(&socket).is_err() {
            assert!(Instant::now() < deadline, "serve never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve { child, socket }
    }

    fn client(&self, args: &[&str]) -> Value {
        let out = ok(Command::new(PARROT)
            .args(args)
            .arg("--json")
            .env("PARROT_SOCKET", &self.socket)
            .output()
            .unwrap());
        serde_json::from_str(out.lines().next().unwrap_or("null")).unwrap()
    }

    /// The agent `tiny` once its driver has answered `agent/list`.
    fn tiny_agent(&self) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let agents = self.client(&["agents"]);
            let tiny = agents["agents"]
                .as_array()
                .and_then(|a| a.iter().find(|a| a["id"] == "tiny"))
                .cloned();
            if let Some(t) = tiny.filter(|t| t.get("harness").is_some()) {
                return t;
            }
            assert!(Instant::now() < deadline, "tiny was never discovered: {agents}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_command_egg_runs_through_serve() {
    let dir = installed();
    let serve = Serve::start(dir.path(), "1.4.0");
    let tiny = serve.tiny_agent();
    assert_eq!(tiny["driver"]["kind"], "process");
    assert_eq!(tiny["driver"]["data"], "3");
    assert_eq!(tiny["harness"]["verified"], true);
    assert_eq!(tiny["usable"], true);

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let new = serve.client(&["new", "tiny", "--dir", work.to_str().unwrap()]);
    let session = new["session"]["id"].as_str().expect("a session").to_string();
    assert!(new["session"].get("harness").is_none(), "{new}");

    let watch_out = dir.path().join("watch.jsonl");
    let mut watch = Command::new(PARROT)
        .args(["watch", &session, "--json"])
        .env("PARROT_SOCKET", &serve.socket)
        .stdout(std::fs::File::create(&watch_out).unwrap())
        .spawn()
        .unwrap();
    serve.client(&["prompt", &session, "hi"]);
    let deadline = Instant::now() + WAIT;
    // The answer is read from the egg's folder, so it arrives only if PARROT_DRIVER_DATA was set.
    while !std::fs::read_to_string(&watch_out)
        .unwrap_or_default()
        .contains(GREETING)
    {
        assert!(
            Instant::now() < deadline,
            "no answer from the tiny driver:\n{}",
            std::fs::read_to_string(&watch_out).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = watch.kill();
    let _ = watch.wait();
}

#[test]
fn an_uncovered_harness_version_marks_the_agent_and_its_sessions() {
    let dir = installed();
    let serve = Serve::start(dir.path(), "2.5.0");
    let tiny = serve.tiny_agent();
    assert_eq!(tiny["harness"]["version"], "2.5.0");
    assert_eq!(tiny["harness"]["verified"], false);
    assert_eq!(tiny["usable"], true);
    assert!(tiny["detail"].as_str().unwrap().contains("unverified"), "{tiny}");

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let new = serve.client(&["new", "tiny", "--dir", work.to_str().unwrap()]);
    assert_eq!(new["session"]["harness"]["verified"], false, "{new}");
}

#[test]
fn probe_records_a_scenario() {
    let dir = installed();
    let out = dir.path().join("probe");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    ok(Command::new(PARROT)
        .args(["probe", "tiny", "hello", "--out"])
        .arg(&out)
        .arg("--dir")
        .arg(&work)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .output()
        .unwrap());
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("probe.json")).unwrap()).unwrap();
    assert_eq!(summary["ok"], true, "{summary}");
    assert_eq!(summary["package"], "example/tiny");
    assert_eq!(summary["harness"]["verified"], true);
    assert_eq!(summary["frames"], 1);
    let transcript = std::fs::read_to_string(out.join("transcript.jsonl")).unwrap();
    assert!(transcript.contains(GREETING), "{transcript}");
    assert!(transcript.contains("\"binding/stop\""), "{transcript}");
    let frames = std::fs::read_to_string(out.join("frames.jsonl")).unwrap();
    assert!(frames.contains("\"prompt\":\"hi\""), "{frames}");

    // A scenario the egg doesn't have fails clearly.
    let missing = Command::new(PARROT)
        .args(["probe", "tiny", "nope", "--out"])
        .arg(&out)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("nope.kdl"));
}
