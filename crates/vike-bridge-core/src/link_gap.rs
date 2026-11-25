//! **The idle-limit evidence of one venue session: the longest gap between its inbound events, said once a minute.**
//!
//! A bridge whose session has no idle limit yet notes each inbound event where it already passes
//! ([`GapWindow::note`]: one time and two counters, no allocation, no log line) and, from a loop or timer it already
//! runs, says ONE `info` line a minute, `link gap summary` ([`GapSummary::say`], with the venue's name): how many events
//! came in the window, the longest gap between two of them that ended in it, and how long ago the last one came. An
//! idle limit is measured from these lines before one is chosen. It is a reading only: nothing acts on it.
//!
//! What an event is, and the loop that asks [`GapWindow::take_due`], are the bridge's: fxcm counts the envelopes its
//! exec thread drains off the shim (`crates/bridges/fxcm/src/execution/mod.rs`'s `drain_events`), dukascopy every line
//! its sidecar writes (`crates/bridges/dukascopy/src/execution/mod.rs`'s `supervise` says it). The times are the
//! caller's, so the math is tested on scripted ones.

use std::time::{Duration, Instant};

/// How long one window runs: one summary line a minute.
pub const GAP_SUMMARY_EVERY: Duration = Duration::from_secs(60);

/// One window of a session's inbound events.
#[derive(Debug)]
pub struct GapWindow {
    /// When this window began: the session's start, or the last summary.
    started: Instant,
    /// The last event, kept across windows, so a gap that spans two windows is counted in the one it ended in.
    last: Option<Instant>,
    events: u64,
    longest: Duration,
}

/// What one window says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapSummary {
    /// How long the window ran (a minute, plus however late the caller asked).
    pub window_ms: u64,
    /// The inbound events in the window.
    pub events: u64,
    /// The longest gap between two events that ended in the window; 0 when none did.
    pub longest_gap_ms: u64,
    /// How long before the summary the last event came; `None` before the session's first.
    pub since_last_ms: Option<u64>,
}

impl GapWindow {
    /// A window that starts at `now`, with no event yet.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self { started: now, last: None, events: 0, longest: Duration::ZERO }
    }

    /// One inbound event, at `at`.
    pub fn note(&mut self, at: Instant) {
        if let Some(last) = self.last {
            self.longest = self.longest.max(at.saturating_duration_since(last));
        }
        self.last = Some(at);
        self.events += 1;
    }

    /// The window's summary once it has run [`GAP_SUMMARY_EVERY`], and the next window starts at `now`; `None` before.
    pub fn take_due(&mut self, now: Instant) -> Option<GapSummary> {
        let window = now.saturating_duration_since(self.started);
        if window < GAP_SUMMARY_EVERY {
            return None;
        }
        let summary = GapSummary {
            window_ms: millis(window),
            events: self.events,
            longest_gap_ms: millis(self.longest),
            since_last_ms: self.last.map(|last| millis(now.saturating_duration_since(last))),
        };
        *self = Self { last: self.last, ..Self::new(now) };
        Some(summary)
    }
}

impl GapSummary {
    /// The minute's one line for `venue`. `since_last_ms` is absent before the session's first event.
    pub fn say(&self, venue: &'static str) {
        tracing::info!(
            venue,
            events = self.events,
            longest_gap_ms = self.longest_gap_ms,
            since_last_ms = self.since_last_ms,
            window_ms = self.window_ms,
            "link gap summary"
        );
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[path = "link_gap_tests.rs"]
#[cfg(test)]
mod link_gap_tests;
