//! The terminal emulator: `alacritty_terminal` with our own scrollback, synchronized updates
//! and snapshots.
//!
//! Scrollback is kept outside alacritty. After every piece of input of at most
//! [`HARVEST_CHUNK`] bytes, the lines that scrolled into alacritty's history are copied into our
//! [`History`] and alacritty's history is cleared. Its history therefore never fills up, the
//! number of new lines is always exact, and no line is lost however much output arrives at once.
//! Every such line is also kept in a log the owner drains with [`Emulator::take_scrolled`], so
//! a consumer of settled snapshots sees each line once even when more lines arrived between two
//! snapshots than the scrollback holds.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as ACell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AColor, CursorShape as ACursorShape, NamedColor, Processor, Rgb,
};

use crate::scan::{Class, Item, Scanner};
use crate::screen::{
    Attrs, CellKind, Color, Cursor, CursorShape, History, KittyFlags, Modes, MouseEncoding, MouseMode, Row,
    RowBuilder, SavedCursor, SavedScreen, Screen, ScreenData, Size, Style,
};

/// Input is handed to the parser in pieces of at most this many bytes, and scrollback is
/// collected after each. Alacritty's own history is sized so it cannot overflow in one piece.
pub const HARVEST_CHUNK: usize = 512;
/// Alacritty's internal history limit. One piece can scroll at most about
/// `HARVEST_CHUNK / 3 * rows` lines (`CSI S` with a large count), which this covers for screens
/// of up to a few hundred rows.
const INTERNAL_HISTORY: usize = 200_000;
/// A synchronized update that grows beyond this is applied without waiting for its end.
const SYNC_BUFFER_LIMIT: usize = 2 * 1024 * 1024;

/// Default colours the emulator reports when an application asks (OSC 10, 11, 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub foreground: (u8, u8, u8),
    pub background: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: (0xd8, 0xd8, 0xd8),
            background: (0x16, 0x16, 0x1a),
            cursor: (0xd8, 0xd8, 0xd8),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EmulatorConfig {
    pub size: Size,
    /// Lines of scrollback kept for snapshots. Default 10 000.
    pub scrollback: usize,
    /// Most lines kept in the log of scrolled lines between two [`Emulator::take_scrolled`]
    /// calls. Older ones are counted as lost. Default 1 000 000.
    pub scrolled_log_limit: usize,
    /// Answer the kitty keyboard protocol's queries and track its flags. Default on.
    pub kitty_keyboard: bool,
    /// Take a snapshot at the end of each synchronized update. Default on.
    pub snapshot_on_sync_end: bool,
    pub palette: Palette,
    /// Cell size in pixels reported to applications that ask for the window size.
    pub cell_pixels: (u16, u16),
}

impl Default for EmulatorConfig {
    fn default() -> Self {
        Self {
            size: Size::default(),
            scrollback: 10_000,
            scrolled_log_limit: 1_000_000,
            kitty_keyboard: true,
            snapshot_on_sync_end: true,
            palette: Palette::default(),
            cell_pixels: (8, 16),
        }
    }
}

impl EmulatorConfig {
    pub fn with_size(size: Size) -> Self {
        Self {
            size,
            ..Default::default()
        }
    }
}

/// What happened during one [`Emulator::feed`].
#[derive(Clone, Default)]
pub struct Fed {
    /// A synchronized update began (and is still open, unless `sync_ended` is also set).
    pub sync_began: bool,
    /// Number of synchronized updates that ended.
    pub sync_ended: u32,
    /// The screen at the end of the last synchronized update that ended, if snapshots on sync
    /// end are on.
    pub sync_frame: Option<Screen>,
    /// The visible state changed.
    pub changed: bool,
}

#[derive(Default)]
struct Events {
    events: Vec<Event>,
}

#[derive(Clone)]
struct Listener(Arc<Mutex<Events>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Ok(mut e) = self.0.lock() {
            e.events.push(event);
        }
    }
}

struct TermSize {
    rows: usize,
    cols: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// A terminal emulator fed with bytes.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    events: Arc<Mutex<Events>>,
    config: EmulatorConfig,
    scanner: Scanner,
    sync: Option<Vec<u8>>,
    history: History,
    scrolled: Vec<Row>,
    scrolled_lost: u64,
    title: Option<Arc<str>>,
    bell_count: u64,
    responses: Vec<u8>,
    received: u64,
    applied: u64,
    version: u64,
    saved_primary: Option<SavedScreen>,
    scroll_region: Option<(u16, u16)>,
    row_cache: Vec<Row>,
    screen_cache: Option<Screen>,
}

impl Emulator {
    pub fn new(config: EmulatorConfig) -> Self {
        let events = Arc::new(Mutex::new(Events::default()));
        let size = clamp_size(config.size);
        let term_config = Config {
            scrolling_history: INTERNAL_HISTORY,
            kitty_keyboard: config.kitty_keyboard,
            ..Config::default()
        };
        let term = Term::new(
            term_config,
            &TermSize {
                rows: size.rows as usize,
                cols: size.cols as usize,
            },
            Listener(events.clone()),
        );
        Self {
            term,
            parser: Processor::new(),
            events,
            history: History::new(config.scrollback),
            config: EmulatorConfig { size, ..config },
            scanner: Scanner::default(),
            sync: None,
            scrolled: Vec::new(),
            scrolled_lost: 0,
            title: None,
            bell_count: 0,
            responses: Vec::new(),
            received: 0,
            applied: 0,
            version: 1,
            saved_primary: None,
            scroll_region: None,
            row_cache: Vec::new(),
            screen_cache: None,
        }
    }

    pub fn config(&self) -> &EmulatorConfig {
        &self.config
    }

    /// Starts counting output offsets at `offset` instead of 0, for an emulator rebuilt from a
    /// transcript that does not start at the beginning. Only before anything was fed.
    pub fn set_base_offset(&mut self, offset: u64) {
        debug_assert_eq!(self.received, 0, "set_base_offset before feeding");
        if self.received == 0 {
            self.received = offset;
            self.applied = offset;
            self.screen_cache = None;
        }
    }

    pub fn size(&self) -> Size {
        self.config.size
    }

    /// Feeds output from the application.
    pub fn feed(&mut self, bytes: &[u8]) -> Fed {
        let before = self.version;
        let mut fed = Fed::default();
        self.received += bytes.len() as u64;
        let mut scanner = std::mem::take(&mut self.scanner);
        scanner.scan(bytes, &mut |item| match item {
            Item::Data(d) => match &mut self.sync {
                Some(buf) => buf.extend_from_slice(d),
                None => self.apply(d),
            },
            Item::Seq(s, Class::SyncBegin) => match &mut self.sync {
                // A new BSU inside an update extends it.
                Some(buf) => buf.extend_from_slice(s),
                None => {
                    self.sync = Some(s.to_vec());
                    fed.sync_began = true;
                    self.version += 1;
                }
            },
            Item::Seq(s, Class::SyncEnd) => match self.sync.take() {
                Some(mut buf) => {
                    buf.extend_from_slice(s);
                    self.apply_sync_buffer(&buf);
                    fed.sync_ended += 1;
                    if self.config.snapshot_on_sync_end {
                        fed.sync_frame = Some(self.screen());
                    }
                }
                None => self.apply(s),
            },
            Item::Seq(s, class) => match &mut self.sync {
                Some(buf) => buf.extend_from_slice(s),
                None => self.apply_seq(s, class),
            },
        });
        self.scanner = scanner;
        if self.sync.as_ref().is_some_and(|b| b.len() > SYNC_BUFFER_LIMIT) {
            self.flush_sync();
        }
        fed.changed = self.version != before;
        fed
    }

    /// Whether a synchronized update is open: its output is buffered and not yet visible.
    pub fn in_sync(&self) -> bool {
        self.sync.is_some()
    }

    /// Applies an open synchronized update now, as if it had ended. Call this when the update
    /// has been open too long (alacritty uses 150 ms).
    pub fn flush_sync(&mut self) -> bool {
        match self.sync.take() {
            Some(buf) => {
                self.apply_sync_buffer(&buf);
                true
            }
            None => false,
        }
    }

    /// Changes the size. Lines pushed off the top go into the scrollback.
    pub fn resize(&mut self, size: Size) {
        let size = clamp_size(size);
        if size == self.config.size {
            return;
        }
        self.config.size = size;
        self.term.resize(TermSize {
            rows: size.rows as usize,
            cols: size.cols as usize,
        });
        self.scroll_region = None;
        if let Some(saved) = &mut self.saved_primary {
            // Approximation: alacritty reflows the hidden primary screen, we only crop or pad.
            let rows = size.rows as usize;
            if saved.rows.len() > rows {
                let cut = saved.rows.len() - rows;
                saved.rows.drain(..cut);
                saved.cursor.row = saved.cursor.row.saturating_sub(cut as u16);
            }
            saved.rows.resize_with(rows, || Row::blank(size.cols));
            for row in &mut saved.rows {
                *row = row.with_cols(size.cols);
            }
            saved.cursor.row = saved.cursor.row.min(size.rows - 1);
            saved.cursor.col = saved.cursor.col.min(size.cols - 1);
        }
        self.harvest();
        self.version += 1;
    }

    /// Bytes the terminal wants to send back to the application (answers to queries such as
    /// cursor position or device attributes). The owner writes them to the application.
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    /// Lines that scrolled into the scrollback since the last call, oldest first, and how many
    /// more were dropped because the log limit was reached.
    pub fn take_scrolled(&mut self) -> (Vec<Row>, u64) {
        (
            std::mem::take(&mut self.scrolled),
            std::mem::take(&mut self.scrolled_lost),
        )
    }

    /// Total bytes fed.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Bytes whose effect is visible: received bytes minus an incomplete escape sequence at the
    /// end and an open synchronized update.
    pub fn applied(&self) -> u64 {
        self.applied
    }

    /// Total lines that ever scrolled into the scrollback.
    pub fn line_count(&self) -> u64 {
        self.history.end()
    }

    pub fn modes(&self) -> Modes {
        let m = *self.term.mode();
        Modes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            app_keypad: m.contains(TermMode::APP_KEYPAD),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse: if m.contains(TermMode::MOUSE_MOTION) {
                MouseMode::Motion
            } else if m.contains(TermMode::MOUSE_DRAG) {
                MouseMode::Drag
            } else if m.contains(TermMode::MOUSE_REPORT_CLICK) {
                MouseMode::Click
            } else {
                MouseMode::None
            },
            mouse_encoding: if m.contains(TermMode::SGR_MOUSE) {
                MouseEncoding::Sgr
            } else if m.contains(TermMode::UTF8_MOUSE) {
                MouseEncoding::Utf8
            } else {
                MouseEncoding::Default
            },
            focus_events: m.contains(TermMode::FOCUS_IN_OUT),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            synchronized_update: self.sync.is_some(),
            line_wrap: m.contains(TermMode::LINE_WRAP),
            origin: m.contains(TermMode::ORIGIN),
            insert: m.contains(TermMode::INSERT),
            newline: m.contains(TermMode::LINE_FEED_NEW_LINE),
            alternate_scroll: m.contains(TermMode::ALTERNATE_SCROLL),
            kitty: {
                let mut k = KittyFlags::empty();
                k.set(
                    KittyFlags::DISAMBIGUATE,
                    m.contains(TermMode::DISAMBIGUATE_ESC_CODES),
                );
                k.set(
                    KittyFlags::REPORT_EVENT_TYPES,
                    m.contains(TermMode::REPORT_EVENT_TYPES),
                );
                k.set(
                    KittyFlags::REPORT_ALTERNATE_KEYS,
                    m.contains(TermMode::REPORT_ALTERNATE_KEYS),
                );
                k.set(
                    KittyFlags::REPORT_ALL_KEYS_AS_ESC,
                    m.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
                );
                k.set(
                    KittyFlags::REPORT_ASSOCIATED_TEXT,
                    m.contains(TermMode::REPORT_ASSOCIATED_TEXT),
                );
                k
            },
        }
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn bell_count(&self) -> u64 {
        self.bell_count
    }

    /// Changes whenever the state changes.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// A snapshot of the current state. Cached until the next change.
    pub fn screen(&mut self) -> Screen {
        if let Some(s) = &self.screen_cache
            && s.version() == self.version
        {
            return s.clone();
        }
        let size = self.config.size;
        let mut rows = Vec::with_capacity(size.rows as usize);
        for line in 0..size.rows as i32 {
            let row = convert_row(&self.term.grid()[Line(line)], size.cols);
            let row = match self.row_cache.get(line as usize) {
                Some(old) if *old == row => old.clone(),
                _ => row,
            };
            rows.push(row);
        }
        self.row_cache = rows.clone();
        let screen = Screen(Arc::new(ScreenData {
            size,
            rows,
            cursor: self.cursor(),
            modes: self.modes(),
            title: self.title.clone(),
            history: self.history.clone(),
            bell_count: self.bell_count,
            version: self.version,
            offset: self.applied,
            saved_primary: if self.term.mode().contains(TermMode::ALT_SCREEN) {
                self.saved_primary.clone()
            } else {
                None
            },
            scroll_region: self.scroll_region,
            pen: convert_style(&self.term.grid().cursor.template),
            saved_cursor: {
                let saved = &self.term.grid().saved_cursor;
                let pen = convert_style(&saved.template);
                if saved.point.line.0 == 0 && saved.point.column.0 == 0 && pen.is_default() {
                    None
                } else {
                    Some(SavedCursor {
                        row: saved.point.line.0.max(0) as u16,
                        col: saved.point.column.0 as u16,
                        pen,
                    })
                }
            },
        }));
        self.screen_cache = Some(screen.clone());
        screen
    }

    fn cursor(&self) -> Cursor {
        let grid = self.term.grid();
        let style = self.term.cursor_style();
        let shape = match style.shape {
            ACursorShape::Block | ACursorShape::Hidden => CursorShape::Block,
            ACursorShape::Underline => CursorShape::Underline,
            ACursorShape::Beam => CursorShape::Bar,
            ACursorShape::HollowBlock => CursorShape::HollowBlock,
        };
        Cursor {
            row: grid.cursor.point.line.0.max(0) as u16,
            col: grid.cursor.point.column.0 as u16,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR) && style.shape != ACursorShape::Hidden,
            shape,
            blinking: style.blinking,
            pending_wrap: grid.cursor.input_needs_wrap,
        }
    }

    fn apply_sync_buffer(&mut self, buf: &[u8]) {
        // The buffer holds complete sequences only; scan it again for the sequences we split at.
        let mut scanner = Scanner::default();
        scanner.scan(buf, &mut |item| match item {
            Item::Data(d) => self.apply(d),
            Item::Seq(s, Class::SyncBegin | Class::SyncEnd) => self.applied += s.len() as u64,
            Item::Seq(s, class) => self.apply_seq(s, class),
        });
        let rest = scanner.held();
        if rest > 0 {
            // Cannot happen with buffers built by `feed`; count the bytes anyway.
            self.applied += rest as u64;
        }
        self.version += 1;
    }

    fn apply_seq(&mut self, seq: &[u8], class: Class) {
        match class {
            Class::AltEnter => {
                if !self.term.mode().contains(TermMode::ALT_SCREEN) {
                    let size = self.config.size;
                    let rows = (0..size.rows as i32)
                        .map(|l| convert_row(&self.term.grid()[Line(l)], size.cols))
                        .collect();
                    self.saved_primary = Some(SavedScreen {
                        rows,
                        cursor: self.cursor(),
                    });
                }
                self.apply(seq);
            }
            Class::ClearHistory => {
                self.apply(seq);
                if !self.term.mode().contains(TermMode::ALT_SCREEN) {
                    self.history.clear();
                }
            }
            Class::Reset => {
                self.apply(seq);
                self.history.clear();
                self.saved_primary = None;
                self.scroll_region = None;
                self.title = None;
            }
            Class::ScrollRegion(top, bottom) => {
                self.apply(seq);
                // Same rules as alacritty's `set_scrolling_region`.
                let rows = self.config.size.rows;
                let top = top.unwrap_or(1).max(1);
                let bottom = bottom.unwrap_or(rows).min(rows);
                if top < bottom {
                    self.scroll_region = if top == 1 && bottom == rows {
                        None
                    } else {
                        Some((top - 1, bottom - 1))
                    };
                }
            }
            Class::SyncBegin | Class::SyncEnd => self.apply(seq),
        }
    }

    fn apply(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        for piece in bytes.chunks(HARVEST_CHUNK) {
            self.parser.advance(&mut self.term, piece);
            self.harvest();
            self.applied += piece.len() as u64;
        }
        // An open sync update inside the parser (only from sequences we do not recognise, such
        // as `CSI ? 2026 ; 1 h`) is applied right away; we do our own synchronisation.
        if self.parser.sync_timeout().sync_timeout().is_some() {
            self.parser.stop_sync(&mut self.term);
            self.harvest();
        }
        self.process_events();
        self.version += 1;
    }

    fn harvest(&mut self) {
        let grid = self.term.grid();
        let n = grid.history_size();
        if n == 0 {
            return;
        }
        let cols = self.config.size.cols;
        for line in (1..=n as i32).rev() {
            let row = convert_row(&grid[Line(-line)], cols);
            self.history.push(row.clone());
            if self.scrolled.len() < self.config.scrolled_log_limit {
                self.scrolled.push(row);
            } else {
                self.scrolled_lost += 1;
            }
        }
        self.term.grid_mut().clear_history();
    }

    fn process_events(&mut self) {
        let events = match self.events.lock() {
            Ok(mut e) => std::mem::take(&mut e.events),
            Err(_) => return,
        };
        for event in events {
            match event {
                Event::Title(t) => self.title = Some(Arc::from(t)),
                Event::ResetTitle => self.title = None,
                Event::Bell => self.bell_count += 1,
                Event::PtyWrite(s) => self.responses.extend_from_slice(s.as_bytes()),
                Event::ColorRequest(index, format) => {
                    let rgb = self.color_for(index);
                    self.responses.extend_from_slice(format(rgb).as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = self.config.size;
                    let (w, h) = self.config.cell_pixels;
                    let s = format(WindowSize {
                        num_lines: size.rows,
                        num_cols: size.cols,
                        cell_width: w,
                        cell_height: h,
                    });
                    self.responses.extend_from_slice(s.as_bytes());
                }
                _ => {}
            }
        }
    }

    fn color_for(&self, index: usize) -> Rgb {
        if let Some(c) = self.term.colors()[index] {
            return c;
        }
        let p = self.config.palette;
        let rgb = |(r, g, b): (u8, u8, u8)| Rgb { r, g, b };
        match index {
            0..=255 => {
                let (r, g, b) = xterm_color(index as u8);
                Rgb { r, g, b }
            }
            i if i == NamedColor::Foreground as usize => rgb(p.foreground),
            i if i == NamedColor::Background as usize => rgb(p.background),
            i if i == NamedColor::Cursor as usize => rgb(p.cursor),
            _ => rgb(p.foreground),
        }
    }
}

fn clamp_size(size: Size) -> Size {
    Size {
        rows: size.rows.max(1),
        cols: size.cols.max(2),
    }
}

fn convert_row(row: &alacritty_terminal::grid::Row<ACell>, cols: u16) -> Row {
    let mut b = RowBuilder::new(cols);
    let mut text = String::new();
    let mut wrapped = false;
    for col in 0..cols as usize {
        let cell = &row[Column(col)];
        let style = convert_style(cell);
        if col + 1 == cols as usize {
            wrapped = cell.flags.contains(Flags::WRAPLINE);
        }
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            b.push("", CellKind::WideTail, style);
            continue;
        }
        if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            b.push("", CellKind::WrapSpacer, style);
            continue;
        }
        text.clear();
        text.push(if cell.c == '\0' { ' ' } else { cell.c });
        if let Some(zw) = cell.zerowidth() {
            text.extend(zw.iter());
        }
        let kind = if cell.flags.contains(Flags::WIDE_CHAR) {
            CellKind::Wide
        } else {
            CellKind::Normal
        };
        b.push(&text, kind, style);
    }
    b.finish(wrapped)
}

fn convert_style(cell: &ACell) -> Style {
    let f = cell.flags;
    let mut attrs = Attrs::empty();
    attrs.set(Attrs::BOLD, f.contains(Flags::BOLD));
    attrs.set(Attrs::DIM, f.contains(Flags::DIM));
    attrs.set(Attrs::ITALIC, f.contains(Flags::ITALIC));
    attrs.set(Attrs::UNDERLINE, f.contains(Flags::UNDERLINE));
    attrs.set(Attrs::DOUBLE_UNDERLINE, f.contains(Flags::DOUBLE_UNDERLINE));
    attrs.set(Attrs::UNDERCURL, f.contains(Flags::UNDERCURL));
    attrs.set(Attrs::DOTTED_UNDERLINE, f.contains(Flags::DOTTED_UNDERLINE));
    attrs.set(Attrs::DASHED_UNDERLINE, f.contains(Flags::DASHED_UNDERLINE));
    attrs.set(Attrs::REVERSE, f.contains(Flags::INVERSE));
    attrs.set(Attrs::STRIKETHROUGH, f.contains(Flags::STRIKEOUT));
    attrs.set(Attrs::HIDDEN, f.contains(Flags::HIDDEN));
    Style {
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        underline_color: cell.underline_color().map(convert_color),
        attrs,
    }
}

fn convert_color(c: AColor) -> Color {
    match c {
        AColor::Spec(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        AColor::Indexed(i) => Color::Indexed(i),
        AColor::Named(n) => {
            let i = n as usize;
            if i < 16 {
                Color::Indexed(i as u8)
            } else {
                match n {
                    NamedColor::DimBlack => Color::Indexed(0),
                    NamedColor::DimRed => Color::Indexed(1),
                    NamedColor::DimGreen => Color::Indexed(2),
                    NamedColor::DimYellow => Color::Indexed(3),
                    NamedColor::DimBlue => Color::Indexed(4),
                    NamedColor::DimMagenta => Color::Indexed(5),
                    NamedColor::DimCyan => Color::Indexed(6),
                    NamedColor::DimWhite => Color::Indexed(7),
                    _ => Color::Default,
                }
            }
        }
    }
}

/// The xterm default for palette entry `i`.
pub fn xterm_color(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

/// How long alacritty waits for a synchronized update to end before showing it anyway.
pub const DEFAULT_SYNC_TIMEOUT: Duration = Duration::from_millis(150);

#[cfg(test)]
mod tests {
    use super::*;

    fn emu(rows: u16, cols: u16) -> Emulator {
        Emulator::new(EmulatorConfig::with_size(Size::new(rows, cols)))
    }

    #[test]
    fn plain_text_and_cursor() {
        let mut e = emu(5, 20);
        e.feed(b"hello\r\nworld");
        let s = e.screen();
        assert_eq!(s.lines()[..2], ["hello", "world"]);
        assert_eq!((s.cursor().row, s.cursor().col), (1, 5));
        assert!(s.cursor().visible);
    }

    #[test]
    fn combining_marks_are_kept() {
        // Regression: novalia's screen model dropped combining marks.
        let mut e = emu(3, 20);
        e.feed("e\u{301}a\u{308}\u{20d7} n\u{303}o ❤\u{fe0f}".as_bytes());
        let s = e.screen();
        let row = s.row(0).unwrap();
        assert_eq!(row.cell(0).text(), "e\u{301}");
        assert_eq!(row.cell(1).text(), "a\u{308}\u{20d7}");
        assert_eq!(row.cell(3).text(), "n\u{303}");
        assert_eq!(row.text(), "e\u{301}a\u{308}\u{20d7} n\u{303}o ❤\u{fe0f}");
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut e = emu(3, 5);
        e.feed("a界b🎉x".as_bytes());
        let s = e.screen();
        let r0 = s.row(0).unwrap();
        assert_eq!(r0.cell(1).kind(), CellKind::Wide);
        assert_eq!(r0.cell(2).kind(), CellKind::WideTail);
        assert_eq!(r0.cell(3).text(), "b");
        // 🎉 does not fit in column 4: a spacer is left and it wraps.
        assert_eq!(r0.cell(4).kind(), CellKind::WrapSpacer);
        assert!(r0.is_wrapped());
        assert_eq!(s.row(1).unwrap().text(), "🎉x");
    }

    #[test]
    fn history_survives_large_bursts() {
        // Regression: novalia lost history when more than 1000 lines arrived between frames.
        let mut e = Emulator::new(EmulatorConfig {
            size: Size::new(10, 40),
            scrollback: 10_000,
            ..Default::default()
        });
        let mut out = String::new();
        for i in 0..25_000 {
            out.push_str(&format!("line {i}\r\n"));
        }
        e.feed(out.as_bytes());
        let s = e.screen();
        // 25 000 lines, 9 of them still visible plus the empty cursor row.
        assert_eq!(s.first_line(), 25_000 - 9);
        assert_eq!(s.history().len(), 10_000);
        assert_eq!(s.history().get(9_999).unwrap().text(), "line 24990");
        assert_eq!(s.row(0).unwrap().text(), "line 24991");
        let (scrolled, lost) = e.take_scrolled();
        assert_eq!(lost, 0);
        assert_eq!(scrolled.len(), 24_991);
        for (i, r) in scrolled.iter().enumerate() {
            assert_eq!(r.text(), format!("line {i}"));
        }
    }

    #[test]
    fn scroll_up_sequences_count_lines() {
        let mut e = emu(5, 10);
        e.feed(b"a\r\nb\r\nc\x1b[100S");
        assert_eq!(e.line_count(), 5);
        let s = e.screen();
        let h: Vec<_> = s.history().iter().map(Row::text).collect();
        assert_eq!(h, ["a", "b", "c", "", ""]);
    }

    #[test]
    fn synchronized_updates_are_buffered() {
        let mut e = emu(3, 20);
        e.feed(b"before");
        let fed = e.feed(b"\x1b[?2026h\x1b[2J\x1b[Hafter");
        assert!(fed.sync_began);
        assert!(e.in_sync());
        let s = e.screen();
        assert_eq!(s.lines()[0], "before");
        assert!(s.modes().synchronized_update);
        assert_eq!(s.offset(), 6);
        let fed = e.feed(b" more\x1b[?2026l");
        assert_eq!(fed.sync_ended, 1);
        assert_eq!(fed.sync_frame.unwrap().lines()[0], "after more");
        assert!(!e.in_sync());
        assert_eq!(e.applied(), e.received());
    }

    #[test]
    fn sync_split_across_feeds_and_flush() {
        let mut e = emu(3, 20);
        e.feed(b"\x1b[?20");
        e.feed(b"26hx");
        assert!(e.in_sync());
        assert_eq!(e.screen().lines()[0], "");
        assert!(e.flush_sync());
        assert_eq!(e.screen().lines()[0], "x");
    }

    #[test]
    fn modes_title_bell() {
        let mut e = emu(3, 20);
        e.feed(
            b"\x1b]2;my title\x07\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x07\x1b[?25l\x1b[>5u",
        );
        let s = e.screen();
        let m = s.modes();
        assert_eq!(s.title(), Some("my title"));
        assert!(m.app_cursor && m.bracketed_paste && m.focus_events);
        assert_eq!(m.mouse, MouseMode::Drag);
        assert_eq!(m.mouse_encoding, MouseEncoding::Sgr);
        assert_eq!(
            m.kitty,
            KittyFlags::DISAMBIGUATE | KittyFlags::REPORT_ALTERNATE_KEYS
        );
        assert_eq!(s.bell_count(), 1);
        assert!(!s.cursor().visible);
    }

    #[test]
    fn alt_screen_keeps_primary_and_history() {
        let mut e = emu(3, 20);
        e.feed(b"one\r\ntwo\r\nthree\r\nfour");
        e.feed(b"\x1b[?1049h\x1b[Hvim here");
        let s = e.screen();
        assert!(s.modes().alt_screen);
        assert_eq!(s.lines()[0], "vim here");
        let saved = s.saved_primary().unwrap();
        assert_eq!(saved.rows[2].text(), "four");
        assert_eq!(s.history().iter().map(Row::text).collect::<Vec<_>>(), ["one"]);
        e.feed(b"\x1b[?1049l");
        let s = e.screen();
        assert_eq!(s.lines(), ["two", "three", "four"]);
        assert!(s.saved_primary().is_none());
    }

    #[test]
    fn clear_scrollback_and_reset() {
        let mut e = emu(2, 10);
        e.feed(b"a\r\nb\r\nc\r\n");
        assert_eq!(e.screen().history().len(), 2);
        e.feed(b"\x1b[3J");
        assert_eq!(e.screen().history().len(), 0);
        assert_eq!(e.line_count(), 2);
        e.feed(b"d\r\ne\r\n\x1bc");
        assert_eq!(e.screen().history().len(), 0);
    }

    #[test]
    fn answers_queries() {
        let mut e = emu(5, 10);
        e.feed(b"ab\x1b[6n\x1b]11;?\x07\x1b[?2026$p\x1b[?u");
        let r = String::from_utf8(e.take_responses()).unwrap();
        assert!(r.contains("\x1b[1;3R"), "{r:?}");
        assert!(r.contains("]11;rgb:"), "{r:?}");
        assert!(r.contains("\x1b[?2026;2$y"), "{r:?}");
        assert!(r.contains("\x1b[?0u"), "{r:?}");
    }

    #[test]
    fn rows_are_shared_between_snapshots() {
        let mut e = emu(3, 10);
        e.feed(b"a\r\nb");
        let s1 = e.screen();
        e.feed(b"c");
        let s2 = e.screen();
        assert!(s1.row(0).unwrap().ptr_eq(s2.row(0).unwrap()));
        assert_eq!(s2.changed_rows(&s1), vec![1]);
    }

    #[test]
    fn scroll_region_is_tracked() {
        let mut e = emu(10, 10);
        e.feed(b"\x1b[2;8r");
        assert_eq!(e.screen().scroll_region(), Some((1, 7)));
        e.feed(b"\x1b[r");
        assert_eq!(e.screen().scroll_region(), None);
    }

    #[test]
    fn resize_moves_lines_into_history() {
        let mut e = emu(5, 10);
        e.feed(b"1\r\n2\r\n3\r\n4\r\n5");
        e.resize(Size::new(2, 10));
        let s = e.screen();
        assert_eq!(s.lines(), ["4", "5"]);
        assert_eq!(
            s.history().iter().map(Row::text).collect::<Vec<_>>(),
            ["1", "2", "3"]
        );
    }
}
