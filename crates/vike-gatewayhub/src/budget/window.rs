//! `SlidingWindow` — a sum of costs over the last `window`, kept on its OWN ring of instants.
//!
//! It is deliberately not `vike_bridge_core::ratelimit::RateGate` (this crate links neither the
//! crate nor the transport stack under it): `RateGate` is a FIFO limiter that BLOCKS the caller, and
//! this is a passive counter that never blocks and never refuses. The caller supplies `now` on every
//! call — there is no clock in here, so the arithmetic is tested with synthetic instants.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The most entries a window keeps. Past it a new cost is MERGED into the newest entry instead of
/// pushed, so the SUM stays exact while the ring stays bounded no matter how fast a caller counts
/// (shadow mode refuses nothing, so a runaway caller would otherwise grow the ring without limit).
/// A merged entry expires with the newest instant it holds, i.e. a little LATE: the counter
/// over-reports for at most one window under a flood, the safe direction for a would-throttle meter.
pub const RING_CAP: usize = 4096;

/// A sliding-window sum: `(instant, cost)` entries inside the last `window`, and their total.
#[derive(Debug, Clone)]
pub struct SlidingWindow {
    window: Duration,
    ring: VecDeque<(Instant, u64)>,
    sum: u64,
}

impl SlidingWindow {
    /// An empty window of length `window`.
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self { window, ring: VecDeque::new(), sum: 0 }
    }

    /// The window's length.
    #[must_use]
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Drop every entry at least one `window` old as of `now`. The window is half-open: an entry
    /// exactly `window` old has left it, so a gate that admits `n` per `window` sees the `n+1`th
    /// admitted at exactly one `window` after the first.
    fn expire(&mut self, now: Instant) {
        while let Some(&(at, cost)) = self.ring.front() {
            if now.saturating_duration_since(at) < self.window {
                break;
            }
            self.sum = self.sum.saturating_sub(cost);
            self.ring.pop_front();
        }
    }

    /// The total cost inside the window as of `now`.
    pub fn used(&mut self, now: Instant) -> u64 {
        self.expire(now);
        self.sum
    }

    /// Add `cost` at `now`. A `now` older than the newest entry is clamped to it, so the ring stays
    /// ordered and front-expiry stays correct when threads hand in slightly stale instants.
    pub fn add(&mut self, now: Instant, cost: u64) {
        if cost == 0 {
            return;
        }
        self.expire(now);
        self.sum = self.sum.saturating_add(cost);
        let at = self.ring.back().map_or(now, |&(newest, _)| newest.max(now));
        if self.ring.len() >= RING_CAP
            && let Some(newest) = self.ring.back_mut()
        {
            *newest = (at, newest.1.saturating_add(cost));
            return;
        }
        self.ring.push_back((at, cost));
    }

    /// How many entries the ring holds (a test and a bound check; not a count of requests).
    #[must_use]
    pub fn entries(&self) -> usize {
        self.ring.len()
    }
}
