//! `parrot eggs` against the in-repo registry `eggs/` and against a static registry packed
//! from it.
#![cfg(unix)]

mod common;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");

fn repo_eggs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eggs")
}

fn parrot(args: &[&str], state: &Path) -> Output {
    Command::new(PARROT)
        .args(args)
        .arg("--state-dir")
        .arg(state)
        .output()
        .expect("parrot runs")
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

fn listed(state: &Path) -> Vec<Value> {
    ok(parrot(&["eggs", "list", "--json"], state))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn entry<'a>(list: &'a [Value], agent: &str) -> &'a Value {
    list.iter()
        .find(|e| e["agent"] == agent)
        .unwrap_or_else(|| panic!("no {agent} in {list:?}"))
}

#[test]
fn nothing_is_installed_by_default() {
    let dir = tempfile::tempdir().unwrap();
    assert!(listed(dir.path()).is_empty());
    let out = ok(parrot(&["eggs", "list"], dir.path()));
    assert!(out.contains("parrot eggs install"), "{out}");
}

#[test]
fn installs_from_the_in_repo_registry() {
    let dir = tempfile::tempdir().unwrap();
    // A data-only egg installs straight from the source folders.
    let out = ok(parrot(
        &[
            "eggs",
            "install",
            "aiviarium/acp",
            "--registry",
            repo_eggs().to_str().unwrap(),
        ],
        dir.path(),
    ));
    assert!(out.contains("installed aiviarium/acp 0.1.0"), "{out}");
    // A driver egg needs its binary, so it installs from the folders with the variants in.
    common::egg_template();
    let staged = common::bin_dir().join(format!("test-eggs-{}/registry", std::process::id()));
    let out = ok(parrot(
        &[
            "eggs",
            "install",
            "aiviarium/claude",
            "codex",
            "--registry",
            staged.to_str().unwrap(),
        ],
        dir.path(),
    ));
    assert!(out.contains("installed aiviarium/claude 0.1.0"), "{out}");
    let list = listed(dir.path());
    for agent in ["acp", "claude", "codex"] {
        assert_eq!(entry(&list, agent)["source"], "installed");
        assert_eq!(entry(&list, agent)["status"], "ok");
    }
    assert!(list.iter().all(|e| e["agent"] != "pi"), "{list:?}");
    let data = entry(&list, "claude")["data"].as_str().unwrap().to_string();
    assert!(Path::new(&data).join("driver.kdl").is_file());
    let key = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    assert!(
        Path::new(&data)
            .join("variants")
            .join(key)
            .join("parrot-driver-claude")
            .is_file()
    );
    ok(parrot(&["eggs", "check"], dir.path()));
}

#[test]
fn packs_and_installs_from_a_static_registry() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    let out = ok(Command::new(PARROT)
        .args(["eggs", "pack", repo_eggs().to_str().unwrap(), "--publish"])
        .arg(&registry)
        .output()
        .unwrap());
    assert!(out.contains("published aiviarium/pi 0.1.0"), "{out}");
    assert!(registry.join("index.json").is_file());
    let state = dir.path().join("state");
    ok(parrot(
        &[
            "eggs",
            "install",
            "aiviarium/acp",
            "--registry",
            registry.to_str().unwrap(),
        ],
        &state,
    ));
    assert_eq!(entry(&listed(&state), "acp")["source"], "installed");
    // pi's driver binary is a variant this registry doesn't have, so its install fails.
    let out = parrot(
        &[
            "eggs",
            "install",
            "aiviarium/pi",
            "--registry",
            registry.to_str().unwrap(),
        ],
        &state,
    );
    assert!(!out.status.success());

    let eggs = dir.path().join("eggs");
    let out = ok(Command::new(PARROT)
        .args(["eggs", "pack", repo_eggs().join("t3").to_str().unwrap(), "--out"])
        .arg(&eggs)
        .output()
        .unwrap());
    assert!(eggs.join("t3-0.1.0.egg").is_file(), "{out}");
}

/// A package folder for agent `id` whose target requires `parrot <range>`.
fn package(root: &Path, id: &str, range: &str, extra: &[(&str, &str)]) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(dir.join("parrot")).unwrap();
    std::fs::write(
        dir.join("package.kdl"),
        format!(
            "format 1\npackage {{\n  group example\n  id {id}\n  version \"1.0.0\"\n}}\ntargets {{\n  parrot {{\n    requires {{\n      parrot \"{range}\"\n    }}\n  }}\n}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("parrot/driver.kdl"),
        "driver \"builtin\" name=\"claude\"\ndata-version \"1\"\n",
    )
    .unwrap();
    for (path, text) in extra {
        let p = dir.join("parrot").join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

#[test]
fn refuses_an_egg_for_another_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let src = package(dir.path(), "later", "^9", &[]);
    let state = dir.path().join("state");
    let out = parrot(&["eggs", "install", src.to_str().unwrap()], &state);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("parrot"), "{err}");

    // An egg installed for an older runtime is refused when it is loaded.
    let src = package(dir.path(), "aged", "^0.1", &[]);
    ok(parrot(&["eggs", "install", src.to_str().unwrap()], &state));
    let installed = state.join("eggs/example/aged/package.kdl");
    let text = std::fs::read_to_string(&installed).unwrap().replace("^0.1", "^9");
    std::fs::write(&installed, text).unwrap();
    let list = listed(&state);
    let e = entry(&list, "aged");
    assert_eq!(e["status"], "refused");
    let detail = e["detail"].as_str().unwrap();
    assert!(
        detail.contains("needs parrot ^9, and this runtime is 0.1.0"),
        "{detail}"
    );
}

#[test]
fn check_reports_bad_data_files() {
    let dir = tempfile::tempdir().unwrap();
    let good = package(
        dir.path(),
        "good",
        "^0.1",
        &[("options.kdl", "option \"model\" role=\"model\"\n")],
    );
    let bad = package(
        dir.path(),
        "bad",
        "^0.1",
        &[("levels.kdl", "level {\n"), ("probes/x.kdl", "jump\n")],
    );
    let state = dir.path().join("state");
    ok(parrot(&["eggs", "check", good.to_str().unwrap()], &state));
    let out = parrot(&["eggs", "check", bad.to_str().unwrap()], &state);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("levels.kdl") && text.contains("probes/x.kdl"),
        "{text}"
    );
    // Installing runs the same check.
    assert!(
        !parrot(&["eggs", "install", bad.to_str().unwrap()], &state)
            .status
            .success()
    );
}
