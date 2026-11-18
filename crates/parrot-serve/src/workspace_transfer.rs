//! Workspace operations that cross runtimes: packing a workspace for `workspace/transfer` or a
//! move, and unpacking one that arrived over a link (spec 8, 24, E1, E9).
//!
//! | Operation | Params | Result |
//! | --- | --- | --- |
//! | `transfer.pack` | `path`, `branch?`, `include?` | `data`, `branch` |
//! | `transfer.unpack` | `workspace`, `data`, `root` | `path`, `branch`, `commit?` |
//!
//! `data` is the packed workspace as JSON text: the branch, the git bundle and the carried
//! patches and untracked files, binary parts in base64. The core splits it into `blob/put`
//! chunks for the link and joins them again on the other side. A received workspace becomes a
//! repository of its own under the runtime's workspace root, named by its workspace id.

use parrot_link::base64;
use parrot_proto::rpc::RpcError;
use parrot_proto::workspace::Carry;
use parrot_workspace::transfer::{self, Bundle, Transfer, UntrackedFile};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Run a transfer operation; `None` when `op` is not one.
pub fn run(op: &str, params: &Value) -> Option<Result<Value, RpcError>> {
    match op {
        "transfer.pack" => Some(pack(params)),
        "transfer.unpack" => Some(unpack(params)),
        _ if op.starts_with("transfer.") => Some(Err(RpcError::new(
            -32001,
            format!("unsupported workspace operation: {op}"),
        )
        .with_data(json!({ "feature": "profile.link" })))),
        _ => None,
    }
}

fn invalid(what: &str) -> RpcError {
    RpcError::new(-32602, format!("invalid params: {what}"))
}

fn wrong_state(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(-32004, e.to_string())
}

fn pack(params: &Value) -> Result<Value, RpcError> {
    let path = params["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| invalid("path"))?;
    let include: Vec<Carry> = match params.get("include").filter(|v| !v.is_null()) {
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| invalid("include"))?,
        None => transfer::default_carry(),
    };
    // Only git workspaces can be transferred (spec 8).
    if !parrot_workspace::git::is_repository(&path).unwrap_or(false) {
        return Err(wrong_state("the workspace is not a git repository"));
    }
    let branch = match params["branch"].as_str() {
        Some(b) => b.to_string(),
        None => parrot_workspace::git::branch(&path)
            .map_err(wrong_state)?
            .ok_or_else(|| wrong_state("the workspace has no branch checked out"))?,
    };
    let t = transfer::pack(&path, &branch, &include).map_err(wrong_state)?;
    let bundle = t.bundle_bytes().map_err(wrong_state)?;
    let data = json!({
        "branch": t.branch,
        "bundle": base64::encode(&bundle),
        "staged": t.staged.as_deref().map(base64::encode),
        "unstaged": t.unstaged.as_deref().map(base64::encode),
        "untracked": t.untracked.iter().map(|f| json!({
            "path": f.path, "contents": base64::encode(&f.contents), "executable": f.executable,
        })).collect::<Vec<_>>(),
    });
    Ok(json!({ "data": data.to_string(), "branch": branch }))
}

fn bytes(v: &Value, what: &str) -> Result<Option<Vec<u8>>, RpcError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => base64::decode(s).map(Some).ok_or_else(|| invalid(what)),
        _ => Err(invalid(what)),
    }
}

/// Whether an id is safe as one path component.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn unpack(params: &Value) -> Result<Value, RpcError> {
    let workspace = params["workspace"].as_str().ok_or_else(|| invalid("workspace"))?;
    // The id comes from the peer, so it may only name a folder directly under the root.
    if !safe_id(workspace) {
        return Err(invalid("workspace"));
    }
    let root = params["root"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| RpcError::new(-32004, "this runtime has no workspace root"))?;
    let text = params["data"].as_str().ok_or_else(|| invalid("data"))?;
    let data: Value = serde_json::from_str(text).map_err(|_| invalid("data"))?;
    let branch = data["branch"]
        .as_str()
        .ok_or_else(|| invalid("branch"))?
        .to_string();
    let bundle = bytes(&data["bundle"], "bundle")?.ok_or_else(|| invalid("bundle"))?;
    let mut untracked = Vec::new();
    for f in data["untracked"].as_array().into_iter().flatten() {
        untracked.push(UntrackedFile {
            path: f["path"]
                .as_str()
                .ok_or_else(|| invalid("untracked"))?
                .to_string(),
            contents: bytes(&f["contents"], "untracked")?.unwrap_or_default(),
            executable: f["executable"].as_bool().unwrap_or(false),
        });
    }
    let t = Transfer {
        branch: branch.clone(),
        bundle: Bundle::Bytes(bundle),
        staged: bytes(&data["staged"], "staged")?,
        unstaged: bytes(&data["unstaged"], "unstaged")?,
        untracked,
    };
    create_root(&root)?;
    let path = root.join(workspace);
    transfer::unpack_new(&path, &t).map_err(wrong_state)?;
    let commit = parrot_workspace::git::head(&path).ok().flatten();
    Ok(json!({ "path": path.to_string_lossy(), "branch": branch, "commit": commit }))
}

fn create_root(root: &Path) -> Result<(), RpcError> {
    std::fs::create_dir_all(root).map_err(|e| wrong_state(format!("cannot create {}: {e}", root.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.name=Made Up",
                "-c",
                "user.email=made-up@example.invalid",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn packs_and_unpacks_through_json_text() {
        let src = tempfile::tempdir().unwrap();
        git(src.path(), &["init", "--quiet", "--initial-branch=parrot/ses_a"]);
        std::fs::write(src.path().join("a.txt"), "one\n").unwrap();
        git(src.path(), &["add", "a.txt"]);
        git(src.path(), &["commit", "--quiet", "-m", "one"]);
        std::fs::write(src.path().join("a.txt"), "two\n").unwrap();
        let packed = run("transfer.pack", &json!({ "path": src.path() }))
            .unwrap()
            .unwrap();
        assert_eq!(packed["branch"], "parrot/ses_a");

        let root = tempfile::tempdir().unwrap();
        let got = run(
            "transfer.unpack",
            &json!({ "workspace": "wsp_b", "data": packed["data"], "root": root.path() }),
        )
        .unwrap()
        .unwrap();
        let path = PathBuf::from(got["path"].as_str().unwrap());
        assert_eq!(path, root.path().join("wsp_b"));
        // Git on Windows may check the file out with CRLF line endings, as its settings say.
        let text = std::fs::read_to_string(path.join("a.txt")).unwrap();
        assert_eq!(text.replace("\r\n", "\n"), "two\n");

        for bad in ["../x", "", "a/b"] {
            let e = run(
                "transfer.unpack",
                &json!({ "workspace": bad, "data": packed["data"], "root": root.path() }),
            )
            .unwrap()
            .unwrap_err();
            assert_eq!(e.code, -32602, "{bad}");
        }
    }

    #[test]
    fn refuses_a_directory_without_git() {
        let dir = tempfile::tempdir().unwrap();
        let e = run("transfer.pack", &json!({ "path": dir.path() }))
            .unwrap()
            .unwrap_err();
        assert_eq!(e.code, -32004);
    }
}
