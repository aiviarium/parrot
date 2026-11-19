//! The environment's roots fence (spec 7, 8): a folder a client picks is inside one of them.

mod common;

use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Input, Peer};
use serde_json::{Value, json};

struct Sim {
    core: Core,
    next_id: u64,
}

impl Sim {
    fn with_roots(roots: &[std::path::PathBuf]) -> Self {
        let clock = ManualClock::default();
        clock.set(1_700_000_000_000);
        let mut core = common::core(Box::new(clock), Box::new(SeqIds::default()));
        core.state.principals.roots = roots.to_vec();
        let mut sim = Self { core, next_id: 0 };
        sim.connect("con_owner");
        sim
    }

    fn connect(&mut self, conn: &str) {
        self.core.handle(Input::Connected {
            conn: conn.into(),
            peer: Peer::Owner,
        });
        let r = self.call(
            conn,
            "initialize",
            json!({
                "protocol": [1], "peer": { "kind": "client", "name": conn, "version": "1" }, "features": {}
            }),
        );
        r.expect("initialize");
    }

    fn call(&mut self, conn: &str, method: &str, params: Value) -> Result<Value, i64> {
        self.next_id += 1;
        let id = json!(self.next_id);
        let effects = self.core.handle(Input::Call {
            conn: conn.into(),
            id: Some(id.clone()),
            method: method.into(),
            params,
        });
        effects
            .iter()
            .find_map(|e| match e {
                parrot_core::Effect::Reply { id: rid, result, .. } if *rid == id => {
                    Some(result.clone().map_err(|e| e.code))
                }
                _ => None,
            })
            .unwrap_or(Err(0))
    }

    fn new_session(&mut self, path: &std::path::Path) -> Result<Value, i64> {
        self.call(
            "con_owner",
            "session/new",
            json!({
                "agent": "claude",
                "workspace": { "kind": "dir", "path": path },
                "start": false,
            }),
        )
    }
}

fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    let inside = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(inside.path().join("src")).unwrap();
    (inside, outside)
}

#[test]
fn allows_a_folder_inside_the_roots() {
    let (inside, _) = dirs();
    let mut sim = Sim::with_roots(&[inside.path().to_path_buf()]);
    let r = sim
        .new_session(&inside.path().join("src"))
        .expect("inside the roots");
    assert!(
        r["session"]["workspace"]
            .as_str()
            .is_some_and(|w| w.starts_with("wsp_")),
        "{r}"
    );
}

#[test]
fn refuses_a_folder_outside_the_roots() {
    let (inside, outside) = dirs();
    let mut sim = Sim::with_roots(&[inside.path().to_path_buf()]);
    assert_eq!(sim.new_session(outside.path()).unwrap_err(), -32003);
}

#[test]
fn refuses_a_folder_that_climbs_out() {
    let (inside, _) = dirs();
    let mut sim = Sim::with_roots(&[inside.path().to_path_buf()]);
    let climbed = inside.path().join("src/../../elsewhere");
    assert_eq!(sim.new_session(&climbed).unwrap_err(), -32003);
}

#[cfg(unix)]
#[test]
fn refuses_a_symlink_that_points_out_of_the_roots() {
    let (inside, outside) = dirs();
    std::fs::create_dir(outside.path().join("work")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("work"), inside.path().join("link")).unwrap();
    let mut sim = Sim::with_roots(&[inside.path().to_path_buf()]);
    // The link stays under the root by name and leaves it once resolved.
    assert_eq!(sim.new_session(&inside.path().join("link")).unwrap_err(), -32003);
}

#[test]
fn refuses_a_cwd_outside_the_workspace() {
    let (inside, outside) = dirs();
    let mut sim = Sim::with_roots(&[inside.path().to_path_buf(), outside.path().to_path_buf()]);
    let outside = outside.path().to_path_buf();
    let r = sim.call(
        "con_owner",
        "session/new",
        json!({
            "agent": "claude",
            "workspace": { "kind": "dir", "path": inside.path() },
            "cwd": outside,
            "start": false,
        }),
    );
    assert_eq!(r.unwrap_err(), -32003);
}

#[test]
fn refuses_a_workspace_outside_the_roots() {
    let (inside, outside) = dirs();
    let mut sim = Sim::with_roots(&[inside.path().to_path_buf()]);
    let env = sim.core.state.principals.env.clone();
    let r = sim.call(
        "con_owner",
        "workspace/create",
        json!({ "env": env, "request": { "kind": "dir", "path": outside.path() } }),
    );
    assert_eq!(r.unwrap_err(), -32003);
}

#[test]
fn reports_the_roots_in_the_environment() {
    let (inside, _) = dirs();
    let root = inside.path().canonicalize().unwrap();
    let mut sim = Sim::with_roots(std::slice::from_ref(&root));
    let envs = sim.call("con_owner", "env/list", json!({})).unwrap();
    assert_eq!(envs["envs"][0]["roots"], json!([root.display().to_string()]));
    assert!(envs["envs"][0]["features"].is_object());
}
