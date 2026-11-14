//! [`TickFraction`]: the ONE rule that reads a tick index back as a price, for [`super::L2Book`],
//! every other book keyed by tick index, and every snap of an order's price or size onto its grid
//! (`vike_model::round_to_step` and the side-directed snaps), so an order and the book name the
//! same number at every level.

/// A tick size held as the exact fraction it stands for, so tick index `n` reads back as the
/// decimal price `n` ticks name: `35` ticks of `0.01` read `0.35` (`35 ÷ 100`, the double nearest
/// 0.35), not `0.35000000000000003` (`35 × 0.01`, the tick's own rounding error multiplied in).
///
/// **The rule**, the first that applies; a price is then `n × numerator ÷ denominator`, one
/// correctly rounded division of two exact integers:
///
/// 1. **A tick that divides one unit** (`ticks_per_unit = round(1 / tick_size)` with
///    `ticks_per_unit × tick_size` within `1e-9` of `1`) is `1 / ticks_per_unit`, so its price is
///    `n ÷ ticks_per_unit`: 0.01, 0.05, 0.25, a binary 1/32, a 1/3, and a tick a few ulps off one
///    of those (an inferred gap such as `100.01 - 100.0`).
/// 2. **Any other decimal tick** keeps its digits: `denominator` is the smallest power of ten,
///    `10^0` to `10^15`, for which `numerator = round(tick_size × denominator)` gives `tick_size`
///    back EXACTLY as `numerator ÷ denominator` (`0.03` is `3/100`, `2.5` is `25/10`, an integer
///    tick is itself over `1`, the plain product, already exact).
/// 3. **Anything else** (`0.1 + 0.2`, a NaN, a non-positive tick) keeps the plain product
///    `n × tick_size`.
///
/// Exact while `|n × numerator| < 2^53`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickFraction {
    numerator: f64,
    denominator: f64,
}

impl TickFraction {
    /// The fraction `tick_size` stands for, by the rule in the type's doc.
    pub fn new(tick_size: f64) -> Self {
        let ticks_per_unit = (1.0 / tick_size).round();
        if ticks_per_unit >= 1.0 && (ticks_per_unit * tick_size - 1.0).abs() < 1e-9 {
            return TickFraction { numerator: 1.0, denominator: ticks_per_unit };
        }
        let mut denominator = 1.0;
        for _ in 0..16 {
            let numerator = (tick_size * denominator).round();
            if numerator >= 1.0 && numerator / denominator == tick_size {
                return TickFraction { numerator, denominator };
            }
            denominator *= 10.0;
        }
        TickFraction { numerator: tick_size, denominator: 1.0 }
    }

    /// The price of `index` ticks: `index × numerator ÷ denominator`. `index` is a whole number
    /// of ticks held as a float, so a NaN or an infinite count reads as one (a snap of a NaN price
    /// stays NaN).
    pub fn price_of(self, index: f64) -> f64 {
        index * self.numerator / self.denominator
    }
}

#[path = "tick_tests.rs"]
#[cfg(test)]
mod tick_tests;
