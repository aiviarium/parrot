//! Finding things on a screen: text locators, rectangular regions and boxes.
//!
//! Locators work like Playwright's `getByText`. A row is split into pieces on box-drawing
//! characters and on runs of two or more spaces; [`Locator::text`] matches a whole piece,
//! [`Locator::contains`] and [`Locator::regex`] match anywhere in a row. A locator can be
//! limited to a [`Rect`], for example the inside of a box found by [`ScreenExt::box_by_title`].

use regex::Regex;

use parrot_term::screen::{CellKind, Row, Screen};

pub use parrot_term::Rect;

/// A piece of text found on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextMatch {
    pub row: u16,
    pub col: u16,
    /// Columns covered.
    pub width: u16,
    pub text: String,
}

impl TextMatch {
    pub fn rect(&self) -> Rect {
        Rect::new(self.row, self.col, 1, self.width)
    }
}

#[derive(Clone, Debug)]
enum Query {
    Exact(String),
    Contains(String),
    Regex(Regex),
}

/// A query for text on a screen.
#[derive(Clone, Debug)]
pub struct Locator {
    query: Query,
    within: Option<Rect>,
    nth: usize,
}

impl Locator {
    /// A piece whose text, trimmed, is exactly `text`. In all queries a no-break space on the
    /// screen matches a space.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            query: Query::Exact(
                parrot_term::screen::normalize_spaces(&text.into())
                    .trim()
                    .to_string(),
            ),
            within: None,
            nth: 0,
        }
    }

    /// `text` anywhere in a row.
    pub fn contains(text: impl Into<String>) -> Self {
        Self {
            query: Query::Contains(parrot_term::screen::normalize_spaces(&text.into()).into_owned()),
            within: None,
            nth: 0,
        }
    }

    /// A regex match anywhere in a row.
    pub fn regex(re: Regex) -> Self {
        Self {
            query: Query::Regex(re),
            within: None,
            nth: 0,
        }
    }

    /// Only matches inside `rect`.
    pub fn within(mut self, rect: Rect) -> Self {
        self.within = Some(match self.within {
            Some(r) => r.intersect(&rect).unwrap_or(Rect::new(rect.row, rect.col, 0, 0)),
            None => rect,
        });
        self
    }

    /// [`Locator::find`] returns the `n`-th match (from 0) instead of the first.
    pub fn nth(mut self, n: usize) -> Self {
        self.nth = n;
        self
    }

    /// Every match, top to bottom, left to right.
    pub fn all(&self, screen: &Screen) -> Vec<TextMatch> {
        let size = screen.size();
        let area = self.within.unwrap_or(Rect::new(0, 0, size.rows, size.cols));
        let mut out = Vec::new();
        for r in area.row..area.bottom().min(size.rows) {
            let Some(row) = screen.row(r) else { continue };
            let line = RowText::new(row, area.col, area.right().min(size.cols));
            match &self.query {
                Query::Exact(t) => {
                    for (start, end) in pieces(&line.text) {
                        if line.text[start..end].trim() == t {
                            let s = start
                                + (line.text[start..end].len() - line.text[start..end].trim_start().len());
                            out.push(line.matched(r, s, s + t.len()));
                        }
                    }
                }
                Query::Contains(t) => {
                    if t.is_empty() {
                        continue;
                    }
                    for (s, _) in line.text.match_indices(t.as_str()) {
                        out.push(line.matched(r, s, s + t.len()));
                    }
                }
                Query::Regex(re) => {
                    for m in re.find_iter(&line.text) {
                        if m.start() < m.end() {
                            out.push(line.matched(r, m.start(), m.end()));
                        }
                    }
                }
            }
        }
        out
    }

    /// The first match, or the `nth`.
    pub fn find(&self, screen: &Screen) -> Option<TextMatch> {
        self.all(screen).into_iter().nth(self.nth)
    }

    pub fn count(&self, screen: &Screen) -> usize {
        self.all(screen).len()
    }

    pub fn exists(&self, screen: &Screen) -> bool {
        self.find(screen).is_some()
    }
}

/// Text of part of a row with a map from byte offsets to columns.
struct RowText {
    text: String,
    /// (byte offset, column) for the start of every cell with text, and the end.
    map: Vec<(usize, u16)>,
}

impl RowText {
    fn new(row: &Row, start: u16, end: u16) -> Self {
        let mut text = String::new();
        let mut map = Vec::new();
        for col in start..end.min(row.cols()) {
            let cell = row.cell(col);
            match cell.kind() {
                CellKind::WideTail => {
                    if col == start {
                        map.push((text.len(), col));
                        text.push(' ');
                    }
                }
                CellKind::WrapSpacer => {
                    map.push((text.len(), col));
                    text.push(' ');
                }
                _ => {
                    map.push((text.len(), col));
                    // A no-break space matches a space.
                    text.push_str(&parrot_term::screen::normalize_spaces(cell.text()));
                }
            }
        }
        map.push((text.len(), end.min(row.cols())));
        Self { text, map }
    }

    fn col_at(&self, byte: usize) -> u16 {
        match self.map.binary_search_by_key(&byte, |(b, _)| *b) {
            Ok(i) => self.map[i].1,
            // Inside a cell (a combining mark): that cell.
            Err(i) => self.map[i.saturating_sub(1)].1,
        }
    }

    fn col_after(&self, byte: usize) -> u16 {
        match self.map.binary_search_by_key(&byte, |(b, _)| *b) {
            Ok(i) => self.map[i].1,
            Err(i) => self.map[i.min(self.map.len() - 1)].1,
        }
    }

    fn matched(&self, row: u16, start: usize, end: usize) -> TextMatch {
        let col = self.col_at(start);
        TextMatch {
            row,
            col,
            width: self.col_after(end) - col,
            text: self.text[start..end].to_string(),
        }
    }
}

/// Box-drawing and block characters separate pieces.
fn is_separator_char(c: char) -> bool {
    ('\u{2500}'..='\u{259f}').contains(&c)
}

/// Byte ranges of the pieces of `text`.
fn pieces(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut spaces = 0;
    for (i, c) in text.char_indices() {
        if is_separator_char(c) {
            if let Some(s) = start.take() {
                out.push((s, i));
            }
            spaces = 0;
            continue;
        }
        if c == ' ' {
            spaces += 1;
            if spaces >= 2
                && let Some(s) = start.take()
            {
                out.push((s, i - 1));
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        spaces = 0;
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out.into_iter()
        .map(|(s, e)| (s, s + text[s..e].trim_end().len()))
        .filter(|(s, e)| s < e)
        .collect()
}

/// A rectangle of a screen.
#[derive(Clone, Copy)]
pub struct Region<'a> {
    screen: &'a Screen,
    rect: Rect,
}

impl<'a> Region<'a> {
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The rows of the region as text, trailing whitespace removed.
    pub fn lines(&self) -> Vec<String> {
        (self.rect.row..self.rect.bottom())
            .filter_map(|r| self.screen.row(r))
            .map(|row| {
                let t = row.text_range(self.rect.col, self.rect.right());
                t.trim_end().to_string()
            })
            .collect()
    }

    /// The rows joined with `\n`, trailing blank rows removed.
    pub fn text(&self) -> String {
        let mut lines = self.lines();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    pub fn contains(&self, needle: &str) -> bool {
        Locator::contains(needle).within(self.rect).exists(self.screen)
    }

    pub fn locate(&self, locator: Locator) -> Locator {
        locator.within(self.rect)
    }
}

/// A box drawn with box-drawing characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoxRegion {
    /// The box including its border.
    pub outer: Rect,
    /// Text in the top border, such as `Title` in `┌─ Title ─┐`.
    pub title: Option<String>,
    /// Text in the bottom border.
    pub footer: Option<String>,
}

impl BoxRegion {
    /// The inside of the box.
    pub fn inner(&self) -> Rect {
        Rect::new(
            self.outer.row + 1,
            self.outer.col + 1,
            self.outer.rows.saturating_sub(2),
            self.outer.cols.saturating_sub(2),
        )
    }
}

fn is_top_left(c: char) -> bool {
    matches!(c, '┌' | '╭' | '╔' | '┏' | '╒' | '╓' | '┍' | '┎')
}
fn is_top_right(c: char) -> bool {
    matches!(c, '┐' | '╮' | '╗' | '┓' | '╕' | '╖' | '┑' | '┒')
}
fn is_bottom_left(c: char) -> bool {
    matches!(c, '└' | '╰' | '╚' | '┗' | '╘' | '╙' | '┕' | '┖')
}
fn is_bottom_right(c: char) -> bool {
    matches!(c, '┘' | '╯' | '╝' | '┛' | '╛' | '╜' | '┙' | '┚')
}
fn is_horizontal(c: char) -> bool {
    matches!(
        c,
        '─' | '━'
            | '═'
            | '┄'
            | '┅'
            | '┈'
            | '┉'
            | '╌'
            | '╍'
            | '┬'
            | '┴'
            | '┼'
            | '╤'
            | '╧'
            | '╥'
            | '╨'
            | '╦'
            | '╩'
            | '┯'
            | '┷'
            | '╪'
            | '╫'
            | '╬'
    )
}
fn is_vertical(c: char) -> bool {
    matches!(
        c,
        '│' | '┃'
            | '║'
            | '┆'
            | '┇'
            | '┊'
            | '┋'
            | '╎'
            | '╏'
            | '├'
            | '┤'
            | '╟'
            | '╢'
            | '╠'
            | '╣'
            | '┼'
            | '┝'
            | '┥'
            | '┠'
            | '┨'
            | '╞'
            | '╡'
    )
}

fn char_at(screen: &Screen, row: u16, col: u16) -> char {
    screen
        .cell(row, col)
        .and_then(|c| c.text().chars().next())
        .unwrap_or(' ')
}

/// Text along a border between two corners, with the border characters removed.
fn border_text(screen: &Screen, row: u16, from: u16, to: u16) -> Option<String> {
    let line = screen.row(row)?.text_range(from, to);
    let t: String = line
        .chars()
        .map(|c| if is_horizontal(c) { ' ' } else { c })
        .collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}

/// Text, region and box queries on a terminal screen.
pub trait ScreenExt {
    /// A rectangle of this screen.
    fn region(&self, rect: Rect) -> Region<'_>;
    /// Every drawn box, outermost and topmost first.
    fn boxes(&self) -> Vec<BoxRegion>;
    /// Finds a box by its top border title.
    fn box_by_title(&self, title: &str) -> Option<BoxRegion>;
    /// Finds the innermost box containing a locator match.
    fn box_containing(&self, locator: &Locator) -> Option<BoxRegion>;
    /// Finds an exact piece of text.
    fn get_by_text(&self, text: &str) -> Option<TextMatch>;
}

impl ScreenExt for Screen {
    /// A rectangle of this screen.
    fn region(&self, rect: Rect) -> Region<'_> {
        Region { screen: self, rect }
    }

    /// Every box drawn with box-drawing characters, outermost and topmost first.
    fn boxes(&self) -> Vec<BoxRegion> {
        let size = self.size();
        let mut out = Vec::new();
        for r in 0..size.rows {
            for c in 0..size.cols {
                if !is_top_left(char_at(self, r, c)) {
                    continue;
                }
                // Walk the top border to the right corner; titles may interrupt it.
                let mut right = None;
                for x in c + 1..size.cols {
                    let ch = char_at(self, r, x);
                    if is_top_right(ch) {
                        right = Some(x);
                        break;
                    }
                    if is_top_left(ch) || is_bottom_left(ch) || is_bottom_right(ch) || is_vertical(ch) {
                        break;
                    }
                }
                let Some(right) = right else { continue };
                // Walk down the left border to the bottom corner.
                let mut bottom = None;
                for y in r + 1..size.rows {
                    let ch = char_at(self, y, c);
                    if is_bottom_left(ch) {
                        bottom = Some(y);
                        break;
                    }
                    if !is_vertical(ch) {
                        break;
                    }
                }
                let Some(bottom) = bottom else { continue };
                if !is_bottom_right(char_at(self, bottom, right)) {
                    continue;
                }
                if !(r + 1..bottom).all(|y| is_vertical(char_at(self, y, right))) {
                    continue;
                }
                out.push(BoxRegion {
                    outer: Rect::new(r, c, bottom - r + 1, right - c + 1),
                    title: border_text(self, r, c + 1, right),
                    footer: border_text(self, bottom, c + 1, right),
                });
            }
        }
        out
    }

    /// The box whose top border has this title: an exact match if there is one, otherwise the
    /// first title that contains it.
    fn box_by_title(&self, title: &str) -> Option<BoxRegion> {
        let boxes = self.boxes();
        boxes
            .iter()
            .find(|b| b.title.as_deref() == Some(title))
            .or_else(|| {
                boxes
                    .iter()
                    .find(|b| b.title.as_deref().is_some_and(|t| t.contains(title)))
            })
            .cloned()
    }

    /// The box containing a match of `locator`, innermost first.
    fn box_containing(&self, locator: &Locator) -> Option<BoxRegion> {
        let m = locator.find(self)?;
        let mut boxes: Vec<BoxRegion> = self
            .boxes()
            .into_iter()
            .filter(|b| b.outer.contains_rect(&m.rect()))
            .collect();
        boxes.sort_by_key(|b| b.outer.rows as u32 * b.outer.cols as u32);
        boxes.into_iter().next()
    }

    /// Shorthand for `Locator::text(text).find(screen)`.
    fn get_by_text(&self, text: &str) -> Option<TextMatch> {
        Locator::text(text).find(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_term::Size;

    fn screen(text: &str) -> Screen {
        let cols = text
            .lines()
            .map(|l| parrot_term::screen::Row::from_text(l, 200).content_len())
            .max()
            .unwrap_or(1)
            .max(1);
        let rows = text.lines().count() as u16;
        Screen::from_text(Size::new(rows, cols), text)
    }

    #[test]
    fn exact_pieces_split_on_borders_and_double_spaces() {
        let s = screen("│ Yes  No │\n Yes, please");
        let m = Locator::text("Yes").all(&s);
        assert_eq!(
            m,
            vec![TextMatch {
                row: 0,
                col: 2,
                width: 3,
                text: "Yes".into()
            }]
        );
        assert_eq!(Locator::text("No").find(&s).unwrap().col, 7);
        assert_eq!(Locator::text("Yes, please").find(&s).unwrap().col, 1);
        assert!(Locator::text("Yes  No").find(&s).is_none());
    }

    #[test]
    fn contains_and_regex_map_columns_through_wide_chars() {
        let s = screen("界界 hello\nab\u{301}c x");
        let m = Locator::contains("hello").find(&s).unwrap();
        assert_eq!((m.row, m.col, m.width), (0, 5, 5));
        let m = Locator::regex(Regex::new(r"c x").unwrap()).find(&s).unwrap();
        assert_eq!((m.row, m.col, m.width), (1, 2, 3));
        let m = Locator::contains("界").nth(1).find(&s).unwrap();
        assert_eq!((m.col, m.width), (2, 2));
        assert_eq!(Locator::contains("界").count(&s), 2);
    }

    #[test]
    fn no_break_spaces_match_spaces() {
        let s = screen("❯\u{a0}Count from 1");
        assert!(s.contains("❯ Count"));
        let m = Locator::contains("❯ Count").find(&s).unwrap();
        assert_eq!((m.col, m.width), (0, 7));
        assert!(Locator::text("❯ Count from 1").exists(&s));
        assert!(Locator::regex(Regex::new("❯ C").unwrap()).exists(&s));
    }

    #[test]
    fn titled_boxes() {
        let s = screen(
            "┌─ Files ─────┐ ╭──────╮\n│ a.txt       │ │ ok   │\n│ b.txt  Open │ ╰──────╯\n└─────────────┘\n  > Open",
        );
        let boxes = s.boxes();
        assert_eq!(boxes.len(), 2);
        let files = s.box_by_title("Files").unwrap();
        assert_eq!(files.outer, Rect::new(0, 0, 4, 15));
        assert_eq!(files.inner(), Rect::new(1, 1, 2, 13));
        assert_eq!(s.region(files.inner()).text(), " a.txt\n b.txt  Open");
        let open = Locator::text("Open").within(files.inner()).all(&s);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].row, 2);
        assert_eq!(Locator::text("Open").count(&s), 1); // "> Open" is one piece
        assert_eq!(Locator::contains("Open").count(&s), 2);
        let ok = s.box_containing(&Locator::text("ok")).unwrap();
        assert_eq!(ok.outer, Rect::new(0, 16, 3, 8));
        assert_eq!(ok.title, None);
    }

    #[test]
    fn nested_boxes() {
        let s = screen(
            "╔═ Outer ════════╗\n║ ┌─ Inner ─┐    ║\n║ │ x       │    ║\n║ └─────────┘    ║\n╚════════════════╝",
        );
        assert_eq!(s.boxes().len(), 2);
        let inner = s.box_containing(&Locator::text("x")).unwrap();
        assert_eq!(inner.title.as_deref(), Some("Inner"));
        assert_eq!(s.box_by_title("Outer").unwrap().outer.rows, 5);
    }
}
