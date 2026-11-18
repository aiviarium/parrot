//! `parrot conform` against the real Claude driver and a real `parrot serve`, both over `fake-claude`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");

/// Builds `fake-claude` once and returns its path, beside the `parrot` binary.
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
        let path = Path::new(PARROT).with_file_name(format!("fake-claude{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "no fake-claude at {}", path.display());
        path
    })
}

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../parrot-driver-claude/fixtures/scenarios")
}

fn conform(args: &[&str]) -> Output {
    Command::new(PARROT)
        .arg("conform")
        .arg("--scenarios")
        .arg(scenarios())
        .args(["--scenario-var", "PARROT_FAKE_CLAUDE_SCENARIO", "--verbose"])
        .args(args)
        .env("PARROT_CLAUDE_BIN", fake_claude())
        .output()
        .expect("parrot conform runs")
}

fn assert_passes(out: &Output, fixtures: usize) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "conform failed\n{stdout}\n{stderr}");
    let passes = stdout.lines().filter(|l| l.starts_with("pass ")).count();
    assert_eq!(passes, fixtures, "{stdout}");
    assert!(stdout.contains(" 0 failed"), "{stdout}");
}

#[test]
fn driver_fixtures_pass_against_the_claude_driver() {
    let out = conform(&[PARROT, "driver", "claude"]);
    assert_passes(&out, parrot_fixture_count("driver"));
}

#[test]
fn runtime_fixtures_pass_against_parrot_serve() {
    let out = conform(&["--runtime"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let pending = stdout.lines().filter(|l| l.starts_with("skip ")).count();
    assert_passes(&out, parrot_fixture_count("runtime") - pending);
}

#[test]
fn a_failing_fixture_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = r#"{"dir": "in", "msg": {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocol": [1], "peer": {"kind": "runtime", "name": "t", "version": "0"}, "features": {}}}}
{"dir": "out", "msg": {"jsonrpc": "2.0", "id": 1, "result": {"protocol": 99}}}
"#;
    std::fs::write(dir.path().join("wrong.jsonl"), fixture).unwrap();
    let out = conform(&[
        "--fixtures",
        dir.path().to_str().unwrap(),
        PARROT,
        "driver",
        "claude",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("FAIL  driver/wrong: line 2: $.result.protocol: expected 99, got 1"),
        "{stdout}"
    );
}

/// The number of built-in fixtures of a suite, from the fixture directory.
fn parrot_fixture_count(suite: &str) -> usize {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parrot-conform/fixtures")
        .join(suite);
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "jsonl")
        })
        .count()
}
