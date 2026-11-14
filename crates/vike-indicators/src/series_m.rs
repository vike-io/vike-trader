//! Whole-series calls on plain `f64` slices for group `m` of the fourth wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! `series::doji(&open, &high, &low, &close)` and the other 17 one-bar candlestick patterns: the same
//! arithmetic as [`crate::indicators::Doji`] and its siblings, over four parallel `f64` slices and
//! with no `Bar` in between. Each returns one value per bar (as many as the shortest slice): `100.0`
//! for a bullish (or presence) signal, `-100.0` for a bearish one, `0.0` for none. A pattern has no
//! parameter, so none of these can fail. A pattern that compares the body with the average body
//! (`SMA(10)` of `|close - open|`) reads `0.0` until ten bars are in.

use crate::fast_m::{
    BeltHoldRule, ClosingMarubozuRule, DojiRule, DragonflyDojiRule, GravestoneDojiRule, HammerRule,
    HangingManRule, HighWaveRule, InvertedHammerRule, LongLineRule, LongleggedDojiRule,
    MarubozuRule, OpeningMarubozuRule, RickshawManRule, ShootingStarRule, ShortLineRule,
    SpinningTopRule, TakuriRule, run1,
};

macro_rules! pattern_series {
    ($($(#[$m:meta])* $name:ident, $Rule:ty;)*) => {
        $(
            $(#[$m])*
            #[inline]
            pub fn $name(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
                run1::<$Rule>(open, high, low, close)
            }
        )*
    };
}

pattern_series! {
    /// `doji`: a tiny body against the average body, with a real range (`100.0` or `0.0`).
    doji, DojiRule;
    /// `hammer`: small body, long lower shadow, short upper one (`100.0` or `0.0`).
    hammer, HammerRule;
    /// `inverted_hammer`: small body, long upper shadow, short lower one (`100.0` or `0.0`).
    inverted_hammer, InvertedHammerRule;
    /// `hanging_man`: the hammer geometry as a bearish signal (`-100.0` or `0.0`).
    hanging_man, HangingManRule;
    /// `shooting_star`: the inverted-hammer geometry as a bearish signal (`-100.0` or `0.0`).
    shooting_star, ShootingStarRule;
    /// `dragonfly_doji`: a doji with a long lower shadow (`100.0` or `0.0`).
    dragonfly_doji, DragonflyDojiRule;
    /// `gravestone_doji`: a doji with a long upper shadow (`-100.0` or `0.0`).
    gravestone_doji, GravestoneDojiRule;
    /// `longlegged_doji`: a doji with both shadows long (`100.0` or `0.0`).
    longlegged_doji, LongleggedDojiRule;
    /// `rickshaw_man`: a long-legged doji with the body near the middle of the range (`100.0` or `0.0`).
    rickshaw_man, RickshawManRule;
    /// `takuri`: a dragonfly doji with an exceptionally long lower shadow (`100.0` or `0.0`).
    takuri, TakuriRule;
    /// `marubozu`: the body is nearly the full range; white `100.0`, black `-100.0`, else `0.0`.
    marubozu, MarubozuRule;
    /// `closing_marubozu`: no shadow on the close side; white `100.0`, black `-100.0`, else `0.0`.
    closing_marubozu, ClosingMarubozuRule;
    /// `spinning_top`: a small body with both shadows larger than the body (`100.0` or `0.0`).
    spinning_top, SpinningTopRule;
    /// `high_wave`: a very small body with very long shadows on both sides (`100.0` or `0.0`).
    high_wave, HighWaveRule;
    /// `long_line`: a body of at least 1.3 average bodies; white `100.0`, black `-100.0`, else `0.0`.
    long_line, LongLineRule;
    /// `short_line`: a short, compact candle (`100.0` or `0.0`).
    short_line, ShortLineRule;
    /// `belt_hold`: a long body opening at its extreme; white `100.0`, black `-100.0`, else `0.0`.
    belt_hold, BeltHoldRule;
    /// `opening_marubozu`: no shadow on the open side; white `100.0`, black `-100.0`, else `0.0`.
    opening_marubozu, OpeningMarubozuRule;
}
