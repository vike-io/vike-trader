//! Indicator implementations, one file per Python category
//! (`vike-trader-app/core/indicators/<category>.py`). Each struct implements
//! [`crate::Indicator`]; `batch_*` kernels are faithful f64 ports of the Python
//! functions, and `on_bar` reproduces `vectorize` bit-for-bit (gated in
//! `tests/parity.rs`). Shared streaming recurrences live in [`state`].

pub(crate) mod macros;
pub(crate) mod state;

mod base;
mod classify;
mod momentum;
mod overlap;
mod patterns;
mod price;
mod statistics;
mod structure;
mod volatility;
mod volume;

pub(crate) use classify::{is_path_dependent, smoothing_period, window_reach};
use vike_marketdata::Bar;

/// Streaming via history-recompute: given the full bar history seen so far
/// (current bar already pushed), return the last column value of each output
/// line of `batch(history)`. Correct-by-construction for any **causal** indicator
/// — `batch(bars[0..=i])[i] == vectorize(full)[i]` because the batch reads only
/// past+present — so `on_bar`-fold == `vectorize` bit-for-bit (the parity gate).
/// `arity` is the number of output lines (used only when `history` is empty).
/// vike-indicators is not the vike-core hot path, so the O(n) per-bar recompute
/// is acceptable for the indicators whose recurrence is not cleanly incremental.
pub(crate) fn stream_tail(
    history: &[Bar],
    batch: impl Fn(&[Bar]) -> Vec<Vec<f64>>,
    arity: usize,
) -> Vec<f64> {
    let cols = batch(history);
    if cols.is_empty() {
        return vec![f64::NAN; arity];
    }
    cols.iter().map(|line| line.last().copied().unwrap_or(f64::NAN)).collect()
}

/// A period parameter (stored as `f64`, see `hist_indicator!`) as a whole bar
/// count — the same `round() as usize` the batch kernels do, floored at 0 so a
/// degenerate/non-finite param can never wrap a `usize`.
pub(crate) fn pbars(v: f64) -> usize {
    if v.is_finite() && v > 0.0 { v.round() as usize } else { 0 }
}

/// `pbars(v) - n`, saturating — the common "warms up one bar before its period"
/// shape used by the `lookback` overrides.
pub(crate) fn pback(v: f64, n: usize) -> usize {
    pbars(v).saturating_sub(n)
}

pub(crate) use base::{
    Atr, Awesome, Bollinger, Cci, Donchian, Ema, Keltner, Macd, Obv, Psar, Roc, Rsi, Sma,
    Stochastic, Vwap, Williams, Wma,
};
pub(crate) use momentum::{
    Ac, Adx, Adxr, Apo, Aroon, Aroonosc, Asi, Bop, ChandeKrollStop, Cmo, ConnorsRsi, Coppock, Dpo,
    ElderRay, Fisher, Kst, Mom, Ppo, RelativeVigor, Rocp, Rocr, Rocr100, SmiErgodic, Stochf,
    Stochrsi, Trix, Tsi, Ultosc, Vortex,
};
pub(crate) use overlap::{
    Alligator, Alma, Dema, Envelopes, Gmma, Hma, Ichimoku, Mcginley, Midpoint, Midprice, Smma,
    Supertrend, T3, Tema, Trima, Vwma, Zlema,
};
pub(crate) use patterns::{
    AbandonedBaby, AdvanceBlock, BeltHold, Breakaway, ClosingMarubozu, ConcealingBabySwallow,
    Counterattack, DarkCloudCover, Doji, DojiStar, DragonflyDoji, Engulfing, EveningDojiStar,
    EveningStar, GapSideSideWhite, GravestoneDoji, Hammer, HangingMan, Harami, HaramiCross,
    HighWave, Hikkake, HikkakeMod, HomingPigeon, IdenticalThreeCrows, InNeck, InvertedHammer,
    Kicking, KickingByLength, LadderBottom, LongLine, LongleggedDoji, Marubozu, MatHold,
    MatchingLow, MeetingLines, MorningDojiStar, MorningStar, OnNeck, OpeningMarubozu, Piercing,
    RickshawMan, RiseFallThreeMethods, SeparatingLines, ShootingStar, ShortLine, SpinningTop,
    StalledPattern, StickSandwich, Takuri, TasukiGap, ThreeBlackCrows, ThreeInside,
    ThreeLineStrike, ThreeOutside, ThreeStarsInSouth, ThreeWhiteSoldiers, Thrusting, Tristar,
    TwoCrows, UniqueThreeRiver, UpsideGapTwoCrows, XsideGapThreeMethods,
};
pub(crate) use price::{Avgprice, Medprice, Typprice, Wclprice};
pub(crate) use statistics::{
    Kurtosis, Linearreg, LinearregAngle, LinearregIntercept, LinearregSlope, Mad, RankCorrelation,
    Skew, StdError, StdErrorBands, Tsf, Var, Zscore,
};
pub(crate) use structure::{PivotPoints, VolumeProfilePoc, WilliamsFractal, Zigzag};
pub(crate) use volatility::{
    BbandsPctb, BbandsWidth, Chop, DonchianWidth, HighLow52w, Hvol, Mass, Natr, RelativeVolatility,
    Stddev, TrueRange, Ulcer,
};
pub(crate) use volume::{Ad, Adosc, Cmf, Efi, Eom, Kvo, Mfi, NetVolume, Nvi, Pvi, Pvt, VolumeOsc};

#[cfg(test)]
mod lookback_raw_tests {
    use super::*;
    use crate::Indicator;

    /// The RAW (un-coerced) constructor path: `with_params` — unlike the registry's
    /// `make_with` — does NOT clamp to the `ParamSpec` grid, so a degenerate param
    /// reaches the `lookback` arithmetic verbatim. Every override must therefore be
    /// saturating: this test fails (debug: overflow panic; release: an absurd
    /// wrapped value) on any override that subtracts non-saturatingly.
    #[test]
    fn with_params_degenerate_never_wraps() {
        macro_rules! probe {
            ($($ty:ty),* $(,)?) => {$(
                for raw in [
                    &[0.0f64][..],
                    &[0.0, 0.0, 0.0][..],
                    &[f64::NAN, f64::NAN, f64::NAN][..],
                    &[-1e9, -1e9, -1e9][..],
                    &[f64::INFINITY, f64::NEG_INFINITY, 0.0][..],
                ] {
                    let ind = <$ty>::with_params(raw);
                    let (lb, full) = (ind.lookback(), ind.lookback_full());
                    assert!(lb < 1_000_000, "{} lookback {lb} wrapped at {raw:?}", ind.name());
                    assert!(full < 1_000_000, "{} lookback_full {full} wrapped at {raw:?}", ind.name());
                    assert!(full >= lb, "{} lookback_full {full} < lookback {lb}", ind.name());
                }
            )*};
        }
        probe!(
            overlap::Dema,
            overlap::Tema,
            overlap::T3,
            overlap::Hma,
            momentum::Adxr,
            momentum::Adx,
            momentum::Tsi,
            momentum::SmiErgodic,
            momentum::Stochf,
            momentum::Stochrsi,
            momentum::Kst,
            momentum::ConnorsRsi,
            momentum::Fisher,
            momentum::RelativeVigor,
            volatility::Mass,
            volatility::RelativeVolatility,
            volume::Kvo,
        );
    }

    /// The path-dependent set is exactly the documented one, and every name in it is a real
    /// registry key.
    ///
    /// ⚠ `asi` and `adosc` JOINED this set after `hist_indicator!`'s history trim was found to be
    /// corrupting them. Both are cumulative by definition — `asi` is the *Accumulative* Swing
    /// Index, a running sum since the mount, and `adosc` is an EMA pair over the cumulative `ad`
    /// line, inheriting its unbounded accumulation — so neither has a finite window and the trim
    /// changed both past 512 bars. They were found by MEASUREMENT (`tests/parity.rs`'s
    /// past-the-trim gate), not by reading this list, which is the point: a pin records an answer,
    /// it cannot derive one.
    #[test]
    fn path_dependent_set_is_pinned() {
        let flagged: Vec<&str> = crate::registry()
            .iter()
            .filter(|m| m.warmup_path_dependent())
            .map(|m| m.name)
            .collect();
        assert_eq!(
            flagged,
            vec![
                "vwap",
                "psar",
                "obv",
                "asi",
                "mcginley",
                "ad",
                "adosc",
                "net_volume",
                "nvi",
                "pvi",
                "pvt"
            ]
        );
        // ⚠ The `lookback() == 0` assertion is now SCOPED rather than applied to the whole set,
        // and the reason matters: a zero lookback was never the DEFINING property of
        // path-dependence, it merely happened to hold for all nine original members. `asi` (1) and
        // `adosc` (an EMA pair) both declare a real warm-up and are path-dependent anyway.
        //
        // Those are independent facts: `lookback` answers "when does a value first appear",
        // path-dependence answers "how far back does it read". Conflating them is what would have
        // kept the exemption from the two indicators that most needed it — so the original
        // assertion is kept for the members it was written about, and the new pair is asserted to
        // be the counterexample rather than quietly skipped.
        for name in &flagged {
            let meta = crate::get(name).unwrap();
            if matches!(*name, "asi" | "adosc") {
                assert!(
                    meta.lookback(&[]) > 0,
                    "{name} is the counterexample: path-dependent WITH a declared warm-up"
                );
                continue;
            }
            assert_eq!(meta.lookback(&[]), 0, "{name} is only misleading when lookback is 0");
        }
    }
}
