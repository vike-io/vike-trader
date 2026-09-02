//! Characterization ("golden behavior") tests for [`vike_chart::chart::draw`] — the 2,000-line
//! immediate-mode-egui orchestration function that is about to be split apart.
//!
//! WHY THIS FILE EXISTS: `draw`'s pure leaves (the `ChartState` data path, the transform/scale
//! helpers) are already well covered by unit tests in `src/`. Its ORCHESTRATION is not: which
//! `ChartActions` field fires for which scripted input, which style/pane branch is taken, that a
//! given `(style × pane-config)` combination renders end-to-end without panicking. This file drives
//! the REAL `draw` headlessly through `egui::Context` (fabricated `RawInput` + pointer/zoom events,
//! a warm-up frame so immediate-mode interaction registers) and asserts the RETURNED
//! [`vike_chart::ChartActions`]. Subsequent PRs that extract pieces of `draw` are gated by "the same
//! action still fires for the same input", not merely "it compiles".
//!
//! This is an INTEGRATION test (separate crate): it exercises `draw` exactly through its public
//! surface, the way `vike-app` calls it, so it also pins that the surface stays callable. No
//! production code is touched.
//!
//! The fixtures, the `Case` scenario builder and the font/timezone prerequisites live in
//! [`common`] — they were MOVED there when `tessellation_goldens.rs` came to need the same
//! scenarios, since two integration tests cannot see each other's items.

mod common;

use common::{
    flat_bars, make_state, price_pane_point, warm_vpin, wave_bars, wave_bars_ot_shifted,
    wave_bars_scaled, Case,
};
use indexmap::IndexMap;
use vike_chart::scale::ScaleAssign;
use vike_chart::{
    draw, get_study, Active, ActiveStudy, ChartActions, ChartInputs, ChartOptions, ChartState,
    ChartStyle, FollowLive, IndicatorDialog, PaneFractions, PaneKey, ScaleMode, SettingsDialog,
};

/// Assert none of the user-action `ChartActions` fields fired (the "no interaction" invariant).
/// `visible_ts`/`hover_ts` are sync-seam OUTPUTS, not user actions, so they are intentionally not
/// constrained here (`visible_ts` is `Some` whenever the series is non-empty).
fn assert_no_actions(a: &ChartActions, ctx: &str) {
    assert!(a.hovered.is_none(), "{ctx}: hovered should be None with no pointer");
    assert!(a.nav_out.is_none(), "{ctx}: nav_out should be None");
    assert!(a.scale_change.is_none(), "{ctx}: scale_change should be None");
    assert!(a.options_change.is_none(), "{ctx}: options_change should be None");
    assert!(a.remove_uid.is_none(), "{ctx}: remove_uid should be None");
    assert!(a.indicator_edit.is_none(), "{ctx}: indicator_edit should be None");
    assert!(a.move_study.is_none(), "{ctx}: move_study should be None");
    assert!(!a.cvd_toggle, "{ctx}: cvd_toggle should be false");
    assert!(!a.interacted, "{ctx}: interacted should be false with no gesture");
    assert!(a.scale_fallback.is_none(), "{ctx}: scale_fallback should be None (Linear)");
}

// ============================ CASE 1: baseline, no interaction ============================
// The essential net: `draw` runs end-to-end without panicking and fires NO action, across a matrix
// of (style × pane-config). This alone catches panics and orchestration breaks in every later
// extraction, for each rendering branch (candle painter, line painter, HeikinAshi two-tier
// transform, Renko reindexing transform + its volume-suppression branch) and each pane layout
// (price-only, +volume, +volume+study-pane).

#[test]
fn case1_candles_price_only() {
    let state = make_state(wave_bars(40));
    let mut c = Case::new(&state);
    c.options.show_volume = false; // price pane only, no sub-panes
    assert_no_actions(&c.run(), "candles/price-only");
}

#[test]
fn case1_candles_with_volume() {
    let state = make_state(wave_bars(40));
    let mut c = Case::new(&state); // default options => volume shown
                                   // Chart single-max default: Volume must be authored into the unified sub-order.
    c.sub_panes = &[PaneKey::Volume];
    assert_no_actions(&c.run(), "candles/+volume");
}

#[test]
fn case1_line_price_only() {
    let state = make_state(wave_bars(40));
    let mut c = Case::new(&state);
    c.style = ChartStyle::Line;
    c.options.show_volume = false;
    assert_no_actions(&c.run(), "line/price-only");
}

#[test]
fn case1_heikin_ashi_with_volume() {
    let state = make_state(wave_bars(40));
    let mut c = Case::new(&state);
    c.style = ChartStyle::HeikinAshi; // exercises the two-tier transform path
    c.sub_panes = &[PaneKey::Volume];
    assert_no_actions(&c.run(), "heikin-ashi/+volume");
}

#[test]
fn case1_renko_volume_auto_suppressed() {
    // Renko is a reindexing transform: `style_preserves_volume(Renko) == false`, so even with
    // `show_volume = true` no volume pane is produced. Proves the transform render path AND the
    // volume-suppression branch both survive end-to-end.
    let state = make_state(wave_bars(60));
    let mut c = Case::new(&state);
    c.style = ChartStyle::Renko; // show_volume stays true (default) — must be ignored
    c.sub_panes = &[PaneKey::Volume]; // authored, but Renko suppresses it downstream
    assert_no_actions(&c.run(), "renko/volume-suppressed");
}

#[test]
fn case1_candles_with_volume_and_study_pane() {
    // Volume pane + one oscillator study pane (RSI is a non-overlay/oscillator indicator). Drives
    // the `visible_study_panes` filter, the study-pane render loop, and the multi-pane height/
    // separator bookkeeping — the richest default pane layout.
    let state = make_state(wave_bars(60));
    let rsi_spec = vike_chart::indicators::get("rsi").expect("rsi indicator registered");
    let rsi = Active::new(1, rsi_spec, &state.bars);
    assert!(!rsi.is_overlay(), "rsi must be an oscillator so it gets its own study pane");
    let indicators = [rsi];
    // Chart single-max default: Volume + the study pane authored as peers in the
    // unified sub-order (Volume must be present in the slice to render now).
    let sub_panes = [PaneKey::Volume, PaneKey::Study(1)];
    let mut study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
    study_pane_of.insert(1, PaneKey::Study(1));

    let mut c = Case::new(&state);
    c.indicators = &indicators;
    c.sub_panes = &sub_panes;
    c.study_pane_of = &study_pane_of;
    assert_no_actions(&c.run(), "candles/+volume+study");
}

/// The frame-level EMPTY-INPUT net for the price pane's sentinel-seeded extremum folds — the
/// vike-chart sibling of the crash PR #1435 fixed in `crates/vike-studio/src/results.rs`'s
/// `distribution_tab`, where an empty-input fold's sentinels reached `Plot::show` and panicked
/// before any frame assertion ran.
///
/// An empty chart is ordinary (a fresh symbol before its first bar arrives), and three empty
/// guards stand between it and the price pane's bounds write:
/// `crates/vike-chart/src/chart/extent.rs`'s `y_raw_ext` answers `None` for an empty slice, its
/// `default_bounds` skips the whole seed-fold under `apply = !series.is_empty()`, and
/// `crates/vike-chart/src/model.rs`'s `refresh_caches` leaves `y_ext` `None` for an empty closed
/// prefix. The pure halves are unit-pinned where they live (extent.rs's
/// `y_autofit_pads_visible_extents` pins `y_raw_ext(&[]).is_none()`); what no unit test could see
/// is the ORCHESTRATION — that the full `draw` completes over the degenerate state at all. Until
/// this test, no scenario in the suite ran it: every fixture has bars.
///
/// The consequence class if a guard went is MILDER than the exemplar's, and the difference is
/// worth recording so nobody "proves" these guards with a mutation that cannot redden: deleting
/// `y_raw_ext`'s empty-return hands the auto-y arm `(f64::INFINITY, f64::NEG_INFINITY)`, which
/// reaches `set_plot_bounds` as an EXACTLY-non-finite y-range — and egui_plot 0.37 sanitizes
/// that to a ±1.0 axis before its "Bad final plot bounds" debug_assert (verified by inspection
/// of `PlotTransform::new` in egui_plot-0.37.0's `axis.rs`, whose `!bounds.is_finite_y()` arm
/// OVERWRITES the axis with `PlotBounds::new_symmetrical(1.0)` and only then asserts).
///
/// ⚠ The exemplar crashed on a NARROWER shape than "huge sentinels", and which shape it is
/// decides which mutations can redden anything. A huge-FINITE pair clears that finiteness arm,
/// and only a THIN axis (`bounds.width() <= 0.0`) then falls into the sanitizer's own centre
/// arithmetic — `emath::fast_midpoint`, a literal `(min + max) / 2`, which overflows to `inf`
/// for two `f64::MAX`-class values of the same sign. A huge-finite but WIDE range (`-f64::MAX`
/// to `f64::MAX`) is `is_valid` and sails through untouched. The exemplar met both conditions at
/// once — every bar centre landed on the SAME ~1.7e308, so its x-axis was huge, finite AND
/// zero-width — which is why the panic it recorded reads `min: [inf, ...], max: [inf, ...]`:
/// that is the SANITIZER's output, not the bounds the code wrote (the measurement lives on
/// `crates/vike-studio/src/results.rs`'s `distribution_tab`, beside the guard it justifies).
///
/// So this test is the characterization net for the degenerate state — a sane frame, no visible
/// window, no actions — and it stays in the suite for the day an egui_plot bump changes those
/// sanitize semantics out from under the in-tree guards.
#[test]
fn an_empty_chart_renders_a_sane_frame_and_no_visible_window() {
    let state = make_state(Vec::new());
    assert!(
        state.bars.is_empty() && state.y_ext.is_none(),
        "premise: a truly empty chart, with no cached y-extent to mask the empty folds"
    );
    let acts = Case::new(&state).run();
    assert!(acts.visible_ts.is_none(), "an empty series has no visible window to broadcast");
    assert_no_actions(&acts, "empty chart");
}

// ==================== CASE 1b: tick-driven microstructure studies ====================

#[test]
fn case1b_study_only_pane_renders_end_to_end() {
    // A sub-pane held open by a microstructure study ALONE — no indicator is authored
    // into it, so this drives the new pane-gate disjunct, the study y-fit fold, the
    // `render_study` paint, the study legend row, and the value-tag fallback.
    let state = make_state(wave_bars(60));
    let studies = [warm_vpin(9, state.closed_len)];
    assert!(!studies[0].is_empty(), "vpin is trades-only: never book-gated");
    let sub_panes = [PaneKey::Volume, PaneKey::Study(9)];

    let mut c = Case::new(&state);
    c.studies = &studies;
    c.sub_panes = &sub_panes;
    assert_no_actions(&c.run(), "candles/+volume+study-pane");
}

#[test]
fn case1b_indicator_and_study_share_one_pane() {
    // The merged case: an RSI oscillator and a VPIN study authored into the SAME pane.
    // Both populations fold into one combined y-fit and paint into one plot; the
    // indicator keeps its full ✕/⚙/⋯ cluster and the study stacks a legend row below.
    let state = make_state(wave_bars(60));
    let rsi_spec = vike_chart::indicators::get("rsi").expect("rsi indicator registered");
    let indicators = [Active::new(1, rsi_spec, &state.bars)];
    let studies = [warm_vpin(1, state.closed_len)]; // same uid ⇒ same PaneKey::Study(1)
    let sub_panes = [PaneKey::Study(1)];
    let mut study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
    study_pane_of.insert(1, PaneKey::Study(1));

    let mut c = Case::new(&state);
    c.indicators = &indicators;
    c.studies = &studies;
    c.sub_panes = &sub_panes;
    c.study_pane_of = &study_pane_of;
    assert_no_actions(&c.run(), "candles/+merged indicator+study pane");
}

#[test]
fn case1b_book_gated_study_with_no_l2_feed_opens_no_pane() {
    // `book_imbalance` needs an L2 book and none was ever fed, so the study reports
    // `is_empty()`: its pane must not open and nothing may be drawn for it. The
    // observable here is that `draw` behaves exactly like the price-only scenario —
    // in particular the price pane still fills the chart, so a pointer placed for the
    // price pane still produces the hovered OHLC readout.
    let state = make_state(flat_bars(30));
    let spec = get_study("book_imbalance").expect("book_imbalance study registered");
    let studies = [ActiveStudy::new(9, spec)];
    assert!(studies[0].is_empty(), "no book fed ⇒ gated");
    assert!(studies[0].series().is_empty(), "a gated study fabricates no values");
    let sub_panes = [PaneKey::Study(9)];

    let mut c = Case::new(&state);
    c.studies = &studies;
    c.sub_panes = &sub_panes;
    c.frames = 3;
    c.pointer = Some(price_pane_point(c.screen));
    let gated = c.run();

    // The same scenario with NO studies at all: identical hovered readout.
    let mut c2 = Case::new(&state);
    c2.sub_panes = &sub_panes;
    c2.frames = 3;
    c2.pointer = Some(price_pane_point(c2.screen));
    let none = c2.run();

    assert_eq!(gated.hovered, none.hovered, "a gated study must not change the layout");
    assert!(gated.hovered.is_some(), "the price pane still fills the chart");
}

// ==================== sentinel-fold sweep: the all-NaN warm-up study pane ====================
// The study-pane degenerate/populated pair of the PR #1435 sibling sweep (template:
// `crates/vike-studio/src/results.rs`'s `distribution_tab` tests). The degenerate half drives the
// one study-pane input that leaves the combined-fit fold with NOTHING finite; the populated half
// proves the same mounting still reaches the real fitted-bounds write.

/// THE DEGENERATE HALF: an oscillator still inside its warm-up. ATR-14 on a 5-bar chart is
/// ordinary — every freshly-opened chart passes through this state — and it leaves the pane with
/// nothing finite: every output value is NaN (5 bars < the 14-bar warm-up) and atr declares no
/// reference bands (`crates/vike-indicators/src/registry.rs`'s `"atr"` row carries `bands: &[]`).
/// So `crates/vike-chart/src/chart/subpanes.rs`'s `draw_one_study_pane` runs its combined-fit
/// fold EMPTY-HANDED: the `(f64::INFINITY, f64::NEG_INFINITY)` seed survives to the
/// `lo.is_finite() && hi.is_finite()` gate, the bounds write is skipped, and the pane must still
/// paint a sane frame around a line `seg_line` breaks at every NaN.
///
/// `show_ob_os_fill` is armed DELIBERATELY, on this bandless oscillator, because that is where
/// the pane's one genuinely load-bearing empty guard sits: `crates/vike-chart/src/render.rs`'s
/// `render_oscillator_parts` seeds its band folds with `f64::NEG_INFINITY`/`f64::INFINITY`, and
/// only the `!a.bands.is_empty()` clause of its fill gate keeps those empty folds out of two
/// painted `Polygon` quads. KILL PROOF (by mutation): delete that clause and BOTH quads paint —
/// the empty folds leave `top` at `-inf` and `bottom` at `+inf`, so both `top < ymax` and
/// `bottom > ymin` hold against the pane's (always finite, always sanitized) `plot_bounds` — and
/// each quad's ±inf PLOT-space y reaches the frame as a `Shape::Path` point of NaN rather than of
/// ±inf, because `position_from_point_y` is `emath::remap` and its `(1 - t) * bottom + t * top` is
/// an `inf + -inf` once `t` goes infinite. `assert_frame_sane` refuses NaN and non-finite alike,
/// so the proof does not turn on which of the two arrives. It is the only BANDLESS scenario arming
/// that fill, so it reddens alone: `crates/vike-chart/tests/study_pane_render.rs`'s
/// `ob_os_fill_path_renders` arms the same fill on vpin, whose three registry band levels give the
/// folds real values and leave the mutation inert there. The bounds-write gate above it, by
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

// ============================ CASE 2: hover over a bar ============================

#[test]
fn case2_hover_over_bar_reports_ohlc() {
    // Uniform bars ⇒ whichever bar the centred pointer resolves to, the hovered OHLC is the same
    // known `[o, h, l, c]`. Asserts the exact readout the title-bar OHLC legend consumes — the hover
    // orchestration (pointer_coordinate → snap-to-bar-index → fill `hovered`) round-trips a bar's
    // O/H/L/C correctly.
    let state = make_state(flat_bars(30));
    let mut c = Case::new(&state);
    c.frames = 3; // seed view (frame 1) → hover registers off frame 2's geometry (read frame 3)
    c.pointer = Some(price_pane_point(c.screen));

    let acts = c.run();
    assert_eq!(
        acts.hovered,
        Some([1.0, 2.0, 0.5, 1.5]),
        "hover over the price pane must report the bar's [o, h, l, c]"
    );
    // The hovered bar's open-time is also broadcast (sync-seam leader signal).
    assert!(acts.hover_ts.is_some(), "hover_ts must be Some while hovering a bar");
    // Hovering is not, by itself, a chart-driving interaction (no drag/zoom/nav).
    assert!(!acts.interacted, "a bare hover must not set interacted");
}

#[test]
fn case2_pointer_outside_plot_no_hover() {
    // Pointer far off-screen (well outside the plot rect): no bar is hovered.
    let state = make_state(flat_bars(30));
    let mut c = Case::new(&state);
    c.frames = 3;
    c.pointer = Some(egui::pos2(5000.0, 5000.0));

    let acts = c.run();
    assert!(acts.hovered.is_none(), "pointer outside the plot must not report a hovered bar");
    assert!(acts.hover_ts.is_none(), "no hover ⇒ no hover_ts");
}

#[test]
fn case2_no_pointer_no_hover() {
    // Belt-and-suspenders: with no pointer event at all, hover is None (the case-1 baseline in
    // isolation, asserted directly on the hover fields).
    let state = make_state(flat_bars(30));
    let c = Case::new(&state);
    let acts = c.run();
    assert!(acts.hovered.is_none());
    assert!(acts.hover_ts.is_none());
}

// ============================ CASE 3: interaction detection ============================
// A scriptable orchestration signal that is ROBUST headless (unlike the bottom-row nav/scale
// buttons, whose screen positions are derived from the plot's post-layout `frame` rect and so are
// too brittle to hit reliably — those literal nav_out/scale_change clicks are deliberately NOT
// asserted here; see the report). A zoom gesture while the pointer is over the plot sets
// `ChartActions::interacted` — the exact branch a later `draw` split could silently drop.

#[test]
fn case3_zoom_over_plot_sets_interacted() {
    let state = make_state(wave_bars(40));
    let mut c = Case::new(&state);
    c.frames = 3; // warm up hover so the plot response reads as hovered on the zoom frame
    c.pointer = Some(price_pane_point(c.screen));
    c.zoom_last = true;

    let acts = c.run();
    assert!(
        acts.interacted,
        "a zoom gesture over the hovered plot must set ChartActions::interacted"
    );
}

#[test]
fn case3_zoom_without_pointer_not_interacted() {
    // Control: the same zoom event with NO pointer over the plot must NOT set interacted (the guard
    // is `response().hovered() && zoom_delta != 1.0`, not zoom alone).
    let state = make_state(wave_bars(40));
    let mut c = Case::new(&state);
    c.frames = 3;
    c.pointer = None;
    c.zoom_last = true;

    let acts = c.run();
    assert!(!acts.interacted, "zoom with no pointer over the plot must not set interacted");
}

// ============================ absolute-shared-axis (SharedLinear) ============================

/// Drive `draw` for `frames` on one persistent context with a single compare `overlay` pinned
/// by `assign`, in `scale` mode. Proves the full public surface (compute + autofit fold +
/// paint z-order) runs end-to-end without panicking for a compare overlay — `SharedLinear` when
/// this helper was written, and `Right`/`Left` since the secondary-axis pair below joined it.
/// Returns the last frame's `ChartActions`.
///
/// `series_pane`: `Some(pk)` moves the compare OUT of the price overlay into its OWN sub-pane
/// (`ChartInputs::series_panes` + `series_pane_of` — the C2b own-pane path `draw` dispatches to
/// `crates/vike-chart/src/chart/subpanes.rs`'s `draw_one_series_pane`). `assign` is then inert:
/// every price-pane overlay compute loop (the Percent block in `chart::draw`, plus
/// `crates/vike-chart/src/chart/price_render.rs`'s `compute_shared_axis_lines` and
/// `compute_secondary_axis_lines`) skips a `series_pane_of` member before consulting
/// `series_scale`. `None` is byte-identical to the pre-parameter harness.
fn run_with_overlay(
    primary: &ChartState,
    overlay_state: &ChartState,
    overlay_sym: &str,
    assign: ScaleAssign,
    scale: ScaleMode,
    frames: usize,
    series_pane: Option<PaneKey>,
) -> ChartActions {
    let ctx = egui::Context::default();
    common::bind_chart_font_families(&ctx);
    let mut follow = FollowLive::default();
    let mut settings = SettingsDialog::default();
    let mut indicator_dialog = IndicatorDialog::default();
    let mut panes = PaneFractions::default();
    let series_panes: Vec<PaneKey> = series_pane.into_iter().collect();
    let mut series_pane_of: IndexMap<String, PaneKey> = IndexMap::new();
    if let Some(pk) = series_pane {
        series_pane_of.insert(overlay_sym.to_string(), pk);
    }
    let mut series_scale: IndexMap<String, ScaleAssign> = IndexMap::new();
    series_scale.insert(overlay_sym.to_string(), assign);
    let overlays = [vike_chart::chart::SeriesInput {
        symbol: overlay_sym,
        state: overlay_state,
        color: egui::Color32::from_rgb(200, 120, 40),
    }];
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(960.0, 620.0));
    let study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
    let mut result = ChartActions::default();
    for f in 0..frames {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            result = draw(
                ui,
                ChartInputs {
                    state: primary,
                    style: ChartStyle::Candles,
                    nav: None,
                    indicators: &[],
                    studies: &[],
                    follow: &mut follow,
                    options: &ChartOptions::default(),
                    settings: &mut settings,
                    indicator_dialog: &mut indicator_dialog,
                    scale,
                    invert: false,
                    panes: &mut panes,
                    sync: None,
                    footprint: None,
                    footprint_gen: 0,
                    cvd_on: false,
                    profile_on: false,
                    of_tick_size: 0.0,
                    sub_panes: &[],
                    study_pane_of: &study_pane_of,
                    overlays: &overlays,
                    series_panes: &series_panes,
                    series_pane_of: &series_pane_of,
                    series_scale: &series_scale,
                    gpu_candles: None,
                },
            );
        });
        // See `Case::run_full` — egui 0.36 panics on a dropped `TexturesDelta` that still holds
        // unapplied deltas, and this pass renders nothing. Cleared BEFORE the assertion for the
        // reason spelled out there: a failing assertion must be able to unwind.
        out.textures_delta.clear();
        // Same shared geometry invariant as `Case::run_full`.
        vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    }
    result
}

#[test]
fn shared_linear_overlay_renders_end_to_end_linear_and_log() {
    // A compare ~2x the primary's magnitude, pinned SharedLinear: the whole absolute-shared-axis
    // path (compute_shared_axis_lines + the auto_y raw-extent fold + the seg_line paint) must run
    // without panicking in BOTH Linear and Log primary modes (SharedLinear is a Linear/Log
    // feature). `visible_ts` is Some whenever the primary series is non-empty.
    let primary = make_state(wave_bars(60));
    let overlay = make_state(wave_bars_scaled(60, 2.0));
    for scale in [ScaleMode::Linear, ScaleMode::Log] {
        let acts =
            run_with_overlay(&primary, &overlay, "CMP", ScaleAssign::SharedLinear, scale, 2, None);
        assert!(acts.visible_ts.is_some(), "{scale:?}: non-empty series ⇒ visible_ts Some");
    }
}

#[test]
fn shared_linear_overlay_in_percent_mode_is_inert() {
    // In Percent primary mode a SharedLinear compare renders nothing (absolute price has no
    // mapping onto a rebased axis) — the render must still complete cleanly (no panic), exactly
    // as a bare Percent chart would.
    let primary = make_state(wave_bars(40));
    let overlay = make_state(wave_bars_scaled(40, 3.0));
    let acts = run_with_overlay(
        &primary,
        &overlay,
        "CMP",
        ScaleAssign::SharedLinear,
        ScaleMode::Percent,
        2,
        None,
    );
    assert!(acts.visible_ts.is_some());
}

// ==================== sentinel-fold sweep: the all-gap compare series pane ====================
// The compare-series-pane degenerate/populated pair of the PR #1435 sibling sweep — and the first
// end-to-end render coverage `draw_one_series_pane` has at all (the SharedLinear tests above
// exercise the price-pane overlay paths; nothing before this drove the C2b own-pane dispatch).

/// THE DEGENERATE HALF: an all-gap compare window. A compare whose history begins only after the
/// primary's ends aligns onto NO primary index — `crates/vike-chart/src/render.rs`'s
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

// ==================== background: solid vs gradient (render output) ====================

/// Every mesh-vertex color emitted this frame, recursing into `Shape::Vec` groups. The
/// gradient background is painted as a `Shape::Mesh`, so its stop colors show up here — this
/// is how the two tests below prove the paint actually reached the render output (not just the
/// config), without a GPU.
fn mesh_vertex_colors(shapes: &[egui::epaint::ClippedShape]) -> Vec<egui::Color32> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Mesh(m) => out.extend(m.vertices.iter().map(|v| v.color)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for cs in shapes {
        walk(&cs.shape, &mut out);
    }
    out
}

#[test]
fn gradient_background_emits_mesh_with_both_stops() {
    // Sentinel stop colors that nothing else in the chart paints, so finding them among the
    // frame's mesh vertices unambiguously proves the gradient background mesh was emitted with
    // the top stop (`bg_top`) at its top and the bottom stop (`bg`) at its bottom.
    let state = make_state(wave_bars(60));
    let mut case = Case::new(&state);
    case.options.bg_gradient = true;
    case.options.bg_top = [1, 2, 3];
    case.options.bg = [6, 7, 8];

    let (_acts, out) = case.run_frames();
    let colors = mesh_vertex_colors(&out.shapes);
    assert!(
        colors.contains(&egui::Color32::from_rgb(1, 2, 3)),
        "top stop reached the render output"
    );
    assert!(
        colors.contains(&egui::Color32::from_rgb(6, 7, 8)),
        "bottom stop reached the render output"
    );
}

#[test]
fn solid_background_never_paints_bg_top() {
    // Gradient OFF (today's default): `bg_top` must be inert. A sentinel `bg_top` that never
    // shows up as a painted vertex proves the solid path is byte-identical to before the feature.
    let state = make_state(wave_bars(60));
    let mut case = Case::new(&state);
    case.options.bg_gradient = false;
    case.options.bg_top = [1, 2, 3];

    let (_acts, out) = case.run_frames();
    let colors = mesh_vertex_colors(&out.shapes);
    assert!(
        !colors.contains(&egui::Color32::from_rgb(1, 2, 3)),
        "solid path ignores bg_top entirely"
    );
}
