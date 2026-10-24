//! The typed-constructor seam: what `Rsi::new(14.0)` checks before it builds.
//!
//! Every indicator type is built three ways. `Default` gives the registry defaults. `new(..)` takes
//! the indicator's own parameters in registry order and REFUSES one the registry would not accept,
//! with a [`ParamError`] naming the parameter. [`crate::registry::make_with`] takes a raw slice and
//! silently clamps it, because a chart dialog's slider needs an answer rather than an error. One
//! range per parameter, spelled once, in the registry row's [`crate::registry::ParamSpec`]; this
//! module only reads it, so `new` and `make_with` cannot disagree about what is in range.
//!
//! `new` builds through the same `with_params` the registry uses, so a value in range gives the same
//! bits through either door; `crates/vike-indicators/src/indicators/macros.rs`'s `typed_new!` writes
//! a test per generated type that proves it against the registry row.

use std::fmt;

/// A parameter outside the range its registry row declares. Carries the numbers, so the message
/// says what was given and what was allowed.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamError {
    /// The registry name of the indicator (`"rsi"`).
    pub indicator: &'static str,
    /// The parameter's display name (`"length"`).
    pub param: &'static str,
    /// The smallest value the registry accepts.
    pub min: f64,
    /// The largest value the registry accepts.
    pub max: f64,
    /// The value that was given. `NaN` is always refused.
    pub got: f64,
}

impl fmt::Display for ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: parameter `{}` = {} is outside {}..={}",
            self.indicator, self.param, self.got, self.min, self.max
        )
    }
}

impl std::error::Error for ParamError {}

/// Check `raw` against the registry row named `indicator`, in the row's parameter order. Used by the
/// generated and hand-written `new(..)`; an entry the row does not declare is ignored, and a name
/// with no row checks nothing (the per-type test in `typed_new!` is what catches that).
pub(crate) fn checked(indicator: &'static str, raw: &[f64]) -> Result<(), ParamError> {
    let specs = crate::registry::get(indicator).map_or(&[][..], |meta| meta.params);
    for (spec, &got) in specs.iter().zip(raw) {
        // `!(a && b)` rather than `got < min || got > max`: a NaN fails both comparisons.
        if !(got >= spec.min && got <= spec.max) {
            return Err(ParamError {
                indicator,
                param: spec.name,
                min: spec.min,
                max: spec.max,
                got,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Indicator;
    use vike_marketdata::Bar;

    /// A deterministic, non-flat OHLCV series long enough to leave every default warm-up behind.
    fn series() -> Vec<Bar> {
        (0..160)
            .map(|i| {
                let x = i as f64;
                let close = 100.0 + 10.0 * (x * 0.21).sin() + x * 0.05;
                let open = close - 0.4 * (x * 0.7).cos();
                let high = close.max(open) + 0.8 + 0.3 * (x * 1.3).sin().abs();
                let low = close.min(open) - 0.8 - 0.3 * (x * 0.9).cos().abs();
                Bar::ohlcv(i * 60_000, open, high, low, close, 1000.0 + (i % 7) as f64 * 25.0)
            })
            .collect()
    }

    /// `typed` (built through the type's own constructor at the registry defaults) answers every bar
    /// with the SAME BITS as the registry row `key` built through `make`.
    pub(crate) fn assert_matches_registry(key: &str, mut typed: Box<dyn Indicator>) {
        let mut registry = crate::registry::make(key).unwrap_or_else(|| {
            panic!("no registry row named {key:?} for a type that says it is it")
        });
        assert_eq!(typed.name(), key, "the type's name() is not its registry key");
        for (i, bar) in series().iter().enumerate() {
            let (a, b) = (typed.on_bar(bar), registry.on_bar(bar));
            assert_eq!(a.len(), b.len(), "{key}: output arity differs at bar {i}");
            for (line, (x, y)) in a.iter().zip(&b).enumerate() {
                assert!(
                    x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
                    "{key}: line {line} differs at bar {i}: typed {x} vs registry {y}"
                );
            }
        }
    }

    #[test]
    fn nan_and_out_of_range_are_refused_with_the_numbers() {
        assert!(checked("rsi", &[14.0]).is_ok());
        let err = checked("rsi", &[0.0]).unwrap_err();
        assert_eq!(
            (err.indicator, err.param, err.min, err.max, err.got),
            ("rsi", "length", 1.0, 1000.0, 0.0)
        );
        assert!(
            checked("rsi", &[f64::NAN]).is_err(),
            "NaN must fail both bounds, not slip past them"
        );
        assert!(checked("rsi", &[1001.0]).is_err());
        assert!(err.to_string().contains("`length`"), "the message names the parameter: {err}");
    }
}
