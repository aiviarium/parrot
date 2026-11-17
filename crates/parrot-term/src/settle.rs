//! Deciding when the screen has settled and a snapshot should be taken.
//!
//! Time is passed in as a [`Duration`] since an arbitrary origin, so the same logic runs on the
//! wall clock in a live session and on a recording's timestamps in a replay.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettleConfig {
    /// Output must be quiet this long. Default 100 ms.
    pub quiet: Duration,
    /// Take a snapshot after this long even if output never goes quiet. Default 1 s; `None`
    /// waits for quiet indefinitely.
    pub max_delay: Option<Duration>,
    /// Apply an open synchronized update after this long. Default 150 ms, as alacritty.
    pub sync_timeout: Duration,
}

impl Default for SettleConfig {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(100),
            max_delay: Some(Duration::from_secs(1)),
            sync_timeout: crate::emulator::DEFAULT_SYNC_TIMEOUT,
        }
    }
}

impl SettleConfig {
    /// 50 ms of quiet, at most 300 ms between snapshots. See
    /// [`crate::SessionConfig::for_automation`].
    pub fn for_automation() -> Self {
        Self {
            quiet: Duration::from_millis(50),
            max_delay: Some(Duration::from_millis(300)),
            ..Self::default()
        }
    }
}

/// Why a snapshot was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettleReason {
    /// Output was quiet for the configured time.
    Quiet,
    /// A synchronized update ended.
    SyncEnd,
    /// A synchronized update did not end in time and was applied.
    SyncTimeout,
    /// Output did not go quiet within the maximum delay.
    MaxDelay,
    /// The size changed.
    Resize,
    /// The output ended.
    Closed,
}

/// Tracks unsettled output.
#[derive(Clone, Debug)]
pub struct Settler {
    config: SettleConfig,
    /// When the first unsettled output arrived.
    dirty_since: Option<Duration>,
    last_output: Duration,
    /// When the open synchronized update began.
    sync_since: Option<Duration>,
}

impl Settler {
    pub fn new(config: SettleConfig) -> Self {
        Self {
            config,
            dirty_since: None,
            last_output: Duration::ZERO,
            sync_since: None,
        }
    }

    pub fn config(&self) -> &SettleConfig {
        &self.config
    }

    /// Output arrived at `now`. `changed` is whether it changed what is visible.
    pub fn output(&mut self, now: Duration, changed: bool) {
        self.last_output = now;
        if changed && self.dirty_since.is_none() {
            self.dirty_since = Some(now);
        }
    }

    pub fn sync_began(&mut self, now: Duration) {
        self.sync_since.get_or_insert(now);
    }

    pub fn sync_ended(&mut self) {
        self.sync_since = None;
    }

    /// A snapshot was just taken.
    pub fn settled(&mut self) {
        self.dirty_since = None;
    }

    /// There is output not yet covered by a snapshot.
    pub fn is_dirty(&self) -> bool {
        self.dirty_since.is_some()
    }

    /// Marks the screen as changed without output (a resize).
    pub fn touch(&mut self, now: Duration) {
        self.dirty_since.get_or_insert(now);
    }

    /// When the open synchronized update times out.
    pub fn sync_deadline(&self) -> Option<Duration> {
        self.sync_since.map(|s| s + self.config.sync_timeout)
    }

    /// The next time [`Settler::poll`] may return something.
    pub fn deadline(&self) -> Option<Duration> {
        let settle = self.dirty_since.and_then(|since| {
            // While a synchronized update is open, quiet does not count.
            let quiet = self
                .sync_since
                .is_none()
                .then(|| self.last_output.max(since) + self.config.quiet);
            let max = self.config.max_delay.map(|m| since + m);
            match (quiet, max) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        });
        match (settle, self.sync_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Whether a snapshot is due at `now`. A synchronized update that is still open blocks a
    /// quiet snapshot, since the screen is waiting for the rest of the frame.
    pub fn poll(&self, now: Duration) -> Option<SettleReason> {
        if let Some(d) = self.sync_deadline()
            && now >= d
        {
            return Some(SettleReason::SyncTimeout);
        }
        let since = self.dirty_since?;
        if self.sync_since.is_none() && now >= self.last_output.max(since) + self.config.quiet {
            return Some(SettleReason::Quiet);
        }
        if let Some(max) = self.config.max_delay
            && now >= since + max
        {
            return Some(SettleReason::MaxDelay);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn quiet_and_max_delay() {
        let mut s = Settler::new(SettleConfig {
            quiet: ms(100),
            max_delay: Some(ms(500)),
            sync_timeout: ms(150),
        });
        assert_eq!(s.deadline(), None);
        s.output(ms(0), true);
        assert_eq!(s.deadline(), Some(ms(100)));
        assert_eq!(s.poll(ms(50)), None);
        s.output(ms(80), true);
        assert_eq!(s.poll(ms(150)), None);
        assert_eq!(s.poll(ms(180)), Some(SettleReason::Quiet));
        // Constant output: max delay.
        for t in (0..600).step_by(50) {
            s.output(ms(t), true);
        }
        assert_eq!(s.poll(ms(550)), Some(SettleReason::MaxDelay));
        s.settled();
        assert!(!s.is_dirty());
        assert_eq!(s.poll(ms(10_000)), None);
    }

    #[test]
    fn open_sync_blocks_quiet_and_times_out() {
        let mut s = Settler::new(SettleConfig::default());
        s.output(ms(0), true);
        s.sync_began(ms(0));
        assert_eq!(s.poll(ms(120)), None);
        assert_eq!(s.deadline(), Some(ms(150)));
        assert_eq!(s.poll(ms(150)), Some(SettleReason::SyncTimeout));
        s.sync_ended();
        assert_eq!(s.poll(ms(150)), Some(SettleReason::Quiet));
    }

    #[test]
    fn output_without_change_does_not_dirty() {
        let mut s = Settler::new(SettleConfig::default());
        s.output(ms(0), false);
        assert_eq!(s.poll(ms(1000)), None);
    }
}
