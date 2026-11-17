//! A terminal session: a byte source and sink, the emulator, the output buffer and settle
//! detection, behind one handle.
//!
//! - [`Session::subscribe_bytes`] streams raw output from an offset, for viewers that resume;
//!   [`Session::attach`] gives a serialised snapshot and the live bytes after it.
//! - [`Session::screen`] is the current screen; [`Session::settled`] streams settled frames,
//!   each with the scrollback lines that arrived since the previous one.
//! - [`Session::write`] and [`Session::send`] write raw or semantic input.
//! - The `wait_*` methods wait on the screen with a timeout and no fixed sleeps.
//! - [`Session::start_recording`] writes an asciinema cast.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::sync::{Notify, broadcast, mpsc, watch};

use crate::buffer::{OffsetError, OutputBuffer};
use crate::cast::{CastHeader, CastWriter, Frame, Timeline};
use crate::emulator::{Emulator, EmulatorConfig};
use crate::input::{Input, encode};
use crate::io::{ByteSink, ByteSource};
use crate::pty::{self, Pty, PtyCommand};
use crate::queries::ResponseFilter;
use crate::screen::{Modes, Screen, Size};
use crate::serialize::serialize;
use crate::settle::{SettleConfig, SettleReason, Settler};

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub emulator: EmulatorConfig,
    pub settle: SettleConfig,
    /// Raw output kept for viewers that resume. Default 4 MiB.
    pub buffer_bytes: usize,
    /// Leave out settled frames equal to the previous one. Default on.
    pub dedup: bool,
    /// Answer the application's queries (cursor position, device attributes, colours). Turn
    /// off when a real terminal also sees the output and answers them. Default on.
    pub answer_queries: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            emulator: EmulatorConfig::default(),
            settle: SettleConfig::default(),
            buffer_bytes: 4 * 1024 * 1024,
            dedup: true,
            answer_queries: true,
        }
    }
}

impl SessionConfig {
    pub fn with_size(size: Size) -> Self {
        Self {
            emulator: EmulatorConfig::with_size(size),
            ..Default::default()
        }
    }

    /// Settling tuned for automation: snapshots after 50 ms of quiet, and at least every
    /// 300 ms for programs that never go quiet (Claude Code animates while it waits).
    pub fn for_automation(size: Size) -> Self {
        let mut c = Self::with_size(size);
        c.settle = SettleConfig::for_automation();
        c
    }
}

/// A chunk of raw output at an absolute offset.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub offset: u64,
    pub data: Arc<[u8]>,
}

impl Chunk {
    pub fn end(&self) -> u64 {
        self.offset + self.data.len() as u64
    }
}

struct Recorder {
    writer: CastWriter<Box<dyn Write + Send>>,
    start: Instant,
}

impl Recorder {
    fn t(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

struct State {
    timeline: Timeline,
    buffer: OutputBuffer,
    recorder: Option<Recorder>,
    latest: Option<Arc<Frame>>,
    bytes_tx: Option<broadcast::Sender<Chunk>>,
    frames_tx: Option<broadcast::Sender<Arc<Frame>>>,
    closed: bool,
    /// Drops viewers' answers to queries the session answered itself.
    responses: ResponseFilter,
}

struct Inner {
    state: Mutex<State>,
    sink: Arc<dyn ByteSink>,
    version: watch::Sender<u64>,
    wake: Notify,
    base: Instant,
    answer_queries: bool,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> Duration {
        self.base.elapsed()
    }

    fn publish(&self, st: &mut State, frames: Vec<Frame>) {
        for f in frames {
            let f = Arc::new(f);
            st.latest = Some(f.clone());
            if let Some(tx) = &st.frames_tx {
                let _ = tx.send(f);
            }
        }
        self.version.send_replace(st.timeline.emu.version());
    }

    fn on_output(&self, data: Vec<u8>) -> Vec<u8> {
        let mut st = self.lock();
        let now = self.now();
        let offset = st.buffer.end();
        st.buffer.push(&data);
        if let Some(r) = &mut st.recorder {
            let t = r.t();
            let _ = r.writer.output(t, &data);
        }
        let mut frames = Vec::new();
        st.timeline.output(now, &data, &mut |f| frames.push(f));
        let responses = st.timeline.emu.take_responses();
        if !responses.is_empty() {
            st.responses.note_query();
        }
        if let Some(tx) = &st.bytes_tx {
            let _ = tx.send(Chunk {
                offset,
                data: Arc::from(data),
            });
        }
        self.publish(&mut st, frames);
        responses
    }

    fn on_timer(&self) {
        let mut st = self.lock();
        let mut frames = Vec::new();
        st.timeline.fire_timers(self.now(), &mut |f| frames.push(f));
        self.publish(&mut st, frames);
    }

    fn on_close(&self) {
        let mut st = self.lock();
        let mut frames = Vec::new();
        st.timeline.fire_timers(self.now(), &mut |f| frames.push(f));
        if st.timeline.emu.in_sync() {
            st.timeline.emu.flush_sync();
            st.timeline.settler.sync_ended();
        }
        if st.timeline.settler.is_dirty() {
            let screen = st.timeline.emu.screen();
            st.timeline
                .emit(self.now(), SettleReason::Closed, screen, &mut |f| frames.push(f));
        }
        self.publish(&mut st, frames);
        if let Some(r) = &mut st.recorder {
            let _ = r.writer.flush();
        }
        st.closed = true;
        st.bytes_tx = None;
        st.frames_tx = None;
        drop(st);
        // Wake waiters so they see the session closed.
        self.version.send_modify(|v| *v += 1);
    }
}

/// A handle to a terminal session. Clones share the session.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl Session {
    /// Starts a session over `source` and `sink`. Must be called inside a tokio runtime.
    pub fn new(source: impl ByteSource, sink: impl ByteSink, config: SessionConfig) -> Session {
        Self::build(source, sink, config, None)
    }

    /// Starts a session that continues an earlier one: `history` is its output from offset `start`
    /// (all of it when `start` is 0), fed to the emulator and buffer first without answering the
    /// queries in it, and `source` carries the output after it. Offsets continue from
    /// `start + history.len()`, so viewers that knew the earlier session can resume.
    pub fn resume(
        source: impl ByteSource,
        sink: impl ByteSink,
        config: SessionConfig,
        history: &[u8],
        start: u64,
    ) -> Session {
        Self::build(source, sink, config, Some((history, start)))
    }

    fn build(
        source: impl ByteSource,
        sink: impl ByteSink,
        config: SessionConfig,
        history: Option<(&[u8], u64)>,
    ) -> Session {
        let (bytes_tx, _) = broadcast::channel(1024);
        let (frames_tx, _) = broadcast::channel(1024);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                timeline: Timeline {
                    emu: Emulator::new(config.emulator.clone()),
                    settler: Settler::new(config.settle),
                    last: None,
                    dedup: config.dedup,
                },
                buffer: OutputBuffer::starting_at(
                    config.buffer_bytes,
                    history.map(|(_, start)| start).unwrap_or(0),
                ),
                recorder: None,
                latest: None,
                bytes_tx: Some(bytes_tx),
                frames_tx: Some(frames_tx),
                closed: false,
                responses: ResponseFilter::default(),
            }),
            sink: Arc::new(sink),
            version: watch::channel(0).0,
            wake: Notify::new(),
            base: Instant::now(),
            answer_queries: config.answer_queries,
        });
        if let Some((bytes, start)) = history {
            let mut st = inner.lock();
            st.timeline.emu.set_base_offset(start);
            st.buffer.push(bytes);
            let mut frames = Vec::new();
            st.timeline.output(inner.now(), bytes, &mut |f| frames.push(f));
            // Queries in the history were answered when they happened.
            st.timeline.emu.take_responses();
            if let Some(f) = frames.pop() {
                st.latest = Some(Arc::new(f));
            }
            inner.version.send_replace(st.timeline.emu.version());
        }
        tokio::spawn(run(inner.clone(), source));
        Session { inner }
    }

    /// Runs `cmd` on a local PTY with the emulator's size.
    pub fn spawn(cmd: &PtyCommand, config: SessionConfig) -> io::Result<(Session, Pty)> {
        let mut cmd = cmd.clone();
        cmd.size = config.emulator.size;
        let (pty, source, sink) = pty::spawn(&cmd)?;
        Ok((Session::new(source, sink, config), pty))
    }

    /// Seconds since the session started, on the clock settled frames use for
    /// [`Frame::time`].
    pub fn elapsed(&self) -> f64 {
        self.inner.now().as_secs_f64()
    }

    /// The current screen.
    pub fn screen(&self) -> Screen {
        self.inner.lock().timeline.emu.screen()
    }

    pub fn size(&self) -> Size {
        self.inner.lock().timeline.emu.size()
    }

    pub fn modes(&self) -> Modes {
        self.inner.lock().timeline.emu.modes()
    }

    /// Whether the output has ended.
    pub fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }

    /// Total bytes of output so far.
    pub fn offset(&self) -> u64 {
        self.inner.lock().buffer.end()
    }

    /// Raw output from `from` on (or from the end, for `None`). Fails when `from` is no longer
    /// buffered; the caller then needs [`Session::attach`].
    pub fn subscribe_bytes(&self, from: Option<u64>) -> Result<ByteSubscription, OffsetError> {
        let st = self.inner.lock();
        let from = from.unwrap_or(st.buffer.end());
        let backlog = st.buffer.read_from(from)?;
        Ok(ByteSubscription {
            next: from,
            backlog: Some(backlog),
            rx: st.bytes_tx.as_ref().map(|tx| tx.subscribe()),
            inner: self.inner.clone(),
        })
    }

    /// A serialised snapshot of the screen and scrollback, the offset it corresponds to, and the
    /// output after it.
    pub fn attach(&self) -> Result<Attach, OffsetError> {
        let mut st = self.inner.lock();
        let screen = st.timeline.emu.screen();
        let backlog = st.buffer.read_from(screen.offset())?;
        Ok(Attach {
            snapshot: serialize(&screen),
            size: screen.size(),
            offset: screen.offset(),
            bytes: ByteSubscription {
                next: screen.offset(),
                backlog: Some(backlog),
                rx: st.bytes_tx.as_ref().map(|tx| tx.subscribe()),
                inner: self.inner.clone(),
            },
        })
    }

    /// Settled frames from now on.
    pub fn settled(&self) -> FrameStream {
        let st = self.inner.lock();
        FrameStream {
            rx: st.frames_tx.as_ref().map(|tx| tx.subscribe()),
        }
    }

    /// The most recent settled frame.
    pub fn latest_settled(&self) -> Option<Arc<Frame>> {
        self.inner.lock().latest.clone()
    }

    /// Writes raw bytes to the application.
    pub async fn write(&self, bytes: &[u8]) -> io::Result<()> {
        {
            let mut st = self.inner.lock();
            if let Some(r) = &mut st.recorder {
                let t = r.t();
                let _ = r.writer.input(t, bytes);
            }
        }
        self.inner.sink.write(bytes).await
    }

    /// Writes input from a viewer such as xterm.js, which answers terminal queries too: answers to
    /// queries the session already answered itself are dropped, so the application sees one.
    pub async fn write_from_viewer(&self, bytes: &[u8]) -> io::Result<()> {
        let bytes = if self.inner.answer_queries {
            self.inner.lock().responses.filter(bytes)
        } else {
            bytes.to_vec()
        };
        if bytes.is_empty() {
            return Ok(());
        }
        self.write(&bytes).await
    }

    /// Encodes `input` for the terminal's current modes and writes it.
    pub async fn send(&self, input: &Input) -> io::Result<()> {
        let bytes = encode(input, &self.modes());
        if bytes.is_empty() {
            return Ok(());
        }
        self.write(&bytes).await
    }

    /// Types text.
    pub async fn type_text(&self, text: &str) -> io::Result<()> {
        self.send(&Input::Text(text.into())).await
    }

    /// Presses a key given by name, such as `enter` or `ctrl+c`.
    pub async fn press(&self, key: &str) -> io::Result<()> {
        let input = Input::parse_key(key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("unknown key {key:?}")))?;
        self.send(&input).await
    }

    pub async fn paste(&self, text: &str) -> io::Result<()> {
        self.send(&Input::Paste(text.into())).await
    }

    /// Resizes the emulator and tells the application.
    pub async fn resize(&self, size: Size) -> io::Result<()> {
        {
            let mut st = self.inner.lock();
            st.timeline.emu.resize(size);
            st.timeline.settler.touch(self.inner.now());
            if let Some(r) = &mut st.recorder {
                let t = r.t();
                let _ = r.writer.resize(t, size);
            }
            self.inner.version.send_replace(st.timeline.emu.version());
        }
        self.inner.wake.notify_one();
        self.inner.sink.resize(size).await
    }

    /// Starts writing an asciinema cast. If the session already has output, the cast starts
    /// with a snapshot of the current screen, so it replays to the same state.
    pub fn start_recording(
        &self,
        writer: impl Write + Send + 'static,
        mut header: CastHeader,
    ) -> io::Result<()> {
        let mut st = self.inner.lock();
        let screen = st.timeline.emu.screen();
        header.width = screen.size().cols;
        header.height = screen.size().rows;
        let mut writer = CastWriter::new(Box::new(writer) as Box<dyn Write + Send>, &header)?;
        if st.buffer.end() > 0 {
            let mut initial = serialize(&screen);
            initial.extend(st.buffer.read_from(screen.offset()).unwrap_or_default());
            writer.output(0.0, &initial)?;
        }
        st.recorder = Some(Recorder {
            writer,
            start: Instant::now(),
        });
        Ok(())
    }

    /// Adds a marker to the recording.
    pub fn marker(&self, label: &str) {
        let mut st = self.inner.lock();
        if let Some(r) = &mut st.recorder {
            let t = r.t();
            let _ = r.writer.marker(t, label);
        }
    }

    /// Stops recording and flushes the cast.
    pub fn stop_recording(&self) -> io::Result<()> {
        let mut st = self.inner.lock();
        match st.recorder.take() {
            Some(mut r) => r.writer.flush(),
            None => Ok(()),
        }
    }

    /// Waits until `predicate` holds for the screen, checking after every change.
    pub async fn wait_for(
        &self,
        mut predicate: impl FnMut(&Screen) -> bool,
        timeout: Duration,
    ) -> Result<Screen, WaitError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rx = self.inner.version.subscribe();
        loop {
            rx.borrow_and_update();
            let screen = self.screen();
            if predicate(&screen) {
                return Ok(screen);
            }
            if self.is_closed() {
                return Err(WaitError::Closed { screen });
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return Err(WaitError::Closed {
                        screen: self.screen(),
                    });
                }
                Err(_) => {
                    return Err(WaitError::Timeout {
                        screen: self.screen(),
                        timeout,
                    });
                }
            }
        }
    }

    /// Waits until the screen shows `pattern`: a string on one row, or a regex over the rows
    /// joined with `\n`.
    pub async fn wait_for_text(
        &self,
        pattern: impl Into<TextPattern>,
        timeout: Duration,
    ) -> Result<Screen, WaitError> {
        let pattern = pattern.into();
        self.wait_for(|s| pattern.matches(s), timeout).await
    }

    /// Waits until output has been quiet for `quiet`. Fails if that cannot happen before
    /// `timeout`. A closed session is stable.
    pub async fn wait_for_stable(&self, quiet: Duration, timeout: Duration) -> Result<Screen, WaitError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rx = self.inner.version.subscribe();
        loop {
            rx.borrow_and_update();
            if self.is_closed() {
                return Ok(self.screen());
            }
            let quiet_end = tokio::time::Instant::now() + quiet;
            if quiet_end > deadline {
                return Err(WaitError::Timeout {
                    screen: self.screen(),
                    timeout,
                });
            }
            match tokio::time::timeout_at(quiet_end, rx.changed()).await {
                Err(_) | Ok(Err(_)) => return Ok(self.screen()),
                Ok(Ok(())) => {}
            }
        }
    }

    /// Waits for a settled frame, after the current one, for which `predicate` holds.
    pub async fn wait_for_settled(
        &self,
        mut predicate: impl FnMut(&Frame) -> bool,
        timeout: Duration,
    ) -> Result<Arc<Frame>, WaitError> {
        let mut frames = self.settled();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, frames.next()).await {
                Ok(Some(f)) => {
                    if predicate(&f) {
                        return Ok(f);
                    }
                }
                Ok(None) => {
                    return Err(WaitError::Closed {
                        screen: self.screen(),
                    });
                }
                Err(_) => {
                    return Err(WaitError::Timeout {
                        screen: self.screen(),
                        timeout,
                    });
                }
            }
        }
    }

    /// Waits until the output ends.
    pub async fn wait_closed(&self) {
        let mut rx = self.inner.version.subscribe();
        while !self.is_closed() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

async fn run(inner: Arc<Inner>, mut source: impl ByteSource) {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
        while let Ok(Some(data)) = source.read().await {
            if tx.send(data).await.is_err() {
                break;
            }
        }
    });
    loop {
        let deadline = inner
            .lock()
            .timeline
            .settler
            .deadline()
            .map(|d| tokio::time::Instant::from_std(inner.base + d));
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(data) => {
                    let responses = inner.on_output(data);
                    if !responses.is_empty() && inner.answer_queries {
                        let _ = inner.sink.write(&responses).await;
                    }
                }
                None => break,
            },
            _ = async { tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)).await }, if deadline.is_some() => {
                inner.on_timer();
            }
            _ = inner.wake.notified() => {}
        }
    }
    inner.on_close();
}

/// Raw output from an offset on.
pub struct ByteSubscription {
    next: u64,
    backlog: Option<Vec<u8>>,
    rx: Option<broadcast::Receiver<Chunk>>,
    inner: Arc<Inner>,
}

impl ByteSubscription {
    /// Offset of the next byte this subscription returns.
    pub fn offset(&self) -> u64 {
        self.next
    }

    /// The next chunk, `Ok(None)` when the output has ended, or an error if the subscriber fell
    /// so far behind that the bytes it needs were dropped.
    pub async fn recv(&mut self) -> Result<Option<Chunk>, OffsetError> {
        if let Some(b) = self.backlog.take()
            && !b.is_empty()
        {
            let c = Chunk {
                offset: self.next,
                data: Arc::from(b),
            };
            self.next = c.end();
            return Ok(Some(c));
        }
        loop {
            let Some(rx) = &mut self.rx else {
                return Ok(None);
            };
            match rx.recv().await {
                Ok(c) => {
                    if c.end() <= self.next {
                        continue;
                    }
                    let skip = self.next.saturating_sub(c.offset) as usize;
                    let c = Chunk {
                        offset: self.next,
                        data: Arc::from(&c.data[skip..]),
                    };
                    self.next = c.end();
                    return Ok(Some(c));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let data = self.inner.lock().buffer.read_from(self.next)?;
                    if data.is_empty() {
                        continue;
                    }
                    let c = Chunk {
                        offset: self.next,
                        data: Arc::from(data),
                    };
                    self.next = c.end();
                    return Ok(Some(c));
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(None),
            }
        }
    }
}

/// What a viewer needs to attach.
pub struct Attach {
    /// Escape sequences that reproduce the screen and scrollback on a fresh terminal of `size`.
    pub snapshot: Vec<u8>,
    pub size: Size,
    /// The output offset the snapshot corresponds to.
    pub offset: u64,
    /// Output from `offset` on.
    pub bytes: ByteSubscription,
}

/// Settled frames as they happen.
pub struct FrameStream {
    rx: Option<broadcast::Receiver<Arc<Frame>>>,
}

impl FrameStream {
    /// The next frame, or `None` when the session has closed. If the consumer fell behind by
    /// more than 1024 frames, the oldest are skipped; their new scrollback lines are then only
    /// available through the screen's history.
    pub async fn next(&mut self) -> Option<Arc<Frame>> {
        let rx = self.rx.as_mut()?;
        loop {
            match rx.recv().await {
                Ok(f) => return Some(f),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// What to wait for in [`Session::wait_for_text`]. A no-break space on the screen matches a
/// space.
#[derive(Clone, Debug)]
pub enum TextPattern {
    /// A string on one row.
    Literal(String),
    /// A regex over the rows joined with `\n`.
    Regex(Regex),
}

impl TextPattern {
    pub fn matches(&self, screen: &Screen) -> bool {
        match self {
            TextPattern::Literal(s) => screen.contains(s),
            TextPattern::Regex(re) => {
                re.is_match(&crate::screen::normalize_spaces(&screen.lines().join("\n")))
            }
        }
    }
}

impl From<&str> for TextPattern {
    fn from(s: &str) -> Self {
        TextPattern::Literal(s.into())
    }
}

impl From<String> for TextPattern {
    fn from(s: String) -> Self {
        TextPattern::Literal(s)
    }
}

impl From<Regex> for TextPattern {
    fn from(r: Regex) -> Self {
        TextPattern::Regex(r)
    }
}

impl From<&Regex> for TextPattern {
    fn from(r: &Regex) -> Self {
        TextPattern::Regex(r.clone())
    }
}

/// A wait that did not succeed. Carries the screen at the end, for error messages.
#[derive(Clone)]
pub enum WaitError {
    Timeout { screen: Screen, timeout: Duration },
    Closed { screen: Screen },
}

impl WaitError {
    pub fn screen(&self) -> &Screen {
        match self {
            WaitError::Timeout { screen, .. } | WaitError::Closed { screen } => screen,
        }
    }
}

impl fmt::Display for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaitError::Timeout { timeout, .. } => write!(f, "timed out after {timeout:?}")?,
            WaitError::Closed { .. } => write!(f, "session closed")?,
        }
        write!(f, "; screen:\n{}", crate::snapshot::to_text(self.screen()))
    }
}

impl fmt::Debug for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for WaitError {}
