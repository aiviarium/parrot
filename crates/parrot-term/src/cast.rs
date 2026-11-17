//! asciinema v2 casts: writing, reading, and replaying through the emulator.
//!
//! A replay feeds the cast's events into an [`Emulator`] on a virtual clock taken from the
//! event timestamps and yields the settled snapshots a live session would have produced, with
//! the same settle rules, without real time passing. The same cast always gives the same frames.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::emulator::{Emulator, EmulatorConfig};
use crate::screen::{Row, Screen, Size};
use crate::settle::{SettleConfig, SettleReason, Settler};

/// The first line of a cast.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CastHeader {
    pub version: u8,
    pub width: u16,
    pub height: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_time_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

impl CastHeader {
    pub fn new(size: Size) -> Self {
        Self {
            version: 2,
            width: size.cols,
            height: size.rows,
            timestamp: None,
            idle_time_limit: None,
            title: None,
            command: None,
            env: None,
        }
    }

    pub fn size(&self) -> Size {
        Size::new(self.height, self.width)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// `o`: output from the application.
    Output,
    /// `i`: input to the application.
    Input,
    /// `r`: resize to `COLSxROWS`.
    Resize,
    /// `m`: a marker with a label.
    Marker,
}

impl EventKind {
    fn code(self) -> &'static str {
        match self {
            EventKind::Output => "o",
            EventKind::Input => "i",
            EventKind::Resize => "r",
            EventKind::Marker => "m",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CastEvent {
    /// Seconds since the start of the recording.
    pub time: f64,
    pub kind: EventKind,
    pub data: String,
}

impl CastEvent {
    /// The size of a resize event.
    pub fn size(&self) -> Option<Size> {
        if self.kind != EventKind::Resize {
            return None;
        }
        let (c, r) = self.data.split_once('x')?;
        Some(Size::new(r.trim().parse().ok()?, c.trim().parse().ok()?))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CastError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("line {line}: {message}")]
    Format { line: usize, message: String },
}

/// A whole cast in memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Cast {
    pub header: CastHeader,
    pub events: Vec<CastEvent>,
}

impl Cast {
    pub fn parse(text: &str) -> Result<Cast, CastError> {
        Self::read(text.as_bytes())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Cast, CastError> {
        Self::read(io::BufReader::new(std::fs::File::open(path)?))
    }

    pub fn read(reader: impl BufRead) -> Result<Cast, CastError> {
        let mut lines = reader.lines().enumerate();
        let fmt = |line: usize, message: String| CastError::Format {
            line: line + 1,
            message,
        };
        let header = loop {
            match lines.next() {
                Some((i, line)) => {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let h: CastHeader =
                        serde_json::from_str(&line).map_err(|e| fmt(i, format!("header: {e}")))?;
                    if h.version != 2 {
                        return Err(fmt(i, format!("unsupported version {}", h.version)));
                    }
                    break h;
                }
                None => return Err(fmt(0, "empty cast".into())),
            }
        };
        let mut events = Vec::new();
        for (i, line) in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let (time, code, data): (f64, String, String) =
                serde_json::from_str(&line).map_err(|e| fmt(i, format!("event: {e}")))?;
            let kind = match code.as_str() {
                "o" => EventKind::Output,
                "i" => EventKind::Input,
                "r" => EventKind::Resize,
                "m" => EventKind::Marker,
                // Unknown event types are skipped, as asciinema's player does.
                _ => continue,
            };
            events.push(CastEvent { time, kind, data });
        }
        Ok(Cast { header, events })
    }

    pub fn write(&self, w: impl Write) -> io::Result<()> {
        let mut cw = CastWriter::new(w, &self.header)?;
        for e in &self.events {
            cw.event(e.time, e.kind, &e.data)?;
        }
        cw.flush()
    }

    pub fn to_text(&self) -> String {
        let mut v = Vec::new();
        let _ = self.write(&mut v);
        String::from_utf8(v).unwrap_or_default()
    }

    /// Markers as `(time, label)`.
    pub fn markers(&self) -> Vec<(f64, String)> {
        self.events
            .iter()
            .filter(|e| e.kind == EventKind::Marker)
            .map(|e| (e.time, e.data.clone()))
            .collect()
    }

    /// Time of the last event.
    pub fn duration(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.time)
    }
}

/// Decodes UTF-8 across chunk boundaries, replacing invalid bytes.
#[derive(Default)]
struct Utf8Carry(Vec<u8>);

impl Utf8Carry {
    fn decode(&mut self, bytes: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.0);
        buf.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest = &buf[..];
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{fffd}');
                            rest = &after[n..];
                        }
                        None => {
                            // Incomplete character at the end: keep it for the next chunk.
                            self.0 = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

/// Writes a cast as events arrive.
pub struct CastWriter<W: Write> {
    w: W,
    out: Utf8Carry,
    input: Utf8Carry,
}

impl<W: Write> CastWriter<W> {
    pub fn new(mut w: W, header: &CastHeader) -> io::Result<Self> {
        serde_json::to_writer(&mut w, header)?;
        w.write_all(b"\n")?;
        Ok(Self {
            w,
            out: Utf8Carry::default(),
            input: Utf8Carry::default(),
        })
    }

    pub fn output(&mut self, time: f64, bytes: &[u8]) -> io::Result<()> {
        let s = self.out.decode(bytes);
        if s.is_empty() {
            return Ok(());
        }
        self.event(time, EventKind::Output, &s)
    }

    pub fn input(&mut self, time: f64, bytes: &[u8]) -> io::Result<()> {
        let s = self.input.decode(bytes);
        if s.is_empty() {
            return Ok(());
        }
        self.event(time, EventKind::Input, &s)
    }

    pub fn resize(&mut self, time: f64, size: Size) -> io::Result<()> {
        self.event(time, EventKind::Resize, &format!("{}x{}", size.cols, size.rows))
    }

    pub fn marker(&mut self, time: f64, label: &str) -> io::Result<()> {
        self.event(time, EventKind::Marker, label)
    }

    pub fn event(&mut self, time: f64, kind: EventKind, data: &str) -> io::Result<()> {
        let data = serde_json::to_string(data)?;
        writeln!(self.w, "[{time:.6}, \"{}\", {data}]", kind.code())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }

    pub fn into_inner(self) -> W {
        self.w
    }
}

#[derive(Clone, Debug)]
pub struct ReplayConfig {
    /// Emulator settings; the size comes from the cast.
    pub emulator: EmulatorConfig,
    pub settle: SettleConfig,
    /// Leave out frames whose screen equals the previous frame's and that brought no new
    /// scrollback lines. Default on.
    pub dedup: bool,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            emulator: EmulatorConfig::default(),
            settle: SettleConfig::default(),
            dedup: true,
        }
    }
}

/// A settled snapshot from a replay or a live session.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Seconds since the start.
    pub time: f64,
    pub reason: SettleReason,
    pub screen: Screen,
    /// Lines that scrolled into the scrollback since the previous frame, oldest first.
    pub new_lines: Vec<Row>,
    /// Lines that scrolled since the previous frame but were dropped from the log.
    pub lost_lines: u64,
}

/// Replays a cast.
pub struct Replay<'a> {
    cast: &'a Cast,
    config: ReplayConfig,
}

impl<'a> Replay<'a> {
    pub fn new(cast: &'a Cast) -> Self {
        Self::with_config(cast, ReplayConfig::default())
    }

    pub fn with_config(cast: &'a Cast, config: ReplayConfig) -> Self {
        Self { cast, config }
    }

    /// All settled frames.
    pub fn frames(&self) -> Vec<Frame> {
        let mut frames = Vec::new();
        self.run(None, &mut |f| frames.push(f));
        frames
    }

    /// The screen after every event up to `time` seconds.
    pub fn screen_at(&self, time: f64) -> Screen {
        self.run(Some(time), &mut |_| {}).screen()
    }

    /// The last settled frame at or before `time`.
    pub fn frame_at(&self, time: f64) -> Option<Frame> {
        let mut last = None;
        self.run(Some(time), &mut |f| last = Some(f));
        last
    }

    /// The screen after the whole cast.
    pub fn final_screen(&self) -> Screen {
        self.run(None, &mut |_| {}).screen()
    }

    fn run(&self, until: Option<f64>, on_frame: &mut dyn FnMut(Frame)) -> Emulator {
        let mut cfg = self.config.emulator.clone();
        cfg.size = self.cast.header.size();
        let mut st = Timeline {
            emu: Emulator::new(cfg),
            settler: Settler::new(self.config.settle),
            last: None,
            dedup: self.config.dedup,
        };
        let secs = |t: f64| Duration::from_secs_f64(t.max(0.0));
        for ev in &self.cast.events {
            if until.is_some_and(|u| ev.time > u) {
                break;
            }
            let t = secs(ev.time);
            st.fire_timers(t, on_frame);
            match ev.kind {
                EventKind::Output => st.output(t, ev.data.as_bytes(), on_frame),
                EventKind::Resize => {
                    if let Some(size) = ev.size() {
                        st.emu.resize(size);
                        st.settler.touch(t);
                    }
                }
                EventKind::Input | EventKind::Marker => {}
            }
        }
        match until {
            Some(u) => st.fire_timers(secs(u), on_frame),
            None => st.fire_timers(Duration::MAX, on_frame),
        }
        st.emu
    }
}

/// Settling for a sequence of timed output, shared by replay and live sessions.
pub(crate) struct Timeline {
    pub emu: Emulator,
    pub settler: Settler,
    pub last: Option<Screen>,
    pub dedup: bool,
}

impl Timeline {
    pub fn output(&mut self, t: Duration, bytes: &[u8], on_frame: &mut dyn FnMut(Frame)) {
        let fed = self.emu.feed(bytes);
        if fed.sync_began {
            self.settler.sync_began(t);
        }
        if fed.sync_ended > 0 && !self.emu.in_sync() {
            self.settler.sync_ended();
        }
        match fed.sync_frame {
            Some(frame) => {
                self.settler.output(t, false);
                let more = self.emu.applied() > frame.offset() || self.emu.in_sync();
                self.emit(t, SettleReason::SyncEnd, frame, on_frame);
                if more && fed.changed {
                    self.settler.output(t, true);
                }
            }
            None => self.settler.output(t, fed.changed),
        }
    }

    pub fn fire_timers(&mut self, now: Duration, on_frame: &mut dyn FnMut(Frame)) {
        while let Some(d) = self.settler.deadline() {
            if d > now {
                break;
            }
            let Some(reason) = self.settler.poll(d) else {
                break;
            };
            if reason == SettleReason::SyncTimeout {
                self.emu.flush_sync();
                self.settler.sync_ended();
            }
            let screen = self.emu.screen();
            self.emit(d, reason, screen, on_frame);
        }
    }

    pub fn emit(
        &mut self,
        t: Duration,
        reason: SettleReason,
        screen: Screen,
        on_frame: &mut dyn FnMut(Frame),
    ) {
        self.settler.settled();
        let (new_lines, lost_lines) = self.emu.take_scrolled();
        if self.dedup
            && new_lines.is_empty()
            && lost_lines == 0
            && self.last.as_ref().is_some_and(|l| l.same_content(&screen))
        {
            return;
        }
        self.last = Some(screen.clone());
        on_frame(Frame {
            time: t.as_secs_f64(),
            reason,
            screen,
            new_lines,
            lost_lines,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cast(events: &[(f64, &str, &str)]) -> Cast {
        Cast {
            header: CastHeader::new(Size::new(4, 20)),
            events: events
                .iter()
                .map(|(t, k, d)| CastEvent {
                    time: *t,
                    kind: match *k {
                        "o" => EventKind::Output,
                        "i" => EventKind::Input,
                        "r" => EventKind::Resize,
                        _ => EventKind::Marker,
                    },
                    data: d.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn write_and_read_back() {
        let mut v = Vec::new();
        let mut w = CastWriter::new(&mut v, &CastHeader::new(Size::new(24, 80))).unwrap();
        w.output(0.1, b"hello \xe7\x95").unwrap();
        w.output(0.2, b"\x8c\x1b[1m\"q\"\n").unwrap();
        w.input(0.3, b"\x03").unwrap();
        w.resize(0.4, Size::new(30, 100)).unwrap();
        w.marker(0.5, "prompt").unwrap();
        w.output(0.6, b"bad \xff byte").unwrap();
        drop(w);
        let text = String::from_utf8(v).unwrap();
        assert!(
            text.starts_with("{\"version\":2,\"width\":80,\"height\":24}\n[0.100000, \"o\", \"hello \"]\n")
        );
        let c = Cast::parse(&text).unwrap();
        assert_eq!(c.events.len(), 6);
        assert_eq!(c.events[1].data, "界\u{1b}[1m\"q\"\n");
        assert_eq!(c.events[2].kind, EventKind::Input);
        assert_eq!(c.events[3].size(), Some(Size::new(30, 100)));
        assert_eq!(c.markers(), vec![(0.5, "prompt".to_string())]);
        assert_eq!(c.events[5].data, "bad \u{fffd} byte");
        assert_eq!(Cast::parse(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn replay_settles_on_quiet_and_sync_end() {
        let c = cast(&[
            (0.0, "o", "$ "),
            (1.0, "o", "l"),
            (1.05, "o", "s"),
            (2.0, "o", "\x1b[?2026h\x1b[2J\x1b[Hframe 1"),
            (2.01, "o", "\x1b[?2026l"),
            (3.0, "o", "\x1b[?2026hframe 2"),
            (5.0, "m", "end"),
        ]);
        let frames = Replay::new(&c).frames();
        let summary: Vec<(String, SettleReason, String)> = frames
            .iter()
            .map(|f| (format!("{:.2}", f.time), f.reason, f.screen.lines()[0].clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("0.10".into(), SettleReason::Quiet, "$".into()),
                ("1.15".into(), SettleReason::Quiet, "$ ls".into()),
                ("2.01".into(), SettleReason::SyncEnd, "frame 1".into()),
                ("3.15".into(), SettleReason::SyncTimeout, "frame 1frame 2".into()),
            ]
        );
        // Deterministic.
        let again: Vec<f64> = Replay::new(&c).frames().iter().map(|f| f.time).collect();
        assert_eq!(again, frames.iter().map(|f| f.time).collect::<Vec<_>>());
        assert_eq!(Replay::new(&c).screen_at(1.02).lines()[0], "$ l");
        assert_eq!(Replay::new(&c).frame_at(1.5).unwrap().screen.lines()[0], "$ ls");
    }

    #[test]
    fn replay_handles_resize_and_reports_new_lines() {
        let mut out = String::new();
        for i in 0..5000 {
            out.push_str(&format!("{i}\r\n"));
        }
        let c = cast(&[(0.0, "o", &out), (0.5, "r", "30x2"), (0.6, "o", "end")]);
        let frames = Replay::new(&c).frames();
        assert_eq!(frames[0].new_lines.len(), 5000 - 3);
        assert_eq!(frames[0].new_lines[4996].text(), "4996");
        let last = frames.last().unwrap();
        assert_eq!(last.screen.size(), Size::new(2, 30));
        assert_eq!(last.screen.lines(), ["4999", "end"]);
        let total: usize = frames.iter().map(|f| f.new_lines.len()).sum();
        assert_eq!(total as u64, last.screen.first_line());
    }
}
