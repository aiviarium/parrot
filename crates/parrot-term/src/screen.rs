//! The immutable screen model.
//!
//! A [`Screen`] is a snapshot of an emulator: visible rows of cells, the cursor, the terminal's
//! modes, its title, and the scrollback. Cloning a screen is one reference count. Rows are shared
//! between snapshots when they did not change, so diffing two snapshots is mostly pointer
//! comparisons.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use bitflags::bitflags;

/// Terminal size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

impl Size {
    pub const fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }
}

impl Default for Size {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

impl fmt::Display for Size {
    /// Formats as `COLSxROWS`, the order asciinema and `stty` use.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

/// A colour as the application set it. Palette colours are not resolved to RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// One of the 256 palette colours; 0 to 15 are the ANSI colours.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags! {
    /// Text attributes of a cell.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct Attrs: u16 {
        const BOLD             = 1 << 0;
        const DIM              = 1 << 1;
        const ITALIC           = 1 << 2;
        const UNDERLINE        = 1 << 3;
        const DOUBLE_UNDERLINE = 1 << 4;
        const UNDERCURL        = 1 << 5;
        const DOTTED_UNDERLINE = 1 << 6;
        const DASHED_UNDERLINE = 1 << 7;
        const REVERSE          = 1 << 8;
        const STRIKETHROUGH    = 1 << 9;
        const HIDDEN           = 1 << 10;
        const ANY_UNDERLINE = Self::UNDERLINE.bits()
            | Self::DOUBLE_UNDERLINE.bits()
            | Self::UNDERCURL.bits()
            | Self::DOTTED_UNDERLINE.bits()
            | Self::DASHED_UNDERLINE.bits();
    }
}

/// The style of a cell: colours and attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// Underline colour, if the application set one apart from the foreground.
    pub underline_color: Option<Color>,
    pub attrs: Attrs,
}

impl Style {
    pub fn is_default(&self) -> bool {
        *self == Style::default()
    }
    pub fn bold(&self) -> bool {
        self.attrs.contains(Attrs::BOLD)
    }
    pub fn dim(&self) -> bool {
        self.attrs.contains(Attrs::DIM)
    }
    pub fn italic(&self) -> bool {
        self.attrs.contains(Attrs::ITALIC)
    }
    /// Any kind of underline.
    pub fn underline(&self) -> bool {
        self.attrs.intersects(Attrs::ANY_UNDERLINE)
    }
    pub fn reverse(&self) -> bool {
        self.attrs.contains(Attrs::REVERSE)
    }
    pub fn strikethrough(&self) -> bool {
        self.attrs.contains(Attrs::STRIKETHROUGH)
    }
    pub fn hidden(&self) -> bool {
        self.attrs.contains(Attrs::HIDDEN)
    }
}

/// What a cell holds, as far as layout goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellKind {
    /// A one-column character, or a blank.
    Normal,
    /// The first column of a two-column character.
    Wide,
    /// The second column of a two-column character. Has no text.
    WideTail,
    /// The last column of a row, left blank because the next character was wide and did not
    /// fit, so it went to the next row. Has no text.
    WrapSpacer,
}

/// A cell of a row, borrowed from the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell<'a> {
    text: &'a str,
    kind: CellKind,
    style: Style,
}

impl<'a> Cell<'a> {
    /// The grapheme cluster in the cell: a base character and any combining marks or variation
    /// selectors that followed it. A blank cell is `" "`; the tail of a wide character and a
    /// wrap spacer are `""`.
    pub fn text(&self) -> &'a str {
        self.text
    }
    pub fn kind(&self) -> CellKind {
        self.kind
    }
    pub fn style(&self) -> Style {
        self.style
    }
    /// Columns this cell's text occupies: 2 for a wide character, 0 for its tail, 1 otherwise.
    pub fn width(&self) -> u16 {
        match self.kind {
            CellKind::Normal | CellKind::WrapSpacer => 1,
            CellKind::Wide => 2,
            CellKind::WideTail => 0,
        }
    }
    /// A blank in the default style.
    pub fn is_empty(&self) -> bool {
        matches!(self.kind, CellKind::Normal | CellKind::WrapSpacer)
            && (self.text == " " || self.text.is_empty())
            && self.style.is_default()
    }
    /// A cell whose text is whitespace or nothing, whatever its style.
    pub fn is_blank(&self) -> bool {
        self.text.chars().all(|c| c == ' ')
    }
}

const BLANK: Cell<'static> = Cell {
    text: " ",
    kind: CellKind::Normal,
    style: Style {
        fg: Color::Default,
        bg: Color::Default,
        underline_color: None,
        attrs: Attrs::empty(),
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Slot {
    start: u32,
    style: u16,
    len: u8,
    kind: u8,
}

const KIND_NORMAL: u8 = 0;
const KIND_WIDE: u8 = 1;
const KIND_TAIL: u8 = 2;
const KIND_SPACER: u8 = 3;

fn kind_to_u8(kind: CellKind) -> u8 {
    match kind {
        CellKind::Normal => KIND_NORMAL,
        CellKind::Wide => KIND_WIDE,
        CellKind::WideTail => KIND_TAIL,
        CellKind::WrapSpacer => KIND_SPACER,
    }
}

fn kind_from_u8(kind: u8) -> CellKind {
    match kind {
        KIND_WIDE => CellKind::Wide,
        KIND_TAIL => CellKind::WideTail,
        KIND_SPACER => CellKind::WrapSpacer,
        _ => CellKind::Normal,
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct RowData {
    /// Texts of the stored cells, concatenated.
    text: String,
    /// One slot per stored cell. Cells past the last slot are blanks in the default style.
    slots: Vec<Slot>,
    styles: Vec<Style>,
    cols: u16,
    wrapped: bool,
}

/// One row of cells. Cheap to clone.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Row(Arc<RowData>);

impl Row {
    /// An empty row of `cols` blank cells.
    pub fn blank(cols: u16) -> Row {
        RowBuilder::new(cols).finish(false)
    }

    /// A row from plain text in the default style, using Unicode widths. Text beyond `cols`
    /// columns is cut off.
    pub fn from_text(text: &str, cols: u16) -> Row {
        let mut b = RowBuilder::new(cols);
        b.push_text(text, Style::default());
        b.finish(false)
    }

    pub fn cols(&self) -> u16 {
        self.0.cols
    }

    /// The row cut or padded to `cols` columns, without reflowing. A wide character that no
    /// longer fits becomes a blank.
    pub fn with_cols(&self, cols: u16) -> Row {
        if cols == self.0.cols {
            return self.clone();
        }
        let mut b = RowBuilder::new(cols);
        for col in 0..cols.min(self.0.cols) {
            let cell = self.cell(col);
            if cell.kind() == CellKind::Wide && col + 1 >= cols {
                b.push(" ", CellKind::Normal, cell.style());
            } else {
                b.push(cell.text(), cell.kind(), cell.style());
            }
        }
        b.finish(self.0.wrapped && cols >= self.0.cols)
    }

    /// The row continues on the next row: the application wrote past the last column.
    pub fn is_wrapped(&self) -> bool {
        self.0.wrapped
    }

    /// Whether two rows are the same allocation; a fast path for diffing.
    pub fn ptr_eq(&self, other: &Row) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn cell(&self, col: u16) -> Cell<'_> {
        match self.0.slots.get(col as usize) {
            Some(slot) => Cell {
                text: &self.0.text[slot.start as usize..slot.start as usize + slot.len as usize],
                kind: kind_from_u8(slot.kind),
                style: self.0.styles[slot.style as usize],
            },
            None => BLANK,
        }
    }

    /// All `cols` cells.
    pub fn cells(&self) -> impl Iterator<Item = Cell<'_>> + '_ {
        (0..self.0.cols).map(move |c| self.cell(c))
    }

    /// Number of leading cells that are not blank in the default style.
    pub fn content_len(&self) -> u16 {
        let mut n = self.0.slots.len();
        while n > 0 && self.cell((n - 1) as u16).is_empty() {
            n -= 1;
        }
        n as u16
    }

    /// The row as text, one column per column: a wide character stands for its two columns and a
    /// wrap spacer is a space. Trailing whitespace is removed.
    pub fn text(&self) -> String {
        let mut s = self.text_untrimmed();
        let trimmed = s.trim_end_matches(' ').len();
        s.truncate(trimmed);
        s
    }

    /// Like [`Row::text`], with every column present.
    pub fn text_untrimmed(&self) -> String {
        let mut s = String::with_capacity(self.0.cols as usize);
        for cell in self.cells() {
            match cell.kind {
                CellKind::WideTail => {}
                CellKind::WrapSpacer => s.push(' '),
                _ => s.push_str(cell.text),
            }
        }
        s
    }

    /// Text of the columns `start..end`. A wide character is included when its first column is
    /// in the range.
    pub fn text_range(&self, start: u16, end: u16) -> String {
        let mut s = String::new();
        for col in start..end.min(self.0.cols) {
            let cell = self.cell(col);
            match cell.kind {
                CellKind::WideTail => {
                    if col == start {
                        s.push(' ');
                    }
                }
                CellKind::WrapSpacer => s.push(' '),
                _ => s.push_str(cell.text),
            }
        }
        s
    }

    /// Maximal runs of columns with the same style, as `(start, end, style)`.
    pub fn style_runs(&self) -> Vec<(u16, u16, Style)> {
        let mut runs: Vec<(u16, u16, Style)> = Vec::new();
        for (col, cell) in self.cells().enumerate() {
            let col = col as u16;
            match runs.last_mut() {
                Some(last) if last.2 == cell.style => last.1 = col + 1,
                _ => runs.push((col, col + 1, cell.style)),
            }
        }
        runs
    }
}

impl fmt::Debug for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Row({:?}{})",
            self.text(),
            if self.is_wrapped() { ", wrapped" } else { "" }
        )
    }
}

/// Builds a [`Row`] cell by cell.
pub struct RowBuilder {
    data: RowData,
}

impl RowBuilder {
    pub fn new(cols: u16) -> Self {
        Self {
            data: RowData {
                text: String::new(),
                slots: Vec::new(),
                styles: Vec::new(),
                cols,
                wrapped: false,
            },
        }
    }

    /// Number of cells pushed so far.
    pub fn len(&self) -> u16 {
        self.data.slots.len() as u16
    }

    pub fn is_empty(&self) -> bool {
        self.data.slots.is_empty()
    }

    /// Appends a cell. `text` is cut to 255 bytes at a character boundary; nothing is pushed
    /// past the row's width.
    pub fn push(&mut self, text: &str, kind: CellKind, style: Style) {
        if self.data.slots.len() >= self.data.cols as usize {
            return;
        }
        let mut text = text;
        if text.len() > 255 {
            let mut end = 255;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text = &text[..end];
        }
        let style_idx = match self.data.styles.iter().position(|s| *s == style) {
            Some(i) => i,
            None => {
                self.data.styles.push(style);
                self.data.styles.len() - 1
            }
        };
        self.data.slots.push(Slot {
            start: self.data.text.len() as u32,
            style: style_idx as u16,
            len: text.len() as u8,
            kind: kind_to_u8(kind),
        });
        self.data.text.push_str(text);
    }

    /// Appends text, split into grapheme clusters with Unicode widths. Zero-width clusters at
    /// the start are dropped. A wide cluster that does not fit in the row is dropped.
    pub fn push_text(&mut self, text: &str, style: Style) {
        use unicode_segmentation::UnicodeSegmentation;
        for g in text.graphemes(true) {
            let w = grapheme_width(g);
            match w {
                0 => {}
                1 => self.push(g, CellKind::Normal, style),
                _ => {
                    if self.len() + 2 > self.data.cols {
                        return;
                    }
                    self.push(g, CellKind::Wide, style);
                    self.push("", CellKind::WideTail, style);
                }
            }
        }
    }

    pub fn finish(mut self, wrapped: bool) -> Row {
        // Drop trailing blanks in the default style; they are implied.
        while let Some(slot) = self.data.slots.last() {
            let text = &self.data.text[slot.start as usize..slot.start as usize + slot.len as usize];
            let blank = (text == " " || text.is_empty())
                && slot.kind == KIND_NORMAL
                && self.data.styles[slot.style as usize].is_default();
            if !blank {
                break;
            }
            self.data.text.truncate(slot.start as usize);
            self.data.slots.pop();
        }
        // Drop styles no longer referenced, renumbering the rest, so equal rows compare equal.
        let mut used = vec![false; self.data.styles.len()];
        for s in &self.data.slots {
            used[s.style as usize] = true;
        }
        if used.iter().any(|u| !u) {
            let mut map = vec![0u16; used.len()];
            let mut styles = Vec::new();
            for (i, u) in used.iter().enumerate() {
                if *u {
                    map[i] = styles.len() as u16;
                    styles.push(self.data.styles[i]);
                }
            }
            for s in &mut self.data.slots {
                s.style = map[s.style as usize];
            }
            self.data.styles = styles;
        }
        self.data.text.shrink_to_fit();
        self.data.slots.shrink_to_fit();
        self.data.wrapped = wrapped;
        Row(Arc::new(self.data))
    }
}

/// Replaces no-break spaces with spaces, for matching text the way it looks. Some TUIs (Claude
/// Code's prompt) write U+00A0 where a space is meant.
pub fn normalize_spaces(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\u{a0}') {
        std::borrow::Cow::Owned(s.replace('\u{a0}', " "))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Display width of a grapheme cluster, as terminals count it: the width of its first
/// character, so a base with combining marks is 1 and an emoji with a variation selector keeps
/// its base width.
pub fn grapheme_width(g: &str) -> u16 {
    use unicode_width::UnicodeWidthChar;
    g.chars().next().and_then(|c| c.width()).unwrap_or(0) as u16
}

/// Cursor shape as set by DECSCUSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
    HollowBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: CursorShape,
    pub blinking: bool,
    /// The cursor is in the last column and the next character will wrap first.
    pub pending_wrap: bool,
}

/// Which mouse events the application asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum MouseMode {
    #[default]
    None,
    /// Mode 1000: presses, releases and wheel.
    Click,
    /// Mode 1002: also motion while a button is held.
    Drag,
    /// Mode 1003: all motion.
    Motion,
}

/// How mouse events are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum MouseEncoding {
    /// The X10 form, `CSI M` with three bytes.
    #[default]
    Default,
    /// Mode 1005: coordinates as UTF-8.
    Utf8,
    /// Mode 1006: `CSI < b ; x ; y M`.
    Sgr,
}

bitflags! {
    /// Kitty keyboard protocol flags, as the application pushed them.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct KittyFlags: u8 {
        const DISAMBIGUATE           = 1;
        const REPORT_EVENT_TYPES     = 2;
        const REPORT_ALTERNATE_KEYS  = 4;
        const REPORT_ALL_KEYS_AS_ESC = 8;
        const REPORT_ASSOCIATED_TEXT = 16;
    }
}

/// Terminal modes that affect input encoding, display or serialisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Modes {
    /// DECCKM: cursor keys send `ESC O A` instead of `ESC [ A`.
    pub app_cursor: bool,
    /// DECKPAM.
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// Mode 1004.
    pub focus_events: bool,
    pub alt_screen: bool,
    /// A synchronized update (mode 2026) has begun and not yet ended.
    pub synchronized_update: bool,
    /// DECAWM.
    pub line_wrap: bool,
    /// DECOM.
    pub origin: bool,
    /// IRM.
    pub insert: bool,
    /// LNM.
    pub newline: bool,
    /// Mode 1007: the wheel sends arrow keys on the alternate screen.
    pub alternate_scroll: bool,
    pub kitty: KittyFlags,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            bracketed_paste: false,
            mouse: MouseMode::None,
            mouse_encoding: MouseEncoding::Default,
            focus_events: false,
            alt_screen: false,
            synchronized_update: false,
            line_wrap: true,
            origin: false,
            insert: false,
            newline: false,
            alternate_scroll: true,
            kitty: KittyFlags::empty(),
        }
    }
}

impl Modes {
    /// Names of the modes that differ from a fresh terminal, for display.
    pub fn names(&self) -> Vec<String> {
        let d = Modes::default();
        let mut v = Vec::new();
        if self.alt_screen {
            v.push("alt-screen".into());
        }
        if self.app_cursor {
            v.push("app-cursor".into());
        }
        if self.app_keypad {
            v.push("app-keypad".into());
        }
        if self.bracketed_paste {
            v.push("bracketed-paste".into());
        }
        match self.mouse {
            MouseMode::None => {}
            MouseMode::Click => v.push("mouse=click".into()),
            MouseMode::Drag => v.push("mouse=drag".into()),
            MouseMode::Motion => v.push("mouse=motion".into()),
        }
        match self.mouse_encoding {
            MouseEncoding::Default => {}
            MouseEncoding::Utf8 => v.push("mouse-encoding=utf8".into()),
            MouseEncoding::Sgr => v.push("mouse-encoding=sgr".into()),
        }
        if self.focus_events {
            v.push("focus-events".into());
        }
        if self.synchronized_update {
            v.push("synchronized-update".into());
        }
        if self.line_wrap != d.line_wrap {
            v.push("no-wrap".into());
        }
        if self.origin {
            v.push("origin".into());
        }
        if self.insert {
            v.push("insert".into());
        }
        if self.newline {
            v.push("newline".into());
        }
        if self.alternate_scroll != d.alternate_scroll {
            v.push("no-alternate-scroll".into());
        }
        if !self.kitty.is_empty() {
            v.push(format!("kitty={}", self.kitty.bits()));
        }
        v
    }

    /// Parses names as [`Modes::names`] produces them. Unknown names are ignored.
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Modes {
        let mut m = Modes::default();
        for n in names {
            match n {
                "alt-screen" => m.alt_screen = true,
                "app-cursor" => m.app_cursor = true,
                "app-keypad" => m.app_keypad = true,
                "bracketed-paste" => m.bracketed_paste = true,
                "mouse=click" => m.mouse = MouseMode::Click,
                "mouse=drag" => m.mouse = MouseMode::Drag,
                "mouse=motion" => m.mouse = MouseMode::Motion,
                "mouse-encoding=utf8" => m.mouse_encoding = MouseEncoding::Utf8,
                "mouse-encoding=sgr" => m.mouse_encoding = MouseEncoding::Sgr,
                "focus-events" => m.focus_events = true,
                "synchronized-update" => m.synchronized_update = true,
                "no-wrap" => m.line_wrap = false,
                "origin" => m.origin = true,
                "insert" => m.insert = true,
                "newline" => m.newline = true,
                "no-alternate-scroll" => m.alternate_scroll = false,
                other => {
                    if let Some(bits) = other.strip_prefix("kitty=").and_then(|b| b.parse().ok()) {
                        m.kitty = KittyFlags::from_bits_truncate(bits);
                    }
                }
            }
        }
        m
    }
}

const HISTORY_CHUNK: usize = 256;

/// Scrollback: rows that scrolled off the top of the primary screen, oldest first.
///
/// Every line that ever entered the scrollback has an absolute number, counting from 0 for the
/// first line of the session. The number keeps increasing when old lines are dropped or the
/// scrollback is cleared, so a consumer can tell exactly which lines it has not seen.
///
/// Cloning is cheap: rows are kept in shared chunks.
#[derive(Clone, Default)]
pub struct History {
    chunks: VecDeque<Arc<Vec<Row>>>,
    /// Rows of the first chunk that were dropped.
    skip: usize,
    len: usize,
    cap: usize,
    /// Absolute number of the first retained line.
    start: u64,
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            ..Default::default()
        }
    }

    /// Number of retained lines.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Maximum number of retained lines.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Absolute number of the oldest retained line.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Absolute number one past the newest line: the total number of lines that ever scrolled
    /// into the scrollback.
    pub fn end(&self) -> u64 {
        self.start + self.len as u64
    }

    /// The `i`-th retained line, oldest first.
    pub fn get(&self, i: usize) -> Option<&Row> {
        if i >= self.len {
            return None;
        }
        let mut i = i + self.skip;
        for chunk in &self.chunks {
            if i < chunk.len() {
                return Some(&chunk[i]);
            }
            i -= chunk.len();
        }
        None
    }

    /// The line with absolute number `n`, if still retained.
    pub fn get_abs(&self, n: u64) -> Option<&Row> {
        if n < self.start {
            return None;
        }
        self.get((n - self.start) as usize)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Row> + '_ {
        let skip = self.skip;
        self.chunks
            .iter()
            .enumerate()
            .flat_map(move |(i, c)| c[if i == 0 { skip } else { 0 }..].iter())
    }

    /// Lines with absolute numbers from `from` to the end. `Err` carries the number of lines
    /// that were requested but are no longer retained.
    pub fn since(&self, from: u64) -> Result<Vec<Row>, u64> {
        if from < self.start {
            return Err(self.start - from);
        }
        let skip = (from - self.start) as usize;
        Ok(self.iter().skip(skip).cloned().collect())
    }

    pub(crate) fn push(&mut self, row: Row) {
        if self.cap == 0 {
            self.start += 1;
            return;
        }
        match self.chunks.back_mut() {
            Some(last) if last.len() < HISTORY_CHUNK => Arc::make_mut(last).push(row),
            _ => {
                let mut v = Vec::with_capacity(HISTORY_CHUNK);
                v.push(row);
                self.chunks.push_back(Arc::new(v));
            }
        }
        self.len += 1;
        while self.len > self.cap {
            self.skip += 1;
            self.len -= 1;
            self.start += 1;
            if self.skip >= self.chunks[0].len() {
                self.chunks.pop_front();
                self.skip = 0;
            }
        }
    }

    /// Sets the number of the first line of an empty history.
    pub(crate) fn start_at(&mut self, n: u64) {
        debug_assert!(self.is_empty());
        self.start = n;
    }

    /// Drops every line; numbering continues.
    pub(crate) fn clear(&mut self) {
        self.start = self.end();
        self.chunks.clear();
        self.skip = 0;
        self.len = 0;
    }
}

impl fmt::Debug for History {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("History")
            .field("start", &self.start)
            .field("len", &self.len)
            .finish()
    }
}

/// The primary screen as it was when the application switched to the alternate screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedScreen {
    pub rows: Vec<Row>,
    pub cursor: Cursor,
}

/// The cursor state DECSC saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedCursor {
    pub row: u16,
    pub col: u16,
    pub pen: Style,
}

#[derive(Clone, Debug)]
pub(crate) struct ScreenData {
    pub size: Size,
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub modes: Modes,
    pub title: Option<Arc<str>>,
    pub history: History,
    pub bell_count: u64,
    pub version: u64,
    pub offset: u64,
    pub saved_primary: Option<SavedScreen>,
    pub scroll_region: Option<(u16, u16)>,
    pub pen: Style,
    pub saved_cursor: Option<SavedCursor>,
}

/// An immutable snapshot of a terminal.
#[derive(Clone)]
pub struct Screen(pub(crate) Arc<ScreenData>);

impl Screen {
    /// A screen of blank rows, for tests and parsers.
    pub fn blank(size: Size) -> Screen {
        Screen::from_rows(size, (0..size.rows).map(|_| Row::blank(size.cols)).collect())
    }

    /// A screen with the given rows in a fresh terminal state. Missing rows are blank; extra rows
    /// are dropped.
    pub fn from_rows(size: Size, mut rows: Vec<Row>) -> Screen {
        rows.resize_with(size.rows as usize, || Row::blank(size.cols));
        Screen(Arc::new(ScreenData {
            size,
            rows,
            cursor: Cursor {
                visible: true,
                ..Default::default()
            },
            modes: Modes::default(),
            title: None,
            history: History::new(0),
            bell_count: 0,
            version: 0,
            offset: 0,
            saved_primary: None,
            scroll_region: None,
            pen: Style::default(),
            saved_cursor: None,
        }))
    }

    /// A screen from lines of plain text.
    pub fn from_text(size: Size, text: &str) -> Screen {
        Screen::from_rows(size, text.lines().map(|l| Row::from_text(l, size.cols)).collect())
    }

    /// Returns a copy with a different cursor, modes, title or history. For tests and parsers.
    pub fn with(&self, f: impl FnOnce(&mut Cursor, &mut Modes, &mut Option<String>, &mut History)) -> Screen {
        let mut data = (*self.0).clone();
        let mut title = data.title.as_deref().map(str::to_string);
        f(&mut data.cursor, &mut data.modes, &mut title, &mut data.history);
        data.title = title.map(Arc::from);
        Screen(Arc::new(data))
    }

    pub fn size(&self) -> Size {
        self.0.size
    }
    pub fn rows(&self) -> &[Row] {
        &self.0.rows
    }
    pub fn row(&self, row: u16) -> Option<&Row> {
        self.0.rows.get(row as usize)
    }
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell<'_>> {
        let r = self.row(row)?;
        (col < r.cols()).then(|| r.cell(col))
    }
    pub fn cursor(&self) -> Cursor {
        self.0.cursor
    }
    pub fn modes(&self) -> Modes {
        self.0.modes
    }
    pub fn title(&self) -> Option<&str> {
        self.0.title.as_deref()
    }
    /// Scrollback of the primary screen. Also available while the alternate screen is active.
    pub fn history(&self) -> &History {
        &self.0.history
    }
    /// Number of BEL characters received so far.
    pub fn bell_count(&self) -> u64 {
        self.0.bell_count
    }
    /// Changes whenever the emulator's state changes; equal versions mean equal screens.
    pub fn version(&self) -> u64 {
        self.0.version
    }
    /// The number of output bytes whose effect this screen shows. Bytes from this offset on,
    /// written to an emulator showing this screen, continue it correctly.
    pub fn offset(&self) -> u64 {
        self.0.offset
    }
    /// Absolute line number of visible row 0 on the primary screen. Equal to the number of lines
    /// that scrolled into the scrollback so far.
    pub fn first_line(&self) -> u64 {
        self.0.history.end()
    }
    /// The primary screen, while the alternate screen is active.
    pub fn saved_primary(&self) -> Option<&SavedScreen> {
        self.0.saved_primary.as_ref()
    }
    /// Scrolling region as `(top, bottom)` rows, inclusive, when it is not the whole screen.
    pub fn scroll_region(&self) -> Option<(u16, u16)> {
        self.0.scroll_region
    }
    /// The style new text is written with.
    pub fn pen(&self) -> Style {
        self.0.pen
    }
    pub fn saved_cursor(&self) -> Option<SavedCursor> {
        self.0.saved_cursor
    }

    /// Visible rows as text, trailing whitespace removed from each.
    pub fn lines(&self) -> Vec<String> {
        self.0.rows.iter().map(Row::text).collect()
    }

    /// Visible rows as text joined with `\n`, with trailing blank rows removed.
    pub fn text(&self) -> String {
        let mut lines = self.lines();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Whether the visible text contains `needle` on one row. A no-break space on the screen
    /// matches a space in `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        let needle = normalize_spaces(needle);
        self.0
            .rows
            .iter()
            .any(|r| normalize_spaces(&r.text_untrimmed()).contains(needle.as_ref()))
    }

    /// Rows whose content differs from the row at the same index in `old`. All rows when the
    /// size differs.
    pub fn changed_rows(&self, old: &Screen) -> Vec<u16> {
        if self.size() != old.size() {
            return (0..self.size().rows).collect();
        }
        self.0
            .rows
            .iter()
            .zip(old.0.rows.iter())
            .enumerate()
            .filter(|(_, (a, b))| !a.ptr_eq(b) && a != b)
            .map(|(i, _)| i as u16)
            .collect()
    }

    /// What changed from `old` to this screen.
    pub fn diff(&self, old: &Screen) -> ScreenDiff {
        ScreenDiff {
            rows: self.changed_rows(old),
            size: self.size() != old.size(),
            cursor: self.cursor() != old.cursor(),
            modes: self.modes() != old.modes(),
            title: self.title() != old.title(),
            new_history_lines: self.first_line().saturating_sub(old.first_line()),
        }
    }

    /// Equal visible content, cursor, modes and title; history and counters are not compared.
    pub fn same_content(&self, other: &Screen) -> bool {
        self.size() == other.size()
            && self.cursor() == other.cursor()
            && self.modes() == other.modes()
            && self.title() == other.title()
            && self.changed_rows(other).is_empty()
    }
}

impl fmt::Debug for Screen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Screen {} cursor {:?}", self.size(), self.cursor())?;
        for l in self.lines() {
            writeln!(f, "│{l}")?;
        }
        Ok(())
    }
}

/// The result of [`Screen::diff`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ScreenDiff {
    /// Visible rows that changed.
    pub rows: Vec<u16>,
    pub size: bool,
    pub cursor: bool,
    pub modes: bool,
    pub title: bool,
    /// Lines that scrolled into the scrollback in between.
    pub new_history_lines: u64,
}

impl ScreenDiff {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
            && !self.size
            && !self.cursor
            && !self.modes
            && !self.title
            && self.new_history_lines == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_from_text_handles_wide_and_combining() {
        let r = Row::from_text("e\u{301}界x", 10);
        assert_eq!(r.cell(0).text(), "e\u{301}");
        assert_eq!(r.cell(1).text(), "界");
        assert_eq!(r.cell(1).kind(), CellKind::Wide);
        assert_eq!(r.cell(2).kind(), CellKind::WideTail);
        assert_eq!(r.cell(3).text(), "x");
        assert_eq!(r.text(), "e\u{301}界x");
        assert_eq!(r.content_len(), 4);
    }

    #[test]
    fn trailing_blanks_are_implicit_and_rows_compare_equal() {
        let mut b = RowBuilder::new(5);
        b.push("a", CellKind::Normal, Style::default());
        b.push(
            " ",
            CellKind::Normal,
            Style {
                attrs: Attrs::BOLD,
                ..Default::default()
            },
        );
        b.push(" ", CellKind::Normal, Style::default());
        let r = b.finish(false);
        assert_eq!(r.content_len(), 2);
        let mut b = RowBuilder::new(5);
        b.push("a", CellKind::Normal, Style::default());
        b.push(
            " ",
            CellKind::Normal,
            Style {
                attrs: Attrs::BOLD,
                ..Default::default()
            },
        );
        assert_eq!(r, b.finish(false));
    }

    #[test]
    fn history_numbers_lines_absolutely() {
        let mut h = History::new(300);
        for i in 0..1000 {
            h.push(Row::from_text(&i.to_string(), 10));
        }
        assert_eq!(h.len(), 300);
        assert_eq!(h.start(), 700);
        assert_eq!(h.end(), 1000);
        assert_eq!(h.get_abs(700).unwrap().text(), "700");
        assert_eq!(h.get(299).unwrap().text(), "999");
        assert_eq!(h.iter().count(), 300);
        assert_eq!(h.iter().next().unwrap().text(), "700");
        assert_eq!(h.since(990).unwrap().len(), 10);
        assert_eq!(h.since(600).unwrap_err(), 100);
        let snapshot = h.clone();
        h.push(Row::from_text("x", 10));
        assert_eq!(snapshot.len(), 300);
        assert_eq!(snapshot.get(299).unwrap().text(), "999");
        h.clear();
        assert_eq!(h.start(), 1001);
        assert!(h.is_empty());
    }

    #[test]
    fn diff_reports_changed_rows() {
        let a = Screen::from_text(Size::new(3, 10), "one\ntwo\nthree");
        let b = Screen::from_text(Size::new(3, 10), "one\nTWO\nthree");
        assert_eq!(b.changed_rows(&a), vec![1]);
        assert!(!b.diff(&a).is_empty());
        assert!(a.diff(&a).is_empty());
    }
}

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

impl Rect {
    pub const fn new(row: u16, col: u16, rows: u16, cols: u16) -> Self {
        Self { row, col, rows, cols }
    }

    pub fn bottom(&self) -> u16 {
        self.row + self.rows
    }

    pub fn right(&self) -> u16 {
        self.col + self.cols
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        row >= self.row && row < self.bottom() && col >= self.col && col < self.right()
    }

    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.row >= self.row
            && other.col >= self.col
            && other.bottom() <= self.bottom()
            && other.right() <= self.right()
    }

    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let row = self.row.max(other.row);
        let col = self.col.max(other.col);
        let bottom = self.bottom().min(other.bottom());
        let right = self.right().min(other.right());
        (row < bottom && col < right).then(|| Rect::new(row, col, bottom - row, right - col))
    }
}
