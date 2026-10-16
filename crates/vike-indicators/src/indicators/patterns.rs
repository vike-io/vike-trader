//! Candlestick patterns — faithful f64 port of vike-trader-app
//! `core/indicators/patterns.py` (63 patterns). Each pattern is a causal
//! per-bar signal returning a single line of `+100.0` (bullish / presence),
//! `-100.0` (bearish), or `0.0` (none) — Python ints as f64. A rolling average
//! body (`avg_body` = SMA(10) of `|close-open|`, NaN warm-up mirroring Python's
//! `None`) supplies the "long/short body" context many patterns use. Streaming
//! is history-recompute via [`hist_indicator!`] (correct-by-construction for
//! these causal patterns; naive-fold order is load-bearing).
#![allow(clippy::needless_range_loop)]
// Candlestick ports mirror the Python `if bullish { ... } elif bearish { ... }`
// shape with a nested directional check; collapsing the nested `if` obscures the
// 1:1 correspondence with patterns.py.
#![allow(clippy::collapsible_if)]

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

const CTX: usize = 10; // bars of context for the rolling average body

/// `_body` — absolute body size `|close - open|`.
fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}

/// `_range` — full high-low range `high - low`.
fn rng(h: f64, l: f64) -> f64 {
    h - l
}

/// `_upper` — upper shadow `high - max(open, close)`.
fn upper(o: f64, h: f64, c: f64) -> f64 {
    h - o.max(c)
}

/// `_lower` — lower shadow `min(open, close) - low`.
fn lower(o: f64, l: f64, c: f64) -> f64 {
    o.min(c) - l
}

/// `_is_white` — bullish candle `close > open`.
fn is_white(o: f64, c: f64) -> bool {
    c > o
}

/// `_is_black` — bearish candle `close < open`.
fn is_black(o: f64, c: f64) -> bool {
    c < o
}

/// `_avg_body` — rolling SMA of `|close-open|` (the "average body" context),
/// aligned, NaN warm-up (Python `None`). Period is `_CTX` (=10) at every site.
fn avg_body(opens: &[f64], closes: &[f64]) -> Vec<f64> {
    let bodies: Vec<f64> = (0..closes.len()).map(|i| (closes[i] - opens[i]).abs()).collect();
    sma(&bodies, CTX)
}

/// `_is_doji` — body tiny relative to the average body (<=10%) and a real range.
/// (`avg is not None` → `!avg.is_nan()`.)
fn is_doji(o: f64, h: f64, l: f64, c: f64, avg: f64) -> bool {
    !avg.is_nan() && avg > 0.0 && body(o, c) <= 0.1 * avg && rng(h, l) > 0.0
}

/// `_is_marubozu` — both shadows <=5% of range (open/close-side near extremes).
fn is_marubozu(o: f64, h: f64, l: f64, c: f64) -> bool {
    let r = rng(h, l);
    if r <= 0.0 {
        return false;
    }
    upper(o, h, c) <= 0.05 * r && lower(o, l, c) <= 0.05 * r
}

#[cfg(test)]
mod flip_rate;
