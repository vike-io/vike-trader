//! Steps on a number line: the decimals that print a step's multiples ([`step_decimals`]), the
//! decimals an axis prints its 1/2/5·10^k marks with ([`axis_mark_decimals`]), and the "nice"
//! 1/2/5·10^k step for a raw one ([`nice_step`], two roundings: [`NiceStepRounding`]).
//!
//! Both answer from the step alone. A caller with a domain limit (a ticket that prints at most twelve
//! decimals, a bucket width derived from a price) keeps that limit at its own site.

/// The decimals that print every multiple of `step` exactly: `0.001` → 3, `0.25` → 2, `1` and `20`
/// → 0, `1e-13` → 13. A negative step reads as its size. `None` for a step that is zero or not a
/// finite number: no decimals belong to it.
///
/// The test is RELATIVE: `step · 10^d` is within a billionth of ITSELF of a whole number. An absolute
/// tolerance answered 0 for every step under 1e-9, because the step was under it too. A step that is
/// not a decimal fraction (a third) stops once the billionth covers its remaining digits, and the
/// loop always ends: past 2^53 every `f64` is whole.
///
/// ⚠ The relative test reads float NOISE as digits: a step that is the difference of two large
/// prices (`65_432.001 - 65_432.0`) carries the prices' rounding, and answers more decimals than the
/// step it was meant to be. Pass the step itself, not a difference; an axis's marks, whose step is
/// one, take [`axis_mark_decimals`].
pub fn step_decimals(step: f64) -> Option<usize> {
    if !step.is_finite() || step == 0.0 {
        return None;
    }
    let mut decimals = 0;
    let mut scaled = step.abs();
    while (scaled - scaled.round()).abs() > 1e-9 * scaled {
        scaled *= 10.0;
        decimals += 1;
    }
    Some(decimals)
}

/// The decimals an axis prints its marks with when they sit `step` apart on a 1/2/5·10^k grid (a
/// chart's gridlines): `0` for a step of 1 or more, `1` for `0.1`/`0.2`/`0.5`, `3` for `0.001`.
/// `None` for a step that is not a positive finite number.
///
/// A DIFFERENT question from [`step_decimals`]'s exact decimals of a tick size: this is the decimals
/// of the step's DECADE, `ceil(-log10(step))`, so a step off the grid answers its decade's (`0.25` →
/// 1, where [`step_decimals`] says 2), and float noise does not count as digits. That matters where
/// the step is the difference of two mapped prices (an inverted axis): `65_432.002 - 65_432.001` is
/// `0.0010000000038417056`, 3 here and 12 by [`step_decimals`]. The `1e-9` keeps a step a hair under
/// its power of ten (`log10(0.001)` is not exactly -3) from costing a digit; a difference that falls
/// further under it (`65_432.001 - 65_432.0`, `0.000999999996565748`) takes one more (4).
/// `libm::log10`, so a label reads the same on every platform.
pub fn axis_mark_decimals(step: f64) -> Option<usize> {
    (step > 0.0 && step.is_finite()).then(|| (-libm::log10(step) - 1e-9).ceil().max(0.0) as usize)
}

/// How [`nice_step`] picks among 1, 2, 5 and 10 times the decade of a raw step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NiceStepRounding {
    /// The smallest nice step at or above the raw one (Heckbert's ceiling: a fraction of its decade
    /// `<= 1` → 1, `<= 2` → 2, `<= 5` → 5, else 10), so a tick count sized by it stays in its budget.
    Up,
    /// The nice step nearest the raw one, cut at the midpoints: under `1.5` → 1, under `3.5` → 2,
    /// under `7.5` → 5, else 10.
    Nearest,
}

/// The "nice" step for `raw`: 1, 2, 5 or 10 times the power of ten at or below it, picked by
/// `rounding`. `NaN` for a `raw` that is not a positive finite number.
///
/// ⚠ **This is a DECADE CLIFF, not a one-ULP wobble**, which is why both the logarithm and the power
/// are the `libm` crate's rather than the platform's
/// (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`). `.floor()`
/// applied to a logarithm is a quantizer, so a last-bit disagreement between glibc and the MSVC
/// runtime matters only near an exact power of ten, but THERE it flips the exponent by a whole one
/// and the answer by a factor of TEN: a `raw` of exactly 100.0 whose `log10` came back as
/// `1.9999999999999998` on one box and `2.0` on the other would be a step of 10 on one and 100 on the
/// other, from identical inputs, with nothing downstream reporting an error. The cliff itself is
/// inherent to "quantize a logarithm" and not a defect; the function must only never answer
/// differently on two machines. Its callers' probes pin it: `crates/vike-chart/tests/libm_platform_probe.rs`
/// (through the chart's tick generator) and `crates/vike-orderflow/tests/libm_platform_probe.rs`
/// (through the footprint bucket width).
pub fn nice_step(raw: f64, rounding: NiceStepRounding) -> f64 {
    if raw <= 0.0 || !raw.is_finite() {
        return f64::NAN;
    }
    let decade = libm::pow(10.0, libm::log10(raw).floor());
    let fraction = raw / decade;
    let nice = match rounding {
        NiceStepRounding::Up => {
            if fraction <= 1.0 {
                1.0
            } else if fraction <= 2.0 {
                2.0
            } else if fraction <= 5.0 {
                5.0
            } else {
                10.0
            }
        }
        NiceStepRounding::Nearest => {
            if fraction < 1.5 {
                1.0
            } else if fraction < 3.5 {
                2.0
            } else if fraction < 7.5 {
                5.0
            } else {
                10.0
            }
        }
    };
    nice * decade
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;
