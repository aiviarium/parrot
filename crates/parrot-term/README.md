# parrot-term

Terminal sessions for programs that you drive from code. parrot-term runs a command on a PTY or reads
any byte stream. It emulates the screen and hands out settled snapshots. It encodes keys and
mouse input for the modes the program asked for, and records and replays asciinema v2 casts.

| Part | What it does |
| --- | --- |
| `Emulator` | Wraps `alacritty_terminal`, with its own scrollback by absolute line number, so a burst of output doesn't lose lines. It handles synchronized updates (mode 2026) itself |
| `Screen` | An immutable snapshot with cells of grapheme clusters and styles, the cursor, modes, title and scrollback. `Screen::diff` lists the rows that changed |
| `Settler` | Decides when a snapshot is due. That is after a pause in output, at the end of a synchronized update, or after a maximum delay |
| `encode` | Turns an `Input` (key, text, paste, mouse, focus) into bytes for the current modes, including the kitty keyboard protocol |
| `pty` | Runs a command on a local PTY |
| `Session` | Ties a byte source and sink to the emulator: raw output from an offset, settled frames, input, waits and recording |
| `serialize` | Writes a screen as escape sequences that draw it again on a fresh terminal |
| `snapshot` | Renders a screen as text you can diff, and parses it back |
| `Cast`, `CastWriter`, `Replay` | Write, read and replay asciinema v2 casts, with marker events |

## Example

```rust
use std::time::Duration;
use parrot_term::{PtyCommand, Session, SessionConfig, Size};

let cmd = PtyCommand::new("sh").args(["-c", "read x; echo \"got:$x\""]);
// Size::new takes rows, then columns.
let (session, pty) = Session::spawn(&cmd, SessionConfig::with_size(Size::new(24, 80)))?;
session.type_text("hi").await?;
session.press("enter").await?;
let screen = session.wait_for_text("got:hi", Duration::from_secs(5)).await?;
pty.wait().await;
```

The tests in `tests/term/` show the rest of the API on recorded casts.

## Fixtures and examples

`fixtures/casts/` has casts of a shell, vim, less, top, nano, Claude Code and Codex. The emulator's
regression tests replay them. The same casts, with their recording notes, are in
`crates/parrot-tui/fixtures/casts/`.

| Example | Use |
| --- | --- |
| `record_casts` | Records the casts in `fixtures/casts/` from scripted scenarios. `--list` lists them |
| `explore` | Drives a program from a small script and prints its screens, for working out what a TUI shows |
