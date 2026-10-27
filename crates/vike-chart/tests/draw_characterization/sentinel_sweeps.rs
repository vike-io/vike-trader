//! The PR #1435 sentinel-fold sweep's degenerate/populated pairs.

use super::{assert_no_actions, run_with_overlay};
use crate::common::{Case, make_state, wave_bars, wave_bars_ot_shifted, wave_bars_scaled};
use indexmap::IndexMap;
use vike_chart::scale::ScaleAssign;
use vike_chart::{Active, ScaleMode, panes::PaneKey};

// ==================== sentinel-fold sweep: the all-NaN warm-up study pane ====================
// The study-pane degenerate/populated pair of the PR #1435 sibling sweep (template:
// `crates/vike-studio/src/panes/results.rs`'s `distribution_tab` tests). The degenerate half drives the
// one study-pane input that leaves the combined-fit fold with NOTHING finite; the populated half
// proves the same mounting still reaches the real fitted-bounds write.

/// THE DEGENERATE HALF: an oscillator still inside its warm-up. ATR-14 on a 5-bar chart is
/// ordinary — every freshly-opened chart passes through this state — and it leaves the pane with
/// nothing finite: every output value is NaN (5 bars < the 14-bar warm-up) and atr declares no
/// reference bands (`crates/vike-indicators/src/registry/base.rs`'s `"atr"` row carries `bands: &[]`).
/// So `crates/vike-chart/src/chart/subpanes.rs`'s `draw_one_study_pane` runs its combined-fit
/// fold EMPTY-HANDED: the `(f64::INFINITY, f64::NEG_INFINITY)` seed survives to the
/// `lo.is_finite() && hi.is_finite()` gate, the bounds write is skipped, and the pane must still
/// paint a sane frame around a line `seg_line` breaks at every NaN.
///
/// `show_ob_os_fill` is armed DELIBERATELY, on this bandless oscillator, because that is where
/// the pane's one genuinely load-bearing empty guard sits: `crates/vike-chart/src/render/oscillator.rs`'s
/// `render_oscillator_parts` seeds its band folds with `f64::NEG_INFINITY`/`f64::INFINITY`, and
/// only the `!a.bands.is_empty()` clause of its fill gate keeps those empty folds out of two
/// painted `Polygon` quads. KILL PROOF (by mutation): delete that clause and BOTH quads paint —
/// the empty folds leave `top` at `-inf` and `bottom` at `+inf`, so both `top < ymax` and
/// `bottom > ymin` hold against the pane's (always finite, always sanitized) `plot_bounds` — and
/// each quad's ±inf PLOT-space y reaches the frame as a `Shape::Path` point of NaN rather than of
/// ±inf, because `position_from_point_y` is `emath::remap` and its `(1 - t) * bottom + t * top` is
/// an `inf + -inf` once `t` goes infinite. `assert_frame_sane` refuses NaN and non-finite alike,
/// so the proof does not turn on which of the two arrives. It is the only BANDLESS scenario arming
/// that fill, so it reddens alone: an oscillator whose registry band levels give the folds real
/// values leaves the mutation inert (the microstructure study pane that used to arm the same fill
/// on vpin, three band levels, was deleted unreachable). The bounds-write gate above it, by
/// contrast, is belt-and-braces on egui_plot 0.37: an exactly-±inf `set_plot_bounds` is sanitized
/// to a ±1.0 axis before the library's own bounds assert (see
/// `an_empty_chart_renders_a_sane_frame_and_no_visible_window`), so deleting THAT gate changes
/// which fallback answers, not whether the frame is sane — recorded here so nobody "proves" it
/// with a mutation that cannot redden.
#[test]
fn an_all_nan_warm_up_oscillator_pane_paints_finite_geometry() {
    let state = make_state(wave_bars(5));
    let spec = vike_chart::indicators::get("atr").expect("atr indicator registered");
    let mut atr = Active::new(1, spec, &state.bars);
    assert!(!atr.is_overlay(), "premise: atr is an oscillator, so it gets its own study pane");
    assert!(atr.bands.is_empty(), "premise: no band level may hand the pane fold a finite value");
    assert_eq!(atr.outputs[0].series.len(), 5, "premise: one output value per bar");
    assert!(
        atr.outputs.iter().all(|l| l.series.iter().all(|v| v.is_nan())),
        "premise: 5 bars < the 14-bar warm-up, or the combined fit would have something finite"
    );
    atr.show_ob_os_fill = true; // arm the fill gate's bandless arm (see the doc above)
    let indicators = [atr];
    let sub_panes = [PaneKey::Study(1)];
    let mut study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
    study_pane_of.insert(1, PaneKey::Study(1));

    let mut c = Case::new(&state);
    c.indicators = &indicators;
    c.sub_panes = &sub_panes;
    c.study_pane_of = &study_pane_of;
    assert_no_actions(&c.run(), "candles/+all-NaN warm-up study pane");
}

/// The pair's other half: a warmed oscillator (60 bars > the 14-bar warm-up) drives the SAME
/// mounting down `draw_one_study_pane`'s POPULATED arm — a non-empty combined fit, the `y_pad`ed
/// bounds write, and a `seg_line` with real runs to paint — so the degenerate half above cannot
/// be satisfied by an early return out of the pane. (Template: results.rs's
/// `a_populated_equity_curve_still_reaches_the_binned_histogram`, whose shape this follows
/// exactly: a premise assertion, then a render that must complete.)
///
/// ⚠ DECLARED RESIDUAL — this doc's first draft claimed the pair would catch "a guard widened by
/// accident to swallow every input", and it does not. The test never observes the bounds WRITE,
/// and nothing cheap here can: `set_plot_bounds` only queues a `BoundsModification`, and the
/// y-axis it resolves to is indistinguishable from egui_plot's own auto-fit, which folds the
/// same `Line` item and pads it by the same 5% default (`margin_fraction`) that this pane's
/// `y_pad(lo, hi, 5.0, 5.0)` applies. So a guard widened to skip every input falls back to an
/// axis that LOOKS the same and this test stays green. What the pair does fence is the class the
/// sweep was hunting: the populated path still completes, still emits paintable geometry
/// (`assert_frame_sane`, every frame) and still fires no spurious action. Proving the write
/// itself belongs to the rung above — a `tessellation_goldens.rs` entry, where the difference is
/// visible as moved geometry, because the write also pins X to the price pane's `[cx.px0,
/// cx.px1]` while an auto-fit would margin out to its own `[0, 59]`.
#[test]
fn a_warmed_oscillator_pane_still_reaches_the_fitted_bounds_write() {
    let state = make_state(wave_bars(60));
    let spec = vike_chart::indicators::get("atr").expect("atr indicator registered");
    let atr = Active::new(1, spec, &state.bars);
    assert!(
        atr.outputs[0].series.last().is_some_and(|v| v.is_finite()),
        "premise: the NEWEST bar's value must be finite — the pane folds only the VISIBLE slice \
         (`skip(i0).take(take)`), which follow-live anchors on the tail, so an `any(is_finite)` \
         over the whole series could be satisfied by a bar the fold never reaches"
    );
    let indicators = [atr];
    let sub_panes = [PaneKey::Study(1)];
    let mut study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
    study_pane_of.insert(1, PaneKey::Study(1));

    let mut c = Case::new(&state);
    c.indicators = &indicators;
    c.sub_panes = &sub_panes;
    c.study_pane_of = &study_pane_of;
    assert_no_actions(&c.run(), "candles/+warmed study pane");
}
// ==================== sentinel-fold sweep: the all-gap compare series pane ====================
// The compare-series-pane degenerate/populated pair of the PR #1435 sibling sweep — and the first
// end-to-end render coverage `draw_one_series_pane` has at all (the SharedLinear tests above
// exercise the price-pane overlay paths; nothing before this drove the C2b own-pane dispatch).

/// THE DEGENERATE HALF: an all-gap compare window. A compare whose history begins only after the
/// primary's ends aligns onto NO primary index — `crates/vike-chart/src/render/oscillator.rs`'s
/// `reindex_by_ot` is a strict floor/as-of join, so every slot is `None` — and
/// `crates/vike-chart/src/chart/subpanes.rs`'s `draw_one_series_pane` runs its close-fold
/// EMPTY-HANDED: the `(f64::INFINITY, f64::NEG_INFINITY)` seed reaches the finite gate, the
/// documented `(0.0, 1.0)` fallback answers, and the UNCONDITIONAL `set_plot_bounds` still
/// writes while `seg_line` draws nothing from an all-NaN line. Ordinary input: compare a
/// newly-listed symbol against an older one.
///
/// On egui_plot 0.37 that finite gate is belt-and-braces against the CRASH class (an
/// exactly-±inf bounds write would be sanitized to a ±1.0 axis — see
/// `an_empty_chart_renders_a_sane_frame_and_no_visible_window` for the mechanism and for why
/// the PR #1435 exemplar, whose sentinels were huge-FINITE, still crashed). So this pair is
/// characterization, not a kill proof: it pins that the all-gap window completes with a sane
/// frame and a DETERMINISTIC in-tree fallback, instead of delegating the degenerate case to
/// whatever the plot library's sanitizer happens to guess this release.
#[test]
fn an_all_gap_compare_series_pane_falls_back_to_finite_bounds() {
    let primary = make_state(wave_bars(60));
    let overlay = make_state(wave_bars_ot_shifted(60, 31_622_400_000)); // 366 days forward
    assert!(
        overlay.bars.first().unwrap().ot > primary.bars.last().unwrap().ot,
        "premise: the compare's history must start after the primary's ends, or a slot aligns"
    );
    let acts = run_with_overlay(
        &primary,
        &overlay,
        "CMP",
        ScaleAssign::Percent,
        ScaleMode::Linear,
        2,
        Some(PaneKey::Series(7)),
    );
    assert!(acts.visible_ts.is_some(), "the primary still renders and broadcasts its window");
}

/// The populated half: a compare on the SAME ot grid as the primary aligns everywhere, so the
/// pane's close-fold is non-empty and the real own-axis path — the `y_pad`ed absolute-price fit,
/// the bounds write, the `seg_line` paint and the legend overlay — runs end-to-end. Together
/// with the test above it fences `draw_one_series_pane` the way the study-pane pair fences
/// `draw_one_study_pane`: degenerate input falls back finitely, ordinary input still fits.
#[test]
fn an_overlapping_compare_series_pane_still_fits_its_own_axis() {
    let primary = make_state(wave_bars(60));
    let overlay = make_state(wave_bars_scaled(60, 2.0));
    assert!(
        overlay.bars.first().unwrap().ot == primary.bars.first().unwrap().ot
            && overlay.bars.last().unwrap().ot == primary.bars.last().unwrap().ot,
        "premise: `wave_bars_scaled` scales PRICES only, so the two ot grids are IDENTICAL and \
         every slot aligns — mere overlap would leave the fold's non-emptiness to luck"
    );
    let acts = run_with_overlay(
        &primary,
        &overlay,
        "CMP",
        ScaleAssign::Percent,
        ScaleMode::Linear,
        2,
        Some(PaneKey::Series(7)),
    );
    assert!(acts.visible_ts.is_some(), "the primary still renders and broadcasts its window");
}

// ============ sentinel-fold sweep: the all-gap compare on the SECONDARY price axis ============
// The SAME ordinary input as the series-pane pair above — a compare whose history starts after the
// primary's ends — reaching the THIRD site the sweep found on this path:
// `crates/vike-chart/src/chart/price_render.rs`'s `compute_secondary_axis_lines`, whose own
// `(f64::INFINITY, f64::NEG_INFINITY)` close-fold is guarded by an `is_finite` skip.
//
// ⚠ Be exact about what was and was not covered, because the tempting summary ("nothing tests
// this") is false and would rot into a second wrong claim. The pure function IS unit-tested in its
// own crate, on a POPULATED Right pin, by that file's
// `secondary_axis_skips_sharedlinear_but_still_handles_right`.
// What had no coverage anywhere is (a) the DEGENERATE window — no test in the tree drives that
// fold empty-handed — and (b) the Right/Left arm END TO END through `draw`:
// every pre-existing overlay scenario here pins `SharedLinear` or `Percent`, and both `continue`
// out of the loop at its `ScaleAssign::Right | ScaleAssign::Left` match before the fold runs, so
// the gutter, the `sec_axis_cell` publish and the label formatter ran in no rendered frame.

/// THE DEGENERATE HALF: an all-gap compare pinned to the secondary ABSOLUTE axis. `reindex_by_ot`
/// resolves every slot `None`, so `sec_lo`/`sec_hi` keep their ±inf seed, the guard skips the
/// overlay, and the frame carries no mapped line and no legend row — while the right gutter is
/// still RESERVED and unlabeled, because `sec_axis_active` is decided from CONFIG rather than from
/// data (a data-dependent gutter would make the candles jump as a compare's history comes and
/// goes) and the axis formatter returns an empty string while `sec_axis_cell` holds its NaN
/// sentinels.
///
/// ⚠ CHARACTERIZATION, NOT A KILL PROOF, recorded so nobody plants the obvious mutation and reads
/// its silence as a pass: deleting that `is_finite` skip emits no bad GEOMETRY. The mapped line is
/// built from the same all-`None` reindex, so every value is NaN and `seg_line` drops the run;
/// what the deletion changes is that `sec_axis_cell` receives ±inf and the gutter prints garbage
/// TEXT at finite positions — and `assert_frame_sane` judges coordinates, never strings. What this
/// pair pins is that the Right/Left arm COMPLETES over the degenerate window and stays paintable:
/// the net for a future change to `reindex_by_ot`, to `remap_to_primary`, or to that formatter.
#[test]
fn an_all_gap_secondary_axis_compare_draws_no_line_and_stays_paintable() {
    let primary = make_state(wave_bars(60));
    let overlay = make_state(wave_bars_ot_shifted(60, 31_622_400_000)); // 366 days forward
    assert!(
        overlay.bars.first().unwrap().ot > primary.bars.last().unwrap().ot,
        "premise: the compare's history must start after the primary's ends, or a slot aligns"
    );
    let acts =
        run_with_overlay(&primary, &overlay, "CMP", ScaleAssign::Right, ScaleMode::Linear, 2, None);
    assert!(acts.visible_ts.is_some(), "the primary still renders and broadcasts its window");
}

/// The populated half: the same Right-pinned compare on the primary's own ot grid, which is the
/// only configuration that drives the secondary axis END TO END — the non-empty `sec_lo`/`sec_hi`
/// fold, `remap_to_primary` over every aligned close, the legend's absolute-price readout, and the
/// `sec_axis_cell` publish the right-gutter formatter reads back after `Plot::show` returns. Its
/// pairing with the test above is the same discipline the two pairs above follow: the degenerate
/// input must be SKIPPED, not the whole arm, and only a populated run can tell those apart.
#[test]
fn an_overlapping_secondary_axis_compare_labels_its_own_gutter() {
    let primary = make_state(wave_bars(60));
    let overlay = make_state(wave_bars_scaled(60, 2.0));
    assert!(
        overlay.bars.first().unwrap().ot == primary.bars.first().unwrap().ot,
        "premise: `wave_bars_scaled` scales PRICES only, so the compare aligns onto every slot"
    );
    let acts =
        run_with_overlay(&primary, &overlay, "CMP", ScaleAssign::Right, ScaleMode::Linear, 2, None);
    assert!(acts.visible_ts.is_some(), "the primary still renders and broadcasts its window");
}
