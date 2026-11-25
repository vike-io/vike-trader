//! [`TacticsInput`], what a rule reads of one of its INPUTS (decision 0271): a stored series beside its own
//! bars, by the name the rule declares ([`crate::Rule::inputs`]), as of now. A bar of interval `I`
//! opened at `ts` is seen once `ts + I` has passed; on a bar the rule decides at the bar's close, so
//! the last value is the input's last bar closed by then, never the bar in progress.
//!
//! **No value is a `None`, never a NaN.** Before the input's first bar closes, and for a name the rule
//! did not declare (the run loads only declared names), every read below is `None` and [`TacticsInput::bars`]
//! is empty. A backtest refuses to start when the store does not cover a declared input over its
//! window, so inside the window a venue's series or a declared lane always has a value.
//!
//! **A user's lane may be SPARSE** (decision 0271's amendment of 2026-11-18): it holds a row only at the
//! bars that carry a value, a backtest refuses it only when it holds no row inside the window, and
//! between two rows the last one is read again (`None` before the first). Tell a fresh value from an
//! old one by its own time: [`TacticsInput::time`] against the current bar's `timestamp`. On a lane of the
//! rule's own interval, `input.time() == Some(bar.timestamp)` is the row of this very bar; an earlier
//! time is the last row before it.

use vike_marketdata::Bar;

/// One input as of now: its bars that have closed, oldest first ([`crate::Tactics::input`]).
#[derive(Debug, Clone, Copy)]
pub struct TacticsInput<'a> {
    closed: &'a [Bar],
}

impl<'a> TacticsInput<'a> {
    /// The view over `closed`, its closed bars oldest first: what `crate::Tactics::input` answers, for a
    /// front end or a test that holds the bars itself.
    pub fn new(closed: &'a [Bar]) -> Self {
        TacticsInput { closed }
    }

    /// The last bar that closed, `None` before the first does.
    pub fn bar(&self) -> Option<&'a Bar> {
        self.closed.last()
    }

    /// The last closed bar's close.
    pub fn close(&self) -> Option<f64> {
        self.bar().map(|b| b.close)
    }

    /// The last closed bar's open.
    pub fn open(&self) -> Option<f64> {
        self.bar().map(|b| b.open)
    }

    /// The last closed bar's high.
    pub fn high(&self) -> Option<f64> {
        self.bar().map(|b| b.high)
    }

    /// The last closed bar's low.
    pub fn low(&self) -> Option<f64> {
        self.bar().map(|b| b.low)
    }

    /// The last closed bar's volume.
    pub fn volume(&self) -> Option<f64> {
        self.bar().map(|b| b.volume)
    }

    /// When the last closed bar OPENED, in epoch ms (a bar's `ts`); it closed one interval later.
    pub fn time(&self) -> Option<i64> {
        self.bar().map(|b| b.timestamp)
    }

    /// The last `n` closed bars, oldest first: fewer while fewer have closed. What the run loaded
    /// before its window is in it (a backtest reads a higher timeframe's whole stored history, a lower
    /// or equal one from one of its bars before the window), so a rule can warm an indicator of the
    /// input on its first bar: `bars(usize::MAX)` is every one.
    pub fn bars(&self, n: usize) -> &'a [Bar] {
        &self.closed[self.closed.len().saturating_sub(n)..]
    }

    /// The closed bars that opened at or after `ts`, oldest first: on a bar, `since(bar.ts)` is the
    /// bars of a lower timeframe inside it (every one of them has closed by the bar's close).
    pub fn since(&self, ts: i64) -> &'a [Bar] {
        &self.closed[self.closed.partition_point(|b| b.timestamp < ts)..]
    }
}
