//! Four- and five-bar candlestick patterns: each reads three or four bars back.
//!
//! Bounded-state steppers (`trims_history() == false`): the kernels and the state are in
//! `crate::fast_n`, the last four or five bars and the ten-bar average body, and the answer is the
//! `+100.0` / `-100.0` / `0.0` the history-recompute kernels gave, bit for bit.

use crate::fast_n::{
    BreakawayK, ConcealingBabySwallowK, HikkakeK, HikkakeModK, LadderBottomK, MatHoldK,
    RiseFallThreeMethodsK, ThreeLineStrikeK, pattern_stepper,
};

pattern_stepper! {
    /// `three_line_strike` — three trend candles then an engulfing fourth.
    ThreeLineStrike, "three_line_strike", Core4, ThreeLineStrikeK
}
pattern_stepper! {
    /// `ladder_bottom` — four stepping blacks then a white reversal (bullish).
    LadderBottom, "ladder_bottom", Core5, LadderBottomK
}
pattern_stepper! {
    /// `concealing_baby_swallow` — four blacks, marubozu pair, engulfing fourth.
    ConcealingBabySwallow, "concealing_baby_swallow", Core4, ConcealingBabySwallowK
}
pattern_stepper! {
    /// `rise_fall_three_methods` — 5-bar continuation (rising/falling).
    RiseFallThreeMethods, "rise_fall_three_methods", Core5, RiseFallThreeMethodsK
}
pattern_stepper! {
    /// `mat_hold` — bullish gap variant of rising three methods.
    MatHold, "mat_hold", Core5, MatHoldK
}
pattern_stepper! {
    /// `hikkake` — inside bar then false breakout that reverses.
    Hikkake, "hikkake", Core4, HikkakeK
}
pattern_stepper! {
    /// `hikkake_mod` — modified 5-bar hikkake with a confirming bar.
    HikkakeMod, "hikkake_mod", Core5, HikkakeModK
}
pattern_stepper! {
    /// `breakaway` — 5-bar gap run then a reversal closing into the gap.
    Breakaway, "breakaway", Core5, BreakawayK
}
