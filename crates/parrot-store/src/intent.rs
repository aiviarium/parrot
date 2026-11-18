//! The intent log: each intent is written before the runtime acts on it, and is still open after a
//! crash until it is finished.

use rusqlite::{Connection, params};
use serde_json::Value;

use crate::db::{Result, json_col, to_i64, to_u64};

/// An intent that has not finished.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    pub id: u64,
    pub kind: String,
    pub payload: Value,
    pub steps: Vec<IntentStep>,
}

/// A step recorded against an intent, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentStep {
    pub step: String,
    pub payload: Value,
}

pub(crate) fn begin(c: &Connection, kind: &str, payload: &Value) -> Result<u64> {
    c.execute(
        "INSERT INTO intents (kind, payload) VALUES (?1, ?2)",
        params![kind, serde_json::to_string(payload)?],
    )?;
    Ok(to_u64(c.last_insert_rowid()))
}

pub(crate) fn step(c: &Connection, id: u64, step: &str, payload: &Value) -> Result<()> {
    c.execute(
        "INSERT INTO intent_steps (intent, n, step, payload)
         VALUES (?1, (SELECT COALESCE(MAX(n), 0) + 1 FROM intent_steps WHERE intent = ?1), ?2, ?3)",
        params![to_i64(id)?, step, serde_json::to_string(payload)?],
    )?;
    Ok(())
}

pub(crate) fn finish(c: &Connection, id: u64) -> Result<bool> {
    Ok(c.execute("DELETE FROM intents WHERE id = ?1", [to_i64(id)?])? > 0)
}

pub(crate) fn open_intents(c: &Connection) -> Result<Vec<Intent>> {
    let mut st = c.prepare_cached("SELECT id, kind, payload FROM intents ORDER BY id")?;
    let mut intents: Vec<Intent> = st
        .query_map([], |r| {
            Ok(Intent {
                id: to_u64(r.get(0)?),
                kind: r.get(1)?,
                payload: json_col(r, 2)?,
                steps: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut st = c.prepare_cached("SELECT step, payload FROM intent_steps WHERE intent = ?1 ORDER BY n")?;
    for intent in &mut intents {
        intent.steps = st
            .query_map([to_i64(intent.id)?], |r| {
                Ok(IntentStep {
                    step: r.get(0)?,
                    payload: json_col(r, 1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(intents)
}
