//! Whole-series calls on plain `f64` slices for group `n` of the fourth wave of indicators (the 27
//! candlestick patterns that read three to five bars), re-exported from [`crate::series`]
//! (`series::<name>(...)`).
//!
//! Every call takes the four parallel price slices (the result is as long as the shortest) and gives
//! `+100.0`, `-100.0` or `0.0` for each bar, `0.0` for the first bars the pattern cannot judge yet.
//! They have no parameter, so none can fail. Each runs the same `push` as the stepper of the same
//! name, so the answer equals `next` / `next_ohlc` / `vectorize` bit for bit.

use crate::fast_n::PatPush;

/// One pass of a pattern over parallel price slices.
#[inline(always)]
fn run<C: PatPush>(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let len = open.len().min(high.len()).min(low.len()).min(close.len());
    let mut core = C::new();
    // Filled first, then written by index (the page faults of a fresh buffer do not land inside the loop).
    let mut out = vec![f64::NAN; len];
    for ((((o, &op), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
        *o = core.push(op, h, l, c);
    }
    out
}

/// `morning_star` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): long black, a small star gapping below it, then a long white closing above the first body's midpoint (bullish). The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn morning_star(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::MorningStarK>>(open, high, low, close)
}

/// `evening_star` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): long white, a small star gapping above it, then a long black closing below the first body's midpoint (bearish). The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn evening_star(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::EveningStarK>>(open, high, low, close)
}

/// `morning_doji_star` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): morning star whose middle bar is a doji (a body of at most a tenth of the average body). The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn morning_doji_star(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::MorningDojiStarK>>(open, high, low, close)
}

/// `evening_doji_star` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): evening star whose middle bar is a doji (a body of at most a tenth of the average body). The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn evening_doji_star(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::EveningDojiStarK>>(open, high, low, close)
}

/// `three_white_soldiers` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three progressing long white candles, each opening inside the previous body and closing higher with short upper shadows. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn three_white_soldiers(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::ThreeWhiteSoldiersK>>(open, high, low, close)
}

/// `three_black_crows` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three progressing long black candles, each opening inside the previous body and closing lower with short lower shadows. The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn three_black_crows(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::ThreeBlackCrowsK>>(open, high, low, close)
}

/// `identical_three_crows` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three long black candles each opening at about the previous close. The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn identical_three_crows(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::IdenticalThreeCrowsK>>(open, high, low, close)
}

/// `three_inside` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a harami (the second body inside the first) followed by a third bar confirming the reversal. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn three_inside(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::ThreeInsideK>>(open, high, low, close)
}

/// `three_outside` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): an engulfing second bar followed by a third bar confirming the reversal. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn three_outside(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::ThreeOutsideK>>(open, high, low, close)
}

/// `three_stars_in_south` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three black candles of diminishing range with the second low above the first. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn three_stars_in_south(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::ThreeStarsInSouthK>>(open, high, low, close)
}

/// `abandoned_baby` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a doji gapping clear of both neighbours (an island), then a confirming bar. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn abandoned_baby(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::AbandonedBabyK>>(open, high, low, close)
}

/// `advance_block` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three rising white candles whose bodies shrink or whose upper shadows grow (a bearish warning). The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn advance_block(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::AdvanceBlockK>>(open, high, low, close)
}

/// `stalled_pattern` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): two long white candles then a small white one opening near the second close (a bearish stall). The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn stalled_pattern(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::StalledPatternK>>(open, high, low, close)
}

/// `two_crows` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a long white candle, a black one gapping up, then a black one closing inside the first body. The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn two_crows(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::TwoCrowsK>>(open, high, low, close)
}

/// `upside_gap_two_crows` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a long white candle, a black one gapping up, then a black one engulfing it but still above the white close. The signal is `-100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn upside_gap_two_crows(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::UpsideGapTwoCrowsK>>(open, high, low, close)
}

/// `tristar` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three dojis with a gapping middle one, signed by the direction of the gap. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn tristar(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::TristarK>>(open, high, low, close)
}

/// `unique_three_river` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a long black candle, a black one inside it with a lower low, then a small white one. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn unique_three_river(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::UniqueThreeRiverK>>(open, high, low, close)
}

/// `stick_sandwich` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): black, white, black with the outer closes equal to within three percent of the average body. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn stick_sandwich(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::StickSandwichK>>(open, high, low, close)
}

/// `xside_gap_three_methods` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a gap between two same-colour candles then a candle of the other colour closing back into the gap. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 2 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn xside_gap_three_methods(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core3<crate::fast_n::XsideGapThreeMethodsK>>(open, high, low, close)
}

/// `three_line_strike` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): three candles trending one way then a fourth of the other colour that engulfs them. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 3 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn three_line_strike(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core4<crate::fast_n::ThreeLineStrikeK>>(open, high, low, close)
}

/// `concealing_baby_swallow` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): four black candles, the first two marubozu, the fourth engulfing the third. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 3 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn concealing_baby_swallow(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core4<crate::fast_n::ConcealingBabySwallowK>>(open, high, low, close)
}

/// `hikkake` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): an inside bar, a false breakout, then a close beyond the inside bar on the other side. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 3 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn hikkake(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core4<crate::fast_n::HikkakeK>>(open, high, low, close)
}

/// `ladder_bottom` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): four black candles stepping down then a white one closing above the fourth open. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 4 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn ladder_bottom(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core5<crate::fast_n::LadderBottomK>>(open, high, low, close)
}

/// `rise_fall_three_methods` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a long candle, three small counter-trend candles inside its range, then a long candle of the first colour closing beyond it. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 4 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn rise_fall_three_methods(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core5<crate::fast_n::RiseFallThreeMethodsK>>(open, high, low, close)
}

/// `mat_hold` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a long white candle, a gapping small black one, two small candles holding above the first open, then a long white breakout. The signal is `+100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 4 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn mat_hold(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core5<crate::fast_n::MatHoldK>>(open, high, low, close)
}

/// `hikkake_mod` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): the modified hikkake. The signal is ` an inside bar, a false breakout, a close beyond the range, and a confirming bar:+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 4 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn hikkake_mod(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core5<crate::fast_n::HikkakeModK>>(open, high, low, close)
}

/// `breakaway` from parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest): a run of four same-colour candles stepping one way, then a candle of the other colour closing back to the second open. The signal is `+100 or -100` where the pattern completes on that bar and `0`
/// elsewhere, including the first 4 bars. It has no parameter, so it cannot fail.
#[inline]
pub fn breakaway(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<crate::fast_n::Core5<crate::fast_n::BreakawayK>>(open, high, low, close)
}
