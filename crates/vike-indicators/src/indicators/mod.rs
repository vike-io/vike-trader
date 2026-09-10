//! Indicator implementations, one file per Python category
//! (`vike-trader-app/core/indicators/<category>.py`). Each struct implements
//! [`crate::Indicator`]; `batch_*` kernels are faithful f64 ports of the Python
//! functions, and `on_bar` reproduces `vectorize` bit-for-bit (gated in
//! `tests/parity.rs`). Shared streaming recurrences live in [`state`].

pub(crate) mod macros;
pub(crate) mod state;

mod base;
mod momentum;
mod overlap;
mod patterns;
mod price;
mod statistics;
mod structure;
mod volatility;
mod volume;

use crate::WindowReach;
use vike_model::Bar;

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

/// The path-dependent warm-up set — see [`crate::Indicator::warmup_path_dependent`].
/// These emit a value from bar one (`lookback() == 0`, truthfully) but that value
/// depends on WHERE the series started, so a caller seeding `lookback()` bars is
/// NOT warm. Keyed by registry name so both the macro-generated indicators and the
/// hand-written `base.rs` ones share ONE list.
///
/// Two distinct shapes live here, and the flag covers both:
/// - **cumulative / recurrence** (`obv`, `ad`, `nvi`, `pvi`, `pvt`, `mcginley`,
///   `psar`, `vwap`): the value integrates every bar since the mount.
/// - **fabricated warm-up placeholder** (`net_volume`): the kernel writes a literal
///   `0.0` at index 0 instead of NaN, so the first-non-NaN index is 0 even though
///   the indicator genuinely needs one prior bar (it reads `close[i-1]`). The `==`
///   gate cannot see this — a non-NaN placeholder is indistinguishable from a real
///   value — which is exactly why the flag, not `lookback()`, is what a caller must
///   consult before treating bar zero as warm. The kernel is NOT changed to emit
///   NaN: that would break `vectorize` parity, which is sacred.
///
/// Membership is machine-checked behaviorally by
/// `tests/lookback.rs::zero_lookback_exact_means_genuinely_stateless_or_flagged`
/// (vectorize a suffix, compare against the tail of the full run), so a newly added
/// zero-lookback indicator cannot silently omit the flag.
pub(crate) fn is_path_dependent(key: &str) -> bool {
    matches!(
        key,
        "psar"
            | "vwap"
            | "mcginley"
            | "obv"
            | "ad"
            | "nvi"
            | "pvi"
            | "pvt"
            | "net_volume"
            // ⚠ `asi` and `adosc` were MISSING, and the omission was silent. Both are cumulative
            // by definition — `asi` is the *Accumulative* Swing Index, a running sum since the
            // mount, and `adosc` is an EMA pair over the cumulative `ad` line, so it inherits
            // `ad`'s unbounded accumulation. Neither has any finite window, so `hist_indicator!`'s
            // history trim changed both. They were found by MEASUREMENT rather than by reading:
            // `parity.rs`'s past-the-trim gate diverged on them at a series length no other test
            // in the tree reached.
            | "asi"
            | "adosc"
    )
}

/// The [`crate::WindowReach`] of a macro-generated indicator, keyed by registry name — the
/// STRUCTURAL declaration `hist_indicator!`'s drain sizes retained history from.
///
/// Keyed by name for the same reason [`is_path_dependent`] is: ONE authority that the macro
/// CONSULTS, so the drain and `parity.rs`'s gate can never hold different answers.
///
/// ⚠ **Every `Finite` row below is a claim about the KERNEL, and the gate cannot check it for
/// you.** It was MEASURED that `stochrsi` looks truncation-invariant from 30 retained bars on
/// `parity.rs`'s own 4,000-bar `synth_bars`, while the Wilder `rsi_vals` it is built on needs ~493
/// bars to forget its seed on every data shape tried (analytically `(13/14)^m < 2^-53 => m > 494`)
/// — its `%K` ratio cancels the seed offset to first order, so on most series the last bit lands
/// the same anyway. A `Finite` row for it would be greenlit by the gate and be WRONG. So the rule
/// for adding one is not "the test passes"; it is:
///
/// > the value at bar `i` is a fold over a FIXED COUNT of consecutive bars ending at `i`, with
/// > nothing carried between bars — and every intermediate series it folds has NaN only as a
/// > LEADING prefix, never an interior gap.
///
/// The second clause is the one that bites. `math::smooth_defined` folds over the DEFINED values
/// of its input, so an interior NaN makes the window span MORE bars than the period — unboundedly
/// many, in the limit. That is a reach question, not a warm-up question, and `lookback_full` says
/// nothing about it.
///
/// **Decay-bounded ([`WindowReach::SmoothedOver`]) — an IIR whose SMOOTHING PERIOD is known, so the
/// bound is over that period rather than a blanket multiple of `lookback_full`:**
/// - `relative_volatility` — `smooth_defined(.., ema, ..)`. An EMA never fully forgets, but it
///   forgets at a rate set by `period`, NOT by `lookback_full = 2p - 2`. Measured stable from
///   **305** bars at `p = 14` (analytic EMA bound `18.4 * 14 = 258`); `37 * 14 = 518` retained
///   against the 1665 the blanket rule gave it.
/// - `stochrsi` — `rsi_vals` is a Wilder recurrence over `rsi_p`, measured to need ~493 bars;
///   `37 * 14 = 518` against 1857.
///
/// **Deliberately withheld, and NOT a decay problem — no factor can help these:**
/// - `hvol` — windows over the DEFINED log returns, and `libm::log(v[i]/v[i-1])` is undefined
///   wherever a close is `<= 0.0`, which is an INTERIOR gap. A 32-bar slack tolerates at most 32
///   such bars and `synth_bars` has no zero prices, so the gate would never see the difference.
/// - `kst` — same shape twice over: `roc` emits NaN wherever the prior close is exactly `0.0`, and
///   `kst_line` is NaN wherever ANY of its four components is, before `smooth_defined` compacts.
///
/// **Everything not named below is `Smoothed`, and that is STILL not a claim it is smoothed** — it
/// is the safe default, and a wrong `Smoothed` costs only memory while a wrong `Finite` is a
/// silently wrong number. The roster has since been walked end to end, though, so the remainder is
/// now a measured set rather than an unexamined one: all 171 registry rows were read against the
/// three disqualifiers above, and what stayed `Smoothed` did so for a stated reason — a recursive
/// `ema`/`rma` term, or a `smooth_defined` fold whose input can carry an interior NaN. This
/// paragraph used to name `donchian_width`, `midpoint`, `ulcer`, `chop`, `mad`, `skew` and the
/// `linearreg` family as finite-but-omitted; every one of them is granted below now, which is what
/// that sentence was promising.
/// The SMOOTHING period a [`WindowReach::SmoothedOver`] row's retention is bounded by, read out of
/// that indicator's own coerced parameters.
///
/// ⚠ It is the period of the RECURRENCE, not the indicator's first parameter and not its warm-up.
/// `stochrsi` smooths with `rsi_p` (its FIRST param) while its `k`/`d` windows are finite; naming
/// the wrong one would under-retain silently, which is why each arm cites the kernel it read.
///
/// Every key `window_reach` marks `SmoothedOver` must have an arm here — `hist_indicator!` calls
/// this to fill in the placeholder, and a missing arm would retain the `KEEP_FLOOR` minimum.
pub(crate) fn smoothing_period(key: &str, params: &[f64]) -> usize {
    let p = |i: usize| params.get(i).copied().unwrap_or(0.0).max(0.0).round() as usize;
    match key {
        // `volatility.rs`'s `batch_relative_volatility`: `smooth_defined(.., ema, period)` over the
        // first parameter.
        "relative_volatility" => p(0),
        // `momentum.rs`'s `batch_stochrsi`: `rsi_vals(.., rsi_p)` — the Wilder recurrence — is the
        // FIRST parameter; `k`/`d` smooth finite windows over its output.
        "stochrsi" => p(0),
        // Unreached: `window_reach` marks no other key `SmoothedOver`, and the gate above pins it.
        _ => 0,
    }
}

pub(crate) fn window_reach(key: &str) -> WindowReach {
    match key {
        // The decay-bounded pair. The `0` is a PLACEHOLDER — `hist_indicator!` replaces it with
        // `smoothing_period(key, params)`, which is the only place an indicator's runtime params
        // are in scope. See this function's doc.
        "relative_volatility" | "stochrsi" => WindowReach::SmoothedOver(0),
        // ---- windows over the closes, directly ---------------------------------------------
        // Each folds exactly `period` consecutive closes and carries nothing. (Every one of these
        // was a running accumulator until the de-accumulation, which is why they are also the
        // rows that just left `NOT_TRUNCATION_INVARIANT`.)
        // ---- the candlestick patterns ------------------------------------------------------
        // ⚠ Every one of these reads through ONE kernel:
        // `crates/vike-indicators/src/indicators/patterns.rs`'s `avg_body`, which is
        // `sma(|close - open|, CTX)`. Three facts make that finite, and all three were checked
        // against the file rather than assumed:
        //   1. `|close - open|` is ALWAYS defined — no division, no `ln`, nothing undefined at
        //      zero — so the series `sma` folds has no interior NaN. That is the trap that
        //      disqualifies `hvol` and `kst`, and it cannot arise here.
        //   2. `patterns.rs` contains no `smooth_defined`, `ema` or `rma` call at all, so there is
        //      no recursive term hiding behind a finite-looking window.
        //   3. `sma` is a per-window fold since the de-accumulation, carrying nothing between bars.
        //
        // Reach is therefore `CTX` plus the few consecutive bars a pattern inspects (at most five,
        // for the three-bar shapes with gaps) — call it ~15. ⚠ None of these declares a `lookback`
        // clause, so `lookback_full()` is 0 and `keep_for` answers with the FLOOR either way:
        // `KEEP_FLOOR` 256 before, `FINITE_FLOOR` 64 now. A 4x cut with 4x margin over the reach,
        // and `finite_window_indicators_are_truncation_invariant_across_shapes_and_params` proves
        // it across four data shapes rather than taking this comment's word for it.
        "abandoned_baby"
        | "advance_block"
        | "belt_hold"
        | "breakaway"
        | "closing_marubozu"
        | "concealing_baby_swallow"
        | "counterattack"
        | "dark_cloud_cover"
        | "doji"
        | "doji_star"
        | "dragonfly_doji"
        | "engulfing"
        | "evening_doji_star"
        | "evening_star"
        | "gap_side_side_white"
        | "gravestone_doji"
        | "hammer"
        | "hanging_man"
        | "harami"
        | "harami_cross"
        | "high_wave"
        | "hikkake"
        | "hikkake_mod"
        | "homing_pigeon"
        | "identical_three_crows"
        | "in_neck"
        | "inverted_hammer"
        | "kicking"
        | "kicking_by_length"
        | "ladder_bottom"
        | "long_line"
        | "longlegged_doji"
        | "marubozu"
        | "mat_hold"
        | "matching_low"
        | "meeting_lines"
        | "morning_doji_star"
        | "morning_star"
        | "on_neck"
        | "opening_marubozu"
        | "piercing"
        | "rickshaw_man"
        | "rise_fall_three_methods"
        | "separating_lines"
        | "shooting_star"
        | "short_line"
        | "spinning_top"
        | "stalled_pattern"
        | "stick_sandwich"
        | "takuri"
        | "tasuki_gap"
        | "three_black_crows"
        | "three_inside"
        | "three_line_strike"
        | "three_outside"
        | "three_stars_in_south"
        | "three_white_soldiers"
        | "thrusting"
        | "tristar"
        | "two_crows"
        | "unique_three_river"
        | "upside_gap_two_crows"
        | "xside_gap_three_methods" => WindowReach::Finite,

        "stddev" | "var" | "zscore" => WindowReach::Finite,
        // `math::sma` over the closes; `bollinger_vals` adds a two-pass window variance around
        // that same mean, over the same `period` bars.
        "envelopes" | "bbands_pctb" | "bbands_width" => WindowReach::Finite,
        // Two window folds over `period` bars (`Σ close*volume`, `Σ volume`) — `clv_series` is
        // per-bar arithmetic with no history at all.
        "vwma" | "cmf" => WindowReach::Finite,
        // ---- windows that compose, with LEADING-ONLY NaN in between ------------------------
        // `sma(c, p1)` then `smooth_defined(.., sma, p2)`: `sma`'s NaN is a leading prefix, so the
        // compaction never skips an interior bar. Reach `p1 + p2 - 1 == period`.
        "trima" => WindowReach::Finite,
        // `c[i - shift] - sma(c, period)[i]`, `shift = period/2 + 1` — reach `max(period, shift+1)`.
        "dpo" => WindowReach::Finite,
        // `ao_vals` = `sma(median,5) - sma(median,34)` (NaN through index 33, a leading prefix),
        // then `smooth_defined(.., sma, 5)`. Reach 38 bars, and `lookback_full` is the constant 37.
        "ac" => WindowReach::Finite,
        // `%K` is a window max/min ratio that emits `0.0` — NOT NaN — on a flat window, so
        // `k_line` too is NaN only as a leading prefix and `%D`'s `smooth_defined` skips nothing.
        // Reach `k + d - 1`, against a `lookback_full` of `(k-1) + (d-1)`.
        "stochf" => WindowReach::Finite,
        // ⚠ `eom` looks like the `hvol`/`kst` hazard and is NOT: `batch_eom`'s degenerate arm
        // writes `raw[i] = 0.0` when the bar has no range or no volume, so the ONLY NaN is
        // `raw[0]` (no previous bar). Leading prefix again. Reach `period + 1`.
        "eom" => WindowReach::Finite,

        // ==== the remaining finite-window kernels ==========================================
        //
        // Each row below was read against its kernel for the SAME three disqualifiers, in the same
        // order, because only one of them is caught by a gate:
        //
        //   1. a recursive term (`ema`/`rma`, or a helper reaching one). The truncation gate DOES
        //      catch a wrong grant here — at `FINITE_FLOOR` = 64 an EMA's surviving influence is
        //      ~2e-3, vastly above the 2^-53 bit-exactness bound — so it fails on the first shape.
        //   2. ⚠ a `smooth_defined` fold over a series that can carry an INTERIOR NaN. The gate does
        //      NOT reliably catch this: it only manifests when the data contains one, and the
        //      synthetic bars have no flat ranges and no non-positive closes. This is the
        //      disqualifier the reading exists for. NONE of the kernels below calls
        //      `smooth_defined` at all.
        //   3. a division / `ln` / `sqrt` that can emit NaN or Inf mid-series — the thing that
        //      CREATES the interior NaN (2) needs. Division alone is not disqualifying; every
        //      divisor below is either param-derived or explicitly guarded.
        //
        // ---- window max/min, folded fresh per bar -----------------------------------------
        // `donchian_vals`-shaped: `fold(h[i+1-p..=i], max)` / `fold(l[..], min)`, carrying nothing.
        // NaN is a LEADING prefix only, and each is its own output — nothing folds it again.
        "donchian_width" | "high_low_52w" | "midpoint" | "midprice" => WindowReach::Finite,
        // Same shape, reporting the ARGMAX/ARGMIN position rather than the value.
        "aroon" | "aroonosc" => WindowReach::Finite,
        // A FIXED five-bar window (a fractal is a local extreme with two bars either side), so the
        // reach is a constant rather than a parameter.
        "williams_fractal" => WindowReach::Finite,
        // ---- per-bar arithmetic over a fixed lag ------------------------------------------
        // `c[i] - c[i-n]` and its ratio forms; reach `n + 1`. `bop` reads ONE bar (`(c-o)/(h-l)`,
        // with the degenerate arm guarded), so its reach is 1.
        "mom" | "rocp" | "rocr" | "rocr100" | "bop" => WindowReach::Finite,
        // `true_range` is `max` of three differences reading bar `i` and `c[i-1]` — two bars. It is
        // the plain TR series, NOT `atr`, which would bring Wilder `rma` and is correctly absent.
        "true_range" => WindowReach::Finite,
        // ---- window sums of a per-bar quantity ---------------------------------------------
        // Each folds a per-bar series (up/down moves, money flow, bp/tr, TR) over `period` bars and
        // divides by a guarded window total. `chop` takes `libm::log10` strictly inside its
        // `rng > 0.0 && sum_tr > 0.0` guard, so it creates no NaN at all.
        "cmo" | "mfi" | "ultosc" | "vortex" | "chop" => WindowReach::Finite,
        // `batch_ulcer`'s drawdown RMS is a window max plus a fold of squared percentage drawdowns
        // over the same window — `sqrt` of a sum of squares, never negative.
        "ulcer" => WindowReach::Finite,
        // ---- weighted window folds ----------------------------------------------------------
        // `alma`'s Gaussian weights are param-derived and identical at every bar; `hma` composes
        // `math::wma`, itself a weighted fold over `period` consecutive closes. Neither carries
        // state, and `wma` is NOT `ema` — the weights are positional, not recursive.
        "alma" | "hma" => WindowReach::Finite,
        // ---- two-pass moment folds over one window ------------------------------------------
        // Mean over the window, then the deviations over that SAME window — the shape `var`/`zscore`
        // above already occupy, and the shape the de-accumulation left behind (no `run_sum` pair
        // survives). Degenerate branches write `0.0`, not NaN.
        "kurtosis" | "skew" | "mad" => WindowReach::Finite,
        // `rank_correlation` sorts a freshly-sliced window per bar; its divisor `p*(p*p-1)` is
        // param-derived and explicitly guarded.
        "rank_correlation" => WindowReach::Finite,
        // ---- closed-form OLS over one window --------------------------------------------------
        // All six route through `statistics.rs`'s `ols`, whose `sx`/`sx2` are exact integer closed
        // forms of the period and whose `sy`/`sxy` are naive folds over the window slice. Nothing
        // is carried between bars.
        "linearreg"
        | "linearreg_slope"
        | "linearreg_angle"
        | "linearreg_intercept"
        | "tsf"
        | "std_error"
        | "std_error_bands" => WindowReach::Finite,
        // ---- structure ------------------------------------------------------------------------
        // `pivot_points` derives all seven lines from the PRIOR period's OHLC alone;
        // `volume_profile_poc` histograms one window of (price, volume) pairs per bar.
        "pivot_points" | "volume_profile_poc" => WindowReach::Finite,

        _ => WindowReach::Smoothed,
    }
}

pub use base::{
    Atr, Awesome, Bollinger, Cci, Donchian, Ema, Keltner, Macd, Obv, Psar, Roc, Rsi, Sma,
    Stochastic, Vwap, Williams, Wma,
};
pub use momentum::{
    Ac, Adx, Adxr, Apo, Aroon, Aroonosc, Asi, Bop, ChandeKrollStop, Cmo, ConnorsRsi, Coppock, Dpo,
    ElderRay, Fisher, Kst, Mom, Ppo, RelativeVigor, Rocp, Rocr, Rocr100, SmiErgodic, Stochf,
    Stochrsi, Trix, Tsi, Ultosc, Vortex,
};
pub use overlap::{
    Alligator, Alma, Dema, Envelopes, Gmma, Hma, Ichimoku, Mcginley, Midpoint, Midprice, Smma,
    Supertrend, T3, Tema, Trima, Vwma, Zlema,
};
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
pub use statistics::{
    Kurtosis, Linearreg, LinearregAngle, LinearregIntercept, LinearregSlope, Mad, RankCorrelation,
    Skew, StdError, StdErrorBands, Tsf, Var, Zscore,
};
pub use structure::{PivotPoints, VolumeProfilePoc, WilliamsFractal, Zigzag};
pub use volatility::{
    BbandsPctb, BbandsWidth, Chop, DonchianWidth, HighLow52w, Hvol, Mass, Natr, RelativeVolatility,
    Stddev, TrueRange, Ulcer,
};
pub use volume::{Ad, Adosc, Cmf, Efi, Eom, Kvo, Mfi, NetVolume, Nvi, Pvi, Pvt, VolumeOsc};

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
