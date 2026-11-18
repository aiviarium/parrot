//! Runs the core's `Effect::Workspace` operations with `parrot-workspace`, off the actor thread.
//!
//! | Operation | Params | Result |
//! | --- | --- | --- |
//! | `worktree.create` | `repo`, `path`, `branch`, `base?` | `path`, `branch`, `base`, `commit?` |
//! | `worktree.remove` | `repo`, `path`, `branch`, `base?`, `force` | `{}` |
//! | `status` | `path`, `kind`, `base?` | `WorkspaceStatusResult` |
//! | `merge` | `from`, `into`, `method`, `message?` | `WorkspaceMergeResult` |
//! | `checkpoint.record` | `path`, `session`, `turn` | `ref`, `commit` |
//! | `checkpoint.restore` | `path`, `session`, `turn` | `ref`, `commit` |
//! | `checkpoint.prune` | `path`, `session` | `removed` |

use parrot_proto::rpc::RpcError;
use parrot_proto::workspace::MergeMethod;
use parrot_workspace::{checkpoint, dir, merge, worktree};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Run one operation; blocking, so the backend calls it from a blocking task.
pub fn run(op: &str, params: Value) -> Result<Value, RpcError> {
    if let Some(result) = crate::workspace_transfer::run(op, &params) {
        return result;
    }
    match op {
        "worktree.create" => create(&params),
        "worktree.remove" => remove(&params),
        "status" => status(&params),
        "merge" => merge_op(&params),
        "checkpoint.record" => {
            let (path, session, turn) = checkpoint_args(&params)?;
            let c = checkpoint::record(&path, &session, turn).map_err(failed)?;
            Ok(json!({ "ref": c.reference, "commit": c.commit }))
        }
        "checkpoint.restore" => {
            let (path, session, turn) = checkpoint_args(&params)?;
            let c = checkpoint::restore(&path, &session, turn).map_err(failed)?;
            Ok(json!({ "ref": c.reference, "commit": c.commit }))
        }
        "checkpoint.prune" => {
            let path = path_of(&params, "path")?;
            let session = str_of(&params, "session")?;
            let removed = checkpoint::prune_session(&path, &session).map_err(failed)?;
            Ok(json!({ "removed": removed }))
        }
        _ => Err(
            RpcError::new(-32001, format!("unsupported workspace operation: {op}"))
                .with_data(json!({ "feature": "profile.tree" })),
        ),
    }
}

/// A failed operation: `-32004` (wrong state) with git's message.
fn failed(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(-32004, e.to_string())
}

fn str_of(params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::new(-32602, format!("invalid params: {key}")))
}

fn path_of(params: &Value, key: &str) -> Result<PathBuf, RpcError> {
    str_of(params, key).map(PathBuf::from)
}

fn checkpoint_args(params: &Value) -> Result<(PathBuf, String, u32), RpcError> {
    let turn = params
        .get("turn")
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: turn"))? as u32;
    Ok((path_of(params, "path")?, str_of(params, "session")?, turn))
}

fn create(params: &Value) -> Result<Value, RpcError> {
    let repo = path_of(params, "repo")?;
    let path = path_of(params, "path")?;
    let branch = str_of(params, "branch")?;
    let base = params.get("base").and_then(Value::as_str);
    let w = worktree::create(worktree::NewWorktree {
        repo: &repo,
        path: &path,
        branch: &branch,
        base,
    })
    .map_err(failed)?;
    // The base as a commit, so status counts stay right when the base branch moves.
    let commit = parrot_workspace::git::head(&w.path).ok().flatten();
    Ok(json!({
        "path": w.path.to_string_lossy(),
        "branch": w.branch,
        "base": commit.clone().unwrap_or(w.base),
        "commit": commit,
    }))
}

fn remove(params: &Value) -> Result<Value, RpcError> {
    let w = worktree::Worktree {
        path: path_of(params, "path")?,
        repo: path_of(params, "repo")?,
        branch: str_of(params, "branch").unwrap_or_default(),
        base: params
            .get("base")
            .and_then(Value::as_str)
            .unwrap_or("HEAD")
            .to_string(),
    };
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    worktree::remove(&w, force).map_err(failed)?;
    Ok(json!({}))
}

fn status(params: &Value) -> Result<Value, RpcError> {
    let path = path_of(params, "path")?;
    let kind = params.get("kind").and_then(Value::as_str).unwrap_or("dir");
    if kind == "worktree"
        && let Some(base) = params.get("base").and_then(Value::as_str)
    {
        let s = worktree::status(&path, base).map_err(failed)?;
        return Ok(
            json!({ "branch": s.branch, "commit": s.commit, "ahead": s.ahead, "behind": s.behind,
            "dirty": s.dirty, "changed": s.changed }),
        );
    }
    let s = dir::DirWorkspace::open(Path::new(&path))
        .and_then(|d| d.status())
        .map_err(failed)?;
    let mut v = json!({ "dirty": s.dirty, "changed": s.changed });
    if let Some(b) = s.branch {
        v["branch"] = json!(b);
    }
    if let Some(c) = s.commit {
        v["commit"] = json!(c);
    }
    Ok(v)
}

fn merge_op(params: &Value) -> Result<Value, RpcError> {
    let from = path_of(params, "from")?;
    let into = path_of(params, "into")?;
    let method = match params.get("method").and_then(Value::as_str) {
        Some("squash") => MergeMethod::Squash,
        _ => MergeMethod::Merge,
    };
    let message = params.get("message").and_then(Value::as_str);
    let r = merge::merge(&from, &into, method, message).map_err(failed)?;
    Ok(serde_json::to_value(r).unwrap_or_else(|_| json!({})))
}
