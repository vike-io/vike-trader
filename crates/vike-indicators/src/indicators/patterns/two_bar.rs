//! Two-bar candlestick patterns: each reads the current bar and the one before it.
//!
//! Hand-written bounded-state steppers (`trims_history() == false`) over the cores of
//! `crate::fast_q`: the previous bar's four prices and, for the seven patterns that compare bodies
//! with the average body, an `AvgBody` (`crate::fast_q`). Streaming one bar is a handful of
//! comparisons instead of the batch kernel re-run over a retained tail; the stream, `vectorize` and
//! `series::<pattern>` are the same `push`, so they agree bit for bit, and the signal is bit for bit
//! the one the old kernels gave (`tests/fast_q.rs` keeps a copy of them and proves it).

use crate::fast_q::{
    CounterattackKernel, DarkCloudCoverKernel, DojiStarKernel, EngulfingKernel,
    GapSideSideWhiteKernel, HaramiCrossKernel, HaramiKernel, HomingPigeonKernel, InNeckKernel,
    KickingByLengthKernel, KickingKernel, MatchingLowKernel, MeetingLinesKernel, OnNeckKernel,
    PiercingKernel, SeparatingLinesKernel, TasukiGapKernel, ThrustingKernel, two_bar_indicator,
};

two_bar_indicator! {
    /// `engulfing` — current body engulfs prior opposite-colour body.
    Engulfing, "engulfing", EngulfingKernel
}
two_bar_indicator! {
    /// `doji_star` — doji gapping away from a prior long body.
    DojiStar, "doji_star", DojiStarKernel
}
two_bar_indicator! {
    /// `harami` — current body inside prior opposite-colour body.
    Harami, "harami", HaramiKernel
}
two_bar_indicator! {
    /// `harami_cross` — harami where the current bar is a doji.
    HaramiCross, "harami_cross", HaramiCrossKernel
}
two_bar_indicator! {
    /// `piercing` — bullish penetration above prior midpoint.
    Piercing, "piercing", PiercingKernel
}
two_bar_indicator! {
    /// `dark_cloud_cover` — bearish penetration below prior midpoint.
    DarkCloudCover, "dark_cloud_cover", DarkCloudCoverKernel
}
two_bar_indicator! {
    /// `counterattack` — opposite-colour bodies closing at ≈same price.
    Counterattack, "counterattack", CounterattackKernel
}
two_bar_indicator! {
    /// `meeting_lines` — counterattack with a tighter close tolerance.
    MeetingLines, "meeting_lines", MeetingLinesKernel
}
two_bar_indicator! {
    /// `separating_lines` — same-colour continuation opening at prior open.
    SeparatingLines, "separating_lines", SeparatingLinesKernel
}
two_bar_indicator! {
    /// `matching_low` — two black candles with equal closes (bullish).
    MatchingLow, "matching_low", MatchingLowKernel
}
two_bar_indicator! {
    /// `on_neck` — bearish continuation closing at ≈prior low.
    OnNeck, "on_neck", OnNeckKernel
}
two_bar_indicator! {
    /// `in_neck` — bearish continuation closing slightly into prior body.
    InNeck, "in_neck", InNeckKernel
}
two_bar_indicator! {
    /// `thrusting` — bearish continuation closing below prior midpoint.
    Thrusting, "thrusting", ThrustingKernel
}
two_bar_indicator! {
    /// `kicking` — opposite-colour marubozu with a gap.
    Kicking, "kicking", KickingKernel
}
two_bar_indicator! {
    /// `kicking_by_length` — kicking, signal by the longer marubozu's colour.
    KickingByLength, "kicking_by_length", KickingByLengthKernel
}
two_bar_indicator! {
    /// `homing_pigeon` — two blacks, second harami-inside the first (bullish).
    HomingPigeon, "homing_pigeon", HomingPigeonKernel
}
two_bar_indicator! {
    /// `gap_side_side_white` — two similar-size white candles gapping up.
    GapSideSideWhite, "gap_side_side_white", GapSideSideWhiteKernel
}
two_bar_indicator! {
    /// `tasuki_gap` — gap then opposite-colour candle staying in the gap.
    TasukiGap, "tasuki_gap", TasukiGapKernel
}
