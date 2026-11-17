//! Terminal sessions: emulator, screen model, input encoding, serialisation for viewers,
//! output buffer, recording and replay.
//!
//! - [`Emulator`] wraps `alacritty_terminal`. Feed it output with [`Emulator::feed`], take an
//!   immutable [`Screen`] with [`Emulator::screen`]. It keeps its own scrollback ([`History`],
//!   10 000 lines by default) with absolute line numbers, so no line is lost however much
//!   arrives at once, and handles synchronized updates (mode 2026) itself.
//! - [`Screen`] has rows of cells holding grapheme clusters and [`Style`]s, the [`Cursor`],
//!   [`Modes`], title, size and scrollback. Clones share everything; [`Screen::diff`] lists
//!   changed rows.
//! - [`Settler`] decides when a snapshot is due: quiet output, the end of a synchronized update,
//!   or a maximum delay.
//! - [`serialize()`] turns a screen into escape sequences that reproduce it on a fresh terminal.
//! - [`OutputBuffer`] keeps the last megabytes of raw output by absolute offset.
//! - [`encode()`] turns an [`Input`] (key, text, paste, mouse, focus) into bytes for the current
//!   modes, including the kitty keyboard protocol.
//! - [`pty`] runs a command on a local PTY; [`io`] has the [`ByteSource`] and [`ByteSink`]
//!   traits any byte stream can implement.
//! - [`Session`] ties a source and sink to the emulator, buffer and settler: raw bytes from an
//!   offset, attach snapshots, settled [`Frame`]s, input, waits, recording.
//! - [`snapshot`] renders a screen as diffable text and parses it back.
//! - [`Cast`], [`CastWriter`] and [`Replay`] write, read and replay asciinema v2 casts.

pub mod buffer;
pub mod cast;
pub mod emulator;
pub mod input;
pub mod io;
pub mod pty;
pub mod queries;
mod scan;
pub mod screen;
pub mod serialize;
pub mod session;
pub mod settle;
pub mod snapshot;

pub use buffer::{OffsetError, OutputBuffer};
pub use cast::{Cast, CastEvent, CastHeader, CastWriter, EventKind, Frame, Replay, ReplayConfig};
pub use emulator::{Emulator, EmulatorConfig, Fed, Palette};
pub use input::{Input, Key, Mods, Mouse, MouseButton, MouseKind, encode, parse_key};
pub use io::{ByteSink, ByteSource};
pub use pty::{ExitStatus, Pty, PtyCommand};
pub use queries::ResponseFilter;
pub use screen::*;
pub use serialize::{SerializeOptions, serialize, serialize_with};
pub use session::{
    Attach, ByteSubscription, Chunk, FrameStream, Session, SessionConfig, TextPattern, WaitError,
};
pub use settle::{SettleConfig, SettleReason, Settler};
