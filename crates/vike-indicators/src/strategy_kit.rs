//! What strategy files read beside the registry's typed indicators, the part of the strategy gallery's
//! shared vocabulary that reads an indicator (copied from it, plan 2b, C3; the rest is
//! `vike_tactics`', which names no indicator, decision 0269): a registry indicator fed once per bar by
//! name ([`IndicatorLine`]), the platform's parabolic SAR ([`parabolic_sar`]), FlexiSuperTrend's median distance
//! ([`FlexiMedian`]), the reference's Heikin-Ashi ticker on the instrument's tick ([`HeikinAshiCandles`]), and the
//! NaN-propagating [`max`] and [`min`] (the one copy; this crate's `Qqe` folds with it too).
//!
//! **Another source for the candles** ([`HeikinAshiCandlesSource`], [`with_heikin_ashi_candles_source`]): a [`HeikinAshiCandles`] made
//! while a source is installed on the thread is built by it instead, for its whole life, from the
//! rule's own bars, as `vike_tactics::higher::with_higher_timeframe_source` does for the frames. The strategy lab's
//! controls (candles off the tick, from the venue's history) install both; with none installed nothing
//! changes.

#![expect(
    clippy::type_complexity,
    reason = "the frame and candle builders' types are written out: their aliases added no name (plan 2b section 8)"
)]

use std::cell::RefCell;
use std::rc::Rc;

use vike_marketdata::Bar;

use crate::indicators::{Atr, HeikinAshi, Psar};
use crate::{Indicator, make_with};
use vike_math::statistics::median_of_sorted;

/// A registry indicator, fed once per bar.
pub struct IndicatorLine(Box<dyn Indicator>);

impl IndicatorLine {
    pub fn new(name: &str, params: &[f64]) -> Self {
        IndicatorLine(make_with(name, params).unwrap_or_else(|| panic!("no indicator {name}")))
    }
    pub fn next(&mut self, bar: &Bar) -> Vec<f64> {
        self.0.on_bar(bar)
    }
}

/// The platform's parabolic SAR, as vike's `Psar` computes it: started on the second bar from the first two closes, a reversal
/// judged on the bar the stop as moved reaches (before the clamp to the last two bars), the factor carried and raised only on
/// a bar that is not a reversal. `Psar::uptrend` is its direction, `Psar::stop_ahead` its level for the next bar.
pub fn parabolic_sar(start: f64, increment: f64, maximum: f64) -> Psar {
    Psar::new(increment, maximum)
        .and_then(|p| p.with_start(start))
        .expect("valid factors")
        .with_test_first(true)
        .with_direction_from_closes(true)
        .with_textbook_steps(true)
}

/// The larger of two numbers, `NaN` when either is: the reference's `max`. `f64::max` returns the other operand instead,
/// which would let a value beat a line that does not exist yet. The workspace's one copy: strategy files and this crate's
/// `Qqe` read it here.
#[inline]
pub fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// The smaller of two numbers, `NaN` when either is: the reference's `min`.
#[inline]
pub fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

/// PresentTrading's FlexiSuperTrend oscillator, as two of its rules share it: twenty Supertrends of the typical price over
/// ATR(10), factors 0.618 up by 0.382, their bands starting at the first bar's midpoint; the median of the twenty distances
/// from the price to each one's line, NaN while one is.
// Left: no `Supertrend` start is this one: each trend starts from the first bar's typical price against its midpoint.
pub struct FlexiMedian {
    atr: Atr,
    /// Each line's bands and trend, `1` up or `0` down, `0` at first.
    lines: Option<[(f64, f64, u8); FlexiMedian::LINES]>,
    source_previous: f64,
}

impl Default for FlexiMedian {
    fn default() -> Self {
        FlexiMedian {
            atr: Atr::new(10.0).expect("a valid length"),
            lines: None,
            source_previous: f64::NAN,
        }
    }
}

impl FlexiMedian {
    const LINES: usize = 20;

    pub fn next(&mut self, bar: &Bar) -> f64 {
        let mid = vike_math::price_transforms::median_price(bar.high, bar.low);
        let source = vike_math::price_transforms::typical_price(bar.high, bar.low, bar.close);
        let atr = self.atr.next(bar);
        let lines = self.lines.get_or_insert([(mid, mid, 0); FlexiMedian::LINES]);
        let mut diffs = [0.0; FlexiMedian::LINES];
        let mut factor = 0.618;
        for ((upper, lower, trend), diff) in lines.iter_mut().zip(diffs.iter_mut()) {
            let up = mid + atr * factor;
            let down = mid - atr * factor;
            if source > *upper {
                *trend = 1;
            } else if source < *lower {
                *trend = 0;
            }
            *upper = if self.source_previous < *upper { min(up, *upper) } else { up };
            *lower = if self.source_previous > *lower { max(down, *lower) } else { down };
            *diff = source - if *trend == 1 { *lower } else { *upper };
            factor += 0.382;
        }
        self.source_previous = source;
        if diffs.iter().any(|d| d.is_nan()) {
            return f64::NAN;
        }
        diffs.sort_by(f64::total_cmp);
        median_of_sorted(&diffs)
    }
}

/// Another builder of [`HeikinAshiCandles`] (the module doc).
pub trait HeikinAshiCandlesSource {
    /// Heikin-Ashi candles, as a [`HeikinAshiCandles`] makes them.
    fn candles(&self) -> Box<dyn FnMut(&Bar) -> Bar + Send>;
}

thread_local! {
    /// The source [`with_heikin_ashi_candles_source`] installed on this thread, if any.
    static CANDLE_SOURCE: RefCell<Option<Rc<dyn HeikinAshiCandlesSource>>> = const { RefCell::new(None) };
}

/// Runs `f` with `source` building every [`HeikinAshiCandles`] made on this thread meanwhile, and the source
/// before it back afterwards, a panic included.
pub fn with_heikin_ashi_candles_source<T>(
    source: Rc<dyn HeikinAshiCandlesSource>,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Rc<dyn HeikinAshiCandlesSource>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            CANDLE_SOURCE.with(|s| *s.borrow_mut() = before);
        }
    }
    let _restore = Restore(CANDLE_SOURCE.with(|s| s.borrow_mut().replace(source)));
    f()
}

/// The reference's Heikin-Ashi ticker of the rule's own bars: candles from the rule's first bar, every
/// price on the symbol's tick (`vike_tactics::Tactics::tick`, read on that bar; unrounded where none is
/// known), the open's recursion on the rounded prices ([`HeikinAshi::with_tick`]). Orders still fill
/// at the real prices.
pub struct HeikinAshiCandles {
    candles: Option<HeikinAshi>,
    /// An installed [`HeikinAshiCandlesSource`]'s candles.
    other: Option<Box<dyn FnMut(&Bar) -> Bar + Send>>,
}

impl Default for HeikinAshiCandles {
    fn default() -> Self {
        let other = CANDLE_SOURCE.with(|s| s.borrow().clone()).map(|s| s.candles());
        HeikinAshiCandles { candles: None, other }
    }
}

impl HeikinAshiCandles {
    /// This bar's candle, its time and volume the bar's; `tick` is the rule's `strategy.tick()`.
    pub fn next(&mut self, bar: &Bar, tick: Option<f64>) -> Bar {
        if let Some(other) = &mut self.other {
            return other(bar);
        }
        let candles = self.candles.get_or_insert_with(|| {
            let plain = HeikinAshi::default();
            match tick {
                Some(tick) => plain.with_tick(tick).expect("a positive tick"),
                None => plain,
            }
        });
        let [open, high, low, close] = candles.next(bar);
        Bar { open, high, low, close, ..bar.clone() }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use vike_marketdata::Bar;

    use super::{
        HeikinAshiCandles, HeikinAshiCandlesSource, max, min, with_heikin_ashi_candles_source,
    };

    /// Hands out the bar it is fed with close 0.
    struct Marked;

    impl HeikinAshiCandlesSource for Marked {
        fn candles(&self) -> Box<dyn FnMut(&Bar) -> Bar + Send> {
            Box::new(|bar| Bar { close: 0.0, ..bar.clone() })
        }
    }

    #[test]
    fn a_nan_keeps_max_and_min_nan() {
        assert!(
            max(1.0, f64::NAN).is_nan() && min(f64::NAN, 1.0).is_nan(),
            "a NaN keeps max and min NaN"
        );
        assert_eq!((max(1.0, 2.0), min(1.0, 2.0)), (2.0, 1.0));
    }

    /// Candles made while a source is installed are the source's for their whole life; ones made after
    /// it is uninstalled are the ticker's again.
    #[test]
    fn candles_made_under_a_source_are_the_sources_and_only_then() {
        let bar = Bar::ohlcv(0, 1.0, 1.0, 1.0, 1.0, 1.0);
        let mut marked =
            with_heikin_ashi_candles_source(Rc::new(Marked), HeikinAshiCandles::default);
        assert_eq!(marked.next(&bar, Some(0.01)).close, 0.0);
        assert_eq!(
            HeikinAshiCandles::default().next(&bar, Some(0.01)).close,
            1.0,
            "no source: the ticker's"
        );
    }
}
