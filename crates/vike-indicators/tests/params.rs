//! ParamSpec / make_with / param-grid parity gates (UX-bundle T7b). Complements
//! `tests/parity.rs`'s defaults-only gate by exercising every registered
//! indicator's parameter surface: `make_with` must reproduce `make()` at
//! defaults bit-for-bit, and the on_bar==vectorize invariant must keep holding
//! away from defaults, at every param's min/max/default+step — not just the
//! defaults `parity.rs` already covers. Bits are compared via `f64::to_bits`;
//! both-NaN counts as equal (warm-up NaNs), same convention as `parity.rs`.

use vike_indicators::{Indicator, coerce, make, make_with, registry};
use vike_model::Bar;

/// Bitwise float equality, with both-NaN treated as equal (mirrors tests/parity.rs).
fn bits_eq(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() { true } else { a.to_bits() == b.to_bits() }
}

/// Deterministic, varied OHLCV — same shape as `tests/parity.rs`'s `synth_bars`
/// (duplicated here: each test file is its own binary, nothing to import from).
fn synth_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let t = i as f64;
        let mut close =
            100.0 + (t * 0.07).sin() * 8.0 + (t * 0.017).cos() * 4.0 + (t * 0.31).sin() * 1.5;
        if i > 0 && i % 17 == 0 {
            close = prev_close; // flat bar
        }
        let open = prev_close;
        let high = open.max(close) + (i % 5) as f64 * 0.3 + 0.5;
        let low = open.min(close) - (i % 7) as f64 * 0.25 - 0.5;
        let volume = 1000.0 + (i % 13) as f64 * 50.0;
        bars.push(Bar {
            ts: i as i64 * 3_600_000, // hourly
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

/// Fold `on_bar` over the series into per-line columns (same shape as `vectorize`).
fn fold_stream(ind: &mut dyn Indicator, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut lines: Vec<Vec<f64>> = Vec::new();
    for bar in bars {
        let out = ind.on_bar(bar);
        if lines.is_empty() {
            lines = vec![Vec::with_capacity(bars.len()); out.len()];
        }
        assert_eq!(out.len(), lines.len(), "on_bar output arity must be stable");
        for (li, v) in out.iter().enumerate() {
            lines[li].push(*v);
        }
    }
    lines
}

fn assert_lines_bit_eq(ctx: &str, a: &[Vec<f64>], b: &[Vec<f64>]) {
    assert_eq!(a.len(), b.len(), "{ctx}: line count mismatch");
    for (li, (sa, sb)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(sa.len(), sb.len(), "{ctx} line {li}: length mismatch");
        for (i, (&va, &vb)) in sa.iter().zip(sb.iter()).enumerate() {
            assert!(
                bits_eq(va, vb),
                "{ctx} line {li} idx {i}: {va:?} ({:#018x}) != {vb:?} ({:#018x})",
                va.to_bits(),
                vb.to_bits(),
            );
        }
    }
}

/// At least 64 bars per the task brief; comfortably past every base-set
/// indicator's default warm-up (the longest default period is MACD's
/// slow=26 + signal=9).
const BARS_N: usize = 120;

/// (a) `make_with(defaults)` must reproduce `make()` bit-for-bit, for every
/// registry entry — both the streaming fold and `vectorize`.
#[test]
fn make_with_defaults_matches_make() {
    let bars = synth_bars(BARS_N);
    for meta in registry() {
        let defaults: Vec<f64> = meta.params.iter().map(|p| p.default).collect();

        let mut a = make(meta.name).unwrap();
        let mut b = make_with(meta.name, &defaults).unwrap();

        let sa = fold_stream(a.as_mut(), &bars);
        let sb = fold_stream(b.as_mut(), &bars);
        assert_lines_bit_eq(
            &format!("{}: make_with(defaults) on_bar vs make() on_bar", meta.name),
            &sa,
            &sb,
        );

        let va = a.vectorize(&bars);
        let vb = b.vectorize(&bars);
        assert_lines_bit_eq(
            &format!("{}: make_with(defaults) vectorize vs make() vectorize", meta.name),
            &va,
            &vb,
        );
    }
}

/// (b) param-grid: for each param, holding sibling params at their default,
/// sweep {min, default, max, default+step} and check on_bar-fold ==
/// vectorize — the internal second-oracle invariant, now exercised at
/// non-default params too (parity.rs only covers defaults).
#[test]
fn param_grid_on_bar_equals_vectorize() {
    let bars = synth_bars(BARS_N);
    for meta in registry() {
        if meta.params.is_empty() {
            continue; // paramless (vwap, obv) — parity.rs already covers these at defaults
        }
        if meta.batch_only {
            continue; // batch_only reads the future — on_bar can't bit-match vectorize
        }
        let defaults: Vec<f64> = meta.params.iter().map(|p| p.default).collect();
        for (pi, spec) in meta.params.iter().enumerate() {
            for &raw_v in &[spec.min, spec.default, spec.max, spec.default + spec.step] {
                let mut raw = defaults.clone();
                raw[pi] = raw_v;
                let coerced = coerce(meta.params, &raw);

                let mut ind = make_with(meta.name, &raw).unwrap();
                let stream = fold_stream(ind.as_mut(), &bars);
                let batch = ind.vectorize(&bars);
                assert_lines_bit_eq(
                    &format!(
                        "{}: param '{}' = {raw_v} (coerced {coerced:?})",
                        meta.name, spec.name
                    ),
                    &stream,
                    &batch,
                );
            }
        }
    }
}

/// (c) `make_with(non-default) -> fold -> reset() -> refold` must equal a
/// fresh `make_with(non-default)` fold — proves `reset()` preserves param
/// fields at non-default values, not just at the constructor's own defaults.
#[test]
fn reset_preserves_non_default_params() {
    let bars = synth_bars(BARS_N);
    for meta in registry() {
        if meta.params.is_empty() {
            continue;
        }
        // Push every param to its max — a point guaranteed != default for
        // every entry in this registry.
        let non_default: Vec<f64> = meta.params.iter().map(|p| p.max).collect();

        let mut ind = make_with(meta.name, &non_default).unwrap();
        let first = fold_stream(ind.as_mut(), &bars);
        ind.reset();
        let second = fold_stream(ind.as_mut(), &bars);
        assert_lines_bit_eq(
            &format!("{}: reset()-then-refold at non-default params", meta.name),
            &first,
            &second,
        );

        let mut fresh = make_with(meta.name, &non_default).unwrap();
        let fresh_fold = fold_stream(fresh.as_mut(), &bars);
        assert_lines_bit_eq(
            &format!("{}: refold-after-reset vs a fresh make_with(non-default)", meta.name),
            &second,
            &fresh_fold,
        );
    }
}

/// (d) epsilon-perturbed defaults on integer-period params must fold
/// identically to the exact defaults — proves the `.round()` coercion in
/// each indicator's `with_params`. Integer-period params are identified by
/// `step == 1.0` (every "length"-shaped param in this registry uses a whole-
/// bar step; the fractional-step params — Bollinger/Keltner `mult`, PSAR
/// `step`/`max_af` — are f64 multipliers read directly, no rounding).
#[test]
fn epsilon_perturbed_integer_params_match_defaults() {
    let bars = synth_bars(BARS_N);
    for meta in registry() {
        if meta.params.is_empty() {
            continue;
        }
        let defaults: Vec<f64> = meta.params.iter().map(|p| p.default).collect();
        for (pi, spec) in meta.params.iter().enumerate() {
            if spec.step != 1.0 {
                continue;
            }
            let mut perturbed = defaults.clone();
            perturbed[pi] = spec.default + 1e-9;

            let mut base = make_with(meta.name, &defaults).unwrap();
            let mut eps = make_with(meta.name, &perturbed).unwrap();
            let sb = fold_stream(base.as_mut(), &bars);
            let se = fold_stream(eps.as_mut(), &bars);
            assert_lines_bit_eq(
                &format!("{}: param '{}' default+1e-9 vs exact default", meta.name, spec.name),
                &sb,
                &se,
            );
        }
    }
}

/// Sanity: every ParamSpec is internally consistent (`min <= max`, `default`
/// inside `[min, max]`) — catches a typo'd range directly instead of it
/// surfacing as an opaque diff buried inside a 120-bar fold comparison.
#[test]
fn param_specs_are_internally_consistent() {
    for meta in registry() {
        for spec in meta.params {
            assert!(
                spec.min <= spec.max,
                "{}: param '{}' min > max ({} > {})",
                meta.name,
                spec.name,
                spec.min,
                spec.max
            );
            assert!(
                spec.default >= spec.min && spec.default <= spec.max,
                "{}: param '{}' default {} outside [{}, {}]",
                meta.name,
                spec.name,
                spec.default,
                spec.min,
                spec.max
            );
        }
    }
}

/// Vwap and Obv declare an empty param surface (as do the paramless price
/// transforms and other stateless indicators added later — the set is no longer
/// exactly these two, so we assert membership, not equality).
#[test]
fn vwap_and_obv_are_paramless() {
    let paramless: Vec<&str> =
        registry().iter().filter(|m| m.params.is_empty()).map(|m| m.name).collect();
    assert!(paramless.contains(&"vwap"), "vwap should be paramless: {paramless:?}");
    assert!(paramless.contains(&"obv"), "obv should be paramless: {paramless:?}");
}
