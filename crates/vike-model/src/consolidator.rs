//! Consolidate ticks into Bars (the tick->bar step). Exact port of `core/consolidator.py`.
//!
//! Quote bars carry the bucket's OPENING best bid/ask (what a next-open market order fills
//! against), OHLC from the mid, and `volume` = tick count. Trade bars take OHLC from price
//! and `volume` = summed trade size, with no bid/ask.
//!
//! Two shapes live here. The `consolidate_*` fns above are the BATCH port — hex-pinned against the
//! FROZEN r1 fixture bytes by `crates/vike-backtest/tests/parity/r1_parity.rs`'s
//! `consolidators_bit_parity`, so they stay literal and are NOT re-expressed over anything.
//! [`BarConsolidator`] is the STREAMING twin for live/event-time folds, where the tick stream
//! arrives one at a time and there is no `&[tick]` to walk.
//!
//! ⚠ That sentence said "pinned against the Python oracle" until 2026-08-28. The bytes it pins
//! against still CAME from the Python app, which is why the port note above cites
//! `core/consolidator.py` and stays — but no exporter survives in this tree, so the pin is not a
//! comparison against a running Python: it asserts that THIS code has not changed its arithmetic
//! unnoticed. `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is the verdict.

use crate::{Bar, QuoteTick, TradeTick};

#[allow(clippy::too_many_arguments)] // local Bar-literal helper, mirrors the Python ctor call
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
    Bar { ts, open: o, high: h, low: l, close: c, volume, funding: None, bid, ask, symbol: None }
}

pub fn consolidate_quotes(ticks: &[QuoteTick], step_ms: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut cur: Option<i64> = None;
    let (mut o, mut h, mut l, mut c) = (0.0, 0.0, 0.0, 0.0);
    let (mut bid, mut ask) = (0.0, 0.0);
    let mut n: u64 = 0;
    for t in ticks {
        // Python `%` is floor-mod → rem_euclid (differs from Rust `%` for negative ts)
        let start = t.ts - t.ts.rem_euclid(step_ms);
        let m = t.mid();
        if Some(start) != cur {
            if let Some(prev) = cur {
                out.push(bar(prev, o, h, l, c, n as f64, Some(bid), Some(ask)));
            }
            cur = Some(start);
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
    if let Some(prev) = cur {
        out.push(bar(prev, o, h, l, c, n as f64, Some(bid), Some(ask)));
    }
    out
}

pub fn consolidate_trades(ticks: &[TradeTick], step_ms: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut cur: Option<i64> = None;
    let (mut o, mut h, mut l, mut c) = (0.0, 0.0, 0.0, 0.0);
    let mut vol = 0.0;
    for t in ticks {
        let start = t.ts - t.ts.rem_euclid(step_ms);
        if Some(start) != cur {
            if let Some(prev) = cur {
                out.push(bar(prev, o, h, l, c, vol, None, None));
            }
            cur = Some(start);
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
    if let Some(prev) = cur {
        out.push(bar(prev, o, h, l, c, vol, None, None));
    }
    out
}

/// A single tick as a degenerate one-price `Bar` for the per-tick engine path.
/// Quote tick -> OHLC = mid, carrying bid/ask (so the fill model crosses the real spread).
pub fn quote_tick_to_bar(tick: &QuoteTick) -> Bar {
    let m = tick.mid();
    bar(tick.ts, m, m, m, m, 0.0, Some(tick.bid), Some(tick.ask))
}

/// Trade tick -> OHLC = price, volume = size, no bid/ask.
pub fn trade_tick_to_bar(tick: &TradeTick) -> Bar {
    bar(tick.ts, tick.price, tick.price, tick.price, tick.price, tick.size, None, None)
}

/// Streaming epoch-aligned OHLCV bucket fold — the live twin of the batch `consolidate_*` above.
///
/// Feed it OHLCV samples (`&Bar`) stamped with EVENT time and it folds them into fixed
/// `interval_ms` windows aligned to the epoch — the same floor-mod bucket
/// (`ts - ts.rem_euclid(interval_ms)`) the batch fns and `vike_backtest::timeframe::resample` use —
/// emitting the completed window when a sample crosses into a later one. O(1) per sample, no
/// allocation. Event time only (never a wall clock), so a scripted feed replays deterministically
/// and a live feed is correct off the venue's own tick ts.
///
/// A per-tick price folds in as a degenerate one-price sample
/// ([`crate::one_price_bar`]); a coarser bar-to-bar roll-up folds the source bar directly.
///
/// **Monotonic close rule**: only a STRICTLY LATER bucket closes the open window. A sample landing
/// in the current window — or an out-of-order EARLIER one — folds in without closing, so the
/// emitted bar stream can never step backwards in time on a jittery live feed. (This is why
/// `vike_ibkr`'s `BarAggregator`, which closes on ANY bucket change, is deliberately NOT built on
/// this: on a backwards bucket the two rules genuinely disagree.)
///
/// The emitted `ts` is the window's START; a caller whose lane stamps bars with the window's CLOSE
/// adds `interval_ms`. `funding`/`bid`/`ask`/`symbol` are dropped — a consolidated window has no
/// single one of each — so the caller re-stamps whatever its lane needs.
pub struct BarConsolidator {
    interval_ms: i64,
    /// The accumulating window (`ts` = bucket start); `None` until the first sample.
    cur: Option<Bar>,
}

impl BarConsolidator {
    /// `interval_ms` is floored at 1 purely so the bucket division cannot divide by zero. That is a
    /// last-resort guard, NOT a policy: a caller with its own notion of a sane default (e.g. "fall
    /// back to 1m") must apply it BEFORE constructing.
    pub fn new(interval_ms: i64) -> Self {
        BarConsolidator { interval_ms: interval_ms.max(1), cur: None }
    }

    /// The window width this consolidator buckets on (post-floor).
    pub fn interval_ms(&self) -> i64 {
        self.interval_ms
    }

    /// The window currently accumulating (not yet closed), if any.
    pub fn forming(&self) -> Option<&Bar> {
        self.cur.as_ref()
    }

    /// Fold one OHLCV sample. Returns `Some(closed)` iff `sample` starts a strictly LATER window —
    /// the returned bar is the just-completed previous one, which the caller must emit BEFORE
    /// acting on `sample` itself.
    pub fn fold(&mut self, sample: &Bar) -> Option<Bar> {
        // Python `%` is floor-mod → rem_euclid (differs from Rust `%` for pre-epoch ts).
        let start = sample.ts - sample.ts.rem_euclid(self.interval_ms);
        match &mut self.cur {
            Some(cur) if start > cur.ts => Some(std::mem::replace(cur, Self::open(start, sample))),
            Some(cur) => {
                cur.high = cur.high.max(sample.high);
                cur.low = cur.low.min(sample.low);
                cur.close = sample.close;
                cur.volume += sample.volume;
                None
            }
            None => {
                self.cur = Some(Self::open(start, sample));
                None
            }
        }
    }

    /// Force-close the open partial window (teardown / manual flush). `None` once drained.
    pub fn flush(&mut self) -> Option<Bar> {
        self.cur.take()
    }

    /// Open a fresh window at `start`, seeded from `sample`'s OHLCV.
    fn open(start: i64, sample: &Bar) -> Bar {
        bar(start, sample.open, sample.high, sample.low, sample.close, sample.volume, None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::one_price_bar;

    fn sample(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        bar(ts, o, h, l, c, v, None, None)
    }

    #[test]
    fn folds_ohlcv_and_closes_on_a_later_bucket() {
        let mut k = BarConsolidator::new(60_000);
        assert!(k.fold(&sample(60_000, 1.0, 2.0, 0.5, 1.5, 10.0)).is_none(), "first sample opens");
        assert!(k.fold(&sample(65_000, 1.5, 3.0, 1.4, 2.8, 5.0)).is_none(), "same bucket folds in");
        // OHLC: open is the FIRST sample's (never overwritten), high/low are the extremes across
        // the window, close is the last sample's, volume sums.
        let f = k.forming().expect("window open");
        assert_eq!(
            (f.ts, f.open, f.high, f.low, f.close, f.volume),
            (60_000, 1.0, 3.0, 0.5, 2.8, 15.0)
        );
        // a sample in a later bucket closes the prior window and opens the next
        let closed = k.fold(&sample(120_000, 2.8, 2.9, 2.7, 2.85, 3.0)).expect("closes prior");
        assert_eq!(
            (closed.ts, closed.open, closed.high, closed.low, closed.close, closed.volume),
            (60_000, 1.0, 3.0, 0.5, 2.8, 15.0)
        );
        assert_eq!(k.forming().expect("next window open").ts, 120_000);
        // flush drains the partial window, then is idempotent
        assert_eq!(k.flush().expect("partial flushes").ts, 120_000);
        assert!(k.flush().is_none());
    }

    #[test]
    fn a_gap_skips_empty_buckets_rather_than_synthesizing_them() {
        // The fold emits only windows that SAW a sample: jumping 60_000 -> 300_000 closes the one
        // open window and opens the 300_000 one — the three untraded buckets between are simply
        // absent (a consolidator never invents a bar nothing happened in).
        let mut k = BarConsolidator::new(60_000);
        assert!(k.fold(&sample(60_000, 1.0, 1.0, 1.0, 1.0, 1.0)).is_none());
        let closed = k.fold(&sample(300_000, 2.0, 2.0, 2.0, 2.0, 1.0)).expect("closes prior");
        assert_eq!(closed.ts, 60_000);
        assert_eq!(k.forming().expect("open").ts, 300_000);
    }

    #[test]
    fn an_out_of_order_sample_folds_in_and_never_closes_backwards() {
        // THE monotonic close rule (why vike_ibkr's `!=`-closing BarAggregator is not built on
        // this): a stale tick must not emit a backwards-stamped bar onto a live bar lane. It folds
        // into the OPEN window instead — extremes still update, the window start does not move.
        let mut k = BarConsolidator::new(60_000);
        k.fold(&sample(120_000, 1.0, 1.0, 1.0, 1.0, 1.0));
        assert!(k.fold(&sample(30_000, 9.9, 9.9, 0.1, 9.9, 2.0)).is_none(), "stale never closes");
        let f = k.forming().expect("window open");
        assert_eq!((f.ts, f.open, f.high, f.low, f.volume), (120_000, 1.0, 9.9, 0.1, 3.0));
    }

    #[test]
    fn buckets_align_to_the_epoch_by_floor_mod_including_pre_epoch_ts() {
        // Python floor-mod (`rem_euclid`), NOT Rust `%`: a negative ts must floor DOWN to its
        // window start (-1 -> -60_000), not truncate toward zero (which would give 0).
        let mut k = BarConsolidator::new(60_000);
        k.fold(&sample(-1, 1.0, 1.0, 1.0, 1.0, 0.0));
        assert_eq!(k.forming().expect("open").ts, -60_000);
        // and mid-window ts floor to the window start, not to themselves
        let mut k = BarConsolidator::new(60_000);
        k.fold(&sample(61_234, 1.0, 1.0, 1.0, 1.0, 0.0));
        assert_eq!(k.forming().expect("open").ts, 60_000);
    }

    #[test]
    fn a_one_price_tick_stream_folds_like_a_tick_bar_synth() {
        // The tick-lane shape (vike_run's TickBarSynthesizer): each tick enters as a degenerate
        // one-price sample, so OHLC tracks the mid and volume stays exactly 0.0.
        let mut k = BarConsolidator::new(60_000);
        for (ts, px) in [(1_000, 0.50), (2_000, 0.40), (3_000, 0.50)] {
            assert!(k.fold(&one_price_bar(ts, px)).is_none());
        }
        let closed = k.fold(&one_price_bar(61_000, 0.50)).expect("window 0 closes");
        assert_eq!(closed.ts, 0);
        assert_eq!(closed.open.to_bits(), 0.50f64.to_bits());
        assert_eq!(closed.high.to_bits(), 0.50f64.to_bits());
        assert_eq!(closed.low.to_bits(), 0.40f64.to_bits()); // the dip
        assert_eq!(closed.close.to_bits(), 0.50f64.to_bits());
        assert_eq!(closed.volume.to_bits(), 0.0f64.to_bits(), "one-price samples carry no volume");
    }

    #[test]
    fn a_nonsensical_interval_cannot_divide_by_zero() {
        // The floor is a bare div-by-zero guard (callers apply their own sane default first).
        for interval in [0, -5] {
            let mut k = BarConsolidator::new(interval);
            assert_eq!(k.interval_ms(), 1);
            assert!(k.fold(&sample(7, 1.0, 1.0, 1.0, 1.0, 0.0)).is_none());
            assert_eq!(k.forming().expect("open").ts, 7);
        }
    }
}
