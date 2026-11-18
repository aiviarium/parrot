//! Transfers arriving from linked peers, kept on disk until they are used (spec 24.2, E9).
//!
//! Each `blob/put` chunk is one file under the transfers directory, in a folder per link and per
//! transfer. The core counts what each link holds and refuses chunks over its cap; this module
//! only writes, reads and deletes. One task runs the operations in the order the core sent them,
//! so a transfer is read only after every chunk the core accepted for it was written.
//!
//! | Operation | Params | Result |
//! | --- | --- | --- |
//! | `blob.put` | `link`, `transfer`, `index`, `data` | `{}` |
//! | `blob.take` | `blob: { link, transfer, parts }` | `data`: the joined text; the files are deleted |
//! | `blob.drop` | `link`, `transfer` | `{}` |
//! | `transfer.unpack` | `workspace`, `root`, `blob` | as in `workspace_transfer`, with the blob as `data` |

use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// One operation and the core call it answers.
pub struct Job {
    pub call: u64,
    pub op: String,
    pub params: Value,
}

/// Whether the blob worker runs `op`: blob operations, and unpacking a workspace from a blob.
pub fn handles(op: &str, params: &Value) -> bool {
    op.starts_with("blob.") || (op == "transfer.unpack" && params.get("blob").is_some())
}

/// Empties the transfers directory. Transfers don't survive a restart: their offers expire.
pub fn clear(root: &Path) {
    if root.exists()
        && let Err(e) = std::fs::remove_dir_all(root)
    {
        eprintln!("parrot: cannot clear {}: {e}", root.display());
    }
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// The folder of one transfer. Ids come from the peer, so they are hex-encoded, never joined as
/// given.
fn folder(root: &Path, link: &str, transfer: &str) -> PathBuf {
    root.join(hex(link)).join(hex(transfer))
}

fn invalid(what: &str) -> RpcError {
    RpcError::new(-32602, format!("invalid params: {what}"))
}

fn io(what: &str, path: &Path, e: std::io::Error) -> RpcError {
    RpcError::new(-32004, format!("cannot {what} {}: {e}", path.display()))
}

fn ids(params: &Value) -> Result<(String, String), RpcError> {
    let link = params["link"].as_str().ok_or_else(|| invalid("link"))?;
    let transfer = params["transfer"].as_str().ok_or_else(|| invalid("transfer"))?;
    Ok((link.to_string(), transfer.to_string()))
}

/// Runs one operation on the transfers under `root`.
pub fn run(root: &Path, op: &str, mut params: Value) -> Result<Value, RpcError> {
    match op {
        "blob.put" => {
            let (link, transfer) = ids(&params)?;
            let index = params["index"].as_u64().ok_or_else(|| invalid("index"))?;
            let data = params["data"].as_str().ok_or_else(|| invalid("data"))?;
            let dir = folder(root, &link, &transfer);
            std::fs::create_dir_all(&dir).map_err(|e| io("create", &dir, e))?;
            let file = dir.join(index.to_string());
            std::fs::write(&file, data).map_err(|e| io("write", &file, e))?;
            Ok(json!({}))
        }
        "blob.take" => take(root, &params["blob"]).map(|data| json!({ "data": data })),
        "blob.drop" => {
            let (link, transfer) = ids(&params)?;
            let dir = folder(root, &link, &transfer);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| io("delete", &dir, e))?;
            }
            Ok(json!({}))
        }
        "transfer.unpack" => {
            let data = take(root, &params["blob"])?;
            if let Some(o) = params.as_object_mut() {
                o.remove("blob");
                o.insert("data".into(), json!(data));
            }
            crate::workspace_transfer::run(op, &params).unwrap_or_else(|| Err(invalid("op")))
        }
        _ => Err(RpcError::new(-32601, format!("unknown blob operation {op}"))),
    }
}

/// Joins a complete transfer's chunks and deletes them.
fn take(root: &Path, blob: &Value) -> Result<String, RpcError> {
    let (link, transfer) = ids(blob)?;
    let parts = blob["parts"].as_u64().ok_or_else(|| invalid("parts"))?;
    let dir = folder(root, &link, &transfer);
    let mut text = String::new();
    for i in 0..parts {
        let file = dir.join(i.to_string());
        let part = std::fs::read_to_string(&file).map_err(|e| io("read", &file, e))?;
        text.push_str(&part);
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        eprintln!("parrot: cannot delete {}: {e}", dir.display());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_land_on_disk_and_are_joined_once() {
        let root = tempfile::tempdir().unwrap();
        for (i, part) in ["one ", "two ", "three"].iter().enumerate() {
            run(
                root.path(),
                "blob.put",
                json!({ "link": "lnk_a", "transfer": "../t", "index": i, "data": part }),
            )
            .unwrap();
        }
        // The peer's ids never leave the transfers folder.
        let dir = folder(root.path(), "lnk_a", "../t");
        assert!(dir.starts_with(root.path()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        let blob = json!({ "link": "lnk_a", "transfer": "../t", "parts": 3 });
        let got = run(root.path(), "blob.take", json!({ "blob": blob })).unwrap();
        assert_eq!(got["data"], "one two three");
        assert!(!dir.exists());
        assert!(run(root.path(), "blob.take", json!({ "blob": blob })).is_err());
    }

    #[test]
    fn a_dropped_transfer_is_deleted() {
        let root = tempfile::tempdir().unwrap();
        let put = json!({ "link": "lnk_a", "transfer": "t", "index": 0, "data": "x" });
        run(root.path(), "blob.put", put).unwrap();
        run(
            root.path(),
            "blob.drop",
            json!({ "link": "lnk_a", "transfer": "t" }),
        )
        .unwrap();
        assert!(!folder(root.path(), "lnk_a", "t").exists());
    }
}
