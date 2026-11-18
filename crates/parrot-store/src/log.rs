//! The update log and raw driver frames.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::db::{Result, json_col, to_i64, to_u64};

/// One stored update.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRow {
    pub seq: u64,
    pub session: Option<String>,
    pub item: Option<String>,
    pub at_ms: u64,
    pub update: Value,
}

/// One raw driver frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRow {
    pub binding: String,
    pub idx: u64,
    pub at_ms: u64,
    pub data_version: String,
    pub frame: Value,
}

const COLS: &str = "seq, session, item, at_ms, body";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LogRow> {
    Ok(LogRow {
        seq: to_u64(r.get(0)?),
        session: r.get(1)?,
        item: r.get(2)?,
        at_ms: to_u64(r.get(3)?),
        update: json_col(r, 4)?,
    })
}

pub(crate) fn append(
    c: &Connection,
    seq: u64,
    session: Option<&str>,
    item: Option<&str>,
    at_ms: u64,
    update: &Value,
) -> Result<()> {
    c.execute(
        "INSERT INTO updates (seq, session, item, at_ms, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            to_i64(seq)?,
            session,
            item,
            to_i64(at_ms)?,
            serde_json::to_string(update)?
        ],
    )?;
    Ok(())
}

pub(crate) fn read_from(c: &Connection, seq: u64, limit: usize) -> Result<Vec<LogRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM updates WHERE seq >= ?1 ORDER BY seq LIMIT ?2"
    ))?;
    let rows = st.query_map(params![to_i64(seq)?, limit as i64], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn read_session(
    c: &Connection,
    session: &str,
    from_seq: u64,
    limit: usize,
) -> Result<Vec<LogRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM updates WHERE session = ?1 AND seq >= ?2 ORDER BY seq LIMIT ?3"
    ))?;
    let rows = st.query_map(params![session, to_i64(from_seq)?, limit as i64], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn scalar(c: &Connection, sql: &str) -> Result<Option<u64>> {
    let v: Option<i64> = c.query_row(sql, [], |r| r.get(0)).optional()?.flatten();
    Ok(v.map(to_u64))
}

pub(crate) fn head(c: &Connection) -> Result<Option<u64>> {
    scalar(c, "SELECT MAX(seq) FROM updates")
}

pub(crate) fn oldest_seq(c: &Connection) -> Result<Option<u64>> {
    scalar(c, "SELECT MIN(seq) FROM updates")
}

pub(crate) fn consolidate(
    c: &Connection,
    session: &str,
    item: &str,
    merge: impl FnOnce(&[LogRow]) -> Value,
) -> Result<Option<u64>> {
    let rows: Vec<LogRow> = {
        let mut st = c.prepare_cached(&format!(
            "SELECT {COLS} FROM updates WHERE session = ?1 AND item = ?2 ORDER BY seq"
        ))?;
        st.query_map(params![session, item], row)?
            .collect::<rusqlite::Result<_>>()?
    };
    let Some(last) = rows.last() else {
        return Ok(None);
    };
    let (seq, at_ms) = (last.seq, last.at_ms);
    let merged = merge(&rows);
    c.execute(
        "DELETE FROM updates WHERE session = ?1 AND item = ?2",
        params![session, item],
    )?;
    append(c, seq, Some(session), Some(item), at_ms, &merged)?;
    Ok(Some(seq))
}

pub(crate) fn put_frame(
    c: &Connection,
    binding: &str,
    idx: u64,
    at_ms: u64,
    data_version: &str,
    frame: &Value,
) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO frames (binding, idx, at_ms, data_version, frame) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![binding, to_i64(idx)?, to_i64(at_ms)?, data_version, serde_json::to_string(frame)?],
    )?;
    Ok(())
}

pub(crate) fn frames(c: &Connection, binding: &str, from: u64, limit: usize) -> Result<Vec<FrameRow>> {
    let mut st = c.prepare_cached(
        "SELECT binding, idx, at_ms, data_version, frame FROM frames
         WHERE binding = ?1 AND idx >= ?2 ORDER BY idx LIMIT ?3",
    )?;
    let rows = st.query_map(params![binding, to_i64(from)?, limit as i64], |r| {
        Ok(FrameRow {
            binding: r.get(0)?,
            idx: to_u64(r.get(1)?),
            at_ms: to_u64(r.get(2)?),
            data_version: r.get(3)?,
            frame: json_col(r, 4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
