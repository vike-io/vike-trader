//! Every indicator as a named type: `indicators::Rsi`, `indicators::Macd`, `indicators::Doji`, ...
//!
//! Each type implements [`crate::Indicator`], so it has the streaming `on_bar` and the batch
//! `vectorize` that give the same numbers bit for bit. Beyond that trait:
//!
//! - `Default` builds it with the registry defaults.
//! - `new(..)` (for a type with parameters) takes the parameters in the order the registry lists
//!   them and returns a [`crate::ParamError`] for a value outside the registry's range. A type
//!   with no parameters has no `new`; use `Default`.
//! - `next(&bar)` feeds one bar and returns the output directly: an `f64` for a one-line
//!   indicator, an `[f64; N]` for N lines (named by the registry row's `outputs`). It is `NaN`
//!   while the indicator is warming up.
//! - A few multi-line indicators also have `next_named(&bar)`, which returns a struct with one
//!   field per line (for example `MacdOutput { macd, signal, hist }`).
//!
//! Candlestick patterns (`Doji`, `Engulfing`, `MorningStar`, ...) have one output line: `+100.0`
//! for a bullish pattern (or the presence of a neutral one), `-100.0` for a bearish pattern and
//! `0.0` for none. They are causal, per-bar signals and do not warm up to `NaN`; several use a
//! 10-bar average candle body as their sense of a "long" or "short" body.
//!
//! The same indicators are available by name through [`crate::registry::registry`], which also describes
//! their parameters, output lines and warm-up.
//
// Indicator implementations, one file per Python category
// (`vike-trader-app/core/indicators/<category>.py`). Each struct implements
// [`crate::Indicator`]; `batch_*` kernels are faithful f64 ports of the Python
// functions, and `on_bar` reproduces `vectorize` bit-for-bit (gated in
// `tests/parity.rs`). Shared streaming recurrences live in `state`.

pub(crate) mod macros;
pub(crate) mod state;

mod base;
mod classify;
mod momentum;
mod oscillators;
mod outputs;
mod overlap;
mod overlays;
mod patterns;
mod price;
mod running;
mod statistics;
mod streams_bands;
mod streams_compound;
mod streams_directional;
mod streams_ema;
mod streams_multi;
mod streams_stats;
mod streams_wilder;
mod streams_window;
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
    if v.is_finite() && v > 0.0 { v.round().min(crate::roll::RING_CAP as f64) as usize } else { 0 }
}

/// `pbars(v) - n`, saturating — the common "warms up one bar before its period"
/// shape used by the `lookback` overrides.
pub(crate) fn pback(v: f64, n: usize) -> usize {
    pbars(v).saturating_sub(n)
}

pub use base::{
    Atr, Awesome, Bollinger, Cci, Donchian, Ema, Keltner, Macd, Obv, Psar, Roc, Rsi, Sma,
    Stochastic, Vwap, Williams, Wma,
};
pub use momentum::{
    Ac, Aroon, Aroonosc, Bop, Cmo, Dpo, ElderRay, Mom, Rocp, Rocr, Rocr100, Stochf, Ultosc, Vortex,
};
pub use oscillators::{AnchoredVwap, HeikinAshi, WilliamsAd};
pub use outputs::{
    AdxOutput, AroonOutput, BollingerOutput, DonchianOutput, IchimokuOutput, KeltnerOutput,
    MacdOutput, StochasticOutput, VortexOutput,
};
pub use overlap::{Alma, Envelopes, Hma, Midpoint, Midprice, Trima, Vwma};
pub use overlays::{AtrTrailingStop, Evwma, Frama, HiloActivator, Kama, Vidya};
pub use patterns::{
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
pub use price::{Avgprice, Medprice, Typprice, Wclprice};
pub use running::{Ad, Adosc, Asi, Mcginley, NetVolume, Nvi, Pvi, Pvt};
pub use statistics::{
    Kurtosis, Linearreg, LinearregAngle, LinearregIntercept, LinearregSlope, Mad, RankCorrelation,
    Skew, StdError, StdErrorBands, Tsf, Var, Zscore,
};
pub use streams_bands::{AccelerationBands, Qstick, Rwi, TtmSqueeze};
pub use streams_compound::{Apo, Coppock, Dema, Kvo, Mass, Ppo, SmiErgodic, Trix, Tsi, Zlema};
pub use streams_directional::{
    Autocorr, ChaikinVolatility, Chandelier, Dx, MinusDi, MinusDm, PlusDi, PlusDm, Starc,
};
pub use streams_ema::{Kst, T3, Tema};
pub use streams_multi::{
    Alligator, Fisher, Gmma, HighLow52w, Ichimoku, RelativeVigor, Stochrsi, VolumeProfilePoc,
};
pub use streams_stats::{Hurst, LinregChannel, MedianMa, RSquared};
pub use streams_wilder::{Adx, Adxr, Efi, Natr, Smma, Supertrend};
pub use streams_window::{ChandeKrollStop, ConnorsRsi, Hvol, RelativeVolatility};
pub use structure::{PivotPoints, WilliamsFractal, Zigzag};
pub use volatility::{BbandsPctb, BbandsWidth, Chop, DonchianWidth, Stddev, TrueRange, Ulcer};
pub use volume::{Cmf, Eom, Mfi, VolumeOsc};

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
            streams_compound::Dema,
            streams_ema::Tema,
            streams_ema::T3,
            overlap::Hma,
            streams_wilder::Adxr,
            streams_wilder::Adx,
            streams_compound::Tsi,
            streams_compound::SmiErgodic,
            momentum::Stochf,
            streams_multi::Stochrsi,
            streams_ema::Kst,
            streams_window::ConnorsRsi,
            streams_multi::Fisher,
            streams_multi::RelativeVigor,
            streams_compound::Mass,
            streams_window::RelativeVolatility,
            streams_compound::Kvo,
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
                "pvt",
                "kama",
                "vidya",
                "frama",
                "evwma",
                "hilo_activator",
                "atr_trailing_stop",
                "williams_ad",
                "anchored_vwap",
                "heikin_ashi"
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
            if matches!(
                *name,
                "asi"
                    | "adosc"
                    | "kama"
                    | "vidya"
                    | "frama"
                    | "evwma"
                    | "hilo_activator"
                    | "atr_trailing_stop"
            ) {
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
