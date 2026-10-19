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
//! surface, the way `vike-desktop` calls it, so it also pins that the surface stays callable. No
//! production code is touched.
//!
//! The fixtures, the `Case` scenario builder and the font/timezone prerequisites live in
//! [`common`] — they were MOVED there when `tessellation_goldens.rs` came to need the same
//! scenarios, since two integration tests cannot see each other's items.

mod common;

#[path = "draw_characterization/appearance.rs"]
mod appearance;
#[path = "draw_characterization/sentinel_sweeps.rs"]
mod sentinel_sweeps;

use common::{Case, flat_bars, make_state, price_pane_point, wave_bars, wave_bars_scaled};
use indexmap::IndexMap;
use vike_chart::scale::ScaleAssign;
use vike_chart::{
    Active, ChartActions, ChartInputs, ChartOptions, FollowLive, IndicatorDialog, PaneFractions,
    ScaleMode, SettingsDialog,
    chart::{draw, style::ChartStyle},
    model::ChartState,
    panes::PaneKey,
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
/// vike-chart sibling of the crash PR #1435 fixed in `crates/vike-studio/src/panes/results.rs`'s
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
/// `crates/vike-studio/src/panes/results.rs`'s `distribution_tab`, beside the guard it justifies).
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
    vike_chart::chart::harness::bind_chart_font_families(&ctx);
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
