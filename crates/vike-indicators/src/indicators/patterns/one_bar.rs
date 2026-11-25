//! One-bar candlestick patterns: each reads only the current bar (and the rolling average body).
//!
//! Every type is a hand-written bounded-state stepper (`trims_history() == false`) over a core in
//! `crate::fast_m`; there is no batch kernel: a type's `vectorize` IS its core folded over the bars,
//! and `crate::series` runs the same core over plain slices, so a stream, a batch and a slice call
//! agree bit for bit. The output is `+100.0`, `-100.0` or `0.0`.

use crate::fast_m::{
    BeltHoldRule, ClosingMarubozuRule, Core1, DojiRule, DragonflyDojiRule, GravestoneDojiRule,
    HammerRule, HangingManRule, HighWaveRule, InvertedHammerRule, LongLineRule, LongleggedDojiRule,
    MarubozuRule, OpeningMarubozuRule, RickshawManRule, ShootingStarRule, ShortLineRule,
    SpinningTopRule, TakuriRule, pattern_stepper,
};

pattern_stepper! {
    /// `doji` — tiny body relative to average body (presence marker).
    Doji, "doji", Core1<DojiRule>
}
pattern_stepper! {
    /// `hammer` — small body, long lower shadow (bullish).
    Hammer, "hammer", Core1<HammerRule>
}
pattern_stepper! {
    /// `inverted_hammer` — small body, long upper shadow (bullish).
    InvertedHammer, "inverted_hammer", Core1<InvertedHammerRule>
}
pattern_stepper! {
    /// `hanging_man` — hammer geometry as a bearish signal.
    HangingMan, "hanging_man", Core1<HangingManRule>
}
pattern_stepper! {
    /// `shooting_star` — inverted-hammer geometry as a bearish signal.
    ShootingStar, "shooting_star", Core1<ShootingStarRule>
}
pattern_stepper! {
    /// `dragonfly_doji` — doji with long lower shadow (bullish).
    DragonflyDoji, "dragonfly_doji", Core1<DragonflyDojiRule>
}
pattern_stepper! {
    /// `gravestone_doji` — doji with long upper shadow (bearish).
    GravestoneDoji, "gravestone_doji", Core1<GravestoneDojiRule>
}
pattern_stepper! {
    /// `longlegged_doji` — doji with long upper AND lower shadows.
    LongleggedDoji, "longlegged_doji", Core1<LongleggedDojiRule>
}
pattern_stepper! {
    /// `rickshaw_man` — long-legged doji with body near the range middle.
    RickshawMan, "rickshaw_man", Core1<RickshawManRule>
}
pattern_stepper! {
    /// `takuri` — dragonfly doji with an exceptionally long lower shadow.
    Takuri, "takuri", Core1<TakuriRule>
}
pattern_stepper! {
    /// `marubozu` — body ≈ full range; white +100 / black -100.
    Marubozu, "marubozu", Core1<MarubozuRule>
}
pattern_stepper! {
    /// `closing_marubozu` — no shadow on the close side.
    ClosingMarubozu, "closing_marubozu", Core1<ClosingMarubozuRule>
}
pattern_stepper! {
    /// `spinning_top` — small body, both shadows larger than body.
    SpinningTop, "spinning_top", Core1<SpinningTopRule>
}
pattern_stepper! {
    /// `high_wave` — very small body, very long upper AND lower shadows.
    HighWave, "high_wave", Core1<HighWaveRule>
}
pattern_stepper! {
    /// `long_line` — long candle (body ≥ 1.3× avg); white +100 / black -100.
    LongLine, "long_line", Core1<LongLineRule>
}
pattern_stepper! {
    /// `short_line` — short, compact candle (presence).
    ShortLine, "short_line", Core1<ShortLineRule>
}
pattern_stepper! {
    /// `belt_hold` — long body opening at its extreme; white +100 / black -100.
    BeltHold, "belt_hold", Core1<BeltHoldRule>
}
pattern_stepper! {
    /// `opening_marubozu` — no shadow on the open side.
    OpeningMarubozu, "opening_marubozu", Core1<OpeningMarubozuRule>
}
