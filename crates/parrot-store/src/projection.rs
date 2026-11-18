//! Key-value projections keyed by `(kind, id)`: sessions, bindings, items, queues, requests and
//! tokens (hashed by the caller).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::db::{Result, json_col};

pub(crate) fn put(c: &Connection, kind: &str, id: &str, value: &Value) -> Result<()> {
    c.execute(
        "INSERT INTO projections (kind, id, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (kind, id) DO UPDATE SET value = excluded.value",
        params![kind, id, serde_json::to_string(value)?],
    )?;
    Ok(())
}

pub(crate) fn get(c: &Connection, kind: &str, id: &str) -> Result<Option<Value>> {
    Ok(c.query_row(
        "SELECT value FROM projections WHERE kind = ?1 AND id = ?2",
        params![kind, id],
        |r| json_col(r, 0),
    )
    .optional()?)
}

pub(crate) fn delete(c: &Connection, kind: &str, id: &str) -> Result<bool> {
    Ok(c.execute(
        "DELETE FROM projections WHERE kind = ?1 AND id = ?2",
        params![kind, id],
    )? > 0)
}

pub(crate) fn list(
    c: &Connection,
    kind: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, Value)>> {
    let mut st = c.prepare_cached(
        "SELECT id, value FROM projections WHERE kind = ?1 AND (?2 IS NULL OR id > ?2)
         ORDER BY id LIMIT ?3",
    )?;
    let rows = st.query_map(params![kind, cursor, limit as i64], |r| {
        Ok((r.get(0)?, json_col(r, 1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
