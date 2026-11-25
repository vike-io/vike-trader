//! The four sub-pane render loops split out of `chart::draw` (chart refactor PR-6):
//! volume, CVD, study (oscillator), and compare-series sub-panes. Each function body
//! is the former inline loop in `draw()`, moved VERBATIM — byte-identical render,
//! including every `set_plot_bounds` call inside a `.show()` closure. The ~13 shared
//! read-only locals `draw` resolves once (the pane layout indices/heights + the shared
//! x-axis context) are bundled into the Copy [`ChartSubPaneContext`]; the per-loop mutated outputs
//! (`panes`, `bottom_frame`, plus `cvd_toggle`/`remove_uid`/`move_study` for their own
//! panes) pass through as `&mut` parameters, exactly mirroring how `draw` used to mutate
//! its own locals in place. See `chart/mod.rs`'s `draw`, which still owns the `if
//! volume_on`/`if cvd_pane_on` gates, the `cx`/`cvd_toggle`/`remove_uid`/`move_study`
//! declarations, and the four call sites (in the unchanged volume -> cvd -> study ->
//! series order).

use super::layout::{pane_separator, sub_pane_plot};
use crate::chart::ChartCompareSeries;
use crate::chart::axis::overlay::{ChartAxisChipFit, paint_value_tag};
use crate::chart::axis::tick_guard::{ChartAxisChip, ChartAxisTextProbe, ChartAxisTickGuard};
use crate::chart::axis::{attach_shared_x_axis, attach_shared_x_grid};
use crate::chart::colors::ChartColors;
use crate::chart::extent::y_pad;
use crate::chart::fmt::{format_compact, format_compact_tick, format_study_tick};
use crate::chart::interact::visible_slice;
use crate::chart::pane::{ChartPaneFractions, ChartPaneKey, ChartPaneMoveTarget};
use crate::chart::state::{ChartState, bar::ChartBar};
use crate::chart::study::ChartStudy;
use crate::chart::study::ChartStudyDialog;
use crate::chart::study::oscillator::{reindex_by_ot, render_oscillator};
use crate::chart::study::overlays::{overlay_visible_range, segment_line};
use egui::{Align, Color32, FontId, Layout, RichText};
use egui_plot::HLine;
use indexmap::IndexMap;
use vike_orderflow::FootprintBar;
use vike_ui_theme::components::button::ThemeButtonIcon;
use vike_ui_theme::components::overlay::menu_item;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::{ThemeTextRole, ThemeTextSize};
use vike_ui_theme::value::chart;

/// Everything the four sub-pane loops read but never mutate: the price pane's resolved
/// x-range, the resolved [`crate::chart::pane::layout::ChartPaneLayout`] heights/indices, the
/// shared grid/axis cosmetics, and the shared x-axis context (PR-3's `ChartAxisXContext`). All fields
/// are `Copy` (slices, references + scalars), so `draw` builds this ONCE (right after
/// `resolve_pane_layout`) and every call site below passes it by shared reference.
#[derive(Clone, Copy)]
pub(crate) struct ChartSubPaneContext<'a> {
    pub(crate) px0: f64,
    pub(crate) px1: f64,
    pub heights: &'a [f32],
    pub(crate) axis_h: f32,
    /// This frame's resolved colours (`crate::chart::colors`): the grid, volume, the zero line, pane
    /// names, value-tag text — the ones the price pane paints with.
    pub colors: &'a ChartColors,
    /// The installed text size: pane names and value tags name a type role (spec §3.3).
    pub text_size: ThemeTextSize,
    /// The axis text's metrics, for each pane's tick-label guard (`ChartAxisTickGuard::y`).
    pub probe: &'a ChartAxisTextProbe,
    /// Per-axis grid visibility (TV vertical/horizontal split), shared with the
    /// price pane so a hidden vertical/horizontal grid stays hidden chart-wide.
    pub(crate) grid_show: egui::Vec2b,
    pub(crate) sec_axis_active: bool,
    pub(crate) xcx: crate::chart::axis::ChartAxisXContext<'a>,
    pub present: &'a [ChartPaneKey],
    pub(crate) avail_for_panes: f32,
    /// Count of PRESENT reorderable sub-panes (Volume + CVD + study panes). The
    /// ↑/↓ reorder controls clamp against this: a sub-pane sits at some `position` in
    /// `1..=n_reorderable` (they are contiguous right after Price), so "↑" is
    /// enabled while `position > 1` and "↓" while `position < n_reorderable`.
    pub(crate) n_reorderable: usize,
}

/// The tick-label guard's view of a sub-pane's value tag (`paint_value_tag`, which paints nothing for
/// a missing or non-finite reading): the chip, kept inside the pane.
fn value_chip(guard: &ChartAxisTickGuard, value: Option<f64>) -> [Option<ChartAxisChip>; 2] {
    [value.filter(|v| v.is_finite()).map(|v| guard.y_chip(v, ChartAxisChipFit::Inside)), None]
}

/// The hover-gated pane ↑/↓ reorder controls shared by every reorderable
/// sub-pane header (Volume, CVD, study — peers now). Drawn inside a
/// right-to-left layout (↓ then ↑, so ↑ lands leftmost, matching the study
/// cluster); each is disabled at the reorderable group's ends. Emits the picked
/// move into `reorder_pane` (fire-once, harvested as `ChartActions::reorder_pane`).
fn pane_reorder_controls(
    ui: &mut egui::Ui,
    pane_key: ChartPaneKey,
    position: usize,
    n_reorderable: usize,
    reorder_pane: &mut Option<(ChartPaneKey, bool)>,
) {
    let down = ThemeButtonIcon::new(icons::MOVE_DOWN, "Move pane down");
    let down = if position < n_reorderable {
        down
    } else {
        down.disabled_because("Already the bottom pane")
    };
    if ui.add(down).clicked() {
        *reorder_pane = Some((pane_key, false));
    }
    let up = ThemeButtonIcon::new(icons::MOVE_UP, "Move pane up");
    let up = if position > 1 { up } else { up.disabled_because("Already the top pane") };
    if ui.add(up).clicked() {
        *reorder_pane = Some((pane_key, true));
    }
}

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
fn draw_pane_overlay(
    ui: &mut egui::Ui,
    plot_rect: egui::Rect,
    pane_key: ChartPaneKey,
    row: usize,
    name: &str,
    name_color: Color32,
    text_size: ThemeTextSize,
    drag: Option<u64>,
    add_cluster: impl FnOnce(&mut egui::Ui),
) {
    let y = plot_rect.top() + space::XS + row as f32 * chart::PANE_OVERLAY_ROW_HEIGHT;
    // The pane's name: the Strong role in Inter semibold.
    let semibold = egui::FontFamily::Name(vike_ui_theme::fonts::SEMIBOLD.into());
    let name_text = RichText::new(name)
        .font(FontId::new(text_size.px(ThemeTextRole::Strong), semibold))
        .color(name_color);
    // NAME at top-left (always visible). Bounded to the left ~55% of the plot
    // width so a long legend never runs under the right cluster.
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(plot_rect.left() + space::MD, y),
        egui::vec2((plot_rect.width() * 0.55).max(0.0), chart::PANE_OVERLAY_ROW_HEIGHT),
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
                egui::Id::unique(("vike_study_drag", uid)),
                ChartPaneDragStudy { uid },
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
    // because the sub-pane plots disable drag/zoom/scroll), and never through a window above.
    let hovered = ui.rect_contains_pointer(plot_rect);
    // ALWAYS allocate the cluster child ui (stable id_salt) so its id is present
    // in BOTH of egui's layout passes — otherwise a hover state that differs
    // across passes toggles the widget in/out and egui flags the id change with a
    // red "changed id between passes" box. Only the BUTTONS are hover-gated.
    let cl_rect = egui::Rect::from_min_max(
        egui::pos2(name_rect.right(), y),
        egui::pos2(plot_rect.right() - space::MD, y + chart::PANE_OVERLAY_ROW_HEIGHT),
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
/// `if volume_on { .. }` body; the caller still owns the `if volume_on` gate (see `chart/mod.rs`).
pub(crate) fn draw_volume_pane(
    ui: &mut egui::Ui,
    cx: &ChartSubPaneContext,
    panes: &mut ChartPaneFractions,
    bottom_frame: &mut egui::Rect,
    // Position-based indexing (chart single-max default): Volume's index in
    // `present` — dispatched from `chart::draw`, so it works wherever the
    // authored order places it.
    position: usize,
    state: &ChartState,
    // Volume-as-indicator: set true when the header ✕ is clicked this frame so
    // the caller can turn off `ChartSettings::show_volume` (fire-once, mirrors
    // `cvd_toggle`). Lets Volume be removed like a normal indicator pane.
    volume_remove: &mut bool,
    // Feature #1 (TradingView parity): pane ↑/↓ reorder, harvested into
    // `ChartActions::reorder_pane` — Volume is now a reorderable peer.
    reorder_pane: &mut Option<(ChartPaneKey, bool)>,
) {
    // `is_bottom` (carries the shared time axis) is simply "last pane".
    let is_bottom = position == cx.present.len() - 1;
    let volume_height = cx.heights[position];
    let guard = ChartAxisTickGuard::y(cx.probe);
    // The LAST bar's volume is this pane's value tag (painted below, after the plot).
    let last = state.bars.last();
    guard.set_chips(value_chip(&guard, last.map(|b| b.v)));
    let mut p = sub_pane_plot(
        "volume",
        volume_height,
        cx.axis_h,
        is_bottom,
        cx.colors.grid,
        cx.grid_show,
        cx.sec_axis_active,
        format_compact_tick,
        &guard,
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
        // chart-perf T5: cached CLOSED-bar max (`ChartState::visible_volume_max_shared`) instead
        // of a per-frame refold; `.max()`'d with the forming bar's volume when it's
        // visible — see the VolumeCandles arm above for the identical pattern/rationale.
        let (volume_low, volume_high) = match (vis.first(), vis.last()) {
            (Some(f), Some(l)) => (f.t as usize, l.t as usize + 1),
            _ => (0, 0),
        };
        let forming_index = state.closed_len;
        let forming_v = if state.bars.len() > forming_index
            && (volume_low..volume_high).contains(&forming_index)
        {
            state.bars[forming_index].v
        } else {
            0.0
        };
        let vmax =
            state.visible_volume_max_shared(volume_low, volume_high).max(forming_v).max(1e-9);
        pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
            [cx.px0, 0.0],
            [cx.px1, vmax * 1.08],
        ));
        let vbars: Vec<egui_plot::Bar> = vis
            .iter()
            .map(|bb| {
                // direction-coloured volume: the market set's volume fills (spec §3.2)
                let fill = if bb.c >= bb.o { cx.colors.up_volume } else { cx.colors.down_volume };
                egui_plot::Bar::new(bb.t, bb.v).fill(fill).width(chart::VOLUME_BAR_WIDTH)
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
        ChartPaneKey::Volume,
        0,
        "Volume",
        cx.colors.text_secondary,
        cx.text_size,
        None,
        |cui| {
            if cui.add(ThemeButtonIcon::new(icons::REMOVE, "Remove volume pane")).clicked() {
                *volume_remove = true;
            }
            pane_reorder_controls(
                cui,
                ChartPaneKey::Volume,
                position,
                cx.n_reorderable,
                reorder_pane,
            );
        },
    );
    // TradingView-parity right-edge value tag (optional per the brief — trivial with
    // the shared helper, so included): the LAST bar's volume, coloured by that bar's
    // candle direction (up/down, matching the bars), compact-formatted like the volume
    // y-axis. No bars ⇒ `None` ⇒ no tag.
    let (up_s, down_s) = (cx.colors.up_graphic, cx.colors.down_graphic);
    paint_value_tag(
        ui,
        vresp.response.rect,
        &vresp.transform,
        last.map(|b| b.v),
        last.map_or(up_s, |b| if b.c >= b.o { up_s } else { down_s }),
        cx.colors,
        cx.text_size,
        format_compact,
    );
    if is_bottom {
        // T4: the volume pane is bottom-most this frame — its frame carries
        // the shared time axis, so the x-drag gutter belongs to IT, not price.
        *bottom_frame = *vresp.transform.frame();
    }
    // T9: the separator that FOLLOWS this pane (boundary `position`) — only when
    // there IS a next pane; when Volume is already the last pane, none follows.
    if !is_bottom {
        pane_separator(
            ui,
            panes,
            cx.present,
            position,
            cx.avail_for_panes,
            chart::MIN_PANE_PX,
            cx.colors,
        );
    }
}

/// CVD sub-pane (SP2): cumulative volume delta derived from the footprint
/// substrate (`orderflow::cvd_from_footprints`), directly below Volume —
/// see the `present` build in `resolve_pane_layout` for the placement rationale. SP2 v1
/// recomputed it fresh every frame (O(bars)); SP3 Task B #2 (SP2 final-review
/// finding B) caches it on `state` (`ChartState::cvd_shared`), keyed by
/// `footprint_generation` (SP3 TB-fix — a monotonic generation, not the footprint
/// slice's own address, see `state/mod.rs`'s `ChartStateCvdCacheKey` doc) — an unchanged
/// frame (the common case) is now an O(1) `Rc` clone instead of re-folding
/// every bar's cells. Was `draw()`'s inline `if cvd_pane_on { .. }` body; the caller
/// still owns the `if cvd_pane_on` gate and the `let mut cvd_toggle = false;` declaration
/// (see `chart/mod.rs`).
pub(crate) fn draw_cvd_pane(
    ui: &mut egui::Ui,
    cx: &ChartSubPaneContext,
    panes: &mut ChartPaneFractions,
    bottom_frame: &mut egui::Rect,
    // Position-based indexing: CVD's index in `present` — dispatched from `chart::draw`.
    position: usize,
    state: &ChartState,
    footprint: Option<&[FootprintBar]>,
    footprint_generation: u64,
    cvd_toggle: &mut bool,
    // Feature #1 (TradingView parity): pane ↑/↓ reorder, harvested into
    // `ChartActions::reorder_pane` — CVD is now a reorderable peer.
    reorder_pane: &mut Option<(ChartPaneKey, bool)>,
) {
    let is_bottom = position == cx.present.len() - 1;
    let cvd_h = cx.heights[position];
    let cvd = state.cvd_shared(footprint.unwrap_or(&[]), footprint_generation);
    let guard = ChartAxisTickGuard::y(cx.probe);
    // The CURRENT CVD reading is this pane's value tag (painted below, after the plot).
    guard.set_chips(value_chip(&guard, cvd.last().copied()));
    let mut p = sub_pane_plot(
        "cvd",
        cvd_h,
        cx.axis_h,
        is_bottom,
        cx.colors.grid,
        cx.grid_show,
        cx.sec_axis_active,
        format_compact_tick,
        &guard,
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
                .color(cx.colors.zero_line)
                .width(stroke::HAIRLINE)
                .style(egui_plot::LineStyle::dashed_loose())
                .allow_hover(false),
        );
        segment_line(
            pui,
            &cvd,
            chart::SERIES_1,
            stroke::LINE,
            egui_plot::LineStyle::Solid,
            cx.px0,
            cx.px1,
            0,
        );
    });
    // TradingView-style overlay: "CVD" legend top-left + `↑ ↓ ✕` cluster
    // top-right (hover-gated). No ⋯/⚙ — CVD has no move-to targets or settings.
    draw_pane_overlay(
        ui,
        cresp.response.rect,
        ChartPaneKey::CumulativeVolumeDelta,
        0,
        "CVD",
        cx.colors.text_secondary,
        cx.text_size,
        None,
        |cui| {
            if cui.add(ThemeButtonIcon::new(icons::REMOVE, "Remove CVD pane")).clicked() {
                *cvd_toggle = true;
            }
            pane_reorder_controls(
                cui,
                ChartPaneKey::CumulativeVolumeDelta,
                position,
                cx.n_reorderable,
                reorder_pane,
            );
        },
    );
    // TradingView-parity right-edge value tag: the CURRENT CVD reading, in the CVD
    // line colour, compact-formatted like the CVD y-axis. Empty/NaN ⇒ no tag.
    paint_value_tag(
        ui,
        cresp.response.rect,
        &cresp.transform,
        cvd.last().copied(),
        chart::SERIES_1,
        cx.colors,
        cx.text_size,
        format_compact,
    );
    if is_bottom {
        // this CVD pane is bottom-most this frame — its frame carries the
        // shared time axis, so the x-drag gutter belongs to IT.
        *bottom_frame = *cresp.transform.frame();
    }
    // T9: the separator that FOLLOWS this pane (boundary `position`) — only when
    // there IS a next pane below CVD.
    if !is_bottom {
        pane_separator(
            ui,
            panes,
            cx.present,
            position,
            cx.avail_for_panes,
            chart::MIN_PANE_PX,
            cx.colors,
        );
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
/// Feature #2 (TradingView parity): the drag-and-drop payload carried by a study
/// legend. Dragging a study's name label (a `dnd_drag_source`) and releasing it
/// over another pane's header (`dnd_drop_zone`) merges the study into that pane —
/// the same relocation the ⋯ "Move to pane N" menu performs, via `move_study`.
#[derive(Clone)]
struct ChartPaneDragStudy {
    uid: u64,
}

/// ONE study sub-pane (chart single-max default): the former `draw_study_panes`
/// loop body, extracted to a single-pane fn so `chart::draw` can dispatch it by
/// position in the authored order (studies, volume, and CVD are now peers). `position`
/// is this pane's index in `cx.present`. Body is otherwise verbatim.
#[expect(clippy::too_many_arguments)]
pub(crate) fn draw_one_study_pane(
    ui: &mut egui::Ui,
    cx: &ChartSubPaneContext,
    panes: &mut ChartPaneFractions,
    bottom_frame: &mut egui::Rect,
    position: usize,
    pane_key: ChartPaneKey,
    visible_study_panes: &[ChartPaneKey],
    indicators: &[ChartStudy],
    study_pane_of: &IndexMap<u64, ChartPaneKey>,
    indicator_dialog: &mut ChartStudyDialog,
    remove_uid: &mut Option<u64>,
    move_study: &mut Option<(u64, ChartPaneMoveTarget)>,
    // Feature #1 (TradingView parity): pane ↑/↓ reorder. `(pane, up)` — the
    // pane the user asked to move and the direction; harvested into
    // `ChartActions::reorder_pane` like `move_study`.
    reorder_pane: &mut Option<(ChartPaneKey, bool)>,
) {
    {
        // Every visible non-overlay indicator authored into THIS pane. Overlays
        // never appear in `study_pane_of` (they render on the price pane), so
        // the `!is_overlay()` guard is belt-and-braces. `visible_study_panes`
        // pre-filters panes with no visible study, so `studies` is non-empty
        // here; on the default assignment it is exactly one study.
        let studies: Vec<&ChartStudy> = indicators
            .iter()
            .filter(|a| {
                a.visible && !a.is_overlay() && study_pane_of.get(&a.uid) == Some(&pane_key)
            })
            .collect();
        // Position-based indexing: this pane's index in `present` drives its
        // height, its separator boundary, and whether it's the bottom-most pane
        // (carries the shared time axis).
        let is_last = position == cx.present.len() - 1;
        let this_osc_h = cx.heights[position];
        // Plot id is PANE-keyed (stable per pane), not per-study-uid — a pane may
        // hold several studies. It feeds only egui's per-widget memory key; the
        // plot bounds are re-asserted inside the closure every frame, so the id
        // string never affects the rendered pixels (byte-identical output). On the
        // default one-study pane this is `pane_Study(<uid>)` where the old id was
        // `osc_<uid>` — a different memory key, same render.
        // The pane's value tag reads its PRIMARY line (see the tag below); resolved here so the
        // axis' tick-label guard knows the tag before the axis is formatted.
        let primary_line = studies.first().and_then(|a| a.outputs.iter().find(|l| l.visible));
        let guard = ChartAxisTickGuard::y(cx.probe);
        guard.set_chips(value_chip(&guard, primary_line.and_then(|l| l.series.last().copied())));
        let mut p = sub_pane_plot(
            format!("pane_{pane_key:?}"),
            this_osc_h,
            cx.axis_h,
            is_last,
            cx.colors.grid,
            cx.grid_show,
            cx.sec_axis_active,
            format_study_tick,
            &guard,
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
            if lo.is_finite() && hi.is_finite() {
                let (flo, fhi) = y_pad(lo, hi, 5.0, 5.0);
                pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                    [cx.px0, flo],
                    [cx.px1, fhi],
                ));
            }
            for a in &studies {
                render_oscillator(pui, a, cx.px0, cx.px1, cx.colors);
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
                cx.colors.text_secondary,
                cx.text_size,
                Some(a.uid),
                |cui| {
                    if cui.add(ThemeButtonIcon::new(icons::REMOVE, "Remove indicator")).clicked() {
                        *remove_uid = Some(a.uid);
                    }
                    // T8: ⚙ opens this oscillator's settings dialog. Clearing the edit
                    // copies marks a FRESH open — the dialog reseeds them from the live
                    // `ChartStudy` (unreachable via the immutable slice here).
                    if cui
                        .add(ThemeButtonIcon::new(icons::SETTINGS, "Indicator settings"))
                        .clicked()
                    {
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
                    let mut options: Vec<(String, ChartPaneMoveTarget)> = Vec::new();
                    if studies.len() > 1 {
                        options.push((
                            "Move to new pane above".to_owned(),
                            ChartPaneMoveTarget::NewAbove(pane_key),
                        ));
                        options.push((
                            "Move to new pane below".to_owned(),
                            ChartPaneMoveTarget::NewBelow(pane_key),
                        ));
                    }
                    for (pj, &other_key) in visible_study_panes.iter().enumerate() {
                        if other_key != pane_key {
                            options.push((
                                format!("Move to pane {}", pj + 1),
                                ChartPaneMoveTarget::Into(other_key),
                            ));
                        }
                    }
                    if !options.is_empty() {
                        let more = cui.add(ThemeButtonIcon::new(icons::MORE, "Move to"));
                        egui::Popup::menu(&more).show(|ui| {
                            for (label, target) in &options {
                                if menu_item(ui, None, label, None).clicked() {
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
                        pane_reorder_controls(
                            cui,
                            pane_key,
                            position,
                            cx.n_reorderable,
                            reorder_pane,
                        );
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
            egui::Id::unique(("vike_study_drop", pane_key)),
            egui::Sense::hover(),
        );
        if let Some(p) = drop.dnd_release_payload::<ChartPaneDragStudy>()
            && study_pane_of.get(&p.uid) != Some(&pane_key)
        {
            *move_study = Some((p.uid, ChartPaneMoveTarget::Into(pane_key)));
        }
        // TradingView-parity right-edge value tag: the CURRENT reading of this pane's
        // PRIMARY line — the first VISIBLE output line of the FIRST study in the pane.
        // ONE tag per pane (a merged multi-study pane still tags just the primary, so
        // the right axis never clutters); coloured to that line and formatted `{:.1}`,
        // matching the study y-axis. Warm-up NaN / empty series ⇒ no tag (the helper
        // filters non-finite), so a still-seeding oscillator is byte-identical to before.
        if let Some(line) = primary_line {
            paint_value_tag(
                ui,
                plot_rect,
                &oresp.transform,
                line.series.last().copied(),
                line.color,
                cx.colors,
                cx.text_size,
                |v| format!("{:.1}", v),
            );
        }
        if is_last {
            // T4: this study pane is bottom-most this frame — the x-drag gutter
            // belongs to IT (it's the one carrying the shared time axis).
            *bottom_frame = *oresp.transform.frame();
        } else {
            // T9: separator between this study pane and the next.
            pane_separator(
                ui,
                panes,
                cx.present,
                position,
                cx.avail_for_panes,
                chart::MIN_PANE_PX,
                cx.colors,
            );
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
/// `chart::draw`. `position` is this pane's index in `cx.present`. Body verbatim.
pub(crate) fn draw_one_series_pane(
    ui: &mut egui::Ui,
    cx: &ChartSubPaneContext,
    panes: &mut ChartPaneFractions,
    bottom_frame: &mut egui::Rect,
    position: usize,
    pane_key: ChartPaneKey,
    s: &ChartCompareSeries,
    series: &[ChartBar],
) {
    {
        // Position-based indexing: this series pane's index in `present`.
        let is_last = position == cx.present.len() - 1;
        let this_series_h = cx.heights[position];
        // Plot id is PANE-keyed (stable per pane, distinct from the study panes'
        // `pane_Study(..)` since `Series`/`Study` Debug differently) — feeds only
        // egui's per-widget memory key; bounds are re-asserted every frame.
        let guard = ChartAxisTickGuard::y(cx.probe);
        let mut p = sub_pane_plot(
            format!("pane_{pane_key:?}"),
            this_series_h,
            cx.axis_h,
            is_last,
            cx.colors.grid,
            cx.grid_show,
            cx.sec_axis_active,
            format_study_tick,
            &guard,
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
            // NaN = `segment_line` breaks the line there). y-fit = those visible
            // reindexed closes' min/max, padded — ABSOLUTE price, own axis.
            let (olo, ohi) = overlay_visible_range(series.len(), cx.px0, cx.px1);
            let reindexed = reindex_by_ot(series, &s.state.bars, olo, ohi);
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            let mut pts: Vec<f64> = Vec::with_capacity(reindexed.len());
            for o in &reindexed {
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
            // — nothing is drawn there anyway (`segment_line` needs >= 2 points).
            let (flo, fhi) =
                if lo.is_finite() && hi.is_finite() { y_pad(lo, hi, 5.0, 5.0) } else { (0.0, 1.0) };
            pui.set_plot_bounds(egui_plot::PlotBounds::from_min_max([cx.px0, flo], [cx.px1, fhi]));
            // `pts` is indexed from `olo` — that's `segment_line`'s `base` offset.
            segment_line(
                pui,
                &pts,
                s.color,
                stroke::LINE,
                egui_plot::LineStyle::Solid,
                cx.px0,
                cx.px1,
                olo,
            );
        });
        // TradingView-style overlay: the symbol legend in its own series color at
        // the plot's top-left (always visible). No control cluster — a compare
        // series pane has no reorder/remove/settings actions wired today (it is
        // not in the reorderable group and there is no series-remove action), so
        // the cluster would be empty; the name matches the old minimal header.
        draw_pane_overlay(
            ui,
            sresp.response.rect,
            pane_key,
            0,
            s.symbol,
            s.color,
            cx.text_size,
            None,
            |_cui| {},
        );
        if is_last {
            // this series pane is bottom-most this frame — its frame carries the
            // shared time axis, so the x-drag gutter belongs to IT.
            *bottom_frame = *sresp.transform.frame();
        } else {
            // T9: separator between this series pane and the next.
            pane_separator(
                ui,
                panes,
                cx.present,
                position,
                cx.avail_for_panes,
                chart::MIN_PANE_PX,
                cx.colors,
            );
        }
    }
}
