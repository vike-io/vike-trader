//! Smoothing constants and decay factors: the exponential moving average's alpha (from a period or
//! a half-life), the weighted moving average's normaliser, and the weight a decayed estimate keeps
//! after a span (by half-life, or by time constant).
//!
//! Each is one expression: a streaming core computes the constants once at construction and a decay
//! factor once per update, and the core keeps its recursion. The spellings are exact: `period as f64`
//! is exact below 2^53, and the normaliser's `2 / (n (n + 1))` is the correctly rounded `1 / T` of
//! the weight total `T = n (n + 1) / 2`. The half-life and decay forms take their transcendental
//! from `libm`.

/// The exponential moving average's smoothing factor for a period: `2 / (period + 1)`.
#[inline]
pub fn exponential_moving_average_alpha(period: usize) -> f64 {
    2.0 / (period as f64 + 1.0)
}

/// The inverse of a linearly weighted average's weight total `1 + 2 + … + n`:
/// `2 / (n (n + 1))`, the same correctly rounded value as `1 / (n (n + 1) / 2)`. `+∞` for `n == 0`.
#[inline]
pub fn weighted_moving_average_normaliser(n: usize) -> f64 {
    let nf = n as f64;
    2.0 / (nf * (nf + 1.0))
}

/// The exponential moving average's smoothing factor for a half-life counted in updates:
/// `α = 1 − 0.5^(1 / half_life)`, so a weight decays to ½ after `half_life` updates, with `libm`'s
/// `pow`. A `half_life` that is not positive (`NaN` included) is `1.0`: no smoothing, the estimate
/// tracks the latest sample.
#[inline]
pub fn alpha_from_half_life(half_life: f64) -> f64 {
    if half_life > 0.0 { 1.0 - libm::pow(0.5, 1.0 / half_life) } else { 1.0 }
}

/// The weight a decayed estimate keeps after `elapsed`, for a `half_life` in the same unit:
/// `0.5^(elapsed / half_life)`, with `libm`'s `pow`. No guard: `half_life > 0` is the caller's.
#[inline]
pub fn half_life_decay(elapsed: f64, half_life: f64) -> f64 {
    libm::pow(0.5, elapsed / half_life)
}

/// The weight a decayed estimate keeps after `elapsed`, for a `time_constant` in the same unit:
/// `exp(−elapsed / time_constant)` (the negation first, then the division), with `libm`'s `exp`. No
/// guard: `time_constant > 0` is the caller's.
#[inline]
pub fn exponential_decay(elapsed: f64, time_constant: f64) -> f64 {
    libm::exp(-elapsed / time_constant)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings of the normaliser the indicator cores used agree to the bit, and the alpha is
    /// the textbook one.
    #[test]
    fn the_constants_are_the_spellings_they_replaced() {
        for n in 1..=1000_usize {
            let integer_form = 1.0 / (n * (n + 1) / 2) as f64;
            assert_eq!(
                weighted_moving_average_normaliser(n).to_bits(),
                integer_form.to_bits(),
                "{n}"
            );
            assert_eq!(
                exponential_moving_average_alpha(n).to_bits(),
                (2.0 / (n as f64 + 1.0)).to_bits(),
                "{n}"
            );
        }
        assert_eq!(exponential_moving_average_alpha(3), 0.5);
    }

    /// The half-life alpha keeps the market maker's golden bits (the expression of
    /// `vike-market-making`'s `ewma_alpha`, moved here verbatim) and its no-smoothing guard; the two
    /// decays hit their defining points exactly.
    #[test]
    fn the_half_life_alpha_and_the_decays() {
        assert_eq!(alpha_from_half_life(32.0).to_bits(), 0x3f95_f134_9237_5800, "half-life 32");
        for not_positive in [0.0, -0.0, -3.0, f64::NAN] {
            assert_eq!(alpha_from_half_life(not_positive), 1.0, "{not_positive}");
        }
        assert_eq!(alpha_from_half_life(1.0), 0.5);
        assert_eq!(half_life_decay(250.0, 250.0), 0.5);
        assert_eq!(half_life_decay(0.0, 250.0), 1.0);
        assert_eq!(half_life_decay(500.0, 250.0), 0.25);
        assert_eq!(exponential_decay(0.0, 60_000.0), 1.0);
        assert_eq!(exponential_decay(60_000.0, 60_000.0).to_bits(), libm::exp(-1.0).to_bits());
    }
}
