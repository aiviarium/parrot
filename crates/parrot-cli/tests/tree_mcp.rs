//! The `tree` profile proof with the orchestrator's agent calling the session functions itself:
//! `fake-claude` gets the injected `parrot` MCP server through `--mcp-config`, starts
//! `parrot mcp`, spawns two children in worktrees, waits on their tasks and merges them. The
//! children commit in their worktrees and finish their tasks over MCP too.
#![cfg(unix)]
// Unix only for now: these runtime tests have not run on Windows yet.

mod common;

use common::{Client, Serve, Setup, fake_claude_env, scenario};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test Person",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn an_orchestrator_delegates_waits_and_merges_through_mcp() {
    let serve = Serve::start(Setup {
        env: fake_claude_env(&scenario("tree-orchestrate.jsonl")),
        ..Default::default()
    });
    let repo = serve.work_dir().join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Test Person"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(repo.join("README.md"), "demo\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "start"]);

    let mut owner = Client::connect(&serve, "owner").await;
    owner.initialize().await;
    let parent = owner
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": repo },
                "grant": ["read", "prompt", "delegate", "workspace"], "title": "Orchestrator" }),
        )
        .await;
    let parent_id = parent["session"]["id"].as_str().unwrap().to_string();
    let from = owner.seen.len();
    owner
        .ok(
            "session/prompt",
            json!({ "session": parent_id, "text": "Orchestrate the release notes." }),
        )
        .await;
    owner
        .wait_update(from, "the orchestrator's turn to end", |u| {
            u["type"] == "turn" && u["turn"]["status"] != "running"
        })
        .await;
    let i = owner
        .wait_update(from, "the orchestrator's reply", |u| {
            u["type"] == "item" && u["item"]["set"]["text"] == "Both children are merged."
        })
        .await;
    assert_eq!(owner.seen[i]["params"]["session"], parent_id.as_str());

    // Both children ran in their own worktrees and finished their tasks over MCP.
    let tree = owner.ok("session/tree", json!({ "session": parent_id })).await;
    let nodes = tree["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3, "{tree}");
    let children: Vec<&str> = nodes
        .iter()
        .filter(|n| n["depth"] == 1)
        .map(|n| n["session"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(children.len(), 2, "{tree}");
    for child in &children {
        let got = owner.ok("session/get", json!({ "session": child })).await;
        assert_eq!(got["session"]["task"]["status"], "done", "{got}");
        assert_ne!(got["session"]["workspace"], parent["session"]["workspace"]);
        // Each child's file is merged into the orchestrator's checkout.
        assert!(
            repo.join(format!("{child}.txt")).exists(),
            "no {child}.txt in the repo"
        );
    }
    let log = git(&repo, &["log", "--format=%s"]);
    assert!(log.contains("add ses_"), "{log}");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");

    // The orchestrator's MCP calls are tool items in its transcript, with the function they
    // reached (spec 25.1).
    let read = owner
        .ok("session/read", json!({ "session": parent_id, "limit": 100 }))
        .await;
    let tools: Vec<&Value> = read["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "tool")
        .collect();
    let functions: Vec<&str> = tools.iter().filter_map(|t| t["function"].as_str()).collect();
    assert_eq!(
        functions,
        [
            "parrot.sessions.spawn",
            "parrot.sessions.spawn",
            "parrot.tasks.wait",
            "parrot.sessions.read",
            "parrot.workspaces.merge",
            "parrot.workspaces.merge"
        ],
        "{read}"
    );
    assert!(
        tools
            .iter()
            .all(|t| t["server"] == "parrot" && t["status"] == "ok"),
        "{read}"
    );
}
