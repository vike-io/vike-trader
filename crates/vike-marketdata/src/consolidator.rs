//! Consolidate ticks into [`Bar`]s (the tick -> bar step), and bars into coarser bars.
//!
//! Buckets are fixed `step_ms` windows aligned to the epoch (`ts - ts.rem_euclid(step_ms)`), and a
//! bar's `ts` is the window START.
//!
//! - Quote bars ([`consolidate_quotes`]) carry the bucket's OPENING best bid/ask (what a next-open
//!   market order fills against), OHLC from the mid, and `volume` = tick count.
//! - Trade bars ([`consolidate_trades`]) take OHLC from price and `volume` = summed trade size,
//!   with no bid/ask.
//! - Coarser bars ([`resample`]) roll fine bars up: OHLC from the window's bars, `volume` summed,
//!   with no bid/ask.
//!
//! The two `consolidate_*` functions and [`resample`] are the BATCH form: they walk a slice that is
//! already in time order. [`BarConsolidator`] is the STREAMING form for live or event-time folds,
//! where samples arrive one at a time and there is no slice to walk.
//
// Engineering notes (not rendered): the `consolidate_*` fns are an exact port of
// `core/consolidator.py`. They are the BATCH port — hex-pinned against the FROZEN r1 fixture bytes
// by `crates/vike-simulator/tests/parity/r1_parity.rs`'s `consolidators_bit_parity`, so they stay literal
// and are NOT re-expressed over anything.
//
// ⚠ That sentence said "pinned against the Python oracle" until 2026-08-28. The bytes it pins
// against still CAME from the Python app, which is why the port note above cites
// `core/consolidator.py` and stays — but no exporter survives in this tree, so the pin is not a
// comparison against a running Python: it asserts that THIS code has not changed its arithmetic
// unnoticed. `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is the verdict.

use crate::{Bar, TickQuote, TickTrade};

fn bar(
    ts: i64,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    volume: f64,
    bid: Option<f64>,
    ask: Option<f64>,
) -> Bar {
    Bar {
        timestamp: ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume,
        funding: None,
        bid,
        ask,
        symbol: None,
    }
}

/// Fold quote ticks (in time order) into bars of `step_ms` milliseconds, aligned to the epoch.
///
/// OHLC come from the quote MID; `volume` is the number of ticks in the bucket; `bid`/`ask` are the
/// bucket's OPENING best bid and ask. A bucket with no ticks produces no bar. `step_ms` must be
/// positive.
pub fn consolidate_quotes(ticks: &[TickQuote], step_ms: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut current: Option<i64> = None;
    let (mut o, mut h, mut l, mut c) = (0.0, 0.0, 0.0, 0.0);
    let (mut bid, mut ask) = (0.0, 0.0);
    let mut n: u64 = 0;
    for t in ticks {
        // Python `%` is floor-mod → rem_euclid (differs from Rust `%` for negative ts)
        let start = t.timestamp - t.timestamp.rem_euclid(step_ms);
        let m = t.mid();
        if Some(start) != current {
            if let Some(previous) = current {
                out.push(bar(previous, o, h, l, c, n as f64, Some(bid), Some(ask)));
            }
            current = Some(start);
            o = m;
            h = m;
            l = m;
            c = m;
            bid = t.bid; // opening quote of the new bucket
            ask = t.ask;
            n = 0;
        } else {
            h = h.max(m);
            l = l.min(m);
            c = m;
        }
        n += 1;
    }
    if let Some(previous) = current {
        out.push(bar(previous, o, h, l, c, n as f64, Some(bid), Some(ask)));
    }
    out
}

/// Fold trade ticks (in time order) into bars of `step_ms` milliseconds, aligned to the epoch.
///
/// OHLC come from the trade price; `volume` is the summed trade size; `bid`/`ask` are `None`. A
/// bucket with no trades produces no bar. `step_ms` must be positive.
pub fn consolidate_trades(ticks: &[TickTrade], step_ms: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut current: Option<i64> = None;
    let (mut o, mut h, mut l, mut c) = (0.0, 0.0, 0.0, 0.0);
    let mut vol = 0.0;
    for t in ticks {
        let start = t.timestamp - t.timestamp.rem_euclid(step_ms);
        if Some(start) != current {
            if let Some(previous) = current {
                out.push(bar(previous, o, h, l, c, vol, None, None));
            }
            current = Some(start);
            o = t.price;
            h = t.price;
            l = t.price;
            c = t.price;
            vol = t.size;
        } else {
            h = h.max(t.price);
            l = l.min(t.price);
            c = t.price;
            vol += t.size;
        }
    }
    if let Some(previous) = current {
        out.push(bar(previous, o, h, l, c, vol, None, None));
    }
    out
}

/// A single quote tick as a degenerate one-price [`Bar`]: open, high, low and close all equal the
/// mid, `volume` is `0.0`, and `bid`/`ask` carry the quote so a consumer can still see the real
/// spread.
pub fn quote_tick_to_bar(tick: &TickQuote) -> Bar {
    let m = tick.mid();
    bar(tick.timestamp, m, m, m, m, 0.0, Some(tick.bid), Some(tick.ask))
}

/// A single trade tick as a degenerate one-price [`Bar`]: OHLC all equal the price, `volume` is the
/// trade size, and `bid`/`ask` are `None`.
pub fn trade_tick_to_bar(tick: &TickTrade) -> Bar {
    bar(tick.timestamp, tick.price, tick.price, tick.price, tick.price, tick.size, None, None)
}

/// Aggregate fine `bars` into coarse `Bar`s aligned to epoch windows of `target_ms`.
/// One coarse bar per window, chronological; the final window may be partial.
///
/// The BATCH bar-to-bar form (the simulator's higher timeframes); [`BarConsolidator`] is the
/// streaming one, on the same floor-mod bucket.
//
// Engineering note (not rendered): an exact port of `core/timeframe.py`'s resample; it moved here
// from `vike_simulator`'s `timeframe` module, byte for byte.
pub fn resample(bars: &[Bar], target_ms: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut current_start: Option<i64> = None;
    let (mut o, mut h, mut l, mut c) = (0.0, 0.0, 0.0, 0.0);
    let mut vol = 0.0;
    for b in bars {
        let start = crate::interval::bar_open(b.timestamp, target_ms); // Python floor-mod
        if Some(start) != current_start {
            if let Some(previous) = current_start {
                out.push(Bar::ohlcv(previous, o, h, l, c, vol));
            }
            current_start = Some(start);
            o = b.open;
            h = b.high;
            l = b.low;
            c = b.close;
            vol = b.volume;
        } else {
            h = h.max(b.high);
            l = l.min(b.low);
            c = b.close;
            vol += b.volume;
        }
    }
    if let Some(previous) = current_start {
        out.push(Bar::ohlcv(previous, o, h, l, c, vol));
    }
    out
}

// Engineering notes (not rendered): the live twin of the batch `consolidate_*` above. The bucket is
// the same floor-mod one (`ts - ts.rem_euclid(interval_ms)`) the batch fns and [`resample`]
// use. A per-tick price folds in as a degenerate one-price sample
// (`crates/vike-model/src/orders/fill_trigger.rs`'s `one_price_bar`, which lives a layer up now).
// This is why `vike_ibkr`'s `BarAggregator`, which closes on ANY bucket change, is deliberately NOT
// built on this: on a backwards bucket the two rules genuinely disagree.
/// Streaming epoch-aligned OHLCV bucket fold: feed it bars one at a time and it folds them into
/// fixed windows.
///
/// Feed it OHLCV samples (`&Bar`) stamped with EVENT time and it folds them into fixed
/// `interval_ms` windows aligned to the epoch, emitting the completed window when a sample crosses
/// into a later one. O(1) per sample, no allocation. Event time only (never a wall clock), so a
/// scripted feed replays deterministically and a live feed is correct off the venue's own
/// timestamps. A single price folds in as a one-price sample; a coarser bar-to-bar roll-up (say 1m
/// into 5m) folds the source bar directly.
///
/// **Monotonic close rule**: only a STRICTLY LATER window closes the open one. A sample landing in
/// the current window, or an out-of-order EARLIER one, folds in without closing, so the emitted bar
/// stream can never step backwards in time on a jittery live feed.
///
/// The emitted `ts` is the window's START; a caller that stamps bars with the window's CLOSE adds
/// `interval_ms`. `funding`/`bid`/`ask`/`symbol` are dropped (a consolidated window has no single
/// one of each), so the caller re-stamps whatever it needs.
///
/// ```
/// use vike_marketdata::{Bar, BarConsolidator};
///
/// let mut c = BarConsolidator::new(60_000);
/// assert!(c.fold(&Bar::ohlcv(0, 1.0, 2.0, 0.5, 1.5, 10.0)).is_none());
/// assert!(c.fold(&Bar::ohlcv(30_000, 1.5, 3.0, 1.0, 2.0, 5.0)).is_none());
/// // A sample in the next minute closes the first window.
/// let closed = c.fold(&Bar::ohlcv(60_000, 2.0, 2.0, 2.0, 2.0, 1.0)).unwrap();
/// assert_eq!((closed.timestamp, closed.high, closed.close, closed.volume), (0, 3.0, 2.0, 15.0));
/// ```
pub struct BarConsolidator {
    interval_ms: i64,
    /// The accumulating window (`ts` = bucket start); `None` until the first sample.
    current: Option<Bar>,
}

impl BarConsolidator {
    /// `interval_ms` is floored at 1 purely so the bucket division cannot divide by zero. That is a
    /// last-resort guard, NOT a policy: a caller with its own notion of a sane default (e.g. "fall
    /// back to 1m") must apply it BEFORE constructing.
    pub fn new(interval_ms: i64) -> Self {
        BarConsolidator { interval_ms: interval_ms.max(1), current: None }
    }

    /// The window width this consolidator buckets on (post-floor).
    pub fn interval_ms(&self) -> i64 {
        self.interval_ms
    }

    /// The window currently accumulating (not yet closed), if any.
    pub fn forming(&self) -> Option<&Bar> {
        self.current.as_ref()
    }

    /// Fold one OHLCV sample. Returns `Some(closed)` iff `sample` starts a strictly LATER window —
    /// the returned bar is the just-completed previous one, which the caller must emit BEFORE
    /// acting on `sample` itself.
    pub fn fold(&mut self, sample: &Bar) -> Option<Bar> {
        // Python `%` is floor-mod → rem_euclid (differs from Rust `%` for pre-epoch ts).
        let start = sample.timestamp - sample.timestamp.rem_euclid(self.interval_ms);
        match &mut self.current {
            Some(current) if start > current.timestamp => {
                Some(std::mem::replace(current, Self::open(start, sample)))
            }
            Some(current) => {
                current.high = current.high.max(sample.high);
                current.low = current.low.min(sample.low);
                current.close = sample.close;
                current.volume += sample.volume;
                None
            }
            None => {
                self.current = Some(Self::open(start, sample));
                None
            }
        }
    }

    /// Force-close the open partial window (teardown / manual flush). `None` once drained.
    pub fn flush(&mut self) -> Option<Bar> {
        self.current.take()
    }

    /// Open a fresh window at `start`, seeded from `sample`'s OHLCV.
    fn open(start: i64, sample: &Bar) -> Bar {
        bar(start, sample.open, sample.high, sample.low, sample.close, sample.volume, None, None)
    }
}

#[path = "consolidator_tests.rs"]
#[cfg(test)]
mod consolidator_tests;
