//! SQLite storage for the Parrot runtime: update log, projections, intent log and migrations.
//!
//! Rows are opaque JSON values; the runtime owns their shape. The API is synchronous, so the
//! runtime calls it from a blocking task.

pub mod db;
pub mod intent;
pub mod log;
pub mod migrations;
pub mod projection;
pub mod prune;

pub use db::{Error, Ops, Result, Store, Tx};
pub use intent::{Intent, IntentStep};
pub use log::{FrameRow, LogRow};
pub use prune::{DEFAULT_RETENTION_MS, PruneReport};
