//! `WindowReach::Finite` retention: its multi-shape backstop, its pinned roster, `SmoothedOver`.

use vike_indicators::{WindowReach, make, make_with, registry};
use vike_marketdata::Bar;

use super::bits_eq;

// =============================================================================================
// The shrunken retention (`WindowReach::Finite`) and its backstops.
// =============================================================================================

/// A tiny deterministic LCG. This crate has no `rand` dev-dep, and the gates below need SEVERAL
/// DATA SHAPES rather than one closed-form oscillation — see
/// [`finite_window_indicators_are_truncation_invariant_across_shapes_and_params`] for why one shape
/// is a weak verifier here.
struct Lcg(u64);
impl Lcg {
    fn unit(&mut self) -> f64 {
        // The Knuth/MMIX constants; the top 53 bits give a uniform in [0, 1).
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The data shapes the finite-retention gate runs. `synth_bars` is ONE smooth oscillation with no
/// zero prices and no long identical runs, and a kernel can be invariant on it for reasons that do
/// not generalise.
const SHAPES: &[&str] = &["random-walk", "trend", "mostly-flat", "high-vol"];

/// Deterministic OHLCV in a named [`SHAPES`] shape. Every shape keeps prices and volumes strictly
/// POSITIVE on purpose: a zero or negative close is what puts an INTERIOR NaN into a `roc`/`ln`
/// series, and the indicators that window over those are deliberately NOT declared
/// `WindowReach::Finite` (see `crates/vike-indicators/src/indicators/classify.rs`'s `window_reach`), so
/// generating one here would be testing a claim nobody made.
fn shaped_bars(shape: &str, n: usize) -> Vec<Bar> {
    let mut rng = Lcg(0x5eed_1234_9876_abcd);
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let u = rng.unit() - 0.5;
        let close = match shape {
            "random-walk" => (prev_close + u * 2.0).max(1.0),
            "trend" => (100.0 + i as f64 * 0.05 + u * 0.4).max(1.0),
            // Long runs of BIT-IDENTICAL closes — the shape that drives every zero-variance and
            // zero-range guard, and the one a smooth oscillation never produces.
            "mostly-flat" => {
                if i % 23 < 18 {
                    prev_close
                } else {
                    (prev_close + u * 3.0).max(1.0)
                }
            }
            _ => (prev_close * (1.0 + u * 0.08)).max(1.0),
        };
        let open = prev_close;
        let high = open.max(close) + rng.unit() * 0.6;
        let low = (open.min(close) - rng.unit() * 0.6).max(0.005);
        let volume = 500.0 + rng.unit() * 5_000.0;
        bars.push(Bar {
            ts: i as i64 * 3_600_000,
            open,
            high,
            low,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        prev_close = close;
    }
    bars
}

/// ⚠ **The backstop for the `WindowReach::Finite` declarations — and it is a BACKSTOP, not the
/// authority.**
///
/// `WindowReach::Finite` cuts an indicator's retained history from `64 * lookback_full` to
/// `lookback_full + 33`, which is what makes the de-accumulated kernels 0.07x-0.91x main's cost per
/// streamed bar instead of 1.1x-36x. It is also an UNVERIFIABLE claim in the general case:
/// `stochrsi` was MEASURED to look truncation-invariant from 30 retained bars on
/// `every_trimmed_indicator_is_truncation_invariant`'s own 4,000-bar `synth_bars`, while the Wilder
/// `rsi_vals` beneath it needs ~493 bars on every data shape tried. A wrong declaration for it would
/// have been greenlit. So the declaration is structural — see
/// `crates/vike-indicators/src/indicators/classify.rs`'s `window_reach`, which cites the kernel for each
/// row — and this test only widens the net.
///
/// What it adds over the registry-wide gate above: FOUR data shapes instead of one — including
/// `mostly-flat`, whose bit-identical runs drive the zero-variance guards a smooth oscillation never
/// reaches — and MIN/DEFAULT/MAX params instead of defaults only, because the retention scales with
/// `lookback_full` and the margin is thinnest where the period is smallest.
///
/// The series length is DERIVED as `2 * keep`, the exact count at which the drain first fires, so
/// this stays cheap as periods grow instead of needing a hard-coded length that would silently stop
/// reaching the deep-period cases.
///
/// NON-VACUOUS: the assertion compares bits including NaN-ness, and these kernels were measured to
/// FAIL it before the de-accumulation. Over the retained range this very gate walks, at
/// `period` 20 and 200 respectively: `zscore` and `cmf` mismatched at 64 of 64 and 232 of 232
/// retained lengths, `var` and `stddev` at 64 of 64 and 231 of 232, `vwma` at 52 of 64 and 230 of
/// 232. `names`/`checked` guard the other direction: if `window_reach` stopped returning `Finite`
/// the loop would iterate nothing and pass.
#[test]
fn finite_window_indicators_are_truncation_invariant_across_shapes_and_params() {
    let mut checked = 0usize;
    let mut names: Vec<&str> = Vec::new();

    for meta in registry() {
        if meta.batch_only {
            continue;
        }
        if make(meta.name).unwrap().window_reach() != WindowReach::Finite {
            continue;
        }
        names.push(meta.name);

        // All-min / defaults / all-max. The `ParamSpec` bounds are legal by construction, so no
        // `coerce` is needed to reach `make_with`.
        let settings: Vec<Vec<f64>> = if meta.params.is_empty() {
            vec![Vec::new()]
        } else {
            vec![
                meta.params.iter().map(|p| p.min).collect(),
                meta.params.iter().map(|p| p.default).collect(),
                meta.params.iter().map(|p| p.max).collect(),
            ]
        };

        for raw in &settings {
            let ind = make_with(meta.name, raw).unwrap();
            assert!(
                ind.trims_history() && !ind.warmup_path_dependent(),
                "{}: declared WindowReach::Finite but the drain never reaches it — a retention \
                 policy on an indicator that does not truncate is a claim nobody checks",
                meta.name
            );
            let keep = vike_indicators::keep_for(ind.lookback_full(), ind.window_reach());
            let n = 2 * keep; // the exact length at which `hist_indicator!` first compacts
            let retained = [keep, keep + keep / 2, (2 * keep).saturating_sub(1)];

            for shape in SHAPES {
                let bars = shaped_bars(shape, n);
                let full = ind.vectorize(&bars);
                for r in retained {
                    let cut = ind.vectorize(&bars[n - r..]);
                    for (line, col) in cut.iter().enumerate() {
                        let (a, b) = (col[col.len() - 1], full[line][n - 1]);
                        assert!(
                            bits_eq(a, b),
                            "{} line {line} params {raw:?} shape {shape}: retaining {r} of {n} \
                             bars gives {a:?} but the full run gives {b:?} — the \
                             `WindowReach::Finite` declaration claims this kernel reaches at most \
                             `lookback_full + 1` bars, and it does not",
                            meta.name
                        );
                    }
                }
                checked += 1;
            }
        }
    }

    assert!(
        names.len() >= 10 && checked >= 40,
        "only {} indicator(s) / {checked} (indicator, params, shape) combinations declare \
         WindowReach::Finite — if that collapsed, this gate is watching nothing and the retention \
         split silently reverted to the conservative multiple: {names:?}",
        names.len()
    );
}

/// The `WindowReach::Finite` roster, pinned — the reviewable record of which kernels were granted
/// the shrunken retention.
///
/// ⚠ **A pin records an answer; it cannot derive one.** Its job is to make ADDING a row a visible
/// event in a diff, because the gate above cannot tell a correct grant from an incorrect one (see
/// its doc). The argument for each row lives with the declaration, in
/// `crates/vike-indicators/src/indicators/classify.rs`'s `window_reach`, next to the kernel it cites.
///
/// The four deliberately WITHHELD rows are asserted too, and they matter more than the granted
/// ones: each is truncation-invariant at today's retention and would look fine in any short test,
/// so nothing but this assertion records that leaving them out was a decision rather than an
/// oversight.
#[test]
fn the_finite_window_roster_is_pinned() {
    let mut granted: Vec<&str> = registry()
        .iter()
        .filter(|m| make(m.name).unwrap().window_reach() == WindowReach::Finite)
        .map(|m| m.name)
        .collect();
    granted.sort_unstable();
    // ⚠ The 63 candlestick patterns are asserted as a FAMILY, derived, not spelled. They share one
    // kernel (`patterns.rs`'s `avg_body`), so they are one decision — and a 63-name wall here would
    // be a second roster to keep in step with the first, which is the rot this repo removes counts
    // for. What is pinned is the PROPERTY: every pattern is Finite, and the family is non-empty.
    let patterns: Vec<&str> = registry()
        .iter()
        .filter(|m| m.category == vike_indicators::Category::Pattern)
        .map(|m| m.name)
        .collect();
    assert!(
        patterns.len() > 50,
        "the pattern family collapsed to {} — the filter is wrong, and a          vacuous family would make the assertion below prove nothing",
        patterns.len()
    );
    for name in &patterns {
        assert_eq!(
            make(name).unwrap().window_reach(),
            WindowReach::Finite,
            "`{name}` reads through `avg_body` = `sma(|close - open|, CTX)`: no division, no `ln`,              no `smooth_defined`, no recursive term — a finite window, and `patterns.rs` contains              no counter-example to that"
        );
    }

    // ...and the individually-reasoned rows, each granted for its own kernel.
    let mut granted: Vec<&str> = registry()
        .iter()
        .filter(|m| {
            make(m.name).unwrap().window_reach() == WindowReach::Finite
                && m.category != vike_indicators::Category::Pattern
        })
        .map(|m| m.name)
        .collect();
    granted.sort_unstable();
    assert_eq!(
        granted,
        vec![
            "ac",
            "alma",
            "aroon",
            "aroonosc",
            "bbands_pctb",
            "bbands_width",
            "bop",
            "chop",
            "cmf",
            "cmo",
            "donchian_width",
            "dpo",
            "envelopes",
            "eom",
            "high_low_52w",
            "hma",
            "kurtosis",
            "linearreg",
            "linearreg_angle",
            "linearreg_intercept",
            "linearreg_slope",
            "mad",
            "mfi",
            "midpoint",
            "midprice",
            "mom",
            "pivot_points",
            "rank_correlation",
            "rocp",
            "rocr",
            "rocr100",
            "skew",
            "std_error",
            "std_error_bands",
            "stddev",
            "stochf",
            "trima",
            "true_range",
            "tsf",
            "ulcer",
            "ultosc",
            "var",
            "volume_profile_poc",
            "vortex",
            "vwma",
            "williams_fractal",
            "zscore",
        ]
    );

    // ⚠ The two IIR rows are DECAY-bounded now, not blanket-bounded. Their reach is set by the
    // SMOOTHING period, which `lookback_full` overstates — so retention fell from 1665/1857 to
    // `37 * 14 = 518` while `every_trimmed_indicator_is_truncation_invariant` stayed green. The
    // period asserted here is the one the RECURRENCE uses, not the indicator's warm-up.
    for (name, period, why) in [
        ("relative_volatility", 14usize, "smooth_defined(.., ema, period) over its FIRST param"),
        ("stochrsi", 14usize, "rsi_vals(.., rsi_p) is a Wilder recurrence over its FIRST param"),
    ] {
        assert_eq!(
            make(name).unwrap().window_reach(),
            WindowReach::SmoothedOver(period),
            "`{name}` is bounded by its smoothing period, not by `lookback_full`: {why}"
        );
    }

    // ...and the two whose reach is unbounded for a NON-decay reason keep the conservative rule.
    // No factor can help an interior NaN gap, so a `SmoothedOver` here would be silently wrong.
    for (name, why) in [
        ("hvol", "windows over DEFINED log returns; ln() is undefined at a non-positive close"),
        ("kst", "windows over DEFINED roc values; roc is undefined at a zero prior close"),
    ] {
        assert_eq!(
            make(name).unwrap().window_reach(),
            WindowReach::Smoothed,
            "`{name}` must keep the conservative retention: {why}. It left \
             NOT_TRUNCATION_INVARIANT on the kernel change alone and is invariant at the 64x \
             retention — which is exactly why granting it a smaller one would look correct."
        );
    }
}

/// ⚠ **A `SmoothedOver` row MUST name a real period, or it silently under-retains.**
///
/// `crate::indicators::window_reach` returns `SmoothedOver(0)` as a placeholder — it is keyed on a
/// `&str` and cannot see an indicator's runtime params — and `hist_indicator!` fills the real value
/// in from `smoothing_period`. A key marked `SmoothedOver` with no `smoothing_period` arm keeps the
/// `0`, and `keep_for` then floors it to `KEEP_FLOOR`: a 64-bar retention on an IIR kernel, which is
/// a WRONG NUMBER rather than a slow one.
///
/// NON-VACUOUS: it reads the period off the CONSTRUCTED indicator, i.e. after the macro's fill-in,
/// so a missing arm shows up as `0` and fails here. Reverting that fill-in makes every row report 0.
#[test]
fn every_smoothed_over_row_names_its_period() {
    let mut checked = 0usize;
    for meta in registry() {
        let ind = make(meta.name).unwrap();
        if let WindowReach::SmoothedOver(period) = ind.window_reach() {
            assert!(
                period > 0,
                "`{}` is marked SmoothedOver but reports period 0 — `smoothing_period` has no arm \
                 for it, so `keep_for` would floor its retention to KEEP_FLOOR on an IIR kernel",
                meta.name
            );
            // ...and the retention must actually BE the decay bound, not the floor it collapses to.
            assert!(
                vike_indicators::keep_for(ind.lookback_full(), ind.window_reach())
                    >= period * vike_indicators::SMOOTHED_FACTOR,
                "`{}`'s retention fell below its own decay bound",
                meta.name
            );
            checked += 1;
        }
    }
    assert!(checked >= 2, "expected the two decay-bounded rows, saw {checked}");
}
