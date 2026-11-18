//! Numbered schema migrations, tracked in `schema_version`.

use rusqlite::{Connection, OptionalExtension};

use crate::db::{Error, Result};

/// Migrations in order; migration `n` (1-based) brings the schema to version `n`.
const MIGRATIONS: &[&str] = &[
    // 1: update log, projections, intent log, frames.
    r#"
    CREATE TABLE updates (
        seq     INTEGER PRIMARY KEY,
        session TEXT,
        item    TEXT,
        at_ms   INTEGER NOT NULL,
        body    TEXT NOT NULL
    );
    CREATE INDEX updates_session ON updates (session, seq);
    CREATE INDEX updates_item ON updates (session, item, seq) WHERE item IS NOT NULL;
    CREATE INDEX updates_at ON updates (at_ms);

    CREATE TABLE projections (
        kind  TEXT NOT NULL,
        id    TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (kind, id)
    ) WITHOUT ROWID;

    CREATE TABLE intents (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        kind    TEXT NOT NULL,
        payload TEXT NOT NULL
    );
    CREATE TABLE intent_steps (
        intent  INTEGER NOT NULL REFERENCES intents (id) ON DELETE CASCADE,
        n       INTEGER NOT NULL,
        step    TEXT NOT NULL,
        payload TEXT NOT NULL,
        PRIMARY KEY (intent, n)
    ) WITHOUT ROWID;

    CREATE TABLE frames (
        binding      TEXT NOT NULL,
        idx          INTEGER NOT NULL,
        at_ms        INTEGER NOT NULL,
        data_version TEXT NOT NULL,
        frame        TEXT NOT NULL,
        PRIMARY KEY (binding, idx)
    ) WITHOUT ROWID;
    CREATE INDEX frames_at ON frames (at_ms);
    "#,
];

/// The schema version this build writes.
pub const LATEST: u32 = MIGRATIONS.len() as u32;

/// The recorded schema version, 0 for a new database.
pub fn current_version(conn: &Connection) -> Result<u32> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    let v: Option<u32> = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .optional()?;
    Ok(v.unwrap_or(0))
}

/// Applies pending migrations in one transaction; refuses a database newer than [`LATEST`].
pub fn migrate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")?;
    let found = current_version(&tx)?;
    if found > LATEST {
        return Err(Error::NewerSchema {
            found,
            supported: LATEST,
        });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        tx.execute_batch(sql)?;
        set_version(&tx, i as u32 + 1)?;
    }
    tx.commit()?;
    Ok(())
}

fn set_version(conn: &Connection, v: u32) -> Result<()> {
    conn.execute("DELETE FROM schema_version", [])?;
    conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [v])?;
    Ok(())
}
