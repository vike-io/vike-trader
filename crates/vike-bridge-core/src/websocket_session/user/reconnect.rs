//! **How loud a connection is while it cannot dial again** (owner ruling B-13): [`ReconnectLog`], a persistent failure
//! speaks when it starts, then on a quiet interval that grows to a ceiling, and starts over once it clears (the cure
//! for 53,160 lines a day on one lane, measured on the CI box on 2026-08-23; [`RECONNECT_LOG`] carries that lane's numbers).
//!
//! The WAIT before the next dial is not here: it is the one ladder of every reconnect loop,
//! `vike_venues::reconnect`'s `ReconnectBackoff` (it moved down there on 2026-11-22, so the hub links below this
//! crate's layer wait on it too).
//!
//! It is pure state: the caller owns the clock, so an own-loop venue (SSE, chunked HTTP, Lightstreamer, protobuf over
//! TCP, a sidecar process) takes it as it is. The shared WS pump's spec-built spawns (`super::spawn_user`,
//! `super::spawn_user_with_resync`) pace their failed-open line with [`RECONNECT_LOG`].

use std::time::Duration;

/// What a [`ReconnectLog`] says on one turn of a failure that holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectLogLine {
    /// The failure has just started: say so. `next_in` is how long the lane stays quiet now.
    Onset { next_in: Duration },
    /// Still failing and its interval is up. `down_for` counts from the onset; `next_in` is the grown
    /// interval that follows this line, so a reader can tell a longer silence from a recovery.
    Heartbeat { down_for: Duration, next_in: Duration },
    /// Still failing, interval not up: say nothing.
    Quiet,
}

/// The log pace of ONE persistent failure: a line at the onset, then one per interval, the interval
/// growing by `decay` per line up to `ceiling`, and everything forgotten on [`Self::clear`] so the next
/// failure starts responsive. Past the ceiling the lane still speaks once per ceiling: an outage that stops
/// being mentioned reads as one that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectLog {
    first: Duration,
    decay: u32,
    ceiling: Duration,
    /// When the failure began; `None` while it is not on.
    since: Option<Duration>,
    /// When it last spoke: the interval is measured from here, never from `since` (that would speak on
    /// every turn once the outage outlived one interval).
    last_said: Option<Duration>,
    /// Heartbeats spoken in this failure (the onset is not one).
    steps: u32,
}

/// The pace every private stream logs a failure it cannot open past at: 5 minutes after the onset, then
/// 15, 45, then hourly. A DISK-SAFETY bound with no venue in it: one lane that spoke on every turn of a
/// one-second loop wrote 53,160 lines (23 MB) in a day on the CI box (2026-08-23), and a flat 300 s still left
/// 288 lines a day about a failure already known.
pub const RECONNECT_LOG: ReconnectLog =
    ReconnectLog::new(Duration::from_secs(300), 3, Duration::from_secs(3600));

impl ReconnectLog {
    /// A pace starting at `first`, growing by `decay` per line, capped at `ceiling`.
    #[must_use]
    pub const fn new(first: Duration, decay: u32, ceiling: Duration) -> Self {
        Self { first, decay, ceiling, since: None, last_said: None, steps: 0 }
    }

    /// One turn on which the failure holds, at `now` (any monotonic reading the caller keeps).
    pub fn tick(&mut self, now: Duration) -> ReconnectLogLine {
        let (Some(since), Some(said)) = (self.since, self.last_said) else {
            self.since = Some(now);
            self.last_said = Some(now);
            self.steps = 0;
            return ReconnectLogLine::Onset { next_in: self.interval(0) };
        };
        if now.saturating_sub(said) < self.interval(self.steps) {
            return ReconnectLogLine::Quiet;
        }
        self.last_said = Some(now);
        self.steps = self.steps.saturating_add(1);
        ReconnectLogLine::Heartbeat {
            down_for: now.saturating_sub(since),
            next_in: self.interval(self.steps),
        }
    }

    /// The failure cleared: the next one is a new onset with the first interval.
    pub fn clear(&mut self) {
        (self.since, self.last_said, self.steps) = (None, None, 0);
    }

    /// The quiet period after the `steps`-th heartbeat. Saturating: an overflow returns the ceiling, never
    /// a short interval.
    fn interval(&self, steps: u32) -> Duration {
        let factor = self.decay.checked_pow(steps).unwrap_or(u32::MAX);
        self.first.checked_mul(factor).unwrap_or(self.ceiling).min(self.ceiling)
    }
}

#[path = "reconnect_tests.rs"]
#[cfg(test)]
mod reconnect_tests;
