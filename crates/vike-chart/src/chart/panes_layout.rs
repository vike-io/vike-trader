//! Pane-geometry free helpers split out of `chart.rs` (chart refactor PR-1):
//! the draggable inter-pane separator strip and the sub-pane right-gutter
//! y-axis set (incl. the cross-pane spacer axis). Bodies are verbatim.
//!
//! Chart refactor PR-2 (Block B) added [`resolve_pane_layout`]: the pure
//! pane-layout resolution (which sub-panes are present, their pixel heights, and
//! the sub-pane present/heights indices) that used to sit inline in
//! [`crate::chart::draw`] — the highest-value extraction, since those indices
//! were "byte-identical by comment" across four sub-pane render loops.

use super::consts::{AXIS_LABEL_H, MIN_PANE_PX, PANE_DIVIDER, PANE_SEP_H, Y_AXIS_GUTTER_W};
use super::series::style_preserves_volume;
use crate::chart::{ChartStyle, SeriesInput};
use crate::indicators::Active;
use crate::model::ChartState;
use crate::panes::{present_panes, PaneFractions, PaneKey};
use crate::studies::ActiveStudy;
use egui::{CursorIcon, Sense};
use indexmap::IndexMap;

/// A [`PANE_SEP_H`]-tall draggable strip between `present[boundary]` and
/// `present[boundary + 1]` (chart-UX bundle T9): hover shows the vertical-
/// resize cursor, a drag moves the boundary via [`PaneFractions::drag`].
/// `avail_px`/`min_px` MUST be the exact values passed to the `layout` call
/// that produced this frame's pane heights — `drag` re-derives the current
/// on-screen heights from them before applying the delta.
pub(crate) fn pane_separator(
    ui: &mut egui::Ui,
    panes: &mut PaneFractions,
    present: &[PaneKey],
    boundary: usize,
    avail_px: f32,
    min_px: f32,
) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, PANE_SEP_H), Sense::drag());
    // TradingView parity: paint a subtle 1px divider at the strip's center so the
    // pane boundary reads (the strip only sensed drags before — stacked sub-panes
    // blended together). Brightens on hover to advertise the resize affordance.
    let line_col = if resp.hovered() || resp.dragged() {
        egui::Color32::from_rgb(96, 104, 116) // brighter on hover — advertises the resize handle
    } else {
        PANE_DIVIDER
    };
    ui.painter().hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, line_col));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        panes.drag(present, boundary, resp.drag_delta().y, avail_px, min_px);
    }
}

/// The right-gutter y-axis set for a SUB-pane (volume / CVD / oscillator /
/// series). `real` is the pane's own single right axis (built with the same
/// placement / `min_thickness` / formatter it always had).
///
/// Without an active secondary price axis this returns `vec![real]`, which is
/// byte-identical to the pre-7b `.y_axis_position(Right).y_axis_min_width(W)
/// .y_axis_formatter(..)` chain (`custom_y_axes` sets `y_axes` wholesale to a
/// single axis with the same fields + the default `AxisHints::new` label
/// spacing).
///
/// C2b Task 7b — THE CROSS-PANE FIX: when a Right/Left-pinned compare overlay
/// gives the PRICE pane a SECOND right axis, that pane's data rect narrows by
/// one gutter (`Y_AXIS_GUTTER_W`). Every other pane x-slaves to the price pane's
/// plot-space x-range, so to keep the candles x-aligned with the sub-pane bars
/// each sub-pane must reserve the IDENTICAL extra gutter — an empty, label-less
/// spacer axis of the same width. `real` stays INNER (its labels sit against the
/// data rect, aligned with the price pane's primary axis); the spacer is the
/// OUTER column (matching the price pane's secondary axis), so no sub-pane label
/// moves relative to the primary and every pane's data-rect x-span is identical.
pub(crate) fn pane_y_axes(
    real: egui_plot::AxisHints<'_>,
    sec_active: bool,
) -> Vec<egui_plot::AxisHints<'_>> {
    if sec_active {
        let spacer = egui_plot::AxisHints::new(egui_plot::Axis::Y)
            .placement(egui_plot::HPlacement::Right)
            .min_thickness(Y_AXIS_GUTTER_W)
            .formatter(|_, _| String::new());
        vec![real, spacer]
    } else {
        vec![real]
    }
}

/// The shared builder for every x-linked sub-pane (volume / CVD / study / series):
/// no drag/zoom/scroll, no native axis-gutter drag or dbl-click reset (we own the
/// gutters), y-axis on the right through `pane_y_axes`. Differs per pane only in the
/// id, body height, whether this pane carries the shared bottom time axis, and the
/// y-tick number formatter (`fmt_compact` for volume/CVD, `{:.1}` for study/series).
/// Byte-identical to the four inline builders it replaces (chart refactor PR-3).
#[allow(clippy::too_many_arguments)] // cosmetic pane-builder knobs; splitting into a struct buys nothing
pub(crate) fn sub_pane_plot<'a>(
    id: impl egui::AsId,
    body_height: f32,
    axis_h: f32,
    show_bottom_axis: bool,
    grid_color: egui::Color32,
    grid_show: egui::Vec2b,
    sec_axis_active: bool,
    y_fmt: fn(f64) -> String,
) -> egui_plot::Plot<'a> {
    egui_plot::Plot::new(id)
        .height(body_height + if show_bottom_axis { axis_h } else { 0.0 })
        .allow_drag(egui::Vec2b::new(false, false))
        .allow_zoom(egui::Vec2b::new(false, false))
        .allow_scroll(false)
        // native gutter-drag + dbl-click reset bypass the tracked single-write (spec §2); we own the gutters.
        .allow_axis_zoom_drag(false)
        .allow_double_click_reset(false)
        .show_x(false)
        .show_y(false)
        .show_axes(egui::Vec2b::new(show_bottom_axis, true))
        .grid_color(grid_color)
        .show_grid(grid_show)
        .custom_y_axes(pane_y_axes(
            egui_plot::AxisHints::new(egui_plot::Axis::Y)
                .placement(egui_plot::HPlacement::Right)
                .min_thickness(Y_AXIS_GUTTER_W)
                .formatter(move |m, _| y_fmt(m.value)),
            sec_axis_active,
        ))
}

/// This frame's resolved PANE LAYOUT (chart refactor PR-2, Block B): which
/// sub-panes are present, their pixel heights, and the present/heights indices
/// the sub-pane render loops address by. A verbatim extraction of the inline
/// bookkeeping block in [`crate::chart::draw`] — the riskiest currently-untested
/// arithmetic in the function (the indices were previously recomputed inline in
/// each of the four sub-pane loops, "byte-identical by comment"), now resolved
/// once and unit-tested. Borrows `overlays` for the `visible_series` refs.
pub(crate) struct PaneLayout<'a> {
    /// Study panes holding ≥1 currently-visible non-overlay study, in authored
    /// order (a HIDDEN study must not leave a blank strip — the C1 filter).
    pub(crate) visible_study_panes: Vec<PaneKey>,
    /// `visible_study_panes.len()`. Diagnostic/test surface — the render dispatch
    /// keys off `n_reorderable` + `present` positions.
    #[allow(dead_code)]
    pub(crate) n_study_panes: usize,
    /// Volume sub-pane shown this frame: `show_volume` AND a volume-preserving
    /// style (Renko/Kagi/... reindex, so their volume is suppressed) AND non-empty.
    /// Diagnostic/test surface — the render dispatch keys off `present` positions.
    #[allow(dead_code)]
    pub(crate) vol_on: bool,
    /// CVD sub-pane shown: `cvd_on` AND footprint data actually present.
    /// Diagnostic/test surface — see `vol_on`.
    #[allow(dead_code)]
    pub(crate) cvd_pane_on: bool,
    /// Each authored own-pane compare series resolved to its live `SeriesInput`
    /// (panes whose symbol has no live input are dropped — degrade to no pane).
    pub(crate) visible_series: Vec<(PaneKey, &'a SeriesInput<'a>)>,
    /// `visible_series.len()`. Diagnostic/test surface — see `vol_on`.
    #[allow(dead_code)]
    pub(crate) n_series_panes: usize,
    /// Count of PRESENT reorderable sub-panes (Volume + CVD + study panes) —
    /// i.e. `present.len() - 1 - n_series_panes`. The group size the pane ↑/↓
    /// reorder controls clamp against (chart single-max default).
    pub(crate) n_reorderable: usize,
    /// The frame's top-to-bottom pane order (`present_panes`): Price, then the
    /// authored unified sub-panes (Volume/CVD/Study peers), then series panes.
    pub(crate) present: Vec<PaneKey>,
    /// Sub-panes under the price plot (`n_study + n_series + vol + cvd`).
    pub(crate) n_below: usize,
    /// Extra height for the bottom pane's shared time axis (`AXIS_LABEL_H` when
    /// `n_below > 0`, else `0.0`).
    pub(crate) axis_h: f32,
    /// Separators + shared-axis chrome subtracted from `avail` before the pane
    /// split, so `heights.sum() + chrome == avail` (no header rows — the pane
    /// legend/controls now overlay the plot). `draw` consumes it only
    /// folded into `avail_for_panes`; it is surfaced here for the layout invariant
    /// (`heights_sum_plus_chrome_equals_avail`) and the per-config chrome asserts,
    /// hence `allow(dead_code)` for the non-test lib build that reads only the fold.
    #[allow(dead_code)]
    pub(crate) chrome: f32,
    /// `(avail - chrome).max(0.0)` — the height handed to [`PaneFractions::layout`]
    /// and the `avail_px` every separator drag re-derives against.
    pub(crate) avail_for_panes: f32,
    /// Per-pane plot heights in `present` order (`heights.len() == present.len()`).
    pub(crate) heights: Vec<f32>,
    /// `heights[0]` — the price pane's height.
    pub(crate) price_h: f32,
}

/// Resolve [`PaneLayout`] for this frame (chart refactor PR-2, Block B). A verbatim
/// extraction of `draw`'s inline block; `avail` (the only value that read `ui`) is
/// passed in so this stays pure. `panes` is mutated exactly as before — a
/// never-seen pane is pinned to its default share by [`PaneFractions::layout`].
///
/// The visibility predicates are the load-bearing part (see the sub-pane loops that
/// index by the returned bases): `visible_study_panes` keeps a study pane only while
/// ≥1 of its assigned studies is visible AND non-overlay (or ≥1 visible, non-gated
/// microstructure study in `micro` is self-keyed to it); `cvd_pane_on` additionally
/// requires real footprint data; `vol_on` is suppressed for reindexing styles.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_pane_layout<'a>(
    avail: f32,
    show_volume: bool,
    style: ChartStyle,
    state: &ChartState,
    indicators: &[Active],
    // Chart single-max default: the authored UNIFIED sub-pane order (Volume/CVD/
    // Study peers) top→bottom, gated to those present this frame below.
    sub_panes: &[PaneKey],
    study_pane_of: &IndexMap<u64, PaneKey>,
    // Tick-driven microstructure studies ([`crate::studies::ActiveStudy`]), each
    // self-keyed to its own `PaneKey::Study(uid)` — a SECOND population that can keep
    // a study pane present. `&[]` (the default at every build site) leaves the gate
    // below reading exactly the indicator predicate it read before.
    micro: &[ActiveStudy],
    cvd_on: bool,
    has_footprint: bool,
    series_panes: &[PaneKey],
    series_pane_of: &IndexMap<String, PaneKey>,
    overlays: &'a [SeriesInput<'a>],
    panes: &mut PaneFractions,
    // Feature #1b (TradingView parity): when the price pane is maximized, hide
    // every sub-pane so price fills the chart.
    price_maximized: bool,
) -> PaneLayout<'a> {
    // Feature #1b: price-pane maximize overrides the toggles up front — with
    // volume/CVD off and no sub/series panes, the normal flow below resolves
    // to `present == [Price]` at full height (chrome == 0), no special-case
    // PaneLayout construction needed. `empty` outlives the shadowed slices.
    let empty: [PaneKey; 0] = [];
    let (show_volume, cvd_on, sub_panes, series_panes): (bool, bool, &[PaneKey], &[PaneKey]) =
        if price_maximized {
            (false, false, &empty, &empty)
        } else {
            (show_volume, cvd_on, sub_panes, series_panes)
        };
    let vol_on = show_volume && style_preserves_volume(style) && !state.bars.is_empty();
    // SP2: the CVD sub-pane needs both the caller's toggle AND actual footprint data.
    let cvd_pane_on = cvd_on && has_footprint;
    // Filter the authored unified sub-pane order to those PRESENT this frame,
    // preserving the user's arrangement: Volume iff `vol_on`, Cvd iff
    // `cvd_pane_on`, a Study pane iff it still holds ≥1 currently-visible
    // non-overlay study (C1 Task 4: a hidden study must not leave a blank strip).
    // This is the ONE place Volume/CVD/Study ordering resolves — they are peers.
    let present_sub: Vec<PaneKey> = sub_panes
        .iter()
        .copied()
        .filter(|pk| match pk {
            PaneKey::Volume => vol_on,
            PaneKey::Cvd => cvd_pane_on,
            // A study pane survives while EITHER population still has something to
            // draw in it: a visible non-overlay indicator authored here, or a visible
            // non-gated microstructure study self-keyed here. `micro == &[]` ⇒ the
            // second disjunct is `false` and this is the pre-studies predicate.
            PaneKey::Study(_) => {
                indicators
                    .iter()
                    .any(|a| a.visible && !a.is_overlay() && study_pane_of.get(&a.uid) == Some(pk))
                    || micro.iter().any(|s| s.visible && !s.is_empty() && s.pane_key() == *pk)
            }
            // Price/Series never appear in the authored unified sub-pane order.
            PaneKey::Price | PaneKey::Series(_) => false,
        })
        .collect();
    let visible_study_panes: Vec<PaneKey> =
        present_sub.iter().copied().filter(|pk| matches!(pk, PaneKey::Study(_))).collect();
    let n_study_panes = visible_study_panes.len();
    // The reorderable group is exactly the present unified sub-panes.
    let n_reorderable = present_sub.len();
    // C2b: resolve each authored series pane to its compare symbol's `SeriesInput`
    // (reverse-look-up the pane's symbol in `series_pane_of`, then that symbol's live
    // input in `overlays`). Panes with no live input drop out — degrade to no pane.
    let visible_series: Vec<(PaneKey, &SeriesInput)> = series_panes
        .iter()
        .filter_map(|&pk| {
            let sym = series_pane_of.iter().find(|&(_, &p)| p == pk).map(|(s, _)| s)?;
            let si = overlays.iter().find(|s| s.symbol == sym.as_str())?;
            Some((pk, si))
        })
        .collect();
    let n_series_panes = visible_series.len();
    let visible_series_panes: Vec<PaneKey> = visible_series.iter().map(|(pk, _)| *pk).collect();
    let n_below = n_reorderable + n_series_panes;
    let axis_h = if n_below > 0 { AXIS_LABEL_H } else { 0.0 };
    let present: Vec<PaneKey> = present_panes(&present_sub, &visible_series_panes);
    // Chrome (separators + shared axis) subtracted up front so
    // `heights.sum() + chrome == avail`. Chart single-max default: there is NO
    // header ROW any more — each sub-pane's legend + control cluster are painted
    // as an OVERLAY on the plot (see `subpanes::draw_pane_overlay`), so the panes
    // reclaim the vertical space the header strips used to take. Only the
    // inter-pane separators and the shared bottom time axis remain as chrome.
    let chrome = n_below as f32 * PANE_SEP_H + axis_h;
    let avail_for_panes = (avail - chrome).max(0.0);
    let heights = panes.layout(&present, avail_for_panes, MIN_PANE_PX);
    let price_h = heights[0];
    PaneLayout {
        visible_study_panes,
        n_study_panes,
        vol_on,
        cvd_pane_on,
        visible_series,
        n_series_panes,
        n_reorderable,
        present,
        n_below,
        axis_h,
        chrome,
        avail_for_panes,
        heights,
        price_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Bar;
    use egui::Color32;

    const AVAIL: f32 = 600.0;

    fn wave(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let base = 100.0 + (i as f64 * 0.30).sin() * 6.0;
                Bar {
                    t: i as f64,
                    ot: 1_700_000_000_000 + i as i64 * 60_000,
                    o: base,
                    h: base + 2.5,
                    l: base - 2.5,
                    c: base + (i as f64 * 0.7).cos(),
                    v: 10.0 + (i % 7) as f64,
                }
            })
            .collect()
    }

    fn st(bars: Vec<Bar>) -> ChartState {
        let mut s = ChartState::default();
        let n = bars.len();
        s.bars = bars;
        s.closed_len = n;
        s.refresh_caches();
        s
    }

    /// One visible oscillator (rsi) authored into `PaneKey::Study(uid)`.
    fn osc(uid: u64, bars: &[Bar]) -> Active {
        let spec = crate::indicators::get("rsi").expect("rsi registered");
        let a = Active::new(uid, spec, bars);
        assert!(!a.is_overlay(), "rsi must be an oscillator (its own study pane)");
        a
    }

    /// A layout call with the empty-by-default series/overlay inputs — the common
    /// case; individual tests override the study / series arguments.
    #[allow(clippy::too_many_arguments)]
    fn layout<'a>(
        avail: f32,
        show_volume: bool,
        style: ChartStyle,
        state: &ChartState,
        indicators: &[Active],
        sub_panes: &[PaneKey],
        study_pane_of: &IndexMap<u64, PaneKey>,
        cvd_on: bool,
        has_footprint: bool,
        panes: &mut PaneFractions,
    ) -> PaneLayout<'a> {
        static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> =
            std::sync::OnceLock::new();
        resolve_pane_layout(
            avail,
            show_volume,
            style,
            state,
            indicators,
            sub_panes,
            study_pane_of,
            &[],
            cvd_on,
            has_footprint,
            &[],
            EMPTY_SP_OF.get_or_init(IndexMap::new),
            &[],
            panes,
            false,
        )
    }

    /// The [`layout`] twin that also feeds tick-driven microstructure studies — the
    /// only inputs that differ, so every other argument is the common minimal case
    /// (no volume/CVD/series, candles, `avail == AVAIL`).
    fn layout_micro<'a>(
        state: &ChartState,
        sub_panes: &[PaneKey],
        micro: &[ActiveStudy],
        panes: &mut PaneFractions,
    ) -> PaneLayout<'a> {
        static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> =
            std::sync::OnceLock::new();
        static EMPTY_STUDY_OF: std::sync::OnceLock<IndexMap<u64, PaneKey>> =
            std::sync::OnceLock::new();
        resolve_pane_layout(
            AVAIL,
            false,
            ChartStyle::Candles,
            state,
            &[],
            sub_panes,
            EMPTY_STUDY_OF.get_or_init(IndexMap::new),
            micro,
            false,
            false,
            &[],
            EMPTY_SP_OF.get_or_init(IndexMap::new),
            &[],
            panes,
            false,
        )
    }

    #[test]
    fn price_only_no_subpanes() {
        let s = st(wave(40));
        let mut pf = PaneFractions::default();
        let sp_of = IndexMap::new();
        let l =
            layout(AVAIL, false, ChartStyle::Candles, &s, &[], &[], &sp_of, false, false, &mut pf);
        assert_eq!(l.present, vec![PaneKey::Price]);
        assert_eq!(l.heights.len(), 1);
        assert_eq!(l.n_below, 0);
        assert_eq!(l.axis_h, 0.0);
        assert_eq!(l.chrome, 0.0);
        assert_eq!(l.avail_for_panes, AVAIL);
        assert_eq!(l.price_h, AVAIL); // price alone takes the whole budget
        assert_eq!(l.n_reorderable, 0);
    }

    #[test]
    fn with_volume() {
        let s = st(wave(40));
        let mut pf = PaneFractions::default();
        let sp_of = IndexMap::new();
        // Volume must be present in the authored unified sub-order to appear.
        let sub = [PaneKey::Volume];
        let l =
            layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, false, false, &mut pf);
        assert!(l.vol_on);
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume]);
        assert_eq!(l.heights.len(), 2);
        assert_eq!(l.n_below, 1);
        assert_eq!(l.n_reorderable, 1);
        assert_eq!(l.axis_h, AXIS_LABEL_H);
        // No header rows now (overlay legend): chrome = n_below(1)*PANE_SEP_H + axis_h
        assert_eq!(l.chrome, PANE_SEP_H + AXIS_LABEL_H);
    }

    #[test]
    fn with_volume_and_cvd() {
        let s = st(wave(40));
        let mut pf = PaneFractions::default();
        let sp_of = IndexMap::new();
        // cvd_on AND footprint present ⇒ the CVD pane resolves; both authored.
        let sub = [PaneKey::Volume, PaneKey::Cvd];
        let l =
            layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, true, true, &mut pf);
        assert!(l.cvd_pane_on);
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume, PaneKey::Cvd]);
        assert_eq!(l.heights.len(), 3);
        assert_eq!(l.n_below, 2);
        assert_eq!(l.n_reorderable, 2);
        // No header rows: chrome = 2*PANE_SEP_H + axis_h
        assert_eq!(l.chrome, 2.0 * PANE_SEP_H + AXIS_LABEL_H);
    }

    #[test]
    fn cvd_after_volume_order_is_honored() {
        // Chart single-max default: the authored order is verbatim — CVD ABOVE
        // Volume renders CVD above Volume (they are peers, no forced sequence).
        let s = st(wave(40));
        let mut pf = PaneFractions::default();
        let sp_of = IndexMap::new();
        let sub = [PaneKey::Cvd, PaneKey::Volume];
        let l =
            layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, true, true, &mut pf);
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Cvd, PaneKey::Volume]);
    }

    #[test]
    fn cvd_needs_footprint_data() {
        // cvd_on but NO footprint ⇒ no CVD pane (default-off gate).
        let s = st(wave(40));
        let mut pf = PaneFractions::default();
        let sp_of = IndexMap::new();
        let sub = [PaneKey::Volume, PaneKey::Cvd];
        let l =
            layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, true, false, &mut pf);
        assert!(!l.cvd_pane_on, "cvd_on alone must not produce a pane without footprint data");
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume]);
    }

    #[test]
    fn one_study_pane() {
        let s = st(wave(60));
        let ind = [osc(1, &s.bars)];
        let sub = [PaneKey::Volume, PaneKey::Study(1)];
        let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
        sp_of.insert(1, PaneKey::Study(1));
        let mut pf = PaneFractions::default();
        let l =
            layout(AVAIL, true, ChartStyle::Candles, &s, &ind, &sub, &sp_of, false, false, &mut pf);
        assert_eq!(l.visible_study_panes, vec![PaneKey::Study(1)]);
        assert_eq!(l.n_study_panes, 1);
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)]);
        assert_eq!(l.heights.len(), 3);
        assert_eq!(l.n_below, 2); // volume + study
        assert_eq!(l.n_reorderable, 2);
        // No header rows: chrome = n_below(2)*PANE_SEP_H + axis_h
        assert_eq!(l.chrome, 2.0 * PANE_SEP_H + AXIS_LABEL_H);
    }

    #[test]
    fn hidden_study_leaves_no_pane() {
        // A study whose indicator is NOT visible must be filtered out (no blank strip).
        let s = st(wave(60));
        let mut ind = [osc(1, &s.bars)];
        ind[0].visible = false;
        let sub = [PaneKey::Study(1)];
        let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
        sp_of.insert(1, PaneKey::Study(1));
        let mut pf = PaneFractions::default();
        let l = layout(
            AVAIL,
            false,
            ChartStyle::Candles,
            &s,
            &ind,
            &sub,
            &sp_of,
            false,
            false,
            &mut pf,
        );
        assert!(l.visible_study_panes.is_empty(), "hidden study ⇒ its pane is dropped");
        assert_eq!(l.n_study_panes, 0);
        assert_eq!(l.present, vec![PaneKey::Price]);
    }

    #[test]
    fn renko_suppresses_volume() {
        // Renko reindexes ⇒ style_preserves_volume(Renko) == false ⇒ no volume pane
        // even with show_volume = true.
        let s = st(wave(60));
        let mut pf = PaneFractions::default();
        let sp_of = IndexMap::new();
        let sub = [PaneKey::Volume];
        let l =
            layout(AVAIL, true, ChartStyle::Renko, &s, &[], &sub, &sp_of, false, false, &mut pf);
        assert!(!l.vol_on, "Renko volume is suppressed");
        assert_eq!(l.present, vec![PaneKey::Price]);
        assert_eq!(l.n_below, 0);
    }

    #[test]
    fn one_series_pane_resolves_from_overlays() {
        // A compare symbol moved into its own pane: series_panes + series_pane_of +
        // a matching overlay ⇒ visible_series resolves and the pane appears last.
        let s = st(wave(40));
        let other = st(wave(20));
        let overlays = [SeriesInput { symbol: "ETHUSDT", state: &other, color: Color32::RED }];
        let mut sp_of_series: IndexMap<String, PaneKey> = IndexMap::new();
        sp_of_series.insert("ETHUSDT".to_string(), PaneKey::Series(7));
        let series_panes = [PaneKey::Series(7)];
        let empty_study: IndexMap<u64, PaneKey> = IndexMap::new();
        let mut pf = PaneFractions::default();
        let l = resolve_pane_layout(
            AVAIL,
            false, // no volume, to isolate the series-pane bookkeeping
            ChartStyle::Candles,
            &s,
            &[],
            &[],
            &empty_study,
            &[],
            false,
            false,
            &series_panes,
            &sp_of_series,
            &overlays,
            &mut pf,
            false,
        );
        assert_eq!(l.n_series_panes, 1);
        assert_eq!(l.visible_series.len(), 1);
        assert_eq!(l.visible_series[0].0, PaneKey::Series(7));
        assert_eq!(l.visible_series[0].1.symbol, "ETHUSDT");
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Series(7)]);
        assert_eq!(l.heights.len(), 2);
        assert_eq!(l.n_below, 1);
        // No volume/cvd/study ⇒ zero reorderable sub-panes; the series pane is
        // present at index 1 of `present`.
        assert_eq!(l.n_reorderable, 0);
        // No header rows: chrome = n_below(1)*PANE_SEP_H + axis_h
        assert_eq!(l.chrome, PANE_SEP_H + AXIS_LABEL_H);
    }

    #[test]
    fn price_maximized_suppresses_every_subpane() {
        // Feature #1b: with price_maximized set, volume + every study pane are
        // hidden and the price pane is the sole present pane at full height —
        // even though show_volume is ON and a study pane exists.
        let s = st(wave(40));
        let ind = [osc(1, &s.bars)];
        let sub = [PaneKey::Volume, PaneKey::Study(1)];
        let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
        sp_of.insert(1, PaneKey::Study(1));
        let empty_series: IndexMap<String, PaneKey> = IndexMap::new();
        let mut pf = PaneFractions::default();
        let l = resolve_pane_layout(
            AVAIL,
            true, // volume ON — maximize must override it
            ChartStyle::Candles,
            &s,
            &ind,
            &sub,
            &sp_of,
            &[],
            false,
            false,
            &[],
            &empty_series,
            &[],
            &mut pf,
            true, // price_maximized
        );
        assert_eq!(l.present, vec![PaneKey::Price]);
        assert_eq!(l.n_study_panes, 0);
        assert!(!l.vol_on);
        assert_eq!(l.n_below, 0);
        assert_eq!(l.heights.len(), 1);
        assert_eq!(l.chrome, 0.0); // no headers / separators / axis when maximized
    }

    #[test]
    fn heights_sum_plus_chrome_equals_avail() {
        // The layout invariant across a rich pane set: study + volume + cvd.
        let s = st(wave(60));
        let ind = [osc(1, &s.bars)];
        let sub = [PaneKey::Volume, PaneKey::Cvd, PaneKey::Study(1)];
        let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
        sp_of.insert(1, PaneKey::Study(1));
        let mut pf = PaneFractions::default();
        let l =
            layout(AVAIL, true, ChartStyle::Candles, &s, &ind, &sub, &sp_of, true, true, &mut pf);
        assert_eq!(l.present.len(), 4); // Price, Volume, Cvd, Study(1)
        let sum: f32 = l.heights.iter().sum();
        assert!(
            (sum + l.chrome - AVAIL).abs() < 0.01,
            "heights.sum({sum}) + chrome({}) != {AVAIL}",
            l.chrome
        );
    }

    // ---- tick-driven microstructure studies (the `micro` population) ----

    /// A VPIN study (trades-only, never book-gated) with one committed sample, so it
    /// is visible AND non-empty — the "this pane has something to draw" case.
    fn vpin(uid: u64) -> ActiveStudy {
        let spec = crate::studies::get_study("vpin").expect("vpin registered");
        // bucket volume 10 / window 2 so a single trade commits a bucket.
        let mut s = ActiveStudy::with_params(uid, spec, vec![10.0, 2.0, 1.0]);
        s.on_trade(
            &vike_model::TradeTick {
                ts: 1,
                local_ts: 0,
                price: 100.0,
                size: 10.0,
                is_buyer_maker: false,
                symbol: String::new(),
            },
            None,
        );
        s
    }

    #[test]
    fn no_studies_is_the_pre_studies_layout() {
        // The byte-identical guarantee, asserted directly: `micro == &[]` resolves the
        // SAME layout as the indicator-only path, including that a Study pane with no
        // indicator behind it stays absent.
        let s = st(wave(40));
        let sub = [PaneKey::Study(9)];
        let mut pf = PaneFractions::default();
        let l = layout_micro(&s, &sub, &[], &mut pf);
        assert_eq!(l.present, vec![PaneKey::Price]);
        assert!(l.visible_study_panes.is_empty());
        assert_eq!(l.n_below, 0);
    }

    #[test]
    fn a_microstructure_study_keeps_its_pane_present() {
        let s = st(wave(40));
        let micro = [vpin(9)];
        let sub = [PaneKey::Study(9)];
        let mut pf = PaneFractions::default();
        let l = layout_micro(&s, &sub, &micro, &mut pf);
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Study(9)]);
        assert_eq!(l.visible_study_panes, vec![PaneKey::Study(9)]);
        assert_eq!(l.n_study_panes, 1);
        assert_eq!(l.n_below, 1);
    }

    #[test]
    fn hidden_microstructure_study_leaves_no_pane() {
        let s = st(wave(40));
        let mut micro = [vpin(9)];
        micro[0].visible = false;
        let sub = [PaneKey::Study(9)];
        let mut pf = PaneFractions::default();
        let l = layout_micro(&s, &sub, &micro, &mut pf);
        assert_eq!(l.present, vec![PaneKey::Price], "a hidden study must not leave a blank strip");
    }

    #[test]
    fn book_gated_study_with_no_l2_feed_leaves_no_pane() {
        // `book_imbalance` needs a book; none was fed, so `is_empty()` holds and the
        // pane must not open at all (rather than opening on a fabricated flat line).
        let s = st(wave(40));
        let spec = crate::studies::get_study("book_imbalance").expect("registered");
        let micro = [ActiveStudy::new(9, spec)];
        assert!(micro[0].is_empty());
        let sub = [PaneKey::Study(9)];
        let mut pf = PaneFractions::default();
        let l = layout_micro(&s, &sub, &micro, &mut pf);
        assert_eq!(l.present, vec![PaneKey::Price]);
    }

    #[test]
    fn an_indicator_and_a_study_can_share_one_pane() {
        // Merged pane: the indicator predicate alone would already keep it present, so
        // this pins that adding the study population does not disturb that resolution.
        let s = st(wave(60));
        let ind = [osc(1, &s.bars)];
        let micro = [vpin(2)];
        let sub = [PaneKey::Study(1)];
        let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
        sp_of.insert(1, PaneKey::Study(1));
        let mut pf = PaneFractions::default();
        static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> =
            std::sync::OnceLock::new();
        let l = resolve_pane_layout(
            AVAIL,
            false,
            ChartStyle::Candles,
            &s,
            &ind,
            &sub,
            &sp_of,
            &micro,
            false,
            false,
            &[],
            EMPTY_SP_OF.get_or_init(IndexMap::new),
            &[],
            &mut pf,
            false,
        );
        assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Study(1)]);
        assert_eq!(l.n_study_panes, 1);
    }

    #[test]
    fn price_maximize_hides_study_panes_too() {
        let s = st(wave(40));
        let micro = [vpin(9)];
        let sub = [PaneKey::Study(9)];
        let mut pf = PaneFractions::default();
        static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> =
            std::sync::OnceLock::new();
        static EMPTY_STUDY_OF: std::sync::OnceLock<IndexMap<u64, PaneKey>> =
            std::sync::OnceLock::new();
        let l = resolve_pane_layout(
            AVAIL,
            false,
            ChartStyle::Candles,
            &s,
            &[],
            &sub,
            EMPTY_STUDY_OF.get_or_init(IndexMap::new),
            &micro,
            false,
            false,
            &[],
            EMPTY_SP_OF.get_or_init(IndexMap::new),
            &[],
            &mut pf,
            true,
        );
        assert_eq!(l.present, vec![PaneKey::Price]);
    }
}
