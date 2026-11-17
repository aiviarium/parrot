//! Segmentation: the raw tree of a screen, with no knowledge of any program.
//!
//! A region (the screen, or the inside of a box) is processed top to bottom:
//!
//! 1. Boxes drawn with box-drawing characters become panes, segmented recursively.
//! 2. A long vertical line splits the rows it spans into a left and a right pane.
//! 3. Rows are classified as blank, rules (`─`, `━`, `═`, `▔`, `╌`, `┄`, ...) or content. Runs of
//!    rows filled with one background colour become panes.
//! 4. Solid rules split the region into sections, dashed rules split sections again. A section
//!    of one to four rows between two rules that starts with a prompt glyph or holds the cursor
//!    is an input; other sections with a rule above them are panes.
//! 5. Blank rows split the rest into blocks. In a block, numbered lines and lines with a
//!    selection marker (`❯`, `›`, `▶`, ...) aligned with their siblings become menus or lists,
//!    lines that look like key hints become hints, spinner lines become progress, aligned
//!    columns become tables, bold first lines become headings, and the rest is text.
//! 6. Afterwards the node under the visible cursor is focused, panes that hold a menu become
//!    dialogs, and a numbered menu outside any pane is grouped with the text above it and the
//!    hint below it into a dialog.

use std::sync::LazyLock;

use regex::Regex;

use super::{Node, Rect, Role, TextStyle, Tree};
use crate::locator::{BoxRegion, ScreenExt};
use parrot_term::screen::{Attrs, CellKind, Color, Screen, Size, Style, normalize_spaces};

/// Builds the raw tree of a screen. Node ids are provisional (`n0`, `n1`, ... in document
/// order) until [`super::assign_ids`] runs.
pub fn segment(screen: &Screen) -> Tree {
    let seg = Seg::new(screen);
    let full = Rect::new(0, 0, seg.size.rows, seg.size.cols);
    let mut root = Node::new(Role::Screen, full, seg.text_of(full), "auto:screen");
    root.children = seg.region(full, &[]);
    post_process(&mut root, &seg);
    let mut n = 0u64;
    root.walk_mut(&mut |node| {
        node.id = format!("n{n}");
        n += 1;
    });
    root.id = "screen".into();
    let cursor = screen.cursor();
    Tree {
        root,
        size: seg.size,
        cursor: cursor.visible.then_some((cursor.row, cursor.col)),
        title: screen.title().map(str::to_string),
        alt_screen: screen.modes().alt_screen,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strength {
    Solid,
    Dashed,
}

#[derive(Clone, Debug)]
enum Item {
    Blank(u16),
    Rule {
        row: u16,
        strength: Strength,
        label: Option<String>,
    },
    Line(u16),
    Box(usize),
    Fill(u16, u16),
}

impl Item {
    fn row(&self, seg: &Seg) -> u16 {
        match self {
            Item::Blank(r) | Item::Line(r) | Item::Fill(r, _) => *r,
            Item::Rule { row, .. } => *row,
            Item::Box(i) => seg.boxes[*i].outer.row,
        }
    }
    fn last_row(&self, seg: &Seg) -> u16 {
        match self {
            Item::Blank(r) | Item::Line(r) => *r,
            Item::Fill(_, end) => end - 1,
            Item::Rule { row, .. } => *row,
            Item::Box(i) => seg.boxes[*i].outer.bottom() - 1,
        }
    }
}

/// Facts about one line of content within a region.
#[derive(Clone, Debug)]
struct Line {
    row: u16,
    /// Region columns.
    col0: u16,
    col1: u16,
    /// First and one past the last non-blank column.
    indent: u16,
    end: u16,
    /// Text from `indent` to `end`, holes blanked.
    text: String,
    /// A selection marker in the first column and the column of the text after it.
    marker: Option<char>,
    body_col: u16,
    /// `1` in `1. Yes`.
    number: Option<String>,
    style: Style,
    hint: bool,
    progress: bool,
    /// Most cells reverse video or on a background of their own.
    highlight: Option<Style>,
}

struct Seg<'a> {
    screen: &'a Screen,
    size: Size,
    chars: Vec<Vec<char>>,
    styles: Vec<Vec<Style>>,
    boxes: Vec<BoxRegion>,
    cursor: Option<(u16, u16)>,
}

const MARKERS: &[char] = &['❯', '›', '▶', '►', '➤', '→', '❱', '»'];
const PROMPT_GLYPHS: &[char] = &['❯', '›', '>', '➜', '▶', 'λ', '$', '%', '❱', '»', '→'];
const ENTRY_GLYPHS: &[char] = &[
    '•', '●', '⏺', '⎿', '└', '✻', '✳', '✶', '✢', '✽', '›', '❯', '◆', '■',
];
const VERTICAL: &[char] = &['│', '┃', '║', '┆', '╎', '┊', '▏', '▕'];

fn rule_strength(c: char) -> Option<Strength> {
    match c {
        '─' | '━' | '═' | '▔' | '▁' | '⎯' | '—' | '=' => Some(Strength::Solid),
        '╌' | '╍' | '┄' | '┅' | '┈' | '┉' | '╴' | '╶' | '-' | '·' => Some(Strength::Dashed),
        _ => None,
    }
}

static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,2}|[a-zA-Z])[.)]\s+\S").unwrap());
static SHORTCUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s\(([A-Za-z0-9]{1,6}|esc|enter|tab)\)\s*$").unwrap());
static STRONG_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(esc|escape|enter|return|ctrl|alt|shift|backspace|pgup|pgdn|f[1-9][0-2]?|[↑↓←→⏎⇧⌥⌘↵?]|\^[a-z]|ctrl\+\S+|alt\+\S+|shift\+\S+|tab|space)(\s*/\s*\S+)?(\s+|$)",
    )
    .unwrap()
});
static KEY_ACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\b(esc|escape|enter|return|tab|space|ctrl\+\w+|\^[a-z]|backspace|shift\+tab)\s+(to|for)\s+\w|\b(press|hit)\s+(any key|esc|escape|enter|return|tab|space|ctrl|\^[a-z]|[a-z]\b))",
    )
    .unwrap()
});
static PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^[^\p{L}\p{N}\s]{1,2}\s+\p{L}[\p{L}\-' ]{2,40}(…|\.\.\.)|^[^\p{L}\p{N}\s]{1,2}\s+\p{L}+\s+\(\d+[smh]\b|^[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏⣾⣽⣻⢿⡿⣟⣯⣷]\s)",
    )
    .unwrap()
});
static INTERRUPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(esc|ctrl\+c|\^c) to (interrupt|cancel the turn|stop)\b").unwrap());
static SEGMENTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" · | • | \| |\s{2,}").unwrap());

fn is_hint_text(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 160 {
        return false;
    }
    let segs: Vec<&str> = SEGMENTS
        .split(t)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() <= 1 {
        return KEY_ACTION.is_match(t) && t.chars().count() <= 80;
    }
    let keyed = segs
        .iter()
        .filter(|s| STRONG_KEY.is_match(s) || KEY_ACTION.is_match(s))
        .count();
    keyed >= 1 && keyed * 2 >= segs.len()
}

fn is_progress_text(text: &str) -> bool {
    PROGRESS.is_match(text.trim()) || (INTERRUPT.is_match(text) && !is_hint_text(text))
}

fn is_busy_text(text: &str) -> bool {
    is_progress_text(text) || INTERRUPT.is_match(text)
}

fn is_checked(text: &str) -> bool {
    text.contains(['✔', '✓', '☑', '◉', '☒'])
        || text.starts_with("[x]")
        || text.starts_with("[X]")
        || text.starts_with("(●)")
}

impl<'a> Seg<'a> {
    fn new(screen: &'a Screen) -> Self {
        let size = screen.size();
        let mut chars = Vec::with_capacity(size.rows as usize);
        let mut styles = Vec::with_capacity(size.rows as usize);
        for r in 0..size.rows {
            let row = screen.row(r);
            let mut cs = Vec::with_capacity(size.cols as usize);
            let mut ss = Vec::with_capacity(size.cols as usize);
            for c in 0..size.cols {
                match row {
                    Some(row) => {
                        let cell = row.cell(c);
                        let ch = match cell.kind() {
                            CellKind::WideTail => '\0',
                            CellKind::WrapSpacer => ' ',
                            _ => cell.text().chars().next().unwrap_or(' '),
                        };
                        cs.push(if ch == '\u{a0}' { ' ' } else { ch });
                        ss.push(cell.style());
                    }
                    None => {
                        cs.push(' ');
                        ss.push(Style::default());
                    }
                }
            }
            chars.push(cs);
            styles.push(ss);
        }
        let cursor = screen.cursor();
        Seg {
            screen,
            size,
            chars,
            styles,
            boxes: screen.boxes(),
            cursor: cursor.visible.then_some((cursor.row, cursor.col)),
        }
    }

    fn ch(&self, row: u16, col: u16) -> char {
        self.chars
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .copied()
            .unwrap_or(' ')
    }

    fn style(&self, row: u16, col: u16) -> Style {
        self.styles
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .copied()
            .unwrap_or_default()
    }

    fn blankish(c: char) -> bool {
        c == ' ' || c == '\0'
    }

    /// Text of a rectangle, rows joined with `\n`, trailing spaces trimmed.
    fn text_of(&self, rect: Rect) -> String {
        let mut lines = Vec::new();
        for r in rect.row..rect.bottom().min(self.size.rows) {
            let t = self
                .screen
                .row(r)
                .map(|row| row.text_range(rect.col, rect.right()))
                .unwrap_or_default();
            lines.push(normalize_spaces(t.trim_end()).into_owned());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        while lines.first().is_some_and(|l| l.is_empty()) {
            lines.remove(0);
        }
        lines.join("\n")
    }

    fn line_text(&self, row: u16, from: u16, to: u16, holes: &[Rect]) -> String {
        if holes.iter().any(|h| h.row <= row && row < h.bottom()) {
            let mut s = String::new();
            for c in from..to {
                if holes.iter().any(|h| h.contains(row, c)) {
                    s.push(' ');
                } else {
                    let ch = self.ch(row, c);
                    if ch != '\0' {
                        s.push(ch);
                    }
                }
            }
            return normalize_spaces(s.trim_end()).into_owned();
        }
        let t = self
            .screen
            .row(row)
            .map(|r| r.text_range(from, to))
            .unwrap_or_default();
        normalize_spaces(t.trim_end()).into_owned()
    }

    /// Most frequent style of the non-blank cells in a rectangle.
    fn dominant_style(&self, rect: Rect) -> Style {
        let mut counts: Vec<(Style, usize)> = Vec::new();
        for r in rect.row..rect.bottom().min(self.size.rows) {
            for c in rect.col..rect.right().min(self.size.cols) {
                if Self::blankish(self.ch(r, c)) {
                    continue;
                }
                let st = self.style(r, c);
                match counts.iter_mut().find(|(s, _)| *s == st) {
                    Some(e) => e.1 += 1,
                    None => counts.push((st, 1)),
                }
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(s, _)| s)
            .unwrap_or_default()
    }

    fn in_hole(holes: &[Rect], row: u16, col: u16) -> bool {
        holes.iter().any(|h| h.contains(row, col))
    }

    /// The background shared by at least 90% of a row's cells in the region, if it is not the
    /// default.
    fn fill_bg(&self, row: u16, rect: Rect, holes: &[Rect]) -> Option<Color> {
        let mut counts: Vec<(Color, u16)> = Vec::new();
        let mut total = 0u16;
        for c in rect.col..rect.right() {
            if Self::in_hole(holes, row, c) {
                continue;
            }
            total += 1;
            let st = self.style(row, c);
            let bg = if st.attrs.contains(Attrs::REVERSE) {
                Color::Indexed(255)
            } else {
                st.bg
            };
            match counts.iter_mut().find(|(b, _)| *b == bg) {
                Some(e) => e.1 += 1,
                None => counts.push((bg, 1)),
            }
        }
        let (bg, n) = counts.into_iter().max_by_key(|(_, n)| *n)?;
        (bg != Color::Default && total > 0 && n as u32 * 10 >= total as u32 * 9).then_some(bg)
    }

    fn rule_of(&self, row: u16, rect: Rect, holes: &[Rect]) -> Option<(Strength, Option<String>)> {
        let mut counts: Vec<(char, u16)> = Vec::new();
        let mut other = 0u16;
        for c in rect.col..rect.right() {
            if Self::in_hole(holes, row, c) {
                continue;
            }
            let ch = self.ch(row, c);
            if Self::blankish(ch) {
                continue;
            }
            if rule_strength(ch).is_some() {
                match counts.iter_mut().find(|(x, _)| *x == ch) {
                    Some(e) => e.1 += 1,
                    None => counts.push((ch, 1)),
                }
            } else {
                other += 1;
            }
        }
        let (ch, n) = counts.into_iter().max_by_key(|(_, n)| *n)?;
        if n < 8 || (n as u32) * 2 < rect.cols as u32 || n < other * 2 {
            return None;
        }
        // The edge of a box (`╭───╮`, `├───┤`) whose other edges are not visible.
        let t = self.line_text(row, rect.col, rect.right(), holes);
        let t = t.trim();
        let corner = |c: Option<char>| c.is_some_and(|c| "╭╮╰╯┌┐└┘├┤╔╗╚╝╠╣┏┓┗┛┣┫".contains(c));
        if corner(t.chars().next()) || corner(t.chars().last()) {
            return None;
        }
        // ASCII and dots only count when they run unbroken, as `-----` does.
        if matches!(ch, '-' | '=' | '·') {
            let t = self.line_text(row, rect.col, rect.right(), holes);
            let run = ch.to_string().repeat(8);
            if !t.contains(&run) {
                return None;
            }
        }
        let text = self.line_text(row, rect.col, rect.right(), holes);
        let label: String = text
            .chars()
            .map(|c| {
                if rule_strength(c).is_some() && c != '·' {
                    ' '
                } else {
                    c
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        Some((rule_strength(ch)?, (!label.is_empty()).then_some(label)))
    }

    fn line(&self, row: u16, rect: Rect, holes: &[Rect]) -> Option<Line> {
        let mut indent = None;
        let mut end = rect.col;
        for c in rect.col..rect.right() {
            if Self::in_hole(holes, row, c) {
                continue;
            }
            if !Self::blankish(self.ch(row, c)) {
                if indent.is_none() {
                    indent = Some(c);
                }
                end = c + 1;
            }
        }
        let indent = indent?;
        // Include the second column of a wide character at the end.
        if end < rect.right() && self.ch(row, end) == '\0' {
            end += 1;
        }
        let text = self.line_text(row, indent, end, holes);
        let first = self.ch(row, indent);
        let mut marker = None;
        let mut body_col = indent;
        if MARKERS.contains(&first) || first == '>' {
            let mut c = indent + 1;
            while c < end && self.ch(row, c) == ' ' {
                c += 1;
            }
            if c > indent + 1 && c < end {
                marker = Some(first);
                body_col = c;
            }
        }
        let body = self.line_text(row, body_col, end, holes);
        let number = NUMBERED.captures(&body).map(|c| c[1].to_string()).filter(|n| {
            n.chars().all(|c| c.is_ascii_digit()) || body.contains(") ") || {
                // A letter followed by `.` must look like an option (`a. Foo`), not `I. said`.
                n.len() == 1 && n.chars().all(|c| c.is_ascii_lowercase())
            }
        });
        if marker == Some('>') && number.is_none() {
            marker = None;
            body_col = indent;
        }
        let lrect = Rect::new(row, indent, 1, end - indent);
        let style = self.dominant_style(lrect);
        let progress = is_progress_text(&text);
        let hint = !progress && is_hint_text(&text);
        let highlight = {
            let mut rev = 0u16;
            let mut total = 0u16;
            for c in indent..end {
                total += 1;
                let st = self.style(row, c);
                if st.attrs.contains(Attrs::REVERSE) || st.bg != Color::Default {
                    rev += 1;
                }
            }
            (total > 0 && rev * 10 >= total * 8).then_some(self.style(row, indent))
        };
        Some(Line {
            row,
            col0: rect.col,
            col1: rect.right(),
            indent,
            end,
            text,
            marker,
            body_col,
            number,
            style,
            hint,
            progress,
            highlight,
        })
    }

    /// Segments a region. `outer_holes` are areas to ignore (unused at the top level).
    fn region(&self, rect: Rect, outer_holes: &[Rect]) -> Vec<Node> {
        if rect.rows == 0 || rect.cols == 0 {
            return Vec::new();
        }
        // Boxes directly inside this region.
        let inside: Vec<usize> = (0..self.boxes.len())
            .filter(|&i| rect.contains_rect(&self.boxes[i].outer) && self.boxes[i].outer != rect)
            .collect();
        let top: Vec<usize> = inside
            .iter()
            .copied()
            .filter(|&i| {
                !inside.iter().any(|&j| {
                    j != i
                        && self.boxes[j].outer.contains_rect(&self.boxes[i].outer)
                        && self.boxes[j].outer != self.boxes[i].outer
                })
            })
            .collect();
        let mut holes: Vec<Rect> = outer_holes.to_vec();
        holes.extend(top.iter().map(|&i| self.boxes[i].outer));

        if let Some(split) = self.vertical_split(rect, &holes) {
            return split;
        }

        // Classify rows.
        let mut items: Vec<Item> = Vec::new();
        let mut fills: Vec<Option<Color>> = Vec::new();
        for r in rect.row..rect.bottom() {
            if let Some(&b) = top.iter().find(|&&i| self.boxes[i].outer.row == r) {
                items.push(Item::Box(b));
                fills.push(None);
                continue;
            }
            let in_box = top.iter().any(|&i| {
                let o = self.boxes[i].outer;
                o.row < r && r < o.bottom()
            });
            let line = self.line(r, rect, &holes);
            if in_box && line.is_none() {
                fills.push(None);
                continue;
            }
            fills.push(if in_box {
                None
            } else {
                self.fill_bg(r, rect, &holes)
            });
            match (line, self.rule_of(r, rect, &holes)) {
                (_, Some((strength, label))) => items.push(Item::Rule {
                    row: r,
                    strength,
                    label,
                }),
                (Some(_), None) => items.push(Item::Line(r)),
                (None, None) => items.push(Item::Blank(r)),
            }
        }
        let items = self.group_fills(items, &fills, rect);
        self.layout(&items, rect, &holes, Strength::Solid)
    }

    /// Replaces runs of rows on one background with [`Item::Fill`].
    fn group_fills(&self, items: Vec<Item>, fills: &[Option<Color>], rect: Rect) -> Vec<Item> {
        let fill_at = |r: u16| -> Option<Color> {
            if r < rect.row || r >= rect.bottom() {
                return None;
            }
            fills.get((r - rect.row) as usize).copied().flatten()
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < items.len() {
            let r = items[i].row(self);
            let bg = match &items[i] {
                Item::Line(_) | Item::Blank(_) => fill_at(r),
                _ => None,
            };
            let Some(bg) = bg else {
                out.push(items[i].clone());
                i += 1;
                continue;
            };
            let mut j = i;
            while j < items.len()
                && matches!(items[j], Item::Line(_) | Item::Blank(_))
                && items[j].row(self) == r + (j - i) as u16
            {
                let rj = items[j].row(self);
                let same = fill_at(rj) == Some(bg);
                // One row on another background inside the fill is a highlight within it.
                let highlight = fill_at(rj).is_some()
                    && j > i
                    && fill_at(rj + 1) == Some(bg)
                    && matches!(items.get(j + 1), Some(Item::Line(_) | Item::Blank(_)));
                if !(same || highlight) {
                    break;
                }
                j += 1;
            }
            let has_text = items[i..j].iter().any(|it| matches!(it, Item::Line(_)));
            let len = j - i;
            let isolated = |row: Option<u16>| -> bool {
                match row {
                    None => true,
                    Some(row) => match items.iter().find(|it| it.row(self) == row) {
                        Some(Item::Line(_)) => fill_at(row).is_some(),
                        _ => true,
                    },
                }
            };
            let above = r.checked_sub(1).filter(|&x| x >= rect.row);
            let below = Some(r + len as u16).filter(|&x| x < rect.bottom());
            if has_text && (len >= 2 || (isolated(above) && isolated(below))) {
                out.push(Item::Fill(r, r + len as u16));
            } else {
                out.extend(items[i..j].iter().cloned());
            }
            i = j;
        }
        out
    }

    fn vertical_split(&self, rect: Rect, holes: &[Rect]) -> Option<Vec<Node>> {
        if rect.cols < 20 || rect.rows < 3 {
            return None;
        }
        let mut best: Option<(u16, u16, u16)> = None; // col, start, end
        for c in rect.col + 4..rect.right().saturating_sub(4) {
            let mut r = rect.row;
            while r < rect.bottom() {
                if VERTICAL.contains(&self.ch(r, c)) && !Self::in_hole(holes, r, c) {
                    let s = r;
                    while r < rect.bottom()
                        && VERTICAL.contains(&self.ch(r, c))
                        && !Self::in_hole(holes, r, c)
                    {
                        r += 1;
                    }
                    if best.is_none_or(|(_, bs, be)| r - s > be - bs) {
                        best = Some((c, s, r));
                    }
                } else {
                    r += 1;
                }
            }
        }
        let (c, s, e) = best?;
        if e - s < 3 || ((e - s) as u32) * 10 < rect.rows as u32 * 4 {
            return None;
        }
        let mut out = Vec::new();
        if s > rect.row {
            out.extend(self.region(Rect::new(rect.row, rect.col, s - rect.row, rect.cols), holes));
        }
        for part in [
            Rect::new(s, rect.col, e - s, c - rect.col),
            Rect::new(s, c + 1, e - s, rect.right() - c - 1),
        ] {
            let children = self.region(part, holes);
            if children.is_empty() {
                continue;
            }
            let mut pane = Node::new(Role::Pane, part, self.text_of(part), "auto:vsplit");
            pane.label = children
                .iter()
                .find(|n| matches!(n.role, Role::Heading | Role::Text))
                .map(|n| n.first_line().trim().to_string());
            pane.children = children;
            out.push(pane);
        }
        if e < rect.bottom() {
            out.extend(self.region(Rect::new(e, rect.col, rect.bottom() - e, rect.cols), holes));
        }
        Some(out)
    }

    fn box_node(&self, i: usize, outer_holes: &[Rect]) -> Node {
        let b = &self.boxes[i];
        let mut node = Node::new(Role::Pane, b.outer, self.text_of(b.inner()), "auto:box");
        node.label = b.title.clone();
        node.children = self.region(b.inner(), outer_holes);
        if let Some(f) = &b.footer {
            let r = Rect::new(b.outer.bottom() - 1, b.outer.col, 1, b.outer.cols);
            let role = if is_hint_text(f) { Role::Hint } else { Role::Text };
            node.children
                .push(Node::new(role, r, f.clone(), "auto:box-footer"));
        }
        node
    }

    /// Lays out items, splitting on rules of `level` strength first.
    fn layout(&self, items: &[Item], rect: Rect, holes: &[Rect], level: Strength) -> Vec<Node> {
        let splits: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, Item::Rule { strength, .. } if *strength == level))
            .map(|(i, _)| i)
            .collect();
        if splits.is_empty() {
            return match level {
                Strength::Solid => self.layout(items, rect, holes, Strength::Dashed),
                Strength::Dashed => self.blocks(items, rect, holes),
            };
        }
        let mut out = Vec::new();
        let mut bounds: Vec<Option<usize>> = vec![None];
        bounds.extend(splits.iter().map(|&s| Some(s)));
        bounds.push(None);
        for w in bounds.windows(2) {
            let (above, below) = (w[0], w[1]);
            let start = above.map_or(0, |a| a + 1);
            let end = below.unwrap_or(items.len());
            if let Some(a) = above
                && let Item::Rule { row, label, .. } = &items[a]
            {
                let r = Rect::new(*row, rect.col, 1, rect.cols);
                let mut sep = Node::new(
                    Role::Separator,
                    r,
                    self.line_text(*row, rect.col, rect.right(), holes),
                    "auto:rule",
                );
                sep.label = label.clone();
                sep.style = self.dominant_style(r).into();
                out.push(sep);
            }
            let seg = &items[start..end];
            let content: Vec<&Item> = seg.iter().filter(|it| !matches!(it, Item::Blank(_))).collect();
            if content.is_empty() {
                continue;
            }
            let first_row = content[0].row(self);
            let last_row = content[content.len() - 1].last_row(self);
            let srect = Rect::new(first_row, rect.col, last_row - first_row + 1, rect.cols);
            let cursor_in = self
                .cursor
                .is_some_and(|(r, c)| srect.contains(r, c) && !Self::in_hole(holes, r, c));
            let all_lines = content.iter().all(|it| matches!(it, Item::Line(_)));
            let starts_with_prompt = match content[0] {
                Item::Line(r) => self
                    .line(*r, rect, holes)
                    .is_some_and(|l| PROMPT_GLYPHS.contains(&self.ch(l.row, l.indent))),
                _ => false,
            };
            if above.is_some()
                && below.is_some()
                && all_lines
                && srect.rows <= 4
                && ((level == Strength::Solid && starts_with_prompt) || cursor_in)
            {
                out.push(self.input_node(srect, holes, "auto:rule-input"));
                continue;
            }
            let children = self.layout(seg, rect, holes, Strength::Dashed);
            if children.is_empty() {
                continue;
            }
            if above.is_some() || children.len() >= 2 {
                let source = if above.is_some() {
                    "auto:rule-section"
                } else {
                    "auto:rule-below"
                };
                let mut pane = Node::new(Role::Pane, srect, self.text_of(srect), source);
                if let Some(a) = above
                    && let Item::Rule { label, .. } = &items[a]
                {
                    pane.label = label.clone();
                }
                pane.children = children;
                out.push(pane);
            } else {
                out.extend(children);
            }
        }
        out
    }

    fn input_node(&self, rect: Rect, holes: &[Rect], source: &str) -> Node {
        let lines: Vec<Line> = (rect.row..rect.bottom())
            .filter_map(|r| self.line(r, Rect::new(r, rect.col, 1, rect.cols), holes))
            .collect();
        let col = lines.iter().map(|l| l.indent).min().unwrap_or(rect.col);
        let bounds = rect;
        let mut node = Node::new(Role::Input, bounds, self.text_of(bounds), source);
        node.style = self.dominant_style(bounds).into();
        // The value: text after the prompt glyph, unless it is a dim placeholder.
        let mut value_lines = Vec::new();
        let mut placeholder = true;
        let mut body = col;
        for (i, l) in lines.iter().enumerate() {
            let from = if i == 0 {
                let mut c = l.indent;
                if PROMPT_GLYPHS.contains(&self.ch(l.row, c)) {
                    c += 1;
                    while c < l.end && self.ch(l.row, c) == ' ' {
                        c += 1;
                    }
                }
                body = c;
                c
            } else {
                body.min(l.indent)
            };
            for c in from..l.end {
                let ch = self.ch(l.row, c);
                if !Self::blankish(ch) && !self.style(l.row, c).attrs.contains(Attrs::DIM) {
                    placeholder = false;
                }
            }
            value_lines.push(self.line_text(l.row, from, l.end, holes));
        }
        let value = value_lines.join("\n").trim().to_string();
        if placeholder && !value.is_empty() {
            node.label = Some(value);
            node.value = Some(String::new());
        } else {
            node.value = Some(value);
        }
        node
    }

    fn blocks(&self, items: &[Item], rect: Rect, holes: &[Rect]) -> Vec<Node> {
        let mut out = Vec::new();
        let mut run: Vec<Line> = Vec::new();
        let flush = |run: &mut Vec<Line>, out: &mut Vec<Node>| {
            if !run.is_empty() {
                out.extend(self.block(std::mem::take(run), rect, holes));
            }
        };
        for it in items {
            match it {
                Item::Line(r) => {
                    if let Some(l) = self.line(*r, rect, holes) {
                        run.push(l);
                    }
                }
                Item::Blank(_) => flush(&mut run, &mut out),
                Item::Rule { row, label, .. } => {
                    flush(&mut run, &mut out);
                    let r = Rect::new(*row, rect.col, 1, rect.cols);
                    let mut sep = Node::new(
                        Role::Separator,
                        r,
                        self.line_text(*row, rect.col, rect.right(), holes),
                        "auto:rule",
                    );
                    sep.label = label.clone();
                    out.push(sep);
                }
                Item::Box(i) => {
                    flush(&mut run, &mut out);
                    out.push(self.box_node(*i, &[]));
                }
                Item::Fill(r0, r1) => {
                    flush(&mut run, &mut out);
                    let frect = Rect::new(*r0, rect.col, r1 - r0, rect.cols);
                    let lines: Vec<Line> = (*r0..*r1).filter_map(|r| self.line(r, rect, holes)).collect();
                    let mut children = Vec::new();
                    let mut buf = Vec::new();
                    let mut prev: Option<u16> = None;
                    for l in lines {
                        if prev.is_some_and(|p| l.row > p + 1) {
                            children.extend(self.block(std::mem::take(&mut buf), rect, holes));
                        }
                        prev = Some(l.row);
                        buf.push(l);
                    }
                    children.extend(self.block(buf, rect, holes));
                    if children.len() == 1 && children[0].role == Role::Input {
                        let mut c = children.pop().unwrap();
                        c.source = "auto:fill-input".into();
                        out.push(c);
                    } else {
                        let mut pane = Node::new(Role::Pane, frect, self.text_of(frect), "auto:fill");
                        pane.style = TextStyle::from(self.style(*r0, rect.col));
                        pane.children = children;
                        out.push(pane);
                    }
                }
            }
        }
        flush(&mut run, &mut out);
        out
    }

    /// Splits a block of adjacent lines into lists, menus, hints, progress, tables, headings
    /// and text.
    fn block(&self, lines: Vec<Line>, rect: Rect, holes: &[Rect]) -> Vec<Node> {
        let n = lines.len();
        if n == 0 {
            return Vec::new();
        }
        // runs: (first line, one past the last line, item start lines)
        let mut runs: Vec<(usize, usize, Vec<usize>)> = Vec::new();
        let mut used = vec![false; n];
        // An input and its continuation lines are never options.
        let mut reserved = vec![false; n];
        if let Some(k) = self.input_start(&lines) {
            reserved[k] = true;
            let mut j = k + 1;
            while j < n && lines[j].indent > lines[k].indent && !lines[j].hint {
                reserved[j] = true;
                j += 1;
            }
        }
        // Numbered options, with continuation lines indented past the number.
        let mut i = 0;
        while i < n {
            if lines[i].number.is_none() {
                i += 1;
                continue;
            }
            let b = lines[i].body_col;
            let mut starts = vec![i];
            let mut j = i + 1;
            while j < n {
                let l = &lines[j];
                if (l.number.is_some() || l.marker.is_some()) && l.body_col == b {
                    starts.push(j);
                } else if l.marker.is_none() && l.number.is_none() && l.indent > b && !l.hint {
                    // continuation
                } else {
                    break;
                }
                j += 1;
            }
            if starts.len() >= 2 {
                used[i..j].iter_mut().for_each(|u| *u = true);
                runs.push((i, j, starts));
                i = j;
            } else {
                i += 1;
            }
        }
        // Options without numbers: one line with a marker, the others aligned with its text.
        for i in 0..n {
            if used[i] || reserved[i] || lines[i].marker.is_none() || lines[i].number.is_some() {
                continue;
            }
            let b = lines[i].body_col;
            // With aligned columns, the other options have their second column in the same
            // place.
            let second = self.field_starts(&lines[i]).get(1).copied();
            let plain = |l: &Line| {
                l.marker.is_none()
                    && l.number.is_none()
                    && l.indent == b
                    && !l.hint
                    && !l.progress
                    && second.is_none_or(|c| self.field_starts(l).contains(&c))
            };
            let mut s = i;
            while s > 0 && !used[s - 1] && !reserved[s - 1] && plain(&lines[s - 1]) {
                s -= 1;
            }
            let mut e = i + 1;
            while e < n && !used[e] && !reserved[e] && plain(&lines[e]) {
                e += 1;
            }
            let count = e - s;
            // Two lines where the first fills the width is wrapped text, not a menu.
            let wrapped = count == 2
                && s == i
                && (lines[i].end as u32) * 10 >= (lines[i].col1 - lines[i].col0) as u32 * 9;
            if count >= 2 && !wrapped {
                used[s..e].iter_mut().for_each(|u| *u = true);
                runs.push((s, e, (s..e).collect()));
            }
        }
        // Options shown by a highlight: aligned lines, exactly one highlighted.
        let mut i = 0;
        while i < n {
            if used[i] {
                i += 1;
                continue;
            }
            let ind = lines[i].indent;
            let mut j = i;
            while j < n && !used[j] && lines[j].indent == ind && !lines[j].hint {
                j += 1;
            }
            if j - i >= 2 {
                let hl: Vec<usize> = (i..j).filter(|&k| lines[k].highlight.is_some()).collect();
                if hl.len() == 1 {
                    used[i..j].iter_mut().for_each(|u| *u = true);
                    runs.push((i, j, (i..j).collect()));
                }
                i = j;
            } else {
                i += 1;
            }
        }
        runs.sort_by_key(|r| r.0);

        let mut out = Vec::new();
        let mut k = 0;
        let mut run_idx = 0;
        while k < n {
            if run_idx < runs.len() && runs[run_idx].0 == k {
                let (s, e, starts) = &runs[run_idx];
                out.push(self.list_node(&lines[*s..*e], starts, *s, holes));
                k = *e;
                run_idx += 1;
                continue;
            }
            let mut e = k;
            while e < n && !used[e] {
                e += 1;
            }
            out.extend(self.paragraph(&lines[k..e], rect, holes));
            k = e.max(k + 1);
        }
        out
    }

    fn list_node(&self, lines: &[Line], starts: &[usize], offset: usize, holes: &[Rect]) -> Node {
        let col = lines.iter().map(|l| l.indent).min().unwrap_or(0);
        let end = lines.iter().map(|l| l.end).max().unwrap_or(col + 1);
        let bounds = Rect::new(lines[0].row, col, lines.len() as u16, end - col);
        let mut items = Vec::new();
        for (idx, &s) in starts.iter().enumerate() {
            let s = s - offset;
            let e = starts.get(idx + 1).map_or(lines.len(), |&x| x - offset);
            let first = &lines[s];
            let last = &lines[e - 1];
            let iend = lines[s..e].iter().map(|l| l.end).max().unwrap_or(first.end);
            let icol = lines[s..e].iter().map(|l| l.indent).min().unwrap_or(first.indent);
            let ib = Rect::new(first.row, icol, last.row - first.row + 1, iend - icol);
            let mut node = Node::new(Role::Menuitem, ib, self.text_of(ib), "auto:item");
            let mut body = self.line_text(first.row, first.body_col, first.end, holes);
            if let Some(num) = &first.number {
                node.key = Some(num.clone());
                body = body[num.len() + 1..].trim_start().to_string();
            }
            node.columns = SEGMENTS
                .split(&body)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if node.columns.len() < 2 {
                node.columns.clear();
            }
            let mut label = body.trim().to_string();
            for l in &lines[s + 1..e] {
                label.push(' ');
                label.push_str(l.text.trim());
            }
            if let Some(c) = SHORTCUT.captures(&label) {
                node.shortcut = Some(c[1].to_string());
                let cut = c.get(0).unwrap().start();
                label.truncate(cut);
            }
            node.state.checked = is_checked(&label);
            node.label = Some(match node.columns.first() {
                Some(c) if e == s + 1 => c.clone(),
                _ => label.split_whitespace().collect::<Vec<_>>().join(" "),
            });
            node.state.selected = first.marker.is_some();
            node.style = first.style.into();
            items.push((node, first.highlight, first.style));
        }
        // Without a marker, a single highlighted item is the selection.
        if !items.iter().any(|(n, _, _)| n.state.selected) {
            let hl: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, (_, h, _))| h.is_some())
                .map(|(i, _)| i)
                .collect();
            if hl.len() == 1 && items.len() >= 2 {
                items[hl[0]].0.state.selected = true;
            }
        }
        // Dim items among bright ones are disabled.
        let dim = items
            .iter()
            .filter(|(_, _, st)| st.attrs.contains(Attrs::DIM))
            .count();
        if dim > 0 && items.len() - dim >= 2 {
            for (n, _, st) in items.iter_mut() {
                n.state.disabled = st.attrs.contains(Attrs::DIM);
            }
        }
        let selected = items.iter().any(|(n, _, _)| n.state.selected);
        let (role, item_role, source) = if selected {
            (Role::Menu, Role::Menuitem, "auto:menu")
        } else {
            (Role::List, Role::Listitem, "auto:list")
        };
        let mut list = Node::new(role, bounds, self.text_of(bounds), source);
        list.children = items
            .into_iter()
            .map(|(mut n, _, _)| {
                n.role = item_role;
                n
            })
            .collect();
        list
    }

    /// The line starting with a prompt glyph that holds the visible cursor, or whose
    /// continuation lines (indented past the glyph) hold it: the start of an input.
    fn input_start(&self, lines: &[Line]) -> Option<usize> {
        let (r, c) = self.cursor?;
        let k = lines.iter().position(|l| l.row == r)?;
        (0..=k).rev().find(|&j| {
            let g = &lines[j];
            PROMPT_GLYPHS.contains(&self.ch(g.row, g.indent))
                && (j == k && c >= g.indent.saturating_sub(1)
                    || (j < k && lines[j + 1..=k].iter().all(|x| x.indent > g.indent)))
        })
    }

    fn paragraph(&self, lines: &[Line], rect: Rect, holes: &[Rect]) -> Vec<Node> {
        let mut out = Vec::new();
        let mut buf: Vec<&Line> = Vec::new();
        let flush = |buf: &mut Vec<&Line>, out: &mut Vec<Node>| {
            if !buf.is_empty() {
                out.extend(self.text_nodes(buf, rect, holes));
                buf.clear();
            }
        };
        let input_start = self.input_start(lines);
        let mut i = 0;
        while i < lines.len() {
            let l = &lines[i];
            let lrect = Rect::new(l.row, l.indent, 1, l.end - l.indent);
            if input_start == Some(i) {
                flush(&mut buf, &mut out);
                let mut j = i + 1;
                while j < lines.len() && lines[j].indent > l.indent && !lines[j].hint && !lines[j].progress {
                    j += 1;
                }
                let r = Rect::new(l.row, rect.col, (j - i) as u16, rect.cols);
                out.push(self.input_node(r, holes, "auto:cursor-input"));
                i = j;
                continue;
            }
            if l.progress || l.hint {
                flush(&mut buf, &mut out);
                let role = if l.progress { Role::Progress } else { Role::Hint };
                let src = if l.progress { "auto:progress" } else { "auto:hint" };
                let mut node = Node::new(role, lrect, l.text.clone(), src);
                node.state.busy = is_busy_text(&l.text);
                node.style = l.style.into();
                out.push(node);
                i += 1;
                continue;
            }
            buf.push(l);
            i += 1;
        }
        flush(&mut buf, &mut out);
        out
    }

    fn text_nodes(&self, lines: &[&Line], _rect: Rect, _holes: &[Rect]) -> Vec<Node> {
        let mut out = Vec::new();
        let mut lines = lines;
        // A bold, short first line followed by more is a heading.
        if lines.len() >= 2 || lines.len() == 1 {
            let l = lines[0];
            let short = l.text.chars().count() <= 60;
            if l.style.attrs.contains(Attrs::BOLD) && short && !l.text.ends_with('.') {
                let r = Rect::new(l.row, l.indent, 1, l.end - l.indent);
                let mut h = Node::new(Role::Heading, r, l.text.clone(), "auto:heading");
                h.style = l.style.into();
                out.push(h);
                lines = &lines[1..];
            }
        }
        if lines.is_empty() {
            return out;
        }
        if let Some(table) = self.table(lines) {
            out.push(table);
            return out;
        }
        let col = lines.iter().map(|l| l.indent).min().unwrap_or(0);
        let end = lines.iter().map(|l| l.end).max().unwrap_or(col + 1);
        let first = lines[0].row;
        let last = lines[lines.len() - 1].row;
        let bounds = Rect::new(first, col, last - first + 1, end - col);
        let bottom = first + 2 >= self.size.rows;
        let role = if lines.len() == 1 && bottom {
            Role::Status
        } else {
            Role::Text
        };
        let source = if role == Role::Status {
            "auto:status"
        } else {
            "auto:text"
        };
        let mut node = Node::new(role, bounds, self.text_of(bounds), source);
        node.style = self.dominant_style(bounds).into();
        out.push(node);
        out
    }

    /// Columns where the fields of a line start: its text after any marker, and every
    /// non-blank after two or more spaces.
    fn field_starts(&self, l: &Line) -> Vec<u16> {
        let mut starts = vec![l.body_col];
        let mut spaces = 0;
        for c in l.body_col..l.end {
            let ch = self.ch(l.row, c);
            if ch == ' ' {
                spaces += 1;
            } else {
                if spaces >= 2 {
                    starts.push(c);
                }
                spaces = 0;
            }
        }
        starts
    }

    /// Lines whose fields start in the same columns.
    fn table(&self, lines: &[&Line]) -> Option<Node> {
        if lines.len() < 2 {
            return None;
        }
        let all: Vec<Vec<u16>> = lines.iter().map(|l| self.field_starts(l)).collect();
        let multi = all.iter().filter(|f| f.len() >= 2).count();
        if multi * 4 < lines.len() * 3 {
            return None;
        }
        let mut cols: Vec<u16> = all.iter().flatten().copied().collect();
        cols.sort_unstable();
        cols.dedup();
        let shared: Vec<u16> = cols
            .into_iter()
            .filter(|c| all.iter().filter(|f| f.contains(c)).count() * 4 >= lines.len() * 3)
            .collect();
        if shared.len() < 2 {
            return None;
        }
        let col = lines.iter().map(|l| l.indent).min().unwrap_or(0);
        let end = lines.iter().map(|l| l.end).max().unwrap_or(col + 1);
        let first = lines[0].row;
        let last = lines[lines.len() - 1].row;
        let bounds = Rect::new(first, col, last - first + 1, end - col);
        let mut table = Node::new(Role::Table, bounds, self.text_of(bounds), "auto:table");
        for l in lines {
            let r = Rect::new(l.row, l.indent, 1, l.end - l.indent);
            let mut row = Node::new(Role::Row, r, l.text.clone(), "auto:row");
            row.columns = SEGMENTS
                .split(&l.text)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            row.style = l.style.into();
            table.children.push(row);
        }
        Some(table)
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    let row = a.row.min(b.row);
    let col = a.col.min(b.col);
    let bottom = a.bottom().max(b.bottom());
    let right = a.right().max(b.right());
    Rect::new(row, col, bottom - row, right - col)
}

fn has_menu(n: &Node) -> bool {
    n.children.iter().any(|c| {
        c.role == Role::Menu
            || (!matches!(c.role, Role::Pane | Role::Dialog) && c.role.is_container() && has_menu(c))
    })
}

fn has_other_than_input(n: &Node) -> (bool, bool) {
    let input = n.children.iter().any(|c| c.role == Role::Input);
    let other = n
        .children
        .iter()
        .any(|c| matches!(c.role, Role::Text | Role::Heading));
    (input, other)
}

fn post_process(root: &mut Node, seg: &Seg) {
    // Focus: the deepest node holding the visible cursor.
    if let Some((r, c)) = seg.cursor {
        fn deepest(n: &mut Node, r: u16, c: u16) -> bool {
            if !n.bounds.contains(r, c)
                && !(n.role == Role::Input && n.bounds.row <= r && r < n.bounds.bottom())
            {
                return false;
            }
            for ch in &mut n.children {
                if deepest(ch, r, c) {
                    return true;
                }
            }
            if n.role != Role::Screen {
                n.state.focused = true;
                return true;
            }
            false
        }
        deepest(root, r, c);
    }
    promote_dialogs(root);
    group_dialogs(root, seg);
    label_menus(root);
}

fn dialog_label(n: &Node) -> Option<String> {
    n.walk()
        .into_iter()
        .skip(1)
        .find(|x| matches!(x.role, Role::Heading | Role::Text))
        .map(|h| h.first_line().trim().to_string())
}

fn promote_dialogs(n: &mut Node) {
    for c in &mut n.children {
        promote_dialogs(c);
    }
    let dialog_source = matches!(n.source.as_str(), "auto:box" | "auto:rule-section" | "auto:fill");
    // A dialog opened by a rule with its options in a section of its own (between dashed
    // rules) is the whole outer section.
    if n.role == Role::Pane
        && n.source == "auto:rule-section"
        && n.children.iter().any(|c| c.source == "auto:rule-section+dialog")
    {
        for c in &mut n.children {
            if c.source == "auto:rule-section+dialog" {
                c.role = Role::Pane;
                c.source = "auto:rule-section".into();
            }
        }
        n.role = Role::Dialog;
        n.source = "auto:rule-section+dialog".into();
        n.label = dialog_label(n);
        return;
    }
    if n.role == Role::Pane && dialog_source {
        let (input, other) = has_other_than_input(n);
        if has_menu(n) || (input && other && n.source == "auto:box") {
            n.role = Role::Dialog;
            n.source = format!("{}+dialog", n.source);
            n.label = dialog_label(n);
        }
    }
}

/// Groups a numbered menu that is not inside a pane with the text above it and the hint
/// below it.
fn group_dialogs(n: &mut Node, seg: &Seg) {
    for c in &mut n.children {
        if !matches!(c.role, Role::Dialog | Role::Menu | Role::List | Role::Table) {
            group_dialogs(c, seg);
        }
    }
    if n.role == Role::Dialog {
        return;
    }
    let mut i = 0;
    while i < n.children.len() {
        let c = &n.children[i];
        let numbered = c.role == Role::Menu && c.children.iter().filter(|x| x.key.is_some()).count() >= 2;
        if !numbered {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > 0 && i - start < 6 {
            let prev = &n.children[start - 1];
            let next = &n.children[start];
            let gap_ok = prev.bounds.bottom() + 2 >= next.bounds.row;
            let kind_ok = matches!(prev.role, Role::Text | Role::Heading);
            let first = prev.text.trim_start().chars().next().unwrap_or(' ');
            if !(gap_ok && kind_ok) || ENTRY_GLYPHS.contains(&first) {
                break;
            }
            start -= 1;
        }
        let mut end = i + 1;
        while end < n.children.len() && end - i <= 2 {
            let next = &n.children[end];
            let prev = &n.children[end - 1];
            if prev.bounds.bottom() + 2 < next.bounds.row || !matches!(next.role, Role::Hint | Role::Text) {
                break;
            }
            let is_hint = next.role == Role::Hint;
            end += 1;
            if is_hint {
                break;
            }
        }
        // Text below the menu only belongs to it if a hint follows.
        while end > i + 1 && n.children[end - 1].role != Role::Hint {
            end -= 1;
        }
        let members: Vec<Node> = n.children.drain(start..end).collect();
        let bounds = members.iter().map(|m| m.bounds).reduce(union).unwrap_or(n.bounds);
        let mut d = Node::new(Role::Dialog, bounds, seg.text_of(bounds), "auto:dialog");
        d.label = members
            .iter()
            .find(|m| matches!(m.role, Role::Heading | Role::Text))
            .map(|m| m.first_line().trim().to_string());
        d.children = members;
        n.children.insert(start, d);
        i = start + 1;
    }
}

/// A menu's label is the question or heading right above it.
fn label_menus(n: &mut Node) {
    for i in 0..n.children.len() {
        if matches!(n.children[i].role, Role::Menu | Role::List) && n.children[i].label.is_none() && i > 0 {
            let prev = &n.children[i - 1];
            if matches!(prev.role, Role::Text | Role::Heading)
                && prev.bounds.bottom() + 1 >= n.children[i].bounds.row
            {
                let l = prev.text.lines().last().unwrap_or("").trim().to_string();
                n.children[i].label = Some(l);
            }
        }
    }
    for c in &mut n.children {
        label_menus(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(text: &str, rows: u16, cols: u16) -> Tree {
        segment(&Screen::from_text(Size::new(rows, cols), text))
    }

    #[test]
    fn hints_and_progress() {
        assert!(is_hint_text("Esc to cancel · Tab to amend"));
        assert!(is_hint_text("enter continue · esc skip"));
        assert!(is_hint_text("Press enter to confirm or esc to cancel"));
        assert!(is_hint_text("⏸ manual mode on · ? for shortcuts · ← for agents"));
        assert!(is_hint_text("? help  esc back  ↑/↓ move  enter open  n new"));
        assert!(!is_hint_text("the end of the file"));
        assert!(!is_hint_text("Enter your name"));
        assert!(!is_hint_text("◐ medium · /effort"));
        assert!(is_progress_text("✢ Thinking…"));
        assert!(is_progress_text("* Julienning… (1s · ↓ 20 tokens)"));
        assert!(is_progress_text("• Working (0s • esc to interrupt)"));
        assert!(!is_progress_text("✻ Sautéed for 1s · done 7:08 PM"));
    }

    #[test]
    fn numbered_menu_with_marker_and_wrapped_item() {
        let t = tree(
            " Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and always allow\n      access from here\n   3. No\n\n Esc to cancel",
            8,
            40,
        );
        let menu = t.with_role(Role::Menu)[0].clone();
        assert_eq!(menu.children.len(), 3);
        assert!(menu.children[0].state.selected);
        assert_eq!(
            menu.children[1].label.as_deref(),
            Some("Yes, and always allow access from here")
        );
        assert_eq!(menu.children[2].key.as_deref(), Some("3"));
        assert_eq!(menu.label.as_deref(), Some("Do you want to proceed?"));
        let dialog = &t.with_role(Role::Dialog)[0];
        assert_eq!(dialog.children.last().unwrap().role, Role::Hint);
    }

    #[test]
    fn rules_make_an_input_and_a_dialog_section() {
        let rule = "─".repeat(40);
        let text = format!("hello\n{rule}\n❯ hi\n{rule}\n  ? for shortcuts");
        let t = tree(&text, 5, 40);
        let input = t.with_role(Role::Input)[0];
        assert_eq!(input.value.as_deref(), Some("hi"));
        assert_eq!(input.source, "auto:rule-input");
        assert_eq!(t.with_role(Role::Separator).len(), 2);
        let text = format!("log\n{rule}\n Pick one\n   No\n ❯ Yes\n\n Enter to confirm · Esc to cancel");
        let t = tree(&text, 7, 40);
        let d = t.with_role(Role::Dialog);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].label.as_deref(), Some("Pick one"));
        let m = t.with_role(Role::Menu)[0];
        assert_eq!(m.selected_index(), Some(1));
    }

    #[test]
    fn boxes_and_tables() {
        let text = "╭─ Title ──────╮\n│ some text    │\n╰──────────────╯\n  a     first\n  bb    second\n  ccc   third";
        let t = tree(text, 6, 20);
        let b = &t.root.children[0];
        assert_eq!(b.role, Role::Pane);
        assert_eq!(b.label.as_deref(), Some("Title"));
        assert_eq!(b.children[0].text, "some text");
        let table = t.with_role(Role::Table)[0];
        assert_eq!(table.children.len(), 3);
        assert_eq!(table.children[1].columns, vec!["bb", "second"]);
    }

    #[test]
    fn input_with_the_cursor_on_a_wrapped_line() {
        let s = Screen::from_text(
            Size::new(3, 30),
            "› first line of a long\n  prompt here\n  and more",
        )
        .with(|cur, _, _, _| {
            cur.row = 1;
            cur.col = 13;
            cur.visible = true;
        });
        let t = segment(&s);
        let input = t.with_role(Role::Input);
        assert_eq!(input.len(), 1);
        assert_eq!(
            input[0].value.as_deref(),
            Some("first line of a long\nprompt here\nand more")
        );
    }

    #[test]
    fn wrapped_marker_line_is_not_a_menu() {
        let long = format!("› {}", "word ".repeat(7));
        let text = format!("{}\n  more words", long.trim_end());
        let t = tree(&text, 2, 38);
        assert!(t.with_role(Role::Menu).is_empty());
    }
}
