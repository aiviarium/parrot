use parrot_store::{DEFAULT_RETENTION_MS, Error, Ops, PruneReport, Store};
use serde_json::json;

fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("state").join("parrot.db")
}

#[test]
fn reopen_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(path(&dir)).unwrap();
        s.append(1, Some("s1"), 10, &json!({"a": 1})).unwrap();
        s.append(2, None, 11, &json!({"b": 2})).unwrap();
        s.put("session", "s1", &json!({"state": "idle"})).unwrap();
        s.put_frame("b1", 0, 10, "v1", &json!({"raw": "x"})).unwrap();
    }
    let s = Store::open(path(&dir)).unwrap();
    assert_eq!(s.head().unwrap(), Some(2));
    assert_eq!(s.oldest_seq().unwrap(), Some(1));
    let rows = s.read_from(0, 10).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].update, json!({"b": 2}));
    assert_eq!(s.read_session("s1", 0, 10).unwrap().len(), 1);
    assert_eq!(s.get("session", "s1").unwrap(), Some(json!({"state": "idle"})));
    let f = s.frames("b1", 0, 10).unwrap();
    assert_eq!(f[0].data_version, "v1");
}

#[cfg(unix)]
#[test]
fn state_dir_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    Store::open(path(&dir)).unwrap();
    let mode = std::fs::metadata(dir.path().join("state"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn migrations_are_idempotent_and_newer_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(&dir);
    let v = Store::open(&p).unwrap().schema_version().unwrap();
    assert_eq!(v, parrot_store::migrations::LATEST);
    assert_eq!(Store::open(&p).unwrap().schema_version().unwrap(), v);

    let conn = rusqlite::Connection::open(&p).unwrap();
    conn.execute("UPDATE schema_version SET version = ?1", [v + 1])
        .unwrap();
    drop(conn);
    match Store::open(&p) {
        Err(Error::NewerSchema { found, supported }) => {
            assert_eq!((found, supported), (v + 1, v));
        }
        other => panic!("expected NewerSchema, got {:?}", other.err()),
    }
}

#[test]
fn intents_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b);
    {
        let s = Store::open(path(&dir)).unwrap();
        a = s.begin("prompt", &json!({"text": "hi"})).unwrap();
        b = s.begin("rebind", &json!({})).unwrap();
        s.step(a, "sent", &json!({"n": 1})).unwrap();
        s.step(a, "confirmed", &json!(null)).unwrap();
        assert!(s.finish(b).unwrap());
    }
    let s = Store::open(path(&dir)).unwrap();
    let open = s.open_intents().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, a);
    assert_eq!(open[0].kind, "prompt");
    let steps: Vec<_> = open[0].steps.iter().map(|s| s.step.as_str()).collect();
    assert_eq!(steps, ["sent", "confirmed"]);
    assert!(s.finish(a).unwrap());
    assert!(!s.finish(a).unwrap());
    assert!(s.open_intents().unwrap().is_empty());
}

#[test]
fn prune_boundaries() {
    let s = Store::memory().unwrap();
    let now = DEFAULT_RETENTION_MS + 1000;
    let cutoff = now - DEFAULT_RETENTION_MS;
    s.append(1, None, cutoff - 1, &json!(1)).unwrap();
    s.append(2, None, cutoff, &json!(2)).unwrap();
    s.append(3, None, now, &json!(3)).unwrap();
    s.put_frame("b", 0, cutoff - 1, "v", &json!(0)).unwrap();
    s.put_frame("b", 1, cutoff, "v", &json!(1)).unwrap();
    let r = s.prune(now, DEFAULT_RETENTION_MS).unwrap();
    assert_eq!(
        r,
        PruneReport {
            updates: 1,
            frames: 1
        }
    );
    assert_eq!(s.oldest_seq().unwrap(), Some(2));
    assert_eq!(s.frames("b", 0, 10).unwrap()[0].idx, 1);
    assert_eq!(
        s.prune(now, DEFAULT_RETENTION_MS).unwrap(),
        PruneReport::default()
    );
}

#[test]
fn consolidation_keeps_seq_order() {
    let s = Store::memory().unwrap();
    s.append_item(1, "s", "i1", 1, &json!({"append": {"text": "a"}}))
        .unwrap();
    s.append(2, Some("s"), 2, &json!({"other": true})).unwrap();
    s.append_item(3, "s", "i1", 3, &json!({"append": {"text": "b"}}))
        .unwrap();
    s.append_item(4, "s", "i2", 4, &json!({"append": {"text": "z"}}))
        .unwrap();
    s.append_item(5, "s", "i1", 5, &json!({"streaming": false}))
        .unwrap();

    let kept = s
        .consolidate("s", "i1", |rows| {
            let text: String = rows
                .iter()
                .filter_map(|r| r.update["append"]["text"].as_str())
                .collect();
            json!({"set": {"text": text}})
        })
        .unwrap();
    assert_eq!(kept, Some(5));
    let rows = s.read_session("s", 0, 10).unwrap();
    let seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [2, 4, 5]);
    assert_eq!(rows[2].update, json!({"set": {"text": "ab"}}));
    assert_eq!(rows[2].item.as_deref(), Some("i1"));
    assert_eq!(s.consolidate("s", "none", |_| json!(null)).unwrap(), None);
}

#[test]
fn projection_list_pages() {
    let s = Store::memory().unwrap();
    for id in ["c", "a", "b"] {
        s.put("item", id, &json!({"id": id})).unwrap();
    }
    s.put("queue", "q", &json!([])).unwrap();
    s.put("item", "a", &json!({"id": "a", "v": 2})).unwrap();
    let page = s.list("item", None, 2).unwrap();
    assert_eq!(page.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(page[0].1["v"], 2);
    let next = s.list("item", Some("b"), 2).unwrap();
    assert_eq!(next.len(), 1);
    assert!(s.delete("item", "c").unwrap());
    assert_eq!(s.get("item", "c").unwrap(), None);
}

#[test]
fn transaction_is_atomic() {
    let mut s = Store::memory().unwrap();
    let r: Result<(), Error> = s.transaction(|tx| {
        tx.append(1, None, 1, &json!(1))?;
        tx.append(1, None, 1, &json!(1))?; // duplicate seq fails
        Ok(())
    });
    assert!(r.is_err());
    assert_eq!(s.head().unwrap(), None);
    s.transaction(|tx| {
        tx.append(1, None, 1, &json!(1))?;
        tx.put("token", "h", &json!({}))
    })
    .unwrap();
    assert_eq!(s.head().unwrap(), Some(1));
}
