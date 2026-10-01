use super::*;

const GRID: &[f64] = &[0.01, 1.0, 63.5, 100.0, 1_234.5, 63_000.0, 1_000_000.0];

#[test]
fn roundtrip_linear() {
    for &y in GRID {
        let m = ScaleMode::Linear.map(y, 0.0);
        let back = ScaleMode::Linear.unmap(m, 0.0);
        let rel = (back - y).abs() / y.abs().max(1.0);
        assert!(rel < 1e-12, "linear roundtrip failed for {y}: back={back}");
    }
}

#[test]
fn roundtrip_log() {
    for &y in GRID {
        let m = ScaleMode::Log.map(y, 0.0);
        let back = ScaleMode::Log.unmap(m, 0.0);
        let rel = (back - y).abs() / y.abs().max(1.0);
        assert!(rel < 1e-12, "log roundtrip failed for {y}: back={back}");
    }
}

// Percent's own domain: y values within a chart-realistic multiple of the
// anchor (the anchor is the first VISIBLE bar's close, §1 of the design
// spec — everything else on screen is within a small multiple of it, never
// orders of magnitude away). `(y/anchor - 1)*100` catastrophically cancels
// when y << anchor (the `- 1` swamps a near-zero ratio in f64), so unlike
// Linear/Log (which tolerate the full GRID's dynamic range unconditionally)
// Percent's grid is anchor-relative.
const PERCENT_GRID_MULTIPLES: &[f64] = &[0.5, 0.9, 0.99, 1.0, 1.01, 1.05, 1.5, 2.0, 5.0];

#[test]
fn roundtrip_percent() {
    let anchor = 63_000.0;
    for &mult in PERCENT_GRID_MULTIPLES {
        let y = anchor * mult;
        let m = ScaleMode::Percent.map(y, anchor);
        let back = ScaleMode::Percent.unmap(m, anchor);
        let rel = (back - y).abs() / y.abs().max(1.0);
        assert!(rel < 1e-12, "percent roundtrip failed for {y}: back={back}");
    }
}

#[test]
fn log_map_of_100_is_exactly_2() {
    assert_eq!(ScaleMode::Log.map(100.0, 0.0), 2.0);
}

#[test]
fn percent_map_of_5_percent_above_anchor() {
    let anchor = 63_000.0;
    let v = ScaleMode::Percent.map(anchor * 1.05, anchor);
    assert!((v - 5.0).abs() < 1e-9, "expected ~5.0, got {v}");
}

// --- C2a Task 2: pct_change (standalone %-normalization helper for multi-symbol overlays) ---

#[test]
fn pct_change_basic_cases() {
    // 110.0/100.0 and 90.0/100.0 are not exactly representable in binary
    // f64 (1.1/0.9 aren't exact binary fractions), so `(value/anchor -
    // 1.0)*100.0` lands a few ULPs off 10.0/-10.0 — the SAME rounding
    // `ScaleMode::Percent::map` already has (see
    // `percent_map_of_5_percent_above_anchor` above, which uses this same
    // epsilon-tolerant style for the identical reason). Bit-identity with
    // the primary's existing formula outranks exact round-number equality
    // here (task brief's own DRY-preservation priority) — an alternate
    // formula ordering, e.g. `(value-anchor)/anchor*100.0`, DOES hit
    // exactly 10.0/-10.0 but is a different (non-bit-identical) rounding,
    // which is exactly what must NOT be introduced as a second, diverging
    // percent-change implementation.
    assert!((pct_change(110.0, 100.0) - 10.0).abs() < 1e-9);
    assert!((pct_change(90.0, 100.0) - (-10.0)).abs() < 1e-9);
    // Exact for both: value == anchor cancels to exactly 0.0 regardless
    // of formula order; the anchor == 0.0 guard returns the literal 0.0.
    assert_eq!(pct_change(100.0, 100.0), 0.0);
    assert_eq!(pct_change(5.0, 0.0), 0.0);
}

#[test]
fn pct_change_nonfinite_anchor_yields_zero() {
    assert_eq!(pct_change(5.0, f64::NAN), 0.0);
    assert_eq!(pct_change(5.0, f64::INFINITY), 0.0);
    assert_eq!(pct_change(5.0, f64::NEG_INFINITY), 0.0);
}

#[test]
fn pct_change_matches_scale_mode_percent_map_for_valid_anchor() {
    // Same rebasing as the primary's ScaleMode::Percent axis (scale.rs's
    // own percent-anchor role) — bit-identical for every valid anchor.
    let anchor = 63_000.0;
    for &mult in PERCENT_GRID_MULTIPLES {
        let y = anchor * mult;
        assert_eq!(pct_change(y, anchor), ScaleMode::Percent.map(y, anchor));
    }
}

#[test]
fn supports_log_requires_strictly_positive_lo() {
    assert!(ScaleMode::Log.supports(1.0, 0.0));
    assert!(!ScaleMode::Log.supports(0.0, 0.0));
    assert!(!ScaleMode::Log.supports(-5.0, 0.0));
}

#[test]
fn supports_percent_requires_nonzero_finite_anchor() {
    assert!(ScaleMode::Percent.supports(0.0, 63_000.0));
    assert!(!ScaleMode::Percent.supports(0.0, 0.0));
    assert!(!ScaleMode::Percent.supports(0.0, f64::NAN));
    assert!(!ScaleMode::Percent.supports(0.0, f64::INFINITY));
}

#[test]
fn supports_linear_always_true() {
    assert!(ScaleMode::Linear.supports(-1.0, 0.0));
    assert!(ScaleMode::Linear.supports(0.0, 0.0));
}

fn assert_strictly_increasing_and_bounded(ticks: &[GridTick], raw_lo: f64, raw_hi: f64) {
    assert!(!ticks.is_empty(), "expected at least one tick");
    for t in ticks {
        assert!(
            t.raw >= raw_lo && t.raw <= raw_hi,
            "tick raw {} out of [{raw_lo}, {raw_hi}]",
            t.raw
        );
        assert!(t.step_mapped > 0.0, "step_mapped must be > 0, got {}", t.step_mapped);
    }
    for w in ticks.windows(2) {
        assert!(
            w[1].raw > w[0].raw,
            "ticks must be strictly increasing: {} then {}",
            w[0].raw,
            w[1].raw
        );
        assert!(
            w[1].mapped > w[0].mapped,
            "mapped ticks must be strictly increasing: {} then {}",
            w[0].mapped,
            w[1].mapped
        );
    }
}

#[test]
fn nice_ticks_log_only_uses_1_2_5_decades() {
    let ticks = nice_ticks(ScaleMode::Log, 100.0, 10_000.0, 0.0, 10);
    assert_strictly_increasing_and_bounded(&ticks, 100.0, 10_000.0);
    assert!(ticks.len() <= 10);
    for t in &ticks {
        let k = libm::log10(t.raw).floor();
        let base = libm::pow(10.0, k);
        let mantissa = t.raw / base;
        let is_125 = [1.0, 2.0, 5.0]
            .iter()
            .any(|&m| (mantissa - m).abs() < 1e-6 || (mantissa / 10.0 - m).abs() < 1e-6);
        assert!(is_125, "raw {} is not a {{1,2,5}}·10^k value (mantissa {})", t.raw, mantissa);
    }
    // sanity: full {1,2,5} set across two decades should be exactly these 7 values
    let expected = [100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0];
    let raws: Vec<f64> = ticks.iter().map(|t| t.raw).collect();
    assert_eq!(raws, expected, "expected the full 1/2/5 decade set, got {raws:?}");
}

#[test]
fn nice_ticks_log_thins_when_over_budget() {
    // 6 decades * 3 candidates = 18 raw candidates; max_ticks=5 forces thinning.
    let ticks = nice_ticks(ScaleMode::Log, 1.0, 1_000_000.0, 0.0, 5);
    assert_strictly_increasing_and_bounded(&ticks, 1.0, 1_000_000.0);
    assert!(ticks.len() <= 5, "expected <= 5 ticks after thinning, got {}", ticks.len());
    for t in &ticks {
        let k = libm::log10(t.raw).round();
        assert!(
            (t.raw - libm::pow(10.0, k)).abs() < t.raw * 1e-9,
            "expected decade-only tick after thinning, got {}",
            t.raw
        );
    }
}

#[test]
fn nice_ticks_linear_uses_round_1_2_5_steps() {
    // Matches egui_plot's own "nice numbers" granularity class: round
    // 1/2/5·10^k steps, sized so the tick count stays within max_ticks.
    let ticks = nice_ticks(ScaleMode::Linear, 0.0, 100.0, 0.0, 10);
    assert_strictly_increasing_and_bounded(&ticks, 0.0, 100.0);
    assert!(ticks.len() <= 10);
    let step = ticks[1].raw - ticks[0].raw;
    let k = libm::log10(step).floor();
    let mantissa = step / libm::pow(10.0, k);
    assert!(
        [1.0, 2.0, 5.0].iter().any(|&m| (mantissa - m).abs() < 1e-6),
        "linear step {step} is not a round 1/2/5·10^k value (mantissa {mantissa})"
    );
    for w in ticks.windows(2) {
        let s = w[1].raw - w[0].raw;
        assert!(
            (s - step).abs() < step * 1e-9,
            "linear ticks must be evenly spaced: {s} vs {step}"
        );
    }
}

#[test]
fn nice_ticks_percent_emits_nice_percent_steps() {
    let anchor = 63_000.0;
    let raw_lo = anchor * 0.9;
    let raw_hi = anchor * 1.1;
    let ticks = nice_ticks(ScaleMode::Percent, raw_lo, raw_hi, anchor, 10);
    assert_strictly_increasing_and_bounded(&ticks, raw_lo, raw_hi);
    assert!(ticks.len() <= 10);
    // mapped values (percent) should themselves be nice 1/2/5·10^k steps
    let step = ticks[1].mapped - ticks[0].mapped;
    let k = libm::log10(step).floor();
    let mantissa = step / libm::pow(10.0, k);
    assert!(
        [1.0, 2.0, 5.0].iter().any(|&m| (mantissa - m).abs() < 1e-6),
        "percent step {step} is not round (mantissa {mantissa})"
    );
    // and each tick's mapped value must equal map(raw, anchor)
    for t in &ticks {
        let recomputed = ScaleMode::Percent.map(t.raw, anchor);
        assert!(
            (recomputed - t.mapped).abs() < 1e-9,
            "GridTick.mapped mismatch: {} vs recomputed {}",
            t.mapped,
            recomputed
        );
    }
}

#[test]
fn nice_ticks_last_tick_reuses_previous_step() {
    let ticks = nice_ticks(ScaleMode::Linear, 0.0, 100.0, 0.0, 10);
    assert!(ticks.len() >= 2);
    let n = ticks.len();
    let prev_step = ticks[n - 2].step_mapped;
    assert!(
        (ticks[n - 1].step_mapped - prev_step).abs() < 1e-9,
        "last tick should reuse the previous step_mapped"
    );
}

#[test]
fn nice_ticks_empty_range_yields_no_ticks() {
    assert!(nice_ticks(ScaleMode::Linear, 5.0, 5.0, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Linear, 5.0, 5.0, 0.0, 0).is_empty());
}

// --- Review-fix wave (T1 review findings 1-4) ---

#[test]
fn nice_ticks_nonfinite_linear_yields_no_ticks() {
    assert!(nice_ticks(ScaleMode::Linear, 0.0, f64::INFINITY, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Linear, f64::NEG_INFINITY, 100.0, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Linear, f64::NAN, 100.0, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Linear, 0.0, f64::NAN, 0.0, 10).is_empty());
}

#[test]
fn nice_ticks_nonfinite_log_yields_no_ticks() {
    // Pre-guard this HANGS (`libm::log10(hi).ceil() as i32` saturates to
    // i32::MAX -> ~2.1e9 candidate iterations pushing into a Vec): written
    // against the FIXED guard expectation and deliberately NOT run in the
    // RED capture (review-fix wave, see task-1-report.md).
    assert!(nice_ticks(ScaleMode::Log, 100.0, f64::INFINITY, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Log, f64::NEG_INFINITY, 10_000.0, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Log, f64::NAN, 10_000.0, 0.0, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Log, 100.0, f64::NAN, 0.0, 10).is_empty());
}

#[test]
fn nice_ticks_nonfinite_percent_yields_no_ticks() {
    let anchor = 63_000.0;
    assert!(nice_ticks(ScaleMode::Percent, 100.0, f64::INFINITY, anchor, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Percent, f64::NEG_INFINITY, 200.0, anchor, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Percent, f64::NAN, 200.0, anchor, 10).is_empty());
    // Percent additionally requires a finite anchor.
    assert!(nice_ticks(ScaleMode::Percent, 100.0, 200.0, f64::NAN, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Percent, 100.0, 200.0, f64::INFINITY, 10).is_empty());
    assert!(nice_ticks(ScaleMode::Percent, 100.0, 200.0, f64::NEG_INFINITY, 10).is_empty());
}

#[test]
fn nice_ticks_respects_small_max_ticks_budget() {
    for mode in [ScaleMode::Linear, ScaleMode::Log, ScaleMode::Percent, ScaleMode::Indexed] {
        let (lo, hi, anchor) = match mode {
            ScaleMode::Linear => (0.0, 100.0, 0.0),
            ScaleMode::Log => (100.0, 10_000.0, 0.0),
            ScaleMode::Percent | ScaleMode::Indexed => (56_700.0, 69_300.0, 63_000.0),
        };
        for max_ticks in 1..=4 {
            let ticks = nice_ticks(mode, lo, hi, anchor, max_ticks);
            assert!(
                ticks.len() <= max_ticks,
                "{mode:?} max_ticks={max_ticks}: got {} ticks",
                ticks.len()
            );
            assert_strictly_increasing_and_bounded(&ticks, lo, hi);
        }
    }
}

// --- T2 wave: effective_mode + convert_bounds (bounds-space migration) ---

#[test]
fn effective_mode_log_falls_back_to_linear_for_nonpositive_raw_lo() {
    assert_eq!(effective_mode(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Log, 0.0, 0.0), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
}

#[test]
fn effective_mode_percent_falls_back_to_linear_for_zero_anchor() {
    assert_eq!(effective_mode(ScaleMode::Percent, 100.0, 0.0), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Percent, 100.0, f64::NAN), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Percent, 100.0, 63_000.0), ScaleMode::Percent);
}

#[test]
fn effective_mode_linear_never_falls_back() {
    assert_eq!(effective_mode(ScaleMode::Linear, -1.0, 0.0), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Linear, f64::NAN, f64::NAN), ScaleMode::Linear);
}

// --- T3 carry-over: has_data (data-absent NaN sentinel vs genuine data) ---

#[test]
fn has_data_log_false_only_for_nan_raw_lo() {
    assert!(has_data(ScaleMode::Log, 100.0, 0.0)); // supported, real data
    assert!(has_data(ScaleMode::Log, -5.0, 0.0)); // genuine non-positive, still real data
    assert!(has_data(ScaleMode::Log, 0.0, 0.0)); // genuine zero, still real data
    assert!(!has_data(ScaleMode::Log, f64::NAN, 0.0)); // empty visible slice sentinel
}

#[test]
fn has_data_percent_false_only_for_nan_anchor() {
    assert!(has_data(ScaleMode::Percent, 100.0, 63_000.0)); // supported, real data
    assert!(has_data(ScaleMode::Percent, 100.0, 0.0)); // genuine zero anchor, still real data
    assert!(has_data(ScaleMode::Percent, 100.0, 1e-310)); // genuine subnormal anchor, still real data
    assert!(!has_data(ScaleMode::Percent, 100.0, f64::NAN)); // no closed bar yet sentinel
}

#[test]
fn has_data_linear_always_true() {
    assert!(has_data(ScaleMode::Linear, f64::NAN, f64::NAN));
}

#[test]
fn convert_bounds_linear_log_linear_round_trip_preserves_raw_range() {
    let (ty0, ty1) = (100.0, 63_000.0); // raw == mapped in Linear (anchor unused)
    let (lo_log, hi_log) = convert_bounds(ScaleMode::Linear, ScaleMode::Log, 0.0, 0.0, ty0, ty1);
    // sanity: now in log10 space. `libm::log10` rather than the method for the reason on the
    // module: the expected value must come from the SAME implementation the code under test
    // uses, or this assertion is comparing two libms rather than checking a conversion.
    assert!((lo_log - libm::log10(100.0)).abs() < 1e-9);
    assert!((hi_log - libm::log10(63_000.0)).abs() < 1e-9);
    let (lo_lin, hi_lin) =
        convert_bounds(ScaleMode::Log, ScaleMode::Linear, 0.0, 0.0, lo_log, hi_log);
    assert!((lo_lin - ty0).abs() < 1e-9, "lo drift: {lo_lin} vs {ty0}");
    assert!((hi_lin - ty1).abs() < 1e-9, "hi drift: {hi_lin} vs {ty1}");
}

#[test]
fn convert_bounds_linear_percent_linear_round_trip_preserves_raw_range() {
    let anchor = 63_000.0;
    let (ty0, ty1) = (56_700.0, 69_300.0); // raw linear bounds, ±10% of anchor
    let (lo_p, hi_p) = convert_bounds(ScaleMode::Linear, ScaleMode::Percent, 0.0, anchor, ty0, ty1);
    assert!((lo_p - (-10.0)).abs() < 1e-9, "expected -10%, got {lo_p}");
    assert!((hi_p - 10.0).abs() < 1e-9, "expected +10%, got {hi_p}");
    let (lo_lin, hi_lin) =
        convert_bounds(ScaleMode::Percent, ScaleMode::Linear, anchor, 0.0, lo_p, hi_p);
    assert!((lo_lin - ty0).abs() < 1e-9, "lo drift: {lo_lin} vs {ty0}");
    assert!((hi_lin - ty1).abs() < 1e-9, "hi drift: {hi_lin} vs {ty1}");
}

#[test]
fn nice_ticks_log_falls_back_to_round_numbers_for_realistic_zoom() {
    // A realistic zoomed-in view (BTC ~$64k, a few hundred dollars wide):
    // no {1,2,5}·10^k value falls in [63_700, 64_200] at all (the nearest
    // are 50_000 and 100_000) — the decade-only candidate set goes
    // EMPTY, which pre-fix meant a BLANK y-axis (found via T2's VIKE_SHOT
    // smoke, the most common real-world Log zoom level). Must fall back
    // to "nice" round numbers sized to the window, still positioned via
    // log10 mapping by the caller.
    let ticks = nice_ticks(ScaleMode::Log, 63_700.0, 64_200.0, 0.0, 10);
    assert!(!ticks.is_empty(), "expected a fallback tick set, got none (blank axis)");
    assert_strictly_increasing_and_bounded(&ticks, 63_700.0, 64_200.0);
}

#[test]
fn nice_ticks_single_tick_step_mapped_fallback_positive() {
    // [150, 250] contains exactly one {1,2,5}*10^k value (200): drives the
    // single-tick step_mapped fallback (no neighbor to derive a step from).
    let ticks = nice_ticks(ScaleMode::Log, 150.0, 250.0, 0.0, 10);
    assert_eq!(ticks.len(), 1, "expected exactly one tick, got {ticks:?}");
    assert_eq!(ticks[0].raw, 200.0);
    assert!(
        ticks[0].step_mapped > 0.0,
        "single-tick fallback step_mapped must be > 0, got {}",
        ticks[0].step_mapped
    );
}

// --- C2b Task 7: ScaleAssign + remap_to_primary (secondary absolute price axis) ---

#[test]
fn scale_assign_default_is_percent() {
    assert_eq!(ScaleAssign::default(), ScaleAssign::Percent);
}

// --- absolute-shared-axis: the new SharedLinear variant ---

#[test]
fn scale_assign_shared_linear_is_distinct_and_serde_round_trips() {
    // The new variant is distinct from every existing one (so the render skips
    // route it correctly).
    for other in [ScaleAssign::Percent, ScaleAssign::Right, ScaleAssign::Left] {
        assert_ne!(ScaleAssign::SharedLinear, other);
    }
    // Persistence: every variant round-trips through JSON (the vike-app-core
    // `series_scale: IndexMap<String, ScaleAssign>` persist path). A workspace saved
    // with a SharedLinear pin reloads to the same variant; an OLD workspace with no
    // entry for a symbol defaults to Percent (absent key), unchanged.
    for v in
        [ScaleAssign::Percent, ScaleAssign::Right, ScaleAssign::Left, ScaleAssign::SharedLinear]
    {
        let js = serde_json::to_string(&v).unwrap();
        let back: ScaleAssign = serde_json::from_str(&js).unwrap();
        assert_eq!(back, v, "round-trip failed for {v:?} (json {js})");
    }
    // The exact wire token, so a rename can't silently break old workspaces.
    assert_eq!(serde_json::to_string(&ScaleAssign::SharedLinear).unwrap(), "\"SharedLinear\"");
}

#[test]
fn remap_to_primary_midpoint_maps_to_primary_midpoint() {
    // 150 is the midpoint of the secondary range 100..200 -> maps to the
    // midpoint of the primary range 0..10.
    let v = remap_to_primary(150.0, 100.0, 200.0, 0.0, 10.0);
    assert!((v - 5.0).abs() < 1e-9, "expected 5.0, got {v}");
}

#[test]
fn remap_to_primary_low_edge_maps_to_primary_lo() {
    let v = remap_to_primary(100.0, 100.0, 200.0, 0.0, 10.0);
    assert!((v - 0.0).abs() < 1e-9, "expected 0.0, got {v}");
}

#[test]
fn remap_to_primary_high_edge_maps_to_primary_hi() {
    let v = remap_to_primary(200.0, 100.0, 200.0, 0.0, 10.0);
    assert!((v - 10.0).abs() < 1e-9, "expected 10.0, got {v}");
}

#[test]
fn remap_to_primary_degenerate_range_yields_primary_midpoint() {
    // sec_lo == sec_hi (flat/one-point secondary range): no divide-by-zero
    // / NaN leak -- falls back to the primary's own midpoint.
    let v = remap_to_primary(5.0, 100.0, 100.0, 0.0, 10.0);
    assert!((v - 5.0).abs() < 1e-9, "expected primary midpoint 5.0, got {v}");
}

// --- C2b Task 7b: inverse_remap (secondary right-axis label formatter) ---

#[test]
fn inverse_remap_is_exact_inverse_of_remap_to_primary() {
    // Round-trip a grid of secondary prices through remap_to_primary and back:
    // the composition must return the original (to f64 rounding) for every
    // non-degenerate range pair — this is the property the labeled gutter
    // relies on (shared plot-space grid mark -> the compare series' real price).
    let (sec_lo, sec_hi) = (3_400.0, 3_450.0);
    let (prim_lo, prim_hi) = (63_000.0, 64_000.0);
    for &v in &[3_400.0, 3_410.0, 3_425.0, 3_448.5, 3_450.0] {
        let pv = remap_to_primary(v, sec_lo, sec_hi, prim_lo, prim_hi);
        let back = inverse_remap(pv, sec_lo, sec_hi, prim_lo, prim_hi);
        assert!((back - v).abs() < 1e-9, "round-trip failed for {v}: back={back}");
    }
}

#[test]
fn inverse_remap_endpoints_and_midpoint() {
    // prim_lo -> sec_lo, prim_hi -> sec_hi, midpoint -> midpoint.
    assert!((inverse_remap(0.0, 100.0, 200.0, 0.0, 10.0) - 100.0).abs() < 1e-9);
    assert!((inverse_remap(10.0, 100.0, 200.0, 0.0, 10.0) - 200.0).abs() < 1e-9);
    assert!((inverse_remap(5.0, 100.0, 200.0, 0.0, 10.0) - 150.0).abs() < 1e-9);
}

#[test]
fn inverse_remap_degenerate_primary_range_is_finite() {
    // prim_lo == prim_hi (flat primary range): no divide-by-zero / NaN leak.
    let v = inverse_remap(5.0, 100.0, 200.0, 7.0, 7.0);
    assert_eq!(v, 100.0, "degenerate primary range must fall back to sec_lo, got {v}");
}

// --- Indexed-to-100 mode (the Percent twin) ---

#[test]
fn indexed_rebases_first_visible_to_100() {
    let anchor = 63_000.0;
    // the anchor itself reads exactly 100
    assert_eq!(indexed(anchor, anchor), 100.0);
    assert_eq!(ScaleMode::Indexed.map(anchor, anchor), 100.0);
    // +5% above anchor reads ~105
    assert!((indexed(anchor * 1.05, anchor) - 105.0).abs() < 1e-9);
    // -10% below anchor reads ~90
    assert!((indexed(anchor * 0.9, anchor) - 90.0).abs() < 1e-9);
}

#[test]
fn indexed_degenerate_anchor_yields_base_100() {
    // zero / non-finite anchor -> the index base 100.0 (no divide-by-zero / NaN leak).
    assert_eq!(indexed(5.0, 0.0), 100.0);
    assert_eq!(indexed(5.0, f64::NAN), 100.0);
    assert_eq!(indexed(5.0, f64::INFINITY), 100.0);
}

#[test]
fn roundtrip_indexed() {
    let anchor = 63_000.0;
    for &mult in PERCENT_GRID_MULTIPLES {
        let y = anchor * mult;
        let m = ScaleMode::Indexed.map(y, anchor);
        let back = ScaleMode::Indexed.unmap(m, anchor);
        let rel = (back - y).abs() / y.abs().max(1.0);
        assert!(rel < 1e-12, "indexed roundtrip failed for {y}: back={back}");
    }
}

#[test]
fn supports_indexed_requires_nonzero_finite_anchor() {
    assert!(ScaleMode::Indexed.supports(0.0, 63_000.0));
    assert!(!ScaleMode::Indexed.supports(0.0, 0.0));
    assert!(!ScaleMode::Indexed.supports(0.0, f64::NAN));
}

#[test]
fn effective_mode_indexed_falls_back_to_linear_for_zero_anchor() {
    assert_eq!(effective_mode(ScaleMode::Indexed, 100.0, 0.0), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Indexed, 100.0, f64::NAN), ScaleMode::Linear);
    assert_eq!(effective_mode(ScaleMode::Indexed, 100.0, 63_000.0), ScaleMode::Indexed);
}

#[test]
fn has_data_indexed_false_only_for_nan_anchor() {
    assert!(has_data(ScaleMode::Indexed, 100.0, 63_000.0)); // supported, real data
    assert!(has_data(ScaleMode::Indexed, 100.0, 0.0)); // genuine zero anchor, still real data
    assert!(!has_data(ScaleMode::Indexed, 100.0, f64::NAN)); // no closed bar yet sentinel
}

#[test]
fn nice_ticks_indexed_emits_plain_index_steps_around_100() {
    let anchor = 63_000.0;
    let raw_lo = anchor * 0.9;
    let raw_hi = anchor * 1.1;
    let ticks = nice_ticks(ScaleMode::Indexed, raw_lo, raw_hi, anchor, 10);
    assert_strictly_increasing_and_bounded(&ticks, raw_lo, raw_hi);
    // mapped values live around 100 (90..110); each equals map(raw, anchor).
    for t in &ticks {
        assert!(t.mapped >= 89.0 && t.mapped <= 111.0, "index tick out of ~[90,110]: {}", t.mapped);
        let recomputed = ScaleMode::Indexed.map(t.raw, anchor);
        assert!((recomputed - t.mapped).abs() < 1e-9);
    }
}

// --- ScaleView: the invert modifier (orthogonal to mode) ---

#[test]
fn scale_view_no_invert_is_byte_identical_to_bare_mode() {
    let anchor = 63_000.0;
    for mode in [ScaleMode::Linear, ScaleMode::Log, ScaleMode::Percent, ScaleMode::Indexed] {
        let view = ScaleView::new(mode, false);
        for &mult in PERCENT_GRID_MULTIPLES {
            let y = 100.0 * mult;
            assert_eq!(view.map(y, anchor), mode.map(y, anchor), "{mode:?} map drift");
            let m = mode.map(y, anchor);
            assert_eq!(view.unmap(m, anchor), mode.unmap(m, anchor), "{mode:?} unmap drift");
        }
    }
}

#[test]
fn scale_view_invert_negates_mapped_and_round_trips() {
    let anchor = 63_000.0;
    for mode in [ScaleMode::Linear, ScaleMode::Log, ScaleMode::Percent, ScaleMode::Indexed] {
        let view = ScaleView::new(mode, true);
        for &mult in PERCENT_GRID_MULTIPLES {
            let y = 100.0 * mult;
            // inverted mapped is the negation of the plain mapped
            assert_eq!(view.map(y, anchor), -mode.map(y, anchor), "{mode:?} invert map");
            // and unmap is the exact inverse of the inverted map
            let back = view.unmap(view.map(y, anchor), anchor);
            let rel = (back - y).abs() / y.abs().max(1.0);
            assert!(rel < 1e-12, "{mode:?} invert round-trip failed for {y}: back={back}");
        }
    }
}

#[test]
fn scale_view_flip_is_its_own_inverse() {
    let v = ScaleView::new(ScaleMode::Linear, true);
    assert_eq!(v.flip(v.flip(42.0)), 42.0);
    let off = ScaleView::new(ScaleMode::Linear, false);
    assert_eq!(off.flip(42.0), 42.0, "flip must be identity when invert is off");
}

#[test]
fn nice_ticks_view_invert_flips_mapped_keeps_raw() {
    // Same call, invert on vs off: the RAW tick positions (the price gridlines)
    // are identical; only each tick's plotted `mapped` y is negated.
    let plain = nice_ticks(ScaleMode::Linear, 0.0, 100.0, 0.0, 10);
    let inv = nice_ticks_view(ScaleView::new(ScaleMode::Linear, true), 0.0, 100.0, 0.0, 10);
    assert_eq!(plain.len(), inv.len());
    for (p, i) in plain.iter().zip(inv.iter()) {
        assert_eq!(p.raw, i.raw, "raw tick positions must match");
        assert_eq!(i.mapped, -p.mapped, "inverted mapped must be the negation");
        assert_eq!(i.step_mapped, p.step_mapped, "step_mapped is invert-invariant (abs distance)");
    }
}

#[test]
fn convert_bounds_view_migrates_on_invert_toggle_alone() {
    // Same mode (Linear), invert flipped false->true: the persisted mapped
    // range [100, 200] must migrate to [-200, -100] (negated + reordered) so
    // the SAME raw price range stays in view, just flipped.
    let (lo, hi) = convert_bounds_view(
        ScaleView::new(ScaleMode::Linear, false),
        ScaleView::new(ScaleMode::Linear, true),
        0.0,
        0.0,
        100.0,
        200.0,
    );
    assert_eq!((lo, hi), (-200.0, -100.0));
}

#[test]
fn convert_bounds_view_no_change_is_byte_identical_passthrough() {
    // identical views (Linear, not inverted) => raw range unchanged.
    let (lo, hi) = convert_bounds_view(
        ScaleView::new(ScaleMode::Linear, false),
        ScaleView::new(ScaleMode::Linear, false),
        0.0,
        0.0,
        100.0,
        200.0,
    );
    assert_eq!((lo, hi), (100.0, 200.0));
}
