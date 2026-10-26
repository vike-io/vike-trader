//! Candlestick patterns — faithful f64 port of vike-trader-app
//! `core/indicators/patterns.py` (63 patterns). Each pattern is a causal
//! per-bar signal returning a single line of `+100.0` (bullish / presence),
//! `-100.0` (bearish), or `0.0` (none) — Python ints as f64. A rolling average
//! body (`avg_body` = SMA(10) of `|close-open|`, NaN warm-up mirroring Python's
//! `None`) supplies the "long/short body" context many patterns use. The patterns
//! themselves are bounded-state steppers over the cores of `crate::fast_m`, `crate::fast_n` and
//! `crate::fast_q`; what is left in this file is `flip_rate`, a test-only measurement, and the
//! few helpers it uses.
#![allow(clippy::needless_range_loop)]
// Candlestick ports mirror the Python `if bullish { ... } elif bearish { ... }`
// shape with a nested directional check; collapsing the nested `if` obscures the
// 1:1 correspondence with patterns.py.
#![allow(clippy::collapsible_if)]

#[cfg(test)]
use crate::math::sma;

mod four_five_bar;
mod one_bar;
mod three_bar;
mod two_bar;

pub use four_five_bar::{
    Breakaway, ConcealingBabySwallow, Hikkake, HikkakeMod, LadderBottom, MatHold,
    RiseFallThreeMethods, ThreeLineStrike,
};
pub use one_bar::{
    BeltHold, ClosingMarubozu, Doji, DragonflyDoji, GravestoneDoji, Hammer, HangingMan, HighWave,
    InvertedHammer, LongLine, LongleggedDoji, Marubozu, OpeningMarubozu, RickshawMan, ShootingStar,
    ShortLine, SpinningTop, Takuri,
};
pub use three_bar::{
    AbandonedBaby, AdvanceBlock, EveningDojiStar, EveningStar, IdenticalThreeCrows,
    MorningDojiStar, MorningStar, StalledPattern, StickSandwich, ThreeBlackCrows, ThreeInside,
    ThreeOutside, ThreeStarsInSouth, ThreeWhiteSoldiers, Tristar, TwoCrows, UniqueThreeRiver,
    UpsideGapTwoCrows, XsideGapThreeMethods,
};
pub use two_bar::{
    Counterattack, DarkCloudCover, DojiStar, Engulfing, GapSideSideWhite, Harami, HaramiCross,
    HomingPigeon, InNeck, Kicking, KickingByLength, MatchingLow, MeetingLines, OnNeck, Piercing,
    SeparatingLines, TasukiGap, Thrusting,
};

// ============================ shared helpers ===============================
// Direct ports of the private helpers at the top of patterns.py.

#[cfg(test)]
const CTX: usize = 10; // bars of context for the rolling average body

/// `_body` — absolute body size `|close - open|`.
#[cfg(test)]
fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}

/// `_range` — full high-low range `high - low`.
#[cfg(test)]
fn rng(h: f64, l: f64) -> f64 {
    h - l
}

/// `_is_doji` — body tiny relative to the average body (<=10%) and a real range.
/// (`avg is not None` → `!avg.is_nan()`.)
#[cfg(test)]
fn is_doji(o: f64, h: f64, l: f64, c: f64, avg: f64) -> bool {
    !avg.is_nan() && avg > 0.0 && body(o, c) <= 0.1 * avg && rng(h, l) > 0.0
}

#[cfg(test)]
mod flip_rate;
