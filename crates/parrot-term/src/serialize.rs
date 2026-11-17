//! Serialising a screen to escape sequences.
//!
//! [`serialize`] turns a [`Screen`] and its scrollback into bytes that, written to a fresh
//! terminal of the same size (xterm.js or any VT emulator), reproduce the scrollback, the visible
//! rows with their styles, the primary screen behind an active alternate screen, the cursor, the
//! scrolling region and the modes. A viewer that attaches gets this, then the live output from
//! [`Screen::offset`] on.
//!
//! Not reproduced: tab stops, character set designations (line drawing is already translated in
//! the cells), the palette set with OSC 4, hyperlinks, and the title stack. The receiving
//! terminal needs at least as much scrollback as the history serialised.

use std::io::Write;

use crate::screen::Attrs;
use crate::screen::{CellKind, Color, CursorShape, MouseEncoding, MouseMode, Row, Screen, Style};

#[derive(Clone, Debug)]
pub struct SerializeOptions {
    /// Include the scrollback. Default on.
    pub history: bool,
    /// Include at most this many of the newest scrollback lines.
    pub max_history: Option<usize>,
    /// Start with a full reset (`ESC c`), for a terminal that is not fresh. Default off.
    pub reset: bool,
}

impl Default for SerializeOptions {
    fn default() -> Self {
        Self {
            history: true,
            max_history: None,
            reset: false,
        }
    }
}

/// Serialises with the default options.
pub fn serialize(screen: &Screen) -> Vec<u8> {
    serialize_with(screen, &SerializeOptions::default())
}

pub fn serialize_with(screen: &Screen, opts: &SerializeOptions) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    let size = screen.size();
    if opts.reset {
        out.extend_from_slice(b"\x1bc");
    }
    let mut pen = Style::default();

    // Scrollback and the primary screen, written as lines so the receiving terminal scrolls the
    // scrollback into its own history.
    let history: Vec<&Row> = if opts.history {
        let h = screen.history();
        let skip = opts.max_history.map_or(0, |m| h.len().saturating_sub(m));
        h.iter().skip(skip).collect()
    } else {
        Vec::new()
    };
    let saved = screen.saved_primary();
    let primary: Vec<&Row> = match saved {
        Some(saved) => saved.rows.iter().collect(),
        None if screen.modes().alt_screen => Vec::new(),
        None => screen.rows().iter().collect(),
    };
    let lines: Vec<&Row> = history.into_iter().chain(primary).collect();
    for (i, row) in lines.iter().enumerate() {
        let next = lines.get(i + 1);
        let continues = row.is_wrapped() && next.is_some_and(|n| n.content_len() > 0);
        write_row(&mut out, row, &mut pen, continues, next.copied());
        if !continues && next.is_some() {
            out.extend_from_slice(b"\r\n");
        }
    }

    if screen.modes().alt_screen {
        if let Some(saved) = saved {
            cup(&mut out, saved.cursor.row, saved.cursor.col);
            if !saved.cursor.visible {
                // Visibility is one mode for both screens; this only makes the saved primary
                // screen match. The current state is set below.
                out.extend_from_slice(b"\x1b[?25l");
            }
        }
        set_pen(&mut out, &mut pen, Style::default());
        out.extend_from_slice(b"\x1b[?1049h");
        for (r, row) in screen.rows().iter().enumerate() {
            if row.content_len() == 0 {
                continue;
            }
            cup(&mut out, r as u16, 0);
            write_row(&mut out, row, &mut pen, false, None);
        }
    }

    out.extend_from_slice(b"\x1b[0m");
    pen = Style::default();

    if let Some((top, bottom)) = screen.scroll_region() {
        let _ = write!(out, "\x1b[{};{}r", top + 1, bottom + 1);
    }
    if let Some(sc) = screen.saved_cursor() {
        cup(&mut out, sc.row, sc.col);
        set_pen(&mut out, &mut pen, sc.pen);
        out.extend_from_slice(b"\x1b7");
    }

    let modes = screen.modes();
    let cursor = screen.cursor();
    let region_top = if modes.origin {
        screen.scroll_region().map_or(0, |r| r.0)
    } else {
        0
    };
    if modes.origin {
        out.extend_from_slice(b"\x1b[?6h");
    }
    let row = cursor.row.saturating_sub(region_top);
    if cursor.pending_wrap && cursor.col + 1 == size.cols {
        // Rewrite the last cell to put the terminal in the "next character wraps" state.
        let line = screen.row(cursor.row);
        let mut col = size.cols - 1;
        if line.is_some_and(|l| l.cell(col).kind() == CellKind::WideTail) {
            col -= 1;
        }
        cup(&mut out, row, col);
        match line {
            Some(l) => {
                let cell = l.cell(col);
                set_pen(&mut out, &mut pen, cell.style());
                let text = cell.text();
                out.extend_from_slice(if text.is_empty() { b" " } else { text.as_bytes() });
            }
            None => out.push(b' '),
        }
    } else {
        cup(&mut out, row, cursor.col);
    }
    set_pen(&mut out, &mut pen, screen.pen());

    if modes.app_cursor {
        out.extend_from_slice(b"\x1b[?1h");
    }
    if modes.app_keypad {
        out.extend_from_slice(b"\x1b=");
    }
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[?2004h");
    }
    match modes.mouse {
        MouseMode::None => {}
        MouseMode::Click => out.extend_from_slice(b"\x1b[?1000h"),
        MouseMode::Drag => out.extend_from_slice(b"\x1b[?1002h"),
        MouseMode::Motion => out.extend_from_slice(b"\x1b[?1003h"),
    }
    match modes.mouse_encoding {
        MouseEncoding::Default => {}
        MouseEncoding::Utf8 => out.extend_from_slice(b"\x1b[?1005h"),
        MouseEncoding::Sgr => out.extend_from_slice(b"\x1b[?1006h"),
    }
    if modes.focus_events {
        out.extend_from_slice(b"\x1b[?1004h");
    }
    if !modes.line_wrap {
        out.extend_from_slice(b"\x1b[?7l");
    }
    if modes.insert {
        out.extend_from_slice(b"\x1b[4h");
    }
    if modes.newline {
        out.extend_from_slice(b"\x1b[20h");
    }
    if !modes.alternate_scroll {
        out.extend_from_slice(b"\x1b[?1007l");
    }
    if !cursor.visible {
        out.extend_from_slice(b"\x1b[?25l");
    } else if saved.is_some_and(|s| !s.cursor.visible) {
        out.extend_from_slice(b"\x1b[?25h");
    }
    if cursor.shape != CursorShape::Block || cursor.blinking {
        let n = match (cursor.shape, cursor.blinking) {
            (CursorShape::Block | CursorShape::HollowBlock, true) => 1,
            (CursorShape::Block | CursorShape::HollowBlock, false) => 2,
            (CursorShape::Underline, true) => 3,
            (CursorShape::Underline, false) => 4,
            (CursorShape::Bar, true) => 5,
            (CursorShape::Bar, false) => 6,
        };
        let _ = write!(out, "\x1b[{n} q");
    }
    if !modes.kitty.is_empty() {
        let _ = write!(out, "\x1b[>{}u", modes.kitty.bits());
    }
    if let Some(title) = screen.title() {
        out.extend_from_slice(b"\x1b]2;");
        out.extend(
            title
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .bytes(),
        );
        out.push(0x07);
    }
    out
}

/// Writes one row's cells from the cursor's current position. With `continues`, the row is
/// written to its full width so the next character wraps, as it did in the original.
fn write_row(out: &mut Vec<u8>, row: &Row, pen: &mut Style, continues: bool, next: Option<&Row>) {
    let cols = row.cols();
    let limit = if continues { cols } else { row.content_len() };
    for col in 0..limit {
        let cell = row.cell(col);
        let text = match cell.kind() {
            CellKind::WideTail => continue,
            CellKind::WrapSpacer => {
                let next_is_wide = next.is_some_and(|n| n.cell(0).kind() == CellKind::Wide);
                if continues && col + 1 == cols && next_is_wide {
                    // The terminal leaves this spacer itself when the wide character wraps.
                    break;
                }
                " "
            }
            _ => cell.text(),
        };
        set_pen(out, pen, cell.style());
        out.extend_from_slice(if text.is_empty() { b" " } else { text.as_bytes() });
    }
}

fn cup(out: &mut Vec<u8>, row: u16, col: u16) {
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
}

fn set_pen(out: &mut Vec<u8>, pen: &mut Style, style: Style) {
    if *pen != style {
        out.extend_from_slice(&sgr(&style));
        *pen = style;
    }
}

/// The SGR sequence that sets exactly `style`, starting from a reset.
pub fn sgr(style: &Style) -> Vec<u8> {
    let mut s = String::from("\x1b[0");
    let a = style.attrs;
    if a.contains(Attrs::BOLD) {
        s.push_str(";1");
    }
    if a.contains(Attrs::DIM) {
        s.push_str(";2");
    }
    if a.contains(Attrs::ITALIC) {
        s.push_str(";3");
    }
    if a.contains(Attrs::UNDERLINE) {
        s.push_str(";4");
    }
    if a.contains(Attrs::DOUBLE_UNDERLINE) {
        s.push_str(";4:2");
    }
    if a.contains(Attrs::UNDERCURL) {
        s.push_str(";4:3");
    }
    if a.contains(Attrs::DOTTED_UNDERLINE) {
        s.push_str(";4:4");
    }
    if a.contains(Attrs::DASHED_UNDERLINE) {
        s.push_str(";4:5");
    }
    if a.contains(Attrs::REVERSE) {
        s.push_str(";7");
    }
    if a.contains(Attrs::HIDDEN) {
        s.push_str(";8");
    }
    if a.contains(Attrs::STRIKETHROUGH) {
        s.push_str(";9");
    }
    color(&mut s, style.fg, 30, 90, 38);
    color(&mut s, style.bg, 40, 100, 48);
    if let Some(u) = style.underline_color {
        match u {
            Color::Default => {}
            Color::Indexed(i) => s.push_str(&format!(";58;5;{i}")),
            Color::Rgb(r, g, b) => s.push_str(&format!(";58;2;{r};{g};{b}")),
        }
    }
    s.push('m');
    s.into_bytes()
}

fn color(s: &mut String, c: Color, base: u8, bright: u8, ext: u8) {
    match c {
        Color::Default => {}
        Color::Indexed(i) if i < 8 => s.push_str(&format!(";{}", base + i)),
        Color::Indexed(i) if i < 16 => s.push_str(&format!(";{}", bright + i - 8)),
        Color::Indexed(i) => s.push_str(&format!(";{ext};5;{i}")),
        Color::Rgb(r, g, b) => s.push_str(&format!(";{ext};2;{r};{g};{b}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Emulator, EmulatorConfig, Size};

    fn emu(size: Size) -> Emulator {
        Emulator::new(EmulatorConfig::with_size(size))
    }

    /// Feeds `input` to one emulator, serialises its screen into a fresh one, and checks they
    /// agree.
    fn round_trip(size: Size, input: &[u8]) -> (Screen, Screen) {
        let mut a = emu(size);
        a.feed(input);
        let sa = a.screen();
        let bytes = serialize(&sa);
        let mut b = emu(size);
        b.feed(&bytes);
        let sb = b.screen();
        assert_same(&sa, &sb, &bytes);
        (sa, sb)
    }

    fn assert_same(a: &Screen, b: &Screen, bytes: &[u8]) {
        let ctx = || String::from_utf8_lossy(bytes).replace('\x1b', "␛");
        assert_eq!(a.rows(), b.rows(), "rows differ; serialised: {}", ctx());
        assert_eq!(a.cursor(), b.cursor(), "cursor; serialised: {}", ctx());
        assert_eq!(a.modes(), b.modes(), "modes; serialised: {}", ctx());
        assert_eq!(a.title(), b.title());
        assert_eq!(a.pen(), b.pen(), "pen");
        assert_eq!(a.scroll_region(), b.scroll_region());
        assert_eq!(a.saved_cursor(), b.saved_cursor(), "saved cursor");
        let ha: Vec<&Row> = a.history().iter().collect();
        let hb: Vec<&Row> = b.history().iter().collect();
        assert_eq!(ha, hb, "history; serialised: {}", ctx());
        assert_eq!(a.saved_primary(), b.saved_primary(), "saved primary");
    }

    #[test]
    fn styles_and_text() {
        round_trip(
            Size::new(6, 30),
            "plain \x1b[1mbold\x1b[0m \x1b[2;3mdim italic\x1b[0m\r\n\x1b[4munder\x1b[24m \x1b[7mrev\x1b[27m \x1b[9mstrike\x1b[0m \x1b[8mhid\x1b[0m\r\n\x1b[31mred\x1b[92m bright\x1b[38;5;200m idx\x1b[38;2;1;2;3m rgb\x1b[0m\r\n\x1b[44m blue bg \x1b[48;2;9;8;7m rgb bg \x1b[0m\r\n\x1b[4:3;58;5;9mcurly\x1b[0m\x1b[46m\x1b[K"
                .as_bytes(),
        );
    }

    #[test]
    fn graphemes_and_wide() {
        round_trip(
            Size::new(4, 9),
            "e\u{301}a\u{308} 界🎉\r\n❤\u{fe0f} 👍\u{1f3fd}\r\nabcdefg界x".as_bytes(),
        );
    }

    #[test]
    fn history_and_wrapped_lines() {
        let mut s = String::new();
        for i in 0..50 {
            s.push_str(&format!(
                "line {i} \x1b[3{}m{}\x1b[0m\r\n",
                i % 8,
                "x".repeat(i % 25)
            ));
        }
        s.push_str("tail");
        let (a, _) = round_trip(Size::new(5, 12), s.as_bytes());
        assert!(a.history().len() > 50);
        assert!(a.history().iter().any(|r| r.is_wrapped()));
    }

    #[test]
    fn cursor_pending_wrap_and_modes() {
        round_trip(
            Size::new(3, 5),
            b"\x1b]2;t\x07abcde\x1b[?1h\x1b=\x1b[?2004h\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[5 q\x1b[>1u\x1b[1;31m",
        );
        round_trip(
            Size::new(3, 5),
            b"\x1b[?25l\x1b[?7l\x1b[4h\x1b[20h\x1b[?1007l\x1b[2;3H",
        );
    }

    #[test]
    fn scroll_region_origin_and_saved_cursor() {
        round_trip(
            Size::new(10, 20),
            b"\x1b[3;4H\x1b[1;32m\x1b7\x1b[0mx\x1b[2;8r\x1b[?6h\x1b[3;2H",
        );
    }

    #[test]
    fn alternate_screen() {
        let mut input = Vec::new();
        for i in 0..20 {
            input.extend_from_slice(format!("shell {i}\r\n").as_bytes());
        }
        input.extend_from_slice(b"$ vim\x1b[?1049h\x1b[H\x1b[44mstatus\x1b[0m\x1b[5;1H~\x1b[2;3H");
        let size = Size::new(6, 20);
        let (_, _) = round_trip(size, &input);
        // Leaving the alternate screen gives the same primary screen in both.
        let mut a = emu(size);
        a.feed(&input);
        let mut b = emu(size);
        b.feed(&serialize(&a.screen()));
        a.feed(b"\x1b[?1049l");
        b.feed(b"\x1b[?1049l");
        assert_eq!(a.screen().rows(), b.screen().rows());
        assert_eq!(a.screen().cursor(), b.screen().cursor());
        assert_eq!(a.screen().lines()[5], "$ vim");
    }

    #[test]
    fn serialised_screen_continues_with_live_bytes() {
        // A viewer gets the snapshot, then the bytes from the snapshot's offset on.
        let size = Size::new(4, 20);
        let stream: &[u8] = b"one\r\ntwo\x1b[?2026h\x1b[2J\x1b[Hframe\x1b[?2026l\r\nthree\x1b[1";
        let rest: &[u8] = b"mbold\r\nfour";
        for split in 0..stream.len() {
            let mut a = emu(size);
            a.feed(&stream[..split]);
            let snap = a.screen();
            let mut all = stream.to_vec();
            all.extend_from_slice(rest);
            let mut b = emu(size);
            b.feed(&serialize(&snap));
            b.feed(&all[snap.offset() as usize..]);
            a.feed(&stream[split..]);
            a.feed(rest);
            assert_eq!(a.screen().rows(), b.screen().rows(), "split at {split}");
            assert_eq!(a.screen().cursor(), b.screen().cursor(), "split at {split}");
        }
    }
}
