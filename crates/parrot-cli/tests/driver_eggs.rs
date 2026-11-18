//! Drivers come only from installed eggs: no egg means no agents, a third-party driver built
//! outside the workspace installs from a registry folder and runs, a data-only egg on the
//! built-in ACP core is one agent under its own id, and the Claude egg's version shim applies.
#![cfg(unix)]
// Waits for the runtime on a Unix socket.

mod common;

use common::{PARROT, WAIT};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn ok(out: Output) -> String {
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "parrot failed:\n{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

/// `parrot serve` on `<dir>/state`, killed on drop.
struct Serve {
    child: Child,
    socket: PathBuf,
}

impl Serve {
    fn start(dir: &Path, env: &[(&str, &Path)]) -> Self {
        let socket = dir.join("sock");
        let mut cmd = Command::new(PARROT);
        cmd.args(["serve", "--state-dir"])
            .arg(dir.join("state"))
            .env("PARROT_SOCKET", &socket)
            .stdin(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("serve starts");
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
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

    /// The agents once `pred` holds for them.
    fn agents_when(&self, what: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let v = self.client(&["agents"]);
            let agents = v["agents"].as_array().cloned().unwrap_or_default();
            if pred(&agents) {
                return agents;
            }
            assert!(Instant::now() < deadline, "{what}: {v}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Starts a session of `agent`, prompts it, and waits for `expect` in its updates.
    fn prompt_until(&self, dir: &Path, agent: &str, text: &str, expect: &str) {
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let new = self.client(&["new", agent, "--dir", work.to_str().unwrap()]);
        let session = new["session"]["id"].as_str().expect("a session").to_string();
        let log = dir.join(format!("watch-{agent}.jsonl"));
        let mut watch = Command::new(PARROT)
            .args(["watch", &session, "--json"])
            .env("PARROT_SOCKET", &self.socket)
            .stdout(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        self.client(&["prompt", &session, text]);
        let deadline = Instant::now() + WAIT;
        while !std::fs::read_to_string(&log).unwrap_or_default().contains(expect) {
            assert!(
                Instant::now() < deadline,
                "{agent} never answered {expect:?}:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = watch.kill();
        let _ = watch.wait();
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn without_eggs_there_are_no_agents_and_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let serve = Serve::start(dir.path(), &[]);
    let v = serve.client(&["agents"]);
    assert_eq!(v["agents"], serde_json::json!([]), "{v}");
    assert!(
        v["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("parrot eggs install"),
        "{v}"
    );
}

/// Builds the example driver, which is not a member of the workspace, and returns its binary.
fn echo_driver() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/driver-echo/Cargo.toml");
    let target = common::bin_dir().join("../external");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .status()
        .expect("cargo runs");
    assert!(status.success(), "building the example driver failed");
    target
        .join("debug")
        .join(format!("parrot-driver-echo{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn a_third_party_driver_installs_from_a_registry_and_runs() {
    let dir = tempfile::tempdir().unwrap();
    // The package folder as a release would have it: the binary in this platform's variant.
    let package = dir.path().join("echo");
    common::copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/driver-echo/egg"),
        &package,
        false,
    );
    let key = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let variant = package.join("parrot/variants").join(key);
    std::fs::create_dir_all(&variant).unwrap();
    std::fs::copy(echo_driver(), variant.join("parrot-driver-echo")).unwrap();

    let registry = dir.path().join("registry");
    let out = ok(Command::new(PARROT)
        .args(["eggs", "pack"])
        .arg(&package)
        .arg("--publish")
        .arg(&registry)
        .output()
        .unwrap());
    assert!(out.contains("published example/echo 0.1.0"), "{out}");
    assert!(registry.join("index.json").is_file());
    let out = ok(Command::new(PARROT)
        .args(["eggs", "install", "example/echo", "--registry"])
        .arg(&registry)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .output()
        .unwrap());
    assert!(out.contains("installed example/echo 0.1.0"), "{out}");

    let serve = Serve::start(dir.path(), &[]);
    let agents = serve.agents_when("echo never appeared", |a| a.iter().any(|a| a["id"] == "echo"));
    assert_eq!(agents.len(), 1, "{agents:?}");
    assert_eq!(agents[0]["driver"]["kind"], "process");
    serve.prompt_until(dir.path(), "echo", "hi", "Polly says echo: hi");
}

#[test]
fn a_data_only_egg_on_the_acp_core_is_one_agent_under_its_own_id() {
    common::build_drivers();
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("gemini");
    std::fs::create_dir_all(package.join("parrot")).unwrap();
    std::fs::write(
        package.join("package.kdl"),
        "format 1\npackage {\n  group example\n  id gemini\n  version \"0.1.0\"\n}\nabout {\n  name \"Example Gemini\"\n}\ntargets {\n  parrot {\n    requires {\n      parrot \"^0.1\"\n    }\n  }\n}\n",
    )
    .unwrap();
    let fake = common::bin_dir().join("fake-acp");
    std::fs::write(
        package.join("parrot/driver.kdl"),
        format!(
            "driver \"builtin\" name=\"acp\"\ncommand \"{}\"\ndata-version \"1\"\n",
            fake.display()
        ),
    )
    .unwrap();
    ok(Command::new(PARROT)
        .args(["eggs", "install"])
        .arg(&package)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .output()
        .unwrap());

    let serve = Serve::start(dir.path(), &[]);
    let agents = serve.agents_when("gemini was never discovered", |a| {
        a.iter()
            .any(|a| a["id"] == "gemini" && a["features"]["session.resume"].is_object())
    });
    assert_eq!(agents.len(), 1, "one agent, not a second `acp`: {agents:?}");
    assert_eq!(agents[0]["name"], "Example Gemini");
    assert_eq!(agents[0]["driver"]["name"], "acp");
    serve.prompt_until(dir.path(), "gemini", "hi", "\"kind\":\"assistant\"");
}

#[test]
fn the_claude_egg_picks_its_version_shim() {
    let dir = tempfile::tempdir().unwrap();
    common::install_eggs(&dir.path().join("state"));
    // fake-claude reports 2.1.327, which the egg's `>=2.1.325` shim covers.
    let serve = Serve::start(dir.path(), &[("PARROT_CLAUDE_BIN", common::fake_claude())]);
    let agents = serve.agents_when("claude was never discovered", |a| {
        a.iter()
            .any(|a| a["id"] == "claude" && a.get("harness").is_some())
    });
    let claude = agents.iter().find(|a| a["id"] == "claude").unwrap();
    assert_eq!(claude["driver"]["kind"], "process", "{claude}");
    let mode = claude["options"]
        .as_array()
        .and_then(|o| o.iter().find(|o| o["id"] == "permissionMode"))
        .unwrap_or_else(|| panic!("no permissionMode: {claude}"));
    assert!(mode.to_string().contains("\"manual\""), "{mode}");
}
