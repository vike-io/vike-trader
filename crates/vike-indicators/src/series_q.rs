//! Whole-series calls on plain `f64` slices for group `q` of the fourth wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! The eighteen two-bar candlestick patterns. Each takes parallel `open`, `high`, `low` and `close`
//! slices (the result is as long as the shortest) and returns one signal per bar: `+100.0`
//! (bullish), `-100.0` (bearish) or `0.0` (none). The first bar is always `0.0` (a two-bar pattern
//! needs a bar before it), and so are the first nine bars of a pattern that compares bodies with
//! the ten-bar average body (`doji_star`, `harami_cross`, `counterattack`, `meeting_lines`,
//! `separating_lines`, `matching_low`, `gap_side_side_white`). A pattern has no parameter, so none
//! of these can fail. Each equals the typed stepper's `next_ohlc` folded over the bars, bit for bit.

use crate::fast_q::run;
use crate::fast_q::{
    CounterattackKernel, DarkCloudCoverKernel, DojiStarKernel, EngulfingKernel,
    GapSideSideWhiteKernel, HaramiCrossKernel, HaramiKernel, HomingPigeonKernel, InNeckKernel,
    KickingByLengthKernel, KickingKernel, MatchingLowKernel, MeetingLinesKernel, OnNeckKernel,
    PiercingKernel, SeparatingLinesKernel, TasukiGapKernel, ThrustingKernel,
};

/// `engulfing` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). The current body engulfs the prior opposite-colour body: `+100` for a white bar
/// after a black one whose close is at or above the prior open and whose open is at or below the
/// prior close, `-100` for the mirror image.
#[inline]
pub fn engulfing(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<EngulfingKernel>(open, high, low, close)
}

/// `doji_star` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). A doji (body at most a tenth of the average body, with a real range) gapping away
/// from a prior body at least as long as the average: `-100` above a white bar, `+100` below a
/// black one.
#[inline]
pub fn doji_star(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<DojiStarKernel>(open, high, low, close)
}

/// `harami` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). The current body strictly inside the prior opposite-colour body: `+100` for a white
/// bar after a black one, `-100` for a black bar after a white one.
#[inline]
pub fn harami(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<HaramiKernel>(open, high, low, close)
}

/// `harami_cross` of parallel `open`, `high`, `low` and `close` slices (the result is as long as
/// the shortest). A harami whose current bar is a doji (body at most a tenth of the average body),
/// its midpoint strictly inside the prior body: `+100` after a black bar, `-100` after a white one.
#[inline]
pub fn harami_cross(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<HaramiCrossKernel>(open, high, low, close)
}

/// `piercing` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). A white bar opening below the prior (black) bar's low and closing above its midpoint
/// but below its open: `+100`.
#[inline]
pub fn piercing(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<PiercingKernel>(open, high, low, close)
}

/// `dark_cloud_cover` of parallel `open`, `high`, `low` and `close` slices (the result is as long
/// as the shortest). A black bar opening above the prior (white) bar's high and closing below its
/// midpoint but above its open: `-100`.
#[inline]
pub fn dark_cloud_cover(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<DarkCloudCoverKernel>(open, high, low, close)
}

/// `counterattack` of parallel `open`, `high`, `low` and `close` slices (the result is as long as
/// the shortest). Opposite-colour bars, each with a body of at least 0.3 average bodies, closing
/// within 0.03 average bodies of each other: `+100` for white after black, `-100` for black after
/// white.
#[inline]
pub fn counterattack(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<CounterattackKernel>(open, high, low, close)
}

/// `meeting_lines` of parallel `open`, `high`, `low` and `close` slices (the result is as long as
/// the shortest). A counterattack with a tighter close tolerance (0.02 average bodies).
#[inline]
pub fn meeting_lines(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<MeetingLinesKernel>(open, high, low, close)
}

/// `separating_lines` of parallel `open`, `high`, `low` and `close` slices (the result is as long
/// as the shortest). Two bars of the same colour opening within 0.01 average bodies of each other:
/// `+100` for two white bars, `-100` for two black ones.
#[inline]
pub fn separating_lines(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<SeparatingLinesKernel>(open, high, low, close)
}

/// `matching_low` of parallel `open`, `high`, `low` and `close` slices (the result is as long as
/// the shortest). Two black bars closing within 0.01 average bodies of each other: `+100`.
#[inline]
pub fn matching_low(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<MatchingLowKernel>(open, high, low, close)
}

/// `on_neck` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). A white bar opening below the prior (black) bar's low and closing within 3% of the
/// prior body of that low: `-100`.
#[inline]
pub fn on_neck(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<OnNeckKernel>(open, high, low, close)
}

/// `in_neck` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). A white bar opening below the prior (black) bar's low and closing slightly into its
/// body (above its close, by at most 15% of the body): `-100`.
#[inline]
pub fn in_neck(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<InNeckKernel>(open, high, low, close)
}

/// `thrusting` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). A white bar opening below the prior (black) bar's low and closing above its close
/// but below its midpoint: `-100`.
#[inline]
pub fn thrusting(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<ThrustingKernel>(open, high, low, close)
}

/// `kicking` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). Two opposite-colour marubozu (both shadows at most 5% of the range) with a gap
/// between them: `+100` for white after black opening above its close, `-100` for black after
/// white opening below its close.
#[inline]
pub fn kicking(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<KickingKernel>(open, high, low, close)
}

/// `kicking_by_length` of parallel `open`, `high`, `low` and `close` slices (the result is as long
/// as the shortest). A kicking whose signal is the colour of the longer marubozu (the current one
/// on a tie).
#[inline]
pub fn kicking_by_length(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<KickingByLengthKernel>(open, high, low, close)
}

/// `homing_pigeon` of parallel `open`, `high`, `low` and `close` slices (the result is as long as
/// the shortest). Two black bars, the second's body strictly inside the first's: `+100`.
#[inline]
pub fn homing_pigeon(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<HomingPigeonKernel>(open, high, low, close)
}

/// `gap_side_side_white` of parallel `open`, `high`, `low` and `close` slices (the result is as
/// long as the shortest). Two white bars with non-zero bodies of similar size (their difference at
/// most half the longer), the second opening above the first's close: `+100`.
#[inline]
pub fn gap_side_side_white(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<GapSideSideWhiteKernel>(open, high, low, close)
}

/// `tasuki_gap` of parallel `open`, `high`, `low` and `close` slices (the result is as long as the
/// shortest). An opposite-colour bar opening strictly inside the prior body: `+100` for black
/// after white closing above the prior open, `-100` for white after black closing below it.
#[inline]
pub fn tasuki_gap(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    run::<TasukiGapKernel>(open, high, low, close)
}
