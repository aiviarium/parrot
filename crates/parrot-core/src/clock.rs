//! Time and ids come from outside, so simulations are deterministic.

pub trait Clock: Send {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

pub trait Ids: Send {
    /// A new id with the given prefix, such as `ses` or `itm` (spec 2).
    fn next(&mut self, prefix: &str) -> String;
}

/// A clock that only moves when told to, for tests.
#[derive(Debug, Default, Clone)]
pub struct ManualClock(pub std::sync::Arc<std::sync::atomic::AtomicU64>);

impl ManualClock {
    pub fn set(&self, ms: u64) {
        self.0.store(ms, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Ids `<prefix>_<n>` counting up, for tests.
#[derive(Debug, Default)]
pub struct SeqIds(u64);

impl SeqIds {
    /// Ids that continue after `n`, for a simulated restart.
    pub fn starting_at(n: u64) -> Self {
        Self(n)
    }
}

impl Ids for SeqIds {
    fn next(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}_{:06}", self.0)
    }
}
