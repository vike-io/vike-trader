//! Probability → position, with separate entry and exit bands.
//!
//! This is a SHARED home rather than a study-local helper, and deliberately so: the offline
//! research converts a concatenated out-of-sample probability series, while a live strategy
//! converts one probability per bar. If those two ever disagree, a forward test silently stops
//! testing what the research measured. One `(state, p) -> state` function, two callers — the same
//! argument that puts [`crate::signal_backtest`] beside it.
//!
//! ⚠ **This is the LAST step of that handoff, and the first one has the same hazard.** A
//! probability only means what the research measured if the FEATURES behind it do, and a live
//! strategy computes those per bar while the study computed them as a matrix.
//! `crates/vike-indicators/src/feature.rs`'s `ColumnFeature` is the seam for that half — same
//! shape, same reason, one layer down — and `crates/vike-indicators/src/test_support.rs`'s
//! `assert_stream_matches_batch` is the gate. Nothing here can detect a feature that drifted:
//! a wrong input produces a perfectly well-formed probability.
//!
//! The bands create a deliberate dead zone: entering long needs `p >= 0.75` but staying long only
//! needs `p >= 0.55`, so a position does not churn on noise around a single threshold.

/// Entry and exit thresholds. Defaults are the locked research constants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HysteresisBands {
    pub entry_long: f64,
    pub entry_short: f64,
    pub exit_long: f64,
    pub exit_short: f64,
}

impl Default for HysteresisBands {
    fn default() -> Self {
        Self { entry_long: 0.75, entry_short: 0.25, exit_long: 0.55, exit_short: 0.34 }
    }
}

/// One transition. `state` and the result are `-1` (short), `0` (flat) or `+1` (long).
///
/// A `NaN` probability HOLDS the current state rather than flattening: NaN means "no estimate
/// this bar" (feature warm-up), and flattening would turn missing information into a trade.
///
/// `state` is a bare `i8`, not an enum, so a caller can pass a value outside `{-1, 0, 1}`. Any such
/// value falls into the SHORT arm below (mirroring the oracle's `else: # state == -1`) rather than
/// being treated as an error or as flat — callers are expected to only ever pass back a value this
/// function itself returned.
pub fn hysteresis_step(state: i8, p: f64, b: HysteresisBands) -> i8 {
    if p.is_nan() {
        return state;
    }
    match state {
        0 => {
            if p >= b.entry_long {
                1
            } else if p <= b.entry_short {
                -1
            } else {
                0
            }
        }
        1 => {
            if p <= b.entry_short {
                -1
            } else if p < b.exit_long {
                0
            } else {
                1
            }
        }
        _ => {
            if p >= b.entry_long {
                1
            } else if p > b.exit_short {
                0
            } else {
                -1
            }
        }
    }
}

/// Fold [`hysteresis_step`] over a probability series, starting flat.
///
/// State is carried across the WHOLE slice and never reset — a caller with several assets must
/// call this once per asset, or one asset's position leaks into the next.
pub fn hysteresis(probs: &[f64], b: HysteresisBands) -> Vec<i8> {
    let mut state = 0i8;
    let mut out = Vec::with_capacity(probs.len());
    for &p in probs {
        state = hysteresis_step(state, p, b);
        out.push(state);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_enters_long_at_the_entry_band_and_short_at_the_other() {
        let b = HysteresisBands::default();
        assert_eq!(hysteresis_step(0, 0.75, b), 1);
        assert_eq!(hysteresis_step(0, 0.25, b), -1);
        assert_eq!(hysteresis_step(0, 0.60, b), 0);
    }

    #[test]
    fn a_long_holds_through_the_gap_between_exit_and_entry() {
        let b = HysteresisBands::default();
        // 0.60 is below entry_long (0.75) but at or above exit_long (0.55): still long.
        assert_eq!(hysteresis_step(1, 0.60, b), 1);
        assert_eq!(hysteresis_step(1, 0.55, b), 1, "at the exit_long band itself: still long");
        assert_eq!(hysteresis_step(1, 0.54, b), 0);
    }

    #[test]
    fn a_long_flips_straight_to_short_when_the_opposite_entry_is_crossed() {
        let b = HysteresisBands::default();
        assert_eq!(hysteresis_step(1, 0.20, b), -1, "must not transit through flat");
    }

    #[test]
    fn a_short_exits_above_its_exit_band_and_flips_at_the_long_entry() {
        let b = HysteresisBands::default();
        assert_eq!(hysteresis_step(-1, 0.40, b), 0);
        assert_eq!(hysteresis_step(-1, 0.34, b), -1, "at the exit_short band itself: still short");
        assert_eq!(hysteresis_step(-1, 0.30, b), -1);
        assert_eq!(hysteresis_step(-1, 0.80, b), 1);
    }

    #[test]
    fn a_nan_probability_holds_the_current_state() {
        let b = HysteresisBands::default();
        assert_eq!(hysteresis_step(1, f64::NAN, b), 1);
        assert_eq!(hysteresis_step(-1, f64::NAN, b), -1);
        assert_eq!(hysteresis_step(0, f64::NAN, b), 0);
    }

    #[test]
    fn the_series_form_carries_state_across_the_whole_input() {
        let b = HysteresisBands::default();
        let out = hysteresis(&[0.80, 0.60, 0.60, 0.50, 0.20], b);
        assert_eq!(out, vec![1, 1, 1, 0, -1]);
    }
}
