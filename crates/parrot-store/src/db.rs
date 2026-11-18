//! The `Store` handle, its error type and atomic batches.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use serde_json::Value;

use crate::intent::{self, Intent};
use crate::log::{self, FrameRow, LogRow};
use crate::prune::{self, PruneReport};
use crate::{migrations, projection};

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "database schema version {found} is newer than this runtime supports ({supported}); upgrade the runtime"
    )]
    NewerSchema { found: u32, supported: u32 },
    #[error("value out of range: {0}")]
    Range(u64),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The runtime's database.
pub struct Store {
    conn: Connection,
}

/// An open transaction; it has every store operation through [`Ops`].
pub struct Tx<'a> {
    tx: rusqlite::Transaction<'a>,
}

/// Store operations, shared by [`Store`] and [`Tx`].
pub trait Ops {
    #[doc(hidden)]
    fn conn(&self) -> &Connection;

    /// Appends an update at `seq`.
    fn append(&self, seq: u64, session: Option<&str>, at_ms: u64, update: &Value) -> Result<()> {
        log::append(self.conn(), seq, session, None, at_ms, update)
    }

    /// Appends an update that patches `item`, so [`Ops::consolidate`] can find it.
    fn append_item(&self, seq: u64, session: &str, item: &str, at_ms: u64, update: &Value) -> Result<()> {
        log::append(self.conn(), seq, Some(session), Some(item), at_ms, update)
    }

    /// Up to `limit` updates with `seq >= from`, in order.
    fn read_from(&self, from: u64, limit: usize) -> Result<Vec<LogRow>> {
        log::read_from(self.conn(), from, limit)
    }

    /// Up to `limit` updates of `session` with `seq >= from_seq`, in order.
    fn read_session(&self, session: &str, from_seq: u64, limit: usize) -> Result<Vec<LogRow>> {
        log::read_session(self.conn(), session, from_seq, limit)
    }

    /// The highest stored `seq`.
    fn head(&self) -> Result<Option<u64>> {
        log::head(self.conn())
    }

    /// The lowest stored `seq`; a replay from before it is unavailable.
    fn oldest_seq(&self) -> Result<Option<u64>> {
        log::oldest_seq(self.conn())
    }

    /// Replaces the item's rows with the one `merge` builds from them, at the last row's `seq`.
    /// Returns that `seq`, or `None` when the item has no rows.
    fn consolidate(
        &self,
        session: &str,
        item: &str,
        merge: impl FnOnce(&[LogRow]) -> Value,
    ) -> Result<Option<u64>>
    where
        Self: Sized,
    {
        log::consolidate(self.conn(), session, item, merge)
    }

    /// Stores raw frame `idx` of `binding`, replacing one with the same index.
    fn put_frame(
        &self,
        binding: &str,
        idx: u64,
        at_ms: u64,
        data_version: &str,
        frame: &Value,
    ) -> Result<()> {
        log::put_frame(self.conn(), binding, idx, at_ms, data_version, frame)
    }

    /// Up to `limit` frames of `binding` with index `>= from`, in order.
    fn frames(&self, binding: &str, from: u64, limit: usize) -> Result<Vec<FrameRow>> {
        log::frames(self.conn(), binding, from, limit)
    }

    /// Inserts or replaces a projection.
    fn put(&self, kind: &str, id: &str, value: &Value) -> Result<()> {
        projection::put(self.conn(), kind, id, value)
    }

    fn get(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        projection::get(self.conn(), kind, id)
    }

    /// Returns whether a row was removed.
    fn delete(&self, kind: &str, id: &str) -> Result<bool> {
        projection::delete(self.conn(), kind, id)
    }

    /// Up to `limit` projections of `kind` with ids after `cursor`, ordered by id.
    fn list(&self, kind: &str, cursor: Option<&str>, limit: usize) -> Result<Vec<(String, Value)>> {
        projection::list(self.conn(), kind, cursor, limit)
    }

    /// Records an intent before acting on it and returns its id.
    fn begin(&self, kind: &str, payload: &Value) -> Result<u64> {
        intent::begin(self.conn(), kind, payload)
    }

    /// Records a completed step of intent `id`.
    fn step(&self, id: u64, step: &str, payload: &Value) -> Result<()> {
        intent::step(self.conn(), id, step, payload)
    }

    /// Closes intent `id` and drops its steps; returns whether it was open.
    fn finish(&self, id: u64) -> Result<bool> {
        intent::finish(self.conn(), id)
    }

    /// Intents not yet finished, oldest first, to continue or roll back after a crash.
    fn open_intents(&self) -> Result<Vec<Intent>> {
        intent::open_intents(self.conn())
    }

    /// Drops updates and frames older than `retention_ms` before `now_ms`.
    fn prune(&self, now_ms: u64, retention_ms: u64) -> Result<PruneReport> {
        prune::prune(self.conn(), now_ms, retention_ms)
    }
}

impl Ops for Store {
    fn conn(&self) -> &Connection {
        &self.conn
    }
}

impl Ops for Tx<'_> {
    fn conn(&self) -> &Connection {
        &self.tx
    }
}

impl Store {
    /// Opens or creates the database at `path`, creating its directory with mode `0700`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_state_dir(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        Self::init(conn)
    }

    /// An in-memory database, for tests.
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        migrations::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Runs `f` in a transaction, committing when it returns `Ok`.
    pub fn transaction<T, E>(
        &mut self,
        f: impl FnOnce(&Tx<'_>) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<Error>,
    {
        let tx = Tx {
            tx: self.conn.transaction().map_err(Error::from)?,
        };
        let out = f(&tx)?;
        tx.tx.commit().map_err(Error::from)?;
        Ok(out)
    }

    /// The schema version of the open database.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::current_version(&self.conn)
    }
}

#[cfg(unix)]
fn create_state_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_state_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    Ok(())
}

pub(crate) fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| Error::Range(v))
}

pub(crate) fn to_u64(v: i64) -> u64 {
    v.max(0) as u64
}

pub(crate) fn json_col(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<serde_json::Value> {
    let s: String = row.get(idx)?;
    serde_json::from_str(&s)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e)))
}
