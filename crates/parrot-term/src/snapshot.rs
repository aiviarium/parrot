//! A readable, diffable text rendering of a screen, for tests and expectations.
//!
//! ```text
//! size 40x4
//! cursor 2,0 visible block
//! title "bash"
//! modes bracketed-paste
//! 0│$ echo hi
//! 1│hi
//! 2│$
//! 3│
//! ```
//!
//! Header lines are `keyword value`. Each row is its number, `│`, and its text with trailing
//! whitespace removed. Scrollback rows, when included, come first, numbered from `-N` to `-1`.
//! With styles, a row with styled cells is followed by a line of style runs:
//!
//! ```text
//! 1│Do you want to proceed?
//!  ┆ 0..2 bold fg=2; 3..6 reverse
//! ```
//!
//! [`parse`] reads the text back. It restores text, size, cursor, title and modes; style lines
//! are skipped.

use std::fmt::Write;

use crate::screen::{Attrs, Color, CursorShape, History, Modes, Row, Screen, Size, Style};

#[derive(Clone, Copy, Debug, Default)]
pub struct SnapshotOptions {
    /// Add a line of style runs after each row with styled cells.
    pub styles: bool,
    /// Include the scrollback (all of it, or the newest `n` lines).
    pub history: Option<usize>,
}

/// The plain rendering.
pub fn to_text(screen: &Screen) -> String {
    to_text_with(screen, SnapshotOptions::default())
}

/// The rendering with style annotations.
pub fn to_styled_text(screen: &Screen) -> String {
    to_text_with(
        screen,
        SnapshotOptions {
            styles: true,
            history: None,
        },
    )
}

pub fn to_text_with(screen: &Screen, opts: SnapshotOptions) -> String {
    let mut s = String::new();
    let size = screen.size();
    let c = screen.cursor();
    let _ = writeln!(s, "size {}x{}", size.cols, size.rows);
    let shape = match c.shape {
        CursorShape::Block => "block",
        CursorShape::Underline => "underline",
        CursorShape::Bar => "bar",
        CursorShape::HollowBlock => "hollow-block",
    };
    let _ = writeln!(
        s,
        "cursor {},{} {} {}{}{}",
        c.row,
        c.col,
        if c.visible { "visible" } else { "hidden" },
        shape,
        if c.blinking { " blinking" } else { "" },
        if c.pending_wrap { " pending-wrap" } else { "" },
    );
    if let Some(t) = screen.title() {
        let _ = writeln!(s, "title {}", serde_json::to_string(t).unwrap_or_default());
    }
    let names = screen.modes().names();
    if !names.is_empty() {
        let _ = writeln!(s, "modes {}", names.join(" "));
    }
    let history: Vec<&Row> = match opts.history {
        Some(n) => {
            let h = screen.history();
            h.iter().skip(h.len().saturating_sub(n)).collect()
        }
        None => Vec::new(),
    };
    if opts.history.is_some() {
        let _ = writeln!(s, "history {} {}", history.len(), screen.first_line());
    }
    let mut width = size.rows.saturating_sub(1).to_string().len();
    if !history.is_empty() {
        width = width.max(format!("-{}", history.len()).len());
    }
    let n = history.len() as i64;
    for (i, row) in history.iter().enumerate() {
        write_row(&mut s, &format!("{}", i as i64 - n), width, row, opts.styles);
    }
    for (i, row) in screen.rows().iter().enumerate() {
        write_row(&mut s, &i.to_string(), width, row, opts.styles);
    }
    s
}

fn write_row(s: &mut String, num: &str, width: usize, row: &Row, styles: bool) {
    let _ = writeln!(s, "{num:>width$}│{}", row.text());
    if styles {
        let runs: Vec<String> = row
            .style_runs()
            .into_iter()
            .filter(|(_, _, st)| !st.is_default())
            .map(|(a, b, st)| format!("{a}..{b} {}", describe_style(&st)))
            .collect();
        if !runs.is_empty() {
            let _ = writeln!(s, "{:>width$}┆ {}", "", runs.join("; "));
        }
    }
}

/// A short description of a style: `bold fg=2 bg=#102030`.
pub fn describe_style(st: &Style) -> String {
    let mut parts: Vec<String> = Vec::new();
    let a = st.attrs;
    for (flag, name) in [
        (Attrs::BOLD, "bold"),
        (Attrs::DIM, "dim"),
        (Attrs::ITALIC, "italic"),
        (Attrs::UNDERLINE, "underline"),
        (Attrs::DOUBLE_UNDERLINE, "double-underline"),
        (Attrs::UNDERCURL, "curly-underline"),
        (Attrs::DOTTED_UNDERLINE, "dotted-underline"),
        (Attrs::DASHED_UNDERLINE, "dashed-underline"),
        (Attrs::REVERSE, "reverse"),
        (Attrs::STRIKETHROUGH, "strike"),
        (Attrs::HIDDEN, "hidden"),
    ] {
        if a.contains(flag) {
            parts.push(name.into());
        }
    }
    let color = |c: Color| match c {
        Color::Default => "default".to_string(),
        Color::Indexed(i) => i.to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    };
    if st.fg != Color::Default {
        parts.push(format!("fg={}", color(st.fg)));
    }
    if st.bg != Color::Default {
        parts.push(format!("bg={}", color(st.bg)));
    }
    if let Some(u) = st.underline_color {
        parts.push(format!("ul={}", color(u)));
    }
    parts.join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

/// Parses the plain part of a snapshot.
pub fn parse(text: &str) -> Result<Screen, ParseError> {
    let err = |line: usize, message: &str| ParseError {
        line: line + 1,
        message: message.into(),
    };
    let mut size: Option<Size> = None;
    let mut cursor = None;
    let mut title = None;
    let mut modes = Modes::default();
    let mut rows: Vec<(i64, String)> = Vec::new();
    let mut first_line = 0u64;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some((num, rest)) = line.split_once('│')
            && let Ok(n) = num.trim().parse::<i64>()
        {
            rows.push((n, rest.to_string()));
            continue;
        }
        if line.trim_start().starts_with('┆') {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "size" => {
                let (c, r) = value.split_once('x').ok_or_else(|| err(i, "bad size"))?;
                size = Some(Size::new(
                    r.trim().parse().map_err(|_| err(i, "bad rows"))?,
                    c.trim().parse().map_err(|_| err(i, "bad cols"))?,
                ));
            }
            "cursor" => {
                let mut words = value.split_whitespace();
                let pos = words.next().ok_or_else(|| err(i, "missing cursor position"))?;
                let (r, c) = pos.split_once(',').ok_or_else(|| err(i, "bad cursor"))?;
                let mut cur = crate::screen::Cursor {
                    row: r.parse().map_err(|_| err(i, "bad cursor row"))?,
                    col: c.parse().map_err(|_| err(i, "bad cursor col"))?,
                    visible: true,
                    ..Default::default()
                };
                for w in words {
                    match w {
                        "visible" => cur.visible = true,
                        "hidden" => cur.visible = false,
                        "block" => cur.shape = CursorShape::Block,
                        "underline" => cur.shape = CursorShape::Underline,
                        "bar" => cur.shape = CursorShape::Bar,
                        "hollow-block" => cur.shape = CursorShape::HollowBlock,
                        "blinking" => cur.blinking = true,
                        "pending-wrap" => cur.pending_wrap = true,
                        _ => return Err(err(i, &format!("unknown cursor word {w}"))),
                    }
                }
                cursor = Some(cur);
            }
            "title" => {
                title = Some(serde_json::from_str::<String>(value).map_err(|_| err(i, "bad title"))?);
            }
            "modes" => modes = Modes::from_names(value.split_whitespace()),
            "history" => {
                first_line = value
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| err(i, "bad history"))?;
            }
            _ => return Err(err(i, &format!("unknown line {line:?}"))),
        }
    }
    let size = size.ok_or_else(|| err(0, "missing size"))?;
    let mut screen_rows = Vec::new();
    let mut history_rows = Vec::new();
    for (n, text) in rows {
        let row = Row::from_text(&text, size.cols);
        if n < 0 {
            history_rows.push(row);
        } else {
            screen_rows.push(row);
        }
    }
    let screen = Screen::from_rows(size, screen_rows);
    Ok(screen.with(|c, m, t, h| {
        if let Some(cur) = cursor {
            *c = cur;
        }
        *m = modes;
        *t = title;
        if !history_rows.is_empty() || first_line > 0 {
            let mut hist = History::new(history_rows.len());
            let skipped = first_line.saturating_sub(history_rows.len() as u64);
            // Number the lines as the original did.
            hist.start_at(skipped);
            for r in history_rows {
                hist.push(r);
            }
            *h = hist;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Emulator, EmulatorConfig};

    #[test]
    fn renders_and_parses_back() {
        let mut e = Emulator::new(EmulatorConfig::with_size(Size::new(4, 24)));
        e.feed(
            "\x1b]2;my \"title\"\x07\x1b[?2004h$ echo 界 e\u{301}\r\n\x1b[1;32mok\x1b[0m done\r\n$ "
                .as_bytes(),
        );
        let s = e.screen();
        let text = to_text(&s);
        assert_eq!(
            text,
            "size 24x4\ncursor 2,2 visible block\ntitle \"my \\\"title\\\"\"\nmodes bracketed-paste\n0│$ echo 界 e\u{301}\n1│ok done\n2│$\n3│\n"
        );
        let parsed = parse(&text).unwrap();
        assert_eq!(to_text(&parsed), text);
        assert_eq!(parsed.lines(), s.lines());
        assert_eq!(parsed.cursor(), s.cursor());
        assert_eq!(parsed.modes(), s.modes());
        assert_eq!(parsed.title(), s.title());
    }

    #[test]
    fn styles_are_annotated_and_skipped_by_the_parser() {
        let mut e = Emulator::new(EmulatorConfig::with_size(Size::new(2, 20)));
        e.feed(b"\x1b[1;31mred\x1b[0m \x1b[7;48;2;1;2;3mrev\x1b[0m");
        let text = to_styled_text(&e.screen());
        assert!(
            text.contains("0│red rev\n ┆ 0..3 bold fg=1; 4..7 reverse bg=#010203\n"),
            "{text}"
        );
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed.lines(), e.screen().lines());
    }

    #[test]
    fn history_rows_round_trip() {
        let mut e = Emulator::new(EmulatorConfig::with_size(Size::new(2, 10)));
        e.feed(b"a\r\nb\r\nc\r\nd");
        let opts = SnapshotOptions {
            styles: false,
            history: Some(100),
        };
        let text = to_text_with(&e.screen(), opts);
        assert!(text.contains("history 2 2\n-2│a\n-1│b\n 0│c\n 1│d\n"), "{text}");
        let parsed = parse(&text).unwrap();
        assert_eq!(to_text_with(&parsed, opts), text);
        assert_eq!(parsed.first_line(), 2);
    }
}
