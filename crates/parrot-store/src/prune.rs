//! Retention: updates and frames older than a window are dropped.

use rusqlite::{Connection, params};

use crate::db::{Result, to_i64};

/// The default retention window, 30 days.
pub const DEFAULT_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// What a prune removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub updates: usize,
    pub frames: usize,
}

/// Removes rows with `at_ms < now_ms - retention_ms`; a row exactly at the cutoff is kept.
pub(crate) fn prune(c: &Connection, now_ms: u64, retention_ms: u64) -> Result<PruneReport> {
    let cutoff = to_i64(now_ms.saturating_sub(retention_ms))?;
    Ok(PruneReport {
        updates: c.execute("DELETE FROM updates WHERE at_ms < ?1", params![cutoff])?,
        frames: c.execute("DELETE FROM frames WHERE at_ms < ?1", params![cutoff])?,
    })
}
