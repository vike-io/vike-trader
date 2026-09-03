//! The four sub-pane render loops split out of `chart::draw` (chart refactor PR-6):
//! volume, CVD, study (oscillator), and compare-series sub-panes. Each function body
//! is the former inline loop in `draw()`, moved VERBATIM — byte-identical render,
//! including every `set_plot_bounds` call inside a `.show()` closure. The ~13 shared
//! read-only locals `draw` resolves once (the pane layout indices/heights + the shared
//! x-axis ctx) are bundled into the Copy [`SubPaneCtx`]; the per-loop mutated outputs
//! (`panes`, `bottom_frame`, plus `cvd_toggle`/`remove_uid`/`move_study` for their own
//! panes) pass through as `&mut` parameters, exactly mirroring how `draw` used to mutate
//! its own locals in place. See `chart/mod.rs`'s `draw`, which still owns the `if
//! vol_on`/`if cvd_pane_on` gates, the `cx`/`cvd_toggle`/`remove_uid`/`move_study`
//! declarations, and the four call sites (in the unchanged volume -> cvd -> study ->
//! series order).

use super::consts::{CVD_COLOR, MIN_PANE_PX};
use super::extent::y_pad;
use super::fmt::fmt_compact;
use super::marks::{attach_shared_x_axis, attach_shared_x_grid};
use super::overlay::paint_value_tag;
use super::panes_layout::{pane_separator, sub_pane_plot};
use crate::chart::SeriesInput;
use crate::indicators::Active;
use crate::interact::visible_slice;
use crate::model::{Bar, ChartState};
use crate::options::{ChartOptions, IndicatorDialog};
use crate::panes::{MoveTarget, PaneFractions, PaneKey};
use crate::render::{
    overlay_visible_range, reindex_by_ot, render_oscillator, render_study, seg_line,
};
use crate::studies::ActiveStudy;
use egui::{Align, Color32, Layout, RichText};
use egui_plot::HLine;
use indexmap::IndexMap;
use vike_orderflow::FootprintBar;
use vike_ui_theme::palette as pal;

/// Everything the four sub-pane loops read but never mutate: the price pane's resolved
/// x-range, the resolved [`crate::chart::panes_layout::PaneLayout`] heights/indices, the
/// shared grid/axis cosmetics, and the shared x-axis ctx (PR-3's `XAxisCtx`). All fields
/// are `Copy` (slices + scalars), so `draw` builds this ONCE (right after
/// `resolve_pane_layout`) and every call site below passes it by shared reference.
#[derive(Clone, Copy)]
pub(crate) struct SubPaneCtx<'a> {
    pub px0: f64,
    pub px1: f64,
    pub heights: &'a [f32],
    pub axis_h: f32,
    pub grid_color: egui::Color32,
    /// Per-axis grid visibility (TV vertical/horizontal split), shared with the
    /// price pane so a hidden vertical/horizontal grid stays hidden chart-wide.
    pub grid_show: egui::Vec2b,
    pub sec_axis_active: bool,
    pub xcx: crate::chart::marks::XAxisCtx<'a>,
    pub present: &'a [PaneKey],
    pub avail_for_panes: f32,
    /// Count of PRESENT reorderable sub-panes (Volume + CVD + study panes). The
    /// ↑/↓ reorder controls clamp against this: a sub-pane sits at some `pos` in
    /// `1..=n_reorderable` (they are contiguous right after Price), so "↑" is
    /// enabled while `pos > 1` and "↓" while `pos < n_reorderable`.
    pub n_reorderable: usize,
}

/// The hover-gated pane ↑/↓ reorder controls shared by every reorderable
/// sub-pane header (Volume, CVD, study — peers now). Drawn inside a
/// right-to-left layout (↓ then ↑, so ↑ lands leftmost, matching the study
/// cluster); each is disabled at the reorderable group's ends. Emits the picked
/// move into `reorder_pane` (fire-once, harvested as `ChartActions::reorder_pane`).
fn pane_reorder_controls(
    ui: &mut egui::Ui,
    pane_key: PaneKey,
    pos: usize,
    n_reorderable: usize,
    reorder_pane: &mut Option<(PaneKey, bool)>,
) {
    ui.add_enabled_ui(pos < n_reorderable, |ui| {
        if ui.small_button("↓").on_hover_text("Move pane down").clicked() {
            *reorder_pane = Some((pane_key, false));
        }
    });
    ui.add_enabled_ui(pos > 1, |ui| {
        if ui.small_button("↑").on_hover_text("Move pane up").clicked() {
            *reorder_pane = Some((pane_key, true));
        }
    });
}

/// Vertical pitch for a merged pane's stacked per-study legends (TradingView-
/// style, painted ON the plot rather than in a header ROW).
const PANE_OVERLAY_ROW_H: f32 = 18.0;

/// TradingView-style overlaid pane header (chart single-max default): there is
/// NO header ROW above the plot any more. The pane NAME is painted at the plot's
/// top-left (ALWAYS visible; a `dnd` drag SOURCE when `drag` is `Some(uid)` — the
/// feature-#2 study-relocate grab), and the control cluster at the plot's
/// top-right, shown ONLY while the pointer is inside `plot_rect` (the EXACT-rect
/// hover gate the volume ✕ pioneered — never an estimated rect). `row` stacks a
/// merged pane's per-study legends (0 = topmost). `add_cluster` fills the right
/// cluster in a `right_to_left` layout, so callers add buttons in visual
/// RIGHT→LEFT order (✕ first) and they read left→right as `↑ ↓ … ✕`; placement is
/// thus identical on every pane.
///
/// Drawn through [`egui::Ui::new_child`] (NOT `put`/`scope_builder`), so the
/// PARENT cursor is deliberately NOT advanced: the plot already moved it past its
/// own bottom, and the following separator / next pane MUST land there. Advancing
/// to this overlay's (plot-top) rect would drag the cursor back UP and overlap the
/// panes — the reason the pre-overlay ✕ was safe only on the bottom pane. The
/// child uis are `id_salt`ed by `pane_key`/`row`, so their ids stay stable no
/// matter which panes were hovered earlier this frame.
#[allow(clippy::too_many_arguments)]
fn draw_pane_overlay(
    ui: &mut egui::Ui,
    plot_rect: egui::Rect,
    pane_key: PaneKey,
    row: usize,
    name: &str,
    name_color: Color32,
    drag: Option<u64>,
    add_cluster: impl FnOnce(&mut egui::Ui),
) {
    let y = plot_rect.top() + 2.0 + row as f32 * PANE_OVERLAY_ROW_H;
    let name_text = RichText::new(name)
        .size(13.0)
        .family(egui::FontFamily::Name("semibold".into()))
        .color(name_color);
    // NAME at top-left (always visible). Bounded to the left ~55% of the plot
    // width so a long legend never runs under the right cluster.
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(plot_rect.left() + 6.0, y),
        egui::vec2((plot_rect.width() * 0.55).max(0.0), PANE_OVERLAY_ROW_H),
    );
    let mut lui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(name_rect)
            .id_salt(("vike_pane_legend", pane_key, row))
            .layout(Layout::left_to_right(Align::Center)),
    );
    match drag {
        Some(uid) => {
            lui.dnd_drag_source(
                egui::Id::new(("vike_study_drag", uid)),
                DragStudy { uid },
                |lui| {
                    lui.label(name_text);
                },
            );
        }
        None => {
            lui.label(name_text);
        }
    }
    // CONTROL CLUSTER at top-right — hover-gated on the EXACT plot rect (an
    // explicit pointer test, NOT the plot response's sense, which is empty
    // because the sub-pane plots disable drag/zoom/scroll).
    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|pt| plot_rect.contains(pt));
    // ALWAYS allocate the cluster child ui (stable id_salt) so its id is present
    // in BOTH of egui's layout passes — otherwise a hover state that differs
    // across passes toggles the widget in/out and egui flags the id change with a
    // red "changed id between passes" box. Only the BUTTONS are hover-gated.
    let cl_rect = egui::Rect::from_min_max(
        egui::pos2(name_rect.right(), y),
        egui::pos2(plot_rect.right() - 6.0, y + PANE_OVERLAY_ROW_H),
    );
    let mut cui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(cl_rect)
            .id_salt(("vike_pane_cluster", pane_key, row))
            .layout(Layout::right_to_left(Align::Center)),
    );
    if hovered {
        add_cluster(&mut cui);
    }
}

/// Volume sub-pane (x-linked, directly under the price plot — TV layout). Uses the RAW
/// bars: every volume-preserving style is index-aligned to them. Was `draw()`'s inline
/// `if vol_on { .. }` body; the caller still owns the `if vol_on` gate (see `chart/mod.rs`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_volume_pane(
    ui: &mut egui::Ui,
    cx: &SubPaneCtx,
    panes: &mut PaneFractions,
    bottom_frame: &mut egui::Rect,
    // Position-based indexing (chart single-max default): Volume's index in
    // `present` — dispatched from `chart::draw`, so it works wherever the
    // authored order places it.
    pos: usize,
    state: &ChartState,
    up_s_col: egui::Color32,
    down_s_col: egui::Color32,
    // Volume-as-indicator: set true when the header ✕ is clicked this frame so
    // the caller can turn off `ChartOptions::show_volume` (fire-once, mirrors
    // `cvd_toggle`). Lets Volume be removed like a normal indicator pane.
    volume_remove: &mut bool,
    // Feature #1 (TradingView parity): pane ↑/↓ reorder, harvested into
    // `ChartActions::reorder_pane` — Volume is now a reorderable peer.
    reorder_pane: &mut Option<(PaneKey, bool)>,
) {
    // `is_bottom` (carries the shared time axis) is simply "last pane".
    let is_bottom = pos == cx.present.len() - 1;
    let vol_h = cx.heights[pos];
    let mut p = sub_pane_plot(
        "volume",
        vol_h,
        cx.axis_h,
        is_bottom,
        cx.grid_color,
        cx.grid_show,
        cx.sec_axis_active,
        fmt_compact,
    );
    if is_bottom {
        p = attach_shared_x_axis(p, cx.xcx);
    } else {
        // Grid-alignment: middle panes share the price pane's grid marks (no labels).
        p = attach_shared_x_grid(p, cx.xcx);
    }
    let vresp = p.show(ui, |pui| {
        // slave x to the price pane; y = 0..visible max volume
        let vis = visible_slice(&state.bars, cx.px0, cx.px1);
        // chart-perf T5: cached CLOSED-bar max (`ChartState::visible_vol_max`) instead
        // of a per-frame refold; `.max()`'d with the forming bar's volume when it's
        // visible — see the VolumeCandles arm above for the identical pattern/rationale.
        let (vol_lo, vol_hi) = match (vis.first(), vis.last()) {
            (Some(f), Some(l)) => (f.t as usize, l.t as usize + 1),
            _ => (0, 0),
        };
        let forming_idx = state.closed_len;
        let forming_v = if state.bars.len() > forming_idx && (vol_lo..vol_hi).contains(&forming_idx)
        {
            state.bars[forming_idx].v
        } else {
            0.0
        };
        let vmax = state.visible_vol_max_shared(vol_lo, vol_hi).max(forming_v).max(1e-9);
        pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
            [cx.px0, 0.0],
            [cx.px1, vmax * 1.08],
        ));
        let vbars: Vec<egui_plot::Bar> = vis
            .iter()
            .map(|bb| {
                // direction-colored at ~70% alpha, vike histogram palette
                let col = if bb.c >= bb.o { up_s_col } else { down_s_col };
                egui_plot::Bar::new(bb.t, bb.v).fill(col.gamma_multiply(0.7)).width(0.8)
            })
            .collect();
        if !vbars.is_empty() {
            pui.bar_chart(egui_plot::BarChart::new("", vbars));
        }
    });
    // TradingView-style overlay (chart single-max default): "Volume" legend at
    // the plot's top-left (always visible) + the `↑ ↓ ✕` cluster at the top-right
    // (hover-gated). No ⋯/⚙ — Volume has no move-to targets or settings dialog.
    draw_pane_overlay(
        ui,
        vresp.response.rect,
        PaneKey::Volume,
        0,
        "Volume",
        pal::TEXT2,
        None,
        |cui| {
            if cui.small_button("✕").on_hover_text("Remove volume pane").clicked() {
                *volume_remove = true;
            }
            pane_reorder_controls(cui, PaneKey::Volume, pos, cx.n_reorderable, reorder_pane);
        },
    );
    // TradingView-parity right-edge value tag (optional per the brief — trivial with
    // the shared helper, so included): the LAST bar's volume, coloured by that bar's
    // candle direction (up/down, matching the bars), compact-formatted like the volume
    // y-axis. No bars ⇒ `None` ⇒ no tag.
    let last = state.bars.last();
    paint_value_tag(
        ui,
        vresp.response.rect,
        &vresp.transform,
        last.map(|b| b.v),
        last.map_or(up_s_col, |b| if b.c >= b.o { up_s_col } else { down_s_col }),
        fmt_compact,
    );
    if is_bottom {
        // T4: the volume pane is bottom-most this frame — its frame carries
        // the shared time axis, so the x-drag gutter belongs to IT, not price.
        *bottom_frame = *vresp.transform.frame();
    }
    // T9: the separator that FOLLOWS this pane (boundary `pos`) — only when
    // there IS a next pane; when Volume is already the last pane, none follows.
    if !is_bottom {
        pane_separator(ui, panes, cx.present, pos, cx.avail_for_panes, MIN_PANE_PX);
    }
}

/// CVD sub-pane (SP2): cumulative volume delta derived from the footprint
/// substrate (`orderflow::cvd_from_footprints`), directly below Volume —
/// see the `present` build in `resolve_pane_layout` for the placement rationale. SP2 v1
/// recomputed it fresh every frame (O(bars)); SP3 Task B #2 (SP2 final-review
/// finding B) caches it on `state` (`ChartState::cvd_shared`), keyed by
/// `footprint_gen` (SP3 TB-fix — a monotonic generation, not the footprint
/// slice's own address, see `model.rs`'s `CvdCacheKey` doc) — an unchanged
/// frame (the common case) is now an O(1) `Rc` clone instead of re-folding
/// every bar's cells. Was `draw()`'s inline `if cvd_pane_on { .. }` body; the caller
/// still owns the `if cvd_pane_on` gate and the `let mut cvd_toggle = false;` declaration
/// (see `chart/mod.rs`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_cvd_pane(
    ui: &mut egui::Ui,
    cx: &SubPaneCtx,
    panes: &mut PaneFractions,
    bottom_frame: &mut egui::Rect,
    // Position-based indexing: CVD's index in `present` — dispatched from `chart::draw`.
    pos: usize,
    state: &ChartState,
    footprint: Option<&[FootprintBar]>,
    footprint_gen: u64,
    cvd_toggle: &mut bool,
    // Feature #1 (TradingView parity): pane ↑/↓ reorder, harvested into
    // `ChartActions::reorder_pane` — CVD is now a reorderable peer.
    reorder_pane: &mut Option<(PaneKey, bool)>,
) {
    let is_bottom = pos == cx.present.len() - 1;
    let cvd_h = cx.heights[pos];
    let cvd = state.cvd_shared(footprint.unwrap_or(&[]), footprint_gen);
    let mut p = sub_pane_plot(
        "cvd",
        cvd_h,
        cx.axis_h,
        is_bottom,
        cx.grid_color,
        cx.grid_show,
        cx.sec_axis_active,
        fmt_compact,
    );
    if is_bottom {
        p = attach_shared_x_axis(p, cx.xcx);
    } else {
        // Grid-alignment: middle panes share the price pane's grid marks (no labels).
        p = attach_shared_x_grid(p, cx.xcx);
    }
    let cresp = p.show(ui, |pui| {
        // x slaved to the price pane; y-autofit over the VISIBLE slice,
        // always including 0.0 so the zero line is never scrolled off.
        let i0 = cx.px0.max(0.0) as usize;
        let take = (cx.px1.max(0.0).ceil() as usize).saturating_sub(i0) + 1;
        let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
        for v in cvd.iter().skip(i0).take(take) {
            if !v.is_nan() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        // Sub-panes keep the fixed 5% pad — TV's Canvas "Margins" apply to the price scale only.
        let (flo, fhi) = y_pad(lo, hi, 5.0, 5.0);
        pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max([cx.px0, flo], [cx.px1, fhi]));
        pui.hline(
            HLine::new("", 0.0)
                .color(Color32::from_gray(80))
                .width(1.0)
                .style(egui_plot::LineStyle::dashed_loose())
                .allow_hover(false),
        );
        seg_line(pui, &cvd, CVD_COLOR, 1.5, egui_plot::LineStyle::Solid, cx.px0, cx.px1, 0);
    });
    // TradingView-style overlay: "CVD" legend top-left + `↑ ↓ ✕` cluster
    // top-right (hover-gated). No ⋯/⚙ — CVD has no move-to targets or settings.
    draw_pane_overlay(ui, cresp.response.rect, PaneKey::Cvd, 0, "CVD", pal::TEXT2, None, |cui| {
        if cui.small_button("✕").on_hover_text("Remove CVD pane").clicked() {
            *cvd_toggle = true;
        }
        pane_reorder_controls(cui, PaneKey::Cvd, pos, cx.n_reorderable, reorder_pane);
    });
    // TradingView-parity right-edge value tag: the CURRENT CVD reading, in the CVD
    // line colour, compact-formatted like the CVD y-axis. Empty/NaN ⇒ no tag.
    paint_value_tag(
        ui,
        cresp.response.rect,
        &cresp.transform,
        cvd.last().copied(),
        CVD_COLOR,
        fmt_compact,
    );
    if is_bottom {
        // this CVD pane is bottom-most this frame — its frame carries the
        // shared time axis, so the x-drag gutter belongs to IT.
        *bottom_frame = *cresp.transform.frame();
    }
    // T9: the separator that FOLLOWS this pane (boundary `pos`) — only when
    // there IS a next pane below CVD.
    if !is_bottom {
        pane_separator(ui, panes, cx.present, pos, cx.avail_for_panes, MIN_PANE_PX);
    }
}

/// Study sub-panes (x-linked to the price plot), each with a header (name +
/// ✕ + ⚙) per study. C1 Task 3: iterate the AUTHORED study panes, not the
/// flat oscillator list — a pane holds one OR MORE studies (`study_pane_of`
/// groups them), all rendered into it under ONE combined y-fit. On the
/// default assignment each pane holds exactly one study, so the header loop
/// runs once, the y-fit fold reduces to the single-oscillator fold, and the
/// boundary bookkeeping is unchanged — byte-identical to the pre-C1 loop.
/// Was `draw()`'s inline `for (pi, &pane_key) in visible_study_panes...` loop; the caller
/// still owns the `remove_uid`/`move_study` declarations (see `chart/mod.rs`).
///
/// **The `opts` shadow** (chart refactor PR-6): the ••• "Move to" menu builds a LOCAL
/// `let mut opts: Vec<(String, MoveTarget)>` inside the header closure below — that local
/// shadows this fn's own `opts: &ChartOptions` parameter for the header closure's scope
/// only. Once that closure returns, `opts` reverts to the fn parameter, so
/// `render_oscillator(pui, a, cx.px0, cx.px1, opts)` inside the LATER `.show()` closure
/// resolves to the `&ChartOptions` parameter — passed bare (not `&opts`) because the
/// parameter is already a reference (the pre-PR-6 call site read `&opts` against an owned
/// `ChartOptions` local; that local is now this reference parameter).
/// Feature #2 (TradingView parity): the drag-and-drop payload carried by a study
/// legend. Dragging a study's name label (a `dnd_drag_source`) and releasing it
/// over another pane's header (`dnd_drop_zone`) merges the study into that pane —
/// the same relocation the ⋯ "Move to pane N" menu performs, via `move_study`.
#[derive(Clone)]
struct DragStudy {
    uid: u64,
}

/// ONE study sub-pane (chart single-max default): the former `draw_study_panes`
/// loop body, extracted to a single-pane fn so `chart::draw` can dispatch it by
/// position in the authored order (studies, volume, and CVD are now peers). `pos`
/// is this pane's index in `cx.present`. Body is otherwise verbatim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_one_study_pane(
    ui: &mut egui::Ui,
    cx: &SubPaneCtx,
    panes: &mut PaneFractions,
    bottom_frame: &mut egui::Rect,
    pos: usize,
    pane_key: PaneKey,
    visible_study_panes: &[PaneKey],
    indicators: &[Active],
    study_pane_of: &IndexMap<u64, PaneKey>,
    // Tick-driven microstructure studies (`ChartInputs::studies`) — the SECOND
    // population this pane can hold, self-keyed by `ActiveStudy::pane_key()` rather
    // than through `study_pane_of`. `&[]` (the default) makes every use of it below a
    // no-op over an empty iterator, so the indicator render is byte-identical.
    micro: &[ActiveStudy],
    opts: &ChartOptions,
    indicator_dialog: &mut IndicatorDialog,
    remove_uid: &mut Option<u64>,
    move_study: &mut Option<(u64, MoveTarget)>,
    // Feature #1 (TradingView parity): pane ↑/↓ reorder. `(pane, up)` — the
    // pane the user asked to move and the direction; harvested into
    // `ChartActions::reorder_pane` like `move_study`.
    reorder_pane: &mut Option<(PaneKey, bool)>,
) {
    {
        // Every visible non-overlay indicator authored into THIS pane. Overlays
        // never appear in `study_pane_of` (they render on the price pane), so
        // the `!is_overlay()` guard is belt-and-braces. `visible_study_panes`
        // pre-filters panes with no visible study, so `studies` is non-empty
        // here UNLESS the pane is held open purely by a microstructure study
        // (`micro_here` below); on the default assignment it is exactly one study.
        let studies: Vec<&Active> = indicators
            .iter()
            .filter(|a| {
                a.visible && !a.is_overlay() && study_pane_of.get(&a.uid) == Some(&pane_key)
            })
            .collect();
        // Every visible, non-gated microstructure study self-keyed to THIS pane. A
        // book-dependent study with no L2 feed reports `is_empty()` and is skipped
        // here exactly as `resolve_pane_layout` skipped it when gating the pane — no
        // pane ever renders a fabricated study line. Empty on the default layout.
        let micro_here: Vec<&ActiveStudy> = micro
            .iter()
            .filter(|s| s.visible && !s.is_empty() && s.pane_key() == pane_key)
            .collect();
        // Position-based indexing: this pane's index in `present` drives its
        // height, its separator boundary, and whether it's the bottom-most pane
        // (carries the shared time axis).
        let is_last = pos == cx.present.len() - 1;
        let this_osc_h = cx.heights[pos];
        // Plot id is PANE-keyed (stable per pane), not per-study-uid — a pane may
        // hold several studies. It feeds only egui's per-widget memory key; the
        // plot bounds are re-asserted inside the closure every frame, so the id
        // string never affects the rendered pixels (byte-identical output). On the
        // default one-study pane this is `pane_Study(<uid>)` where the old id was
        // `osc_<uid>` — a different memory key, same render.
        let mut p = sub_pane_plot(
            format!("pane_{pane_key:?}"),
            this_osc_h,
            cx.axis_h,
            is_last,
            cx.grid_color,
            cx.grid_show,
            cx.sec_axis_active,
            |v| format!("{:.1}", v),
        );
        if is_last {
            p = attach_shared_x_axis(p, cx.xcx);
        } else {
            // Grid-alignment: middle panes share the price grid marks (no labels).
            p = attach_shared_x_grid(p, cx.xcx);
        }
        let oresp = p.show(ui, |pui| {
            // slave x to the price pane; y = COMBINED fit over EVERY study in the
            // pane — each study's band levels + its output series over the visible
            // slice. min/max are order-independent (commutative/associative,
            // NaN-filtered), so folding all studies gives the same bounds whatever
            // their order; with one study this reduces EXACTLY to the pre-C1
            // single-oscillator fold (bands, then outputs). `i0`/`take` are
            // loop-invariant (functions of px0/px1 only), hoisted above the fold.
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            let i0 = cx.px0.max(0.0) as usize;
            let take = (cx.px1.max(0.0).ceil() as usize).saturating_sub(i0) + 1;
            for a in &studies {
                // Hidden bands/plots (T8 Style tab) must not expand the pane's y-autofit.
                // Per-level `show` gates each band (RSI "Style" per-level parity).
                if a.show_bands {
                    for band in &a.bands {
                        if band.show {
                            lo = lo.min(band.value);
                            hi = hi.max(band.value);
                        }
                    }
                }
                for line in &a.outputs {
                    if !line.visible {
                        continue;
                    }
                    for v in line.series.iter().skip(i0).take(take) {
                        if !v.is_nan() {
                            lo = lo.min(*v);
                            hi = hi.max(*v);
                        }
                    }
                }
            }
            // Microstructure studies fold into the SAME combined fit, by the same
            // rule (visible bands + visible output lines over the visible slice).
            // min/max are commutative/associative, so folding this population after
            // the indicators gives the identical bounds whatever the order — and
            // with `micro_here` empty this loop contributes nothing at all.
            for s in &micro_here {
                if s.show_bands {
                    for band in &s.bands {
                        if band.show {
                            lo = lo.min(band.value);
                            hi = hi.max(band.value);
                        }
                    }
                }
                for line in &s.outputs {
                    if !line.visible {
                        continue;
                    }
                    for v in line.series.iter().skip(i0).take(take) {
                        if !v.is_nan() {
                            lo = lo.min(*v);
                            hi = hi.max(*v);
                        }
                    }
                }
            }
            if lo.is_finite() && hi.is_finite() {
                let (flo, fhi) = y_pad(lo, hi, 5.0, 5.0);
                pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                    [cx.px0, flo],
                    [cx.px1, fhi],
                ));
            }
            for a in &studies {
                render_oscillator(pui, a, cx.px0, cx.px1, opts);
            }
            // Studies paint AFTER the indicators (same z-order rule as within either
            // population: authored order, later on top), through the shared
            // `render_oscillator_parts` body — a study line is machinery-identical to
            // an indicator line.
            for s in &micro_here {
                render_study(pui, s, cx.px0, cx.px1, opts);
            }
        });
        let plot_rect = oresp.response.rect;
        // TradingView-style overlay (chart single-max default): NO header ROW.
        // One legend per study is painted at the plot's top-left (stacked when a
        // merge left >1 study in this pane — matches the old one-header-row-per-
        // study behavior), each a `dnd` drag SOURCE; the `↑ ↓ ⋯ ⚙ ✕` cluster is
        // painted at the plot's top-right, hover-gated on the EXACT plot rect. The
        // pane-level ↑/↓ ride the FIRST study's row only (`si == 0`); every
        // study keeps its own ⋯/⚙/✕.
        for (si, a) in studies.iter().enumerate() {
            draw_pane_overlay(
                ui,
                plot_rect,
                pane_key,
                si,
                a.spec.pretty,
                pal::TEXT2,
                Some(a.uid),
                |cui| {
                    if cui.small_button("✕").on_hover_text("Remove indicator").clicked() {
                        *remove_uid = Some(a.uid);
                    }
                    // T8: ⚙ opens this oscillator's settings dialog. Clearing the edit
                    // copies marks a FRESH open — the dialog reseeds them from the live
                    // `Active` (unreachable via the immutable slice here).
                    if cui.small_button("⚙").on_hover_text("Indicator settings").clicked() {
                        indicator_dialog.open_uid = Some(a.uid);
                        indicator_dialog.working = None;
                        indicator_dialog.snapshot = None;
                    }
                    // ⋯ "Move to" menu (C1 Task 4): relocate THIS study to a fresh pane
                    // above/below its current one, or merge it into another present study
                    // pane. Same guards as before: "new pane above/below" only when the
                    // study is NOT already alone in its pane, and never list the study's
                    // OWN pane as a merge target. Gathered first so ⋯ is suppressed when
                    // no move is possible (the default single-oscillator layout).
                    //
                    // NOTE this `opts` LOCAL Vec deliberately shadows the fn's own
                    // `opts: &ChartOptions` parameter for this closure's scope only.
                    let mut opts: Vec<(String, MoveTarget)> = Vec::new();
                    if studies.len() > 1 {
                        opts.push((
                            "Move to new pane above".to_owned(),
                            MoveTarget::NewAbove(pane_key),
                        ));
                        opts.push((
                            "Move to new pane below".to_owned(),
                            MoveTarget::NewBelow(pane_key),
                        ));
                    }
                    for (pj, &other_key) in visible_study_panes.iter().enumerate() {
                        if other_key != pane_key {
                            opts.push((
                                format!("Move to pane {}", pj + 1),
                                MoveTarget::Into(other_key),
                            ));
                        }
                    }
                    if !opts.is_empty() {
                        cui.menu_button("⋯", |ui| {
                            for (label, target) in &opts {
                                if ui.button(label).clicked() {
                                    *move_study = Some((a.uid, *target));
                                    ui.close();
                                }
                            }
                        });
                    }
                    // Feature #1 — pane ↑/↓ move controls, drawn ONCE per pane (on its
                    // first study row). Reorders this pane within the WHOLE sub-pane
                    // group (studies + Volume + CVD, all peers); disabled at the ends.
                    if si == 0 {
                        pane_reorder_controls(cui, pane_key, pos, cx.n_reorderable, reorder_pane);
                    }
                },
            );
        }
        // Microstructure-study legends, stacked BELOW the indicator rows in the same
        // overlay column (rows continue at `studies.len()`). Deliberately minimal
        // chrome for now: no drag source (a study is self-keyed to its pane, so there
        // is no `study_pane_of` entry to rewrite) and no ✕/⚙ (`ChartActions::remove_uid`
        // /`IndicatorDialog` both resolve their uid against `indicators`, which would
        // silently no-op on a study uid — a study is added/removed/edited from the app's
        // own study menu). The pane ↑/↓ controls DO ride the first row of a pane that
        // holds only studies, so such a pane stays reorderable like every other.
        for (si, s) in micro_here.iter().enumerate() {
            let row = studies.len() + si;
            draw_pane_overlay(
                ui,
                plot_rect,
                pane_key,
                row,
                s.spec.pretty,
                pal::TEXT2,
                None,
                |cui| {
                    if studies.is_empty() && si == 0 {
                        pane_reorder_controls(cui, pane_key, pos, cx.n_reorderable, reorder_pane);
                    }
                },
            );
        }
        // Feature #2: the whole plot rect is a DROP ZONE — a study dragged from
        // another pane and released anywhere over THIS pane merges into it (the
        // old per-header-row `dnd_drop_zone`, now the overlaid plot rect). Skip a
        // study already assigned here (no-op self-merge).
        let drop = ui.interact(
            plot_rect,
            egui::Id::new(("vike_study_drop", pane_key)),
            egui::Sense::hover(),
        );
        if let Some(p) = drop.dnd_release_payload::<DragStudy>() {
            if study_pane_of.get(&p.uid) != Some(&pane_key) {
                *move_study = Some((p.uid, MoveTarget::Into(pane_key)));
            }
        }
        // TradingView-parity right-edge value tag: the CURRENT reading of this pane's
        // PRIMARY line — the first VISIBLE output line of the FIRST study in the pane.
        // ONE tag per pane (a merged multi-study pane still tags just the primary, so
        // the right axis never clutters); coloured to that line and formatted `{:.1}`,
        // matching the study y-axis. Warm-up NaN / empty series ⇒ no tag (the helper
        // filters non-finite), so a still-seeding oscillator is byte-identical to before.
        // Study-only pane (no indicator in it): the tag falls back to the FIRST
        // microstructure study's first visible line, by the same one-tag-per-pane rule.
        // A pane with an indicator resolves on the first arm and is byte-identical.
        let primary_line = studies
            .first()
            .and_then(|a| a.outputs.iter().find(|l| l.visible))
            .or_else(|| micro_here.first().and_then(|s| s.outputs.iter().find(|l| l.visible)));
        if let Some(line) = primary_line {
            paint_value_tag(
                ui,
                plot_rect,
                &oresp.transform,
                line.series.last().copied(),
                line.color,
                |v| format!("{:.1}", v),
            );
        }
        if is_last {
            // T4: this study pane is bottom-most this frame — the x-drag gutter
            // belongs to IT (it's the one carrying the shared time axis).
            *bottom_frame = *oresp.transform.frame();
        } else {
            // T9: separator between this study pane and the next.
            pane_separator(ui, panes, cx.present, pos, cx.avail_for_panes, MIN_PANE_PX);
        }
    }
}

/// Compare-series sub-panes (C2b): a compare symbol moved OUT of the price
/// %-overlay into its OWN pane (present AFTER every study pane — see
/// `present_panes`). Each renders its symbol's closes as an ABSOLUTE-price
/// line, reindexed by open-time onto the primary's visible index domain
/// (T1 `reindex_by_ot`, strict floor/as-of — a 2nd symbol's bars have their
/// own index space that won't line up positionally), x-slaved to the price
/// pane's `(px0, px1)` like every other sub-pane, with its OWN y-autofit over
/// the visible reindexed closes (absolute price, NOT %: own pane ⇒ own axis).
/// Mirrors the study loop's header + separator + `bottom_frame` bookkeeping.
/// Was `draw()`'s inline `for (si, &(pane_key, s)) in visible_series...` loop
/// (see `chart/mod.rs`); extracted to a single-pane fn dispatched by position in
/// `chart::draw`. `pos` is this pane's index in `cx.present`. Body verbatim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_one_series_pane(
    ui: &mut egui::Ui,
    cx: &SubPaneCtx,
    panes: &mut PaneFractions,
    bottom_frame: &mut egui::Rect,
    pos: usize,
    pane_key: PaneKey,
    s: &SeriesInput,
    series: &[Bar],
) {
    {
        // Position-based indexing: this series pane's index in `present`.
        let is_last = pos == cx.present.len() - 1;
        let this_series_h = cx.heights[pos];
        // Plot id is PANE-keyed (stable per pane, distinct from the study panes'
        // `pane_Study(..)` since `Series`/`Study` Debug differently) — feeds only
        // egui's per-widget memory key; bounds are re-asserted every frame.
        let mut p = sub_pane_plot(
            format!("pane_{pane_key:?}"),
            this_series_h,
            cx.axis_h,
            is_last,
            cx.grid_color,
            cx.grid_show,
            cx.sec_axis_active,
            |v| format!("{:.1}", v),
        );
        if is_last {
            p = attach_shared_x_axis(p, cx.xcx);
        } else {
            // Grid-alignment: middle panes share the price grid marks (no labels).
            p = attach_shared_x_grid(p, cx.xcx);
        }
        let sresp = p.show(ui, |pui| {
            // Reindex the compare symbol's bars onto the PRIMARY's visible index
            // window by open-time (floor/as-of), then plot its close at each
            // primary index (a primary bar with no overlay bar at/before it =
            // NaN = `seg_line` breaks the line there). y-fit = those visible
            // reindexed closes' min/max, padded — ABSOLUTE price, own axis.
            let (olo, ohi) = overlay_visible_range(series.len(), cx.px0, cx.px1);
            let reidx = reindex_by_ot(series, &s.state.bars, olo, ohi);
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            let mut pts: Vec<f64> = Vec::with_capacity(reidx.len());
            for o in &reidx {
                match o {
                    Some(j) => {
                        let c = s.state.bars[*j].c;
                        lo = lo.min(c);
                        hi = hi.max(c);
                        pts.push(c);
                    }
                    None => pts.push(f64::NAN),
                }
            }
            // Always x-slave to the price pane; a degenerate all-gap window (the
            // overlay has no bar in range) falls back to a harmless unit y-range
            // — nothing is drawn there anyway (`seg_line` needs >= 2 points).
            let (flo, fhi) =
                if lo.is_finite() && hi.is_finite() { y_pad(lo, hi, 5.0, 5.0) } else { (0.0, 1.0) };
            pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max([cx.px0, flo], [cx.px1, fhi]));
            // `pts` is indexed from `olo` — that's `seg_line`'s `base` offset.
            seg_line(pui, &pts, s.color, 1.5, egui_plot::LineStyle::Solid, cx.px0, cx.px1, olo);
        });
        // TradingView-style overlay: the symbol legend in its own series color at
        // the plot's top-left (always visible). No control cluster — a compare
        // series pane has no reorder/remove/settings actions wired today (it is
        // not in the reorderable group and there is no series-remove action), so
        // the cluster would be empty; the name matches the old minimal header.
        draw_pane_overlay(ui, sresp.response.rect, pane_key, 0, s.symbol, s.color, None, |_cui| {});
        if is_last {
            // this series pane is bottom-most this frame — its frame carries the
            // shared time axis, so the x-drag gutter belongs to IT.
            *bottom_frame = *sresp.transform.frame();
        } else {
            // T9: separator between this series pane and the next.
            pane_separator(ui, panes, cx.present, pos, cx.avail_for_panes, MIN_PANE_PX);
        }
    }
}
