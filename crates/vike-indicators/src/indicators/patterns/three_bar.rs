//! Three-bar candlestick patterns: each reads the current bar and the two before it.
//!
//! Bounded-state steppers (`trims_history() == false`): the kernels and the state are in
//! `crate::fast_n`, the last three bars and the ten-bar average body, and the answer is the
//! `+100.0` / `-100.0` / `0.0` the history-recompute kernels gave, bit for bit.

use crate::fast_n::{
    AbandonedBabyK, AdvanceBlockK, EveningDojiStarK, EveningStarK, IdenticalThreeCrowsK,
    MorningDojiStarK, MorningStarK, StalledPatternK, StickSandwichK, ThreeBlackCrowsK,
    ThreeInsideK, ThreeOutsideK, ThreeStarsInSouthK, ThreeWhiteSoldiersK, TristarK, TwoCrowsK,
    UniqueThreeRiverK, UpsideGapTwoCrowsK, XsideGapThreeMethodsK, pattern_stepper,
};

pattern_stepper! {
    /// `morning_star` — long black, star, long white (bullish).
    MorningStar, "morning_star", Core3, MorningStarK
}
pattern_stepper! {
    /// `evening_star` — long white, star, long black (bearish).
    EveningStar, "evening_star", Core3, EveningStarK
}
pattern_stepper! {
    /// `morning_doji_star` — morning star whose star is a doji.
    MorningDojiStar, "morning_doji_star", Core3, MorningDojiStarK
}
pattern_stepper! {
    /// `evening_doji_star` — evening star whose star is a doji.
    EveningDojiStar, "evening_doji_star", Core3, EveningDojiStarK
}
pattern_stepper! {
    /// `three_white_soldiers` — three progressing long white candles.
    ThreeWhiteSoldiers, "three_white_soldiers", Core3, ThreeWhiteSoldiersK
}
pattern_stepper! {
    /// `three_black_crows` — three progressing long black candles.
    ThreeBlackCrows, "three_black_crows", Core3, ThreeBlackCrowsK
}
pattern_stepper! {
    /// `identical_three_crows` — three crows each opening ≈ at prior close.
    IdenticalThreeCrows, "identical_three_crows", Core3, IdenticalThreeCrowsK
}
pattern_stepper! {
    /// `three_inside` — harami then confirming third bar.
    ThreeInside, "three_inside", Core3, ThreeInsideK
}
pattern_stepper! {
    /// `three_outside` — engulfing then confirming third bar.
    ThreeOutside, "three_outside", Core3, ThreeOutsideK
}
pattern_stepper! {
    /// `three_stars_in_south` — three black candles of diminishing range.
    ThreeStarsInSouth, "three_stars_in_south", Core3, ThreeStarsInSouthK
}
pattern_stepper! {
    /// `abandoned_baby` — doji island reversal with gaps on both sides.
    AbandonedBaby, "abandoned_baby", Core3, AbandonedBabyK
}
pattern_stepper! {
    /// `advance_block` — three whites weakening (bearish warning).
    AdvanceBlock, "advance_block", Core3, AdvanceBlockK
}
pattern_stepper! {
    /// `stalled_pattern` — two long whites then a small stalling white.
    StalledPattern, "stalled_pattern", Core3, StalledPatternK
}
pattern_stepper! {
    /// `two_crows` — long white, gap-up black, black closing into bar1 body.
    TwoCrows, "two_crows", Core3, TwoCrowsK
}
pattern_stepper! {
    /// `upside_gap_two_crows` — gap-up black then engulfing black in the gap.
    UpsideGapTwoCrows, "upside_gap_two_crows", Core3, UpsideGapTwoCrowsK
}
pattern_stepper! {
    /// `tristar` — three dojis with a gapping middle (reversal by direction).
    Tristar, "tristar", Core3, TristarK
}
pattern_stepper! {
    /// `unique_three_river` — long black, lower-low black, small white (bullish).
    UniqueThreeRiver, "unique_three_river", Core3, UniqueThreeRiverK
}
pattern_stepper! {
    /// `stick_sandwich` — black/white/black with equal outer closes (bullish).
    StickSandwich, "stick_sandwich", Core3, StickSandwichK
}
pattern_stepper! {
    /// `xside_gap_three_methods` — gap then a partial-fill continuation candle.
    XsideGapThreeMethods, "xside_gap_three_methods", Core3, XsideGapThreeMethodsK
}
