//! The price-pane render passes that run AFTER `chart::draw`'s ONE `set_plot_bounds`
//! single-write (chart refactor PR-7): the secondary-axis (Right/Left compare-overlay) line
//! compute, the per-style series paint dispatch (candles/bars/line/…, including the LOD
//! decimation + the GPU candle seam), the volume-profile overlay, and the five small
//! fixed-order overlay paints (indicator overlays -> C2a %-lines -> secondary-axis lines ->
//! user drawings -> last-price dashed hline). Each function body is the former inline block
//! in `draw`'s `plot.show` content closure, moved VERBATIM — byte-identical render, same
//! paint z-order, same reads. `chart::draw` still owns everything upstream of the write (the
//! deferred-bounds "single-write" machinery, the `overlay_lines` %-compare fold that feeds
//! the PRE-write auto_y autofit, and the `follow`/`map`/`vis_now` setup) and the tiny
//! hover-detect block that runs just after these paints — see that fn for the finalized
//! `x0`/`x1`/`ty0`/`ty1`/`map`/`vis_now` these four functions consume but never recompute.

use super::consts::{PROFILE_COLOR, PROFILE_POC_COLOR, PROFILE_VA_COLOR};
use super::extent::{cell_px_for, orderflow_index_bounds, orderflow_tick_size};
use crate::chart::{ChartStyle, SeriesInput};
use crate::indicators::Active;
use crate::lod::lod_decimate;
use crate::model::{Bar, ChartState};
use crate::options::ChartOptions;
use crate::panes::PaneKey;
use crate::render::{
    GpuCandleItem, draw_bars, draw_baseline, draw_candles, draw_columns, draw_footprint,
    draw_hlc_area, draw_kagi, draw_line, draw_pnf, draw_step, draw_volume_candles,
    overlay_visible_range, reindex_by_ot, render_overlay, seg_line,
};
use crate::scale::{self, ScaleAssign, ScaleMode};
use egui::Color32;
use egui_plot::{HLine, PlotPoints};
use indexmap::IndexMap;
use std::cell::Cell;
use vike_orderflow::FootprintBar;

/// C2b Task 7: secondary-axis (Right/Left) compare-overlay line COMPUTE — was `draw`'s inline
/// `let secondary_lines = if !overlays.is_empty() { .. }` block (chart refactor PR-7), moved
/// verbatim except `cx0`/`cx1` (the closure's tracked-range locals) are renamed to the params
/// `x0`/`x1` — identical values, since the caller only invokes this AFTER its
/// `let (x0, x1) = (cx0, cx1);` finalize.
///
/// Each Right/Left-pinned overlay renders on its OWN absolute price axis: its visible
/// reindexed closes define its own range `(sec_lo, sec_hi)`, linearly remapped into the
/// primary's NOW-RESOLVED plot-space y-range `[ty0, ty1]` (`scale::remap_to_primary`) so it
/// fills the pane on its own scale. Computed AFTER ty0/ty1 (unlike the Percent overlays in the
/// caller's still-inline `overlay_lines` block) precisely so it maps into the FINAL range
/// WITHOUT feeding the primary's autofit — the secondary axis is independent of the primary's
/// scale/auto_y and works in Linear/Log/Percent alike. `Left` shares this path (single
/// secondary axis in 7b). The FIRST drawable overlay here publishes its (sec_lo, sec_hi) +
/// (ty0, ty1) to `sec_axis_cell` so the labeled right price gutter (built by the Plot builder
/// in `chart::draw`) reads out its real prices. EMPTY `overlays` / no Right pin ⇒ this whole
/// block is a no-op (byte-identical).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_secondary_axis_lines(
    series: &[Bar],
    overlays: &[SeriesInput],
    series_pane_of: &IndexMap<String, PaneKey>,
    series_scale: &IndexMap<String, ScaleAssign>,
    x0: f64,
    x1: f64,
    ty0: f64,
    ty1: f64,
    overlay_legend: &mut Vec<(Color32, String, f64, bool)>,
    sec_axis_cell: &Cell<(f64, f64, f64, f64)>,
) -> Vec<(Color32, Vec<f64>, usize)> {
    if !overlays.is_empty() {
        let (olo, ohi) = overlay_visible_range(series.len(), x0, x1);
        let mut out = Vec::new();
        for s in overlays {
            if series_pane_of.contains_key(s.symbol) {
                continue;
            }
            // Only Right/Left ride the secondary ABSOLUTE axis here. Percent (the default)
            // is a shared-% line built in `draw`; SharedLinear (absolute-shared-axis feature)
            // renders on the PRIMARY axis via `compute_shared_axis_lines` — neither may leak
            // into this secondary-axis remap, so anything that isn't Right/Left is skipped.
            // Default (empty `series_scale` ⇒ Percent) still skips ⇒ byte-identical.
            if !matches!(
                series_scale.get(s.symbol).copied().unwrap_or_default(),
                ScaleAssign::Right | ScaleAssign::Left
            ) {
                continue;
            }
            let reidx = reindex_by_ot(series, &s.state.bars, olo, ohi);
            let (mut sec_lo, mut sec_hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for o in reidx.iter().flatten() {
                let c = s.state.bars[*o].c;
                sec_lo = sec_lo.min(c);
                sec_hi = sec_hi.max(c);
            }
            // All-gap visible window (no overlay bar in range): nothing to map
            // — skip (no line to draw; `remap_to_primary` needs a real range).
            if !sec_lo.is_finite() || !sec_hi.is_finite() {
                continue;
            }
            let pvs: Vec<f64> = reidx
                .iter()
                .map(|o| match o {
                    Some(j) => {
                        scale::remap_to_primary(s.state.bars[*j].c, sec_lo, sec_hi, ty0, ty1)
                    }
                    None => f64::NAN,
                })
                .collect();
            // Task 7 review Minor-2: surface a Right-pinned line's ABSOLUTE
            // last-visible (non-gap) close in the OHLC legend — a compact readout
            // complementing its labeled secondary axis; `false` marks it a PRICE,
            // not a %-value, so the legend formatter drops the `%` suffix.
            if let Some(last_c) = reidx.iter().rev().flatten().next().map(|&j| s.state.bars[j].c) {
                overlay_legend.push((s.color, s.symbol.to_string(), last_c, false));
            }
            // C2b Task 7b: the FIRST drawable secondary overlay owns the
            // labeled right gutter — publish its own range + the resolved
            // primary plot-space range `(ty0, ty1)` for the secondary-axis
            // label formatter (read after `.show()` returns). `out.is_empty()`
            // ⇒ this is that first one (the axis tracks overlay #1, matching a
            // single secondary scale). `ty0`/`ty1` are already finalized above.
            if out.is_empty() {
                sec_axis_cell.set((sec_lo, sec_hi, ty0, ty1));
            }
            out.push((s.color, pvs, olo));
        }
        out
    } else {
        Vec::new()
    }
}

/// The result of [`compute_shared_axis_lines`] (absolute-shared-axis feature): the
/// draw-ready mapped close lines PLUS the combined RAW visible extent to fold into the
/// primary's y-autofit.
pub(crate) struct SharedAxisLines {
    /// Per shared-axis compare: `(color, mapped_close_line, base_index)`. Values are
    /// ALREADY in the primary's plot-space (`map`-ped closes) — drawn directly by
    /// [`paint_price_overlays`]'s `seg_line`, NOT re-mapped. Gaps are `NaN` (seg_line
    /// breaks the line there). EMPTY ⇒ nothing to draw.
    pub(crate) lines: Vec<(Color32, Vec<f64>, usize)>,
    /// Combined RAW (min-low, max-high) across every shared compare's visible reindexed
    /// bars, restricted to bars whose mapped extents are finite (so a non-positive Log-mode
    /// low can never corrupt the primary's bounds). `None` when no shared compare has a
    /// finite visible range this frame — the autofit fold is then a no-op (byte-identical).
    pub(crate) raw_ext: Option<(f64, f64)>,
}

/// Absolute-shared-axis compare-overlay COMPUTE: render each [`ScaleAssign::SharedLinear`]
/// compare at its TRUE ABSOLUTE price on the PRIMARY axis, sharing the one price scale.
///
/// Unlike Percent (rebased to first-visible on a shared % axis) and Right/Left (its OWN
/// secondary axis remapped into `[ty0, ty1]`), a shared compare's closes pass through the
/// PRIMARY's own `map` (Linear identity / Log `log10`) — the identical mapping the primary
/// candles use — so the two truly share one scale. Its visible OHLC extents are returned raw
/// so `draw`'s auto_y fold can expand the primary y-range to encompass them (a shared compare
/// is never clipped). ts-alignment is the SAME `reindex_by_ot` floor/as-of the other compare
/// paths use, so x-authority stays the primary's.
///
/// Gated by the caller on `matches!(eff_mode, Linear | Log)` — absolute price has no sensible
/// mapping onto a Percent/Indexed (rebased) primary axis, so a SharedLinear pin renders
/// nothing there. Symbols moved to their own pane (`series_pane_of`) or not pinned
/// `SharedLinear` (`series_scale`) are skipped. EMPTY `overlays` / no SharedLinear pin ⇒ an
/// empty result (byte-identical no-op). The last-visible (non-gap) ABSOLUTE close is pushed
/// to `overlay_legend` (`is_pct=false`, like the Right path) for the OHLC legend readout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_shared_axis_lines(
    series: &[Bar],
    overlays: &[SeriesInput],
    series_pane_of: &IndexMap<String, PaneKey>,
    series_scale: &IndexMap<String, ScaleAssign>,
    x0: f64,
    x1: f64,
    eff_mode: ScaleMode,
    map: &dyn Fn(f64) -> f64,
    overlay_legend: &mut Vec<(Color32, String, f64, bool)>,
) -> SharedAxisLines {
    // Absolute price only shares a raw/log axis — never a rebased Percent/Indexed one.
    if overlays.is_empty() || !matches!(eff_mode, ScaleMode::Linear | ScaleMode::Log) {
        return SharedAxisLines { lines: Vec::new(), raw_ext: None };
    }
    let (olo, ohi) = overlay_visible_range(series.len(), x0, x1);
    let mut lines = Vec::new();
    let (mut raw_lo, mut raw_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for s in overlays {
        if series_pane_of.contains_key(s.symbol) {
            continue;
        }
        if !matches!(
            series_scale.get(s.symbol).copied().unwrap_or_default(),
            ScaleAssign::SharedLinear
        ) {
            continue;
        }
        let reidx = reindex_by_ot(series, &s.state.bars, olo, ohi);
        // Fold this compare's visible OHLC (low/high) into the combined raw extent — but
        // only bars whose MAPPED low AND high are finite. In Linear that's every finite
        // price; in Log it drops a non-positive low so `log10` can never inject a NaN into
        // the primary's autofit bounds. The line itself (closes) is drawn separately below.
        let mut any = false;
        for j in reidx.iter().flatten() {
            let (l, h) = (s.state.bars[*j].l, s.state.bars[*j].h);
            if map(l).is_finite() && map(h).is_finite() {
                raw_lo = raw_lo.min(l);
                raw_hi = raw_hi.max(h);
                any = true;
            }
        }
        // A compare that aligns onto NO visible primary bar (all-gap window) contributes no
        // line and no extent — skip it (nothing to draw, nothing to fold).
        if !any {
            continue;
        }
        // Mapped close line (gaps = NaN → seg_line breaks). Shares the primary's `map`, so it
        // lands on the identical absolute-price axis as the primary candles.
        let pvs: Vec<f64> = reidx
            .iter()
            .map(|o| match o {
                Some(j) => map(s.state.bars[*j].c),
                None => f64::NAN,
            })
            .collect();
        // Absolute last-visible close in the OHLC legend (`false` ⇒ a PRICE, not a %).
        if let Some(last_c) = reidx.iter().rev().flatten().next().map(|&j| s.state.bars[j].c) {
            overlay_legend.push((s.color, s.symbol.to_string(), last_c, false));
        }
        lines.push((s.color, pvs, olo));
    }
    let raw_ext = (raw_lo.is_finite() && raw_hi.is_finite()).then_some((raw_lo, raw_hi));
    SharedAxisLines { lines, raw_ext }
}

/// The per-style series PAINT dispatch (candles/bars/line/…, including the LOD Phase 1
/// decimation and the GPU candle seam) — was `draw`'s inline LOD setup + `match style { .. }`
/// dispatch in the `plot.show` content closure, moved verbatim (chart refactor PR-7).
/// `vis_now` is the caller's ALREADY-COMPUTED `visible_slice(series, x0, x1)` (the
/// VolumeCandles arm's window fold reuses it) — never recomputed here. `gpu_candles`'s
/// lifetime `'a` is tied to `plot_ui`'s own `PlotUi<'a>` parameter: a pushed `GpuCandleItem<'a>`
/// must satisfy `PlotUi::add`'s `impl PlotItem + 'a` bound, so the two borrows are unified to
/// the SAME `'a` here (the caller's actual `PlotUi` lifetime is a `draw()`-local scope shorter
/// than `gpu_candles`'s true `ChartInputs<'a>` lifetime; that longer borrow narrows to the
/// shorter one covariantly, which is exactly what already happens at today's inline call site).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_price_series<'a>(
    plot_ui: &mut egui_plot::PlotUi<'a>,
    style: ChartStyle,
    series: &[Bar],
    state: &ChartState,
    vis_now: &[Bar],
    x0: f64,
    x1: f64,
    y_lo: f64,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
    gpu_candles: Option<&'a dyn Fn(Vec<crate::render::CandleInstance>, egui::Rect) -> egui::Shape>,
    footprint: Option<&[FootprintBar]>,
    of_tick_size: f64,
) {
    use ChartStyle::*;

    // LOD Phase 1 (Task 2): decimate the visible slice ONCE, before dispatch, to at most one
    // min-max OHLC bucket per screen column — `n_cols` is the plot's pixel width. At normal
    // zoom (visible bar count <= n_cols, the common case) `lod_decimate` returns
    // `Cow::Borrowed` of EXACTLY the slice `vis(series, x0, x1)` used to yield internally, so
    // every candle/bar arm below (Hollow / Bars / HlcBars|HighLow / the Footprint no-data
    // fallback / the transform-style default) stays byte-identical with zero new allocation.
    // Only a visible count beyond the column budget (zoomed way out) decimates — the feature.
    let n_cols = plot_ui.response().rect.width().max(1.0) as usize;
    // LAZY: only the candle/bar arms below call `lod()`; the ~11 non-candle styles
    // (Line/Area/Columns/VolumeCandles/Kagi/PnF/…) never do, so they pay ZERO decimation
    // cost (no wasted fold/alloc on those styles when zoomed out). At most one match arm
    // runs per frame, so `lod()` is invoked at most once.
    let lod = || lod_decimate(series, x0, x1, n_cols);

    match style {
        Hollow => {
            // GPU Phase 2: push a GpuCandleItem at the SAME z-slot draw_candles occupies
            // when `gpu_candles` is wired; else the byte-identical egui painter.
            if let Some(build) = gpu_candles {
                plot_ui.add(GpuCandleItem::new(lod().as_ref(), true, &map, opts, build));
            } else {
                draw_candles(plot_ui, lod().as_ref(), true, &map, opts);
            }
        }
        Bars => draw_bars(plot_ui, lod().as_ref(), false, &map, opts),
        HlcBars | HighLow => draw_bars(plot_ui, lod().as_ref(), true, &map, opts),
        Line => draw_line(plot_ui, series, x0, x1, false, false, y_lo, &map, opts),
        LineMarkers => draw_line(plot_ui, series, x0, x1, false, true, y_lo, &map, opts),
        Area => draw_line(plot_ui, series, x0, x1, true, false, y_lo, &map, opts),
        StepLine => draw_step(plot_ui, series, x0, x1, &map, opts),
        Baseline => draw_baseline(plot_ui, series, x0, x1, &map, opts),
        HlcArea => draw_hlc_area(plot_ui, series, x0, x1, &map, opts),
        Columns => draw_columns(plot_ui, series, x0, x1, y_lo, &map, opts),
        VolumeCandles => {
            // chart-perf T5: the CLOSED-bar max is cached (`ChartState::visible_vol_max`,
            // keyed by (lo, hi, cache_key)) instead of refolded every frame; `vis_now`
            // (computed by the caller before the bounds single-write, over this same
            // (x0,x1) range — unchanged since) is reused for the index window so this
            // doesn't re-derive `visible_slice`'s index math. The forming bar (index
            // `state.closed_len`) is excluded from the cache by design — added back here
            // with `.max()` exactly like today's inline fold over `bars` (closed +
            // forming) implicitly included it.
            let (vol_lo, vol_hi) = match (vis_now.first(), vis_now.last()) {
                (Some(f), Some(l)) => (f.t as usize, l.t as usize + 1),
                _ => (0, 0),
            };
            let forming_idx = state.closed_len;
            let forming_v =
                if state.bars.len() > forming_idx && (vol_lo..vol_hi).contains(&forming_idx) {
                    state.bars[forming_idx].v
                } else {
                    0.0
                };
            let vmax = state.visible_vol_max_shared(vol_lo, vol_hi).max(forming_v);
            draw_volume_candles(plot_ui, series, x0, x1, &map, opts, vmax);
        }
        Kagi => {
            // structured Kagi computed once inside the transform cache (chart-perf T6)
            if let Some(k) = state.cached_kagi_shared() {
                draw_kagi(plot_ui, k.as_ref(), &map, opts);
            }
        }
        PointFigure => {
            // structured P&F columns + box computed once inside the transform cache (T6)
            if let Some(pnf) = state.cached_pnf_shared() {
                draw_pnf(plot_ui, &pnf.0, pnf.1, &map, opts);
            }
        }
        // Footprint (SP2, T5): NOT a transform style (absent from the `owned` gate in the
        // caller), so `series` is just the raw bars here — draw straight off
        // `state.bars`/`footprint`, index-aligned per that field's doc. `of_tick_size`/the
        // visible-range derivation are shared with the volume-profile overlay
        // (`orderflow_tick_size`) so the two always bucket the same visible range identically.
        // Any reason cells can't be drawn this frame (no footprint data yet, or a
        // degenerate/off-history visible range) falls back to plain candles rather than
        // leaving the price pane blank.
        Footprint => {
            // `and_then` short-circuits on `footprint: None` — the common (default-off)
            // case — so `orderflow_index_bounds`/`orderflow_tick_size` only run when there's
            // actually footprint data to bucket. `(i0, i1)` is carried through the plan (not
            // just `fps`/`ts`) so `draw_footprint` can be windowed to it below (SP3 Task B
            // #3, SP2 final-review finding B) — the same visible bounds the profile overlay
            // uses, so both bucket an identical range.
            let plan = footprint.and_then(|fps| {
                orderflow_index_bounds(&state.bars, x0, x1).map(|(i0, i1)| {
                    (fps, i0, i1, orderflow_tick_size(&state.bars, i0, i1, of_tick_size))
                })
            });
            match plan {
                Some((fps, i0, i1, ts)) if ts > 0.0 => {
                    let cell_px = cell_px_for(plot_ui.transform(), ts);
                    draw_footprint(plot_ui, &state.bars, fps, &map, ts, opts, cell_px, i0, i1);
                }
                _ => {
                    // Footprint no-data fallback: same GPU-or-egui candle branch.
                    if let Some(build) = gpu_candles {
                        plot_ui.add(GpuCandleItem::new(lod().as_ref(), false, &map, opts, build));
                    } else {
                        draw_candles(plot_ui, lod().as_ref(), false, &map, opts);
                    }
                }
            }
        }
        // Candles, HeikinAshi, Renko, Range, LineBreak → candles on the transformed series
        _ => {
            // GPU-or-egui candle branch (see the Hollow arm above).
            if let Some(build) = gpu_candles {
                plot_ui.add(GpuCandleItem::new(lod().as_ref(), false, &map, opts, build));
            } else {
                draw_candles(plot_ui, lod().as_ref(), false, &map, opts);
            }
        }
    }
}

/// Volume-profile overlay (SP2, T4): a visible-range volume-at-price histogram + POC +
/// value-area band painted on the price pane, pinned to the RIGHT edge of the visible
/// x-range — was `draw`'s inline `if let (true, Some(fps)) = (profile_on, footprint) { .. }`
/// block (chart refactor PR-7), moved verbatim. Default-off — see the doc on
/// `ChartInputs::profile_on`/`footprint`; vike-app doesn't wire a real footprint substrate in
/// until Task 7, so this is dead code in production today (`footprint: None` at the one
/// construction site). Bar indices, not `series` (the possibly-transformed proxy for
/// Renko/Kagi/etc. styles): `footprint` is index-aligned to `state.bars` (see that field's
/// doc), same as the CVD pane (Task 3).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_volume_profile(
    plot_ui: &mut egui_plot::PlotUi,
    profile_on: bool,
    footprint: Option<&[FootprintBar]>,
    state: &ChartState,
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    of_tick_size: f64,
) {
    if let (true, Some(fps)) = (profile_on, footprint) {
        // Same asymmetric ceil/floor-then-clamp shape as `visible_ts_bounds` (i0 only
        // floored at 0, i1 only ceilinged at `last` — NOT both independently clamped to the
        // same side) so a visible window entirely off-history (panned past either edge)
        // correctly fails `i0f <= i1f` instead of aliasing onto bar 0 or the last bar.
        // `orderflow_index_bounds`/`orderflow_tick_size` (SP2, T5) are the same helpers the
        // Footprint style's dispatch arm uses, so the two always bucket an identical
        // visible range by an identical tick_size.
        if let Some((i0, i1)) = orderflow_index_bounds(&state.bars, x0, x1) {
            // tick_size: caller-pinned (`of_tick_size > 0.0`), else derived from the visible
            // price extent (~36 buckets across `state.bars[i0..=i1]`'s H/L range) — a
            // degenerate (zero-width/non-finite) extent yields no overlay this frame rather
            // than a zero/garbage bucket width.
            let ts = orderflow_tick_size(&state.bars, i0, i1, of_tick_size);
            if ts > 0.0 {
                let prof = state.visible_profile_shared(fps, ts, i0, i1);
                if !prof.bins.is_empty() {
                    // Histogram bars, right-pinned: base at the right x edge, each bar's
                    // span growing LEFT with volume, the biggest bin sized to ~18% of
                    // the visible x-width. `width` is the MAPPED half-tick span around
                    // the bin's price (not the raw `ts`) so bars stay one bucket wide in
                    // Log/Percent scale space too, not just Linear.
                    let max_total =
                        prof.bins.iter().fold(0.0_f64, |m, b| m.max(b.buy_vol + b.sell_vol));
                    if max_total > 0.0 {
                        let max_span = (x1 - x0).max(0.0) * 0.18;
                        let half = ts / 2.0;
                        let bars: Vec<egui_plot::Bar> = prof
                            .bins
                            .iter()
                            .map(|b| {
                                let span = (b.buy_vol + b.sell_vol) / max_total * max_span;
                                let width = (map(b.price + half) - map(b.price - half)).abs();
                                egui_plot::Bar::new(map(b.price), -span)
                                    .base_offset(x1)
                                    .width(width)
                            })
                            .collect();
                        plot_ui.bar_chart(
                            egui_plot::BarChart::new("vprofile", bars)
                                .horizontal()
                                .color(PROFILE_COLOR)
                                .allow_hover(false),
                        );
                    }
                    plot_ui.hline(
                        HLine::new("", map(prof.poc))
                            .color(PROFILE_POC_COLOR)
                            .width(1.0)
                            .allow_hover(false),
                    );
                    let (va_lo, va_hi) = prof.value_area;
                    plot_ui.polygon(
                        egui_plot::Polygon::new(
                            "",
                            vec![
                                [x0, map(va_lo)],
                                [x1, map(va_lo)],
                                [x1, map(va_hi)],
                                [x0, map(va_hi)],
                            ],
                        )
                        .fill_color(PROFILE_VA_COLOR)
                        .allow_hover(false),
                    );
                }
            }
        }
    }
}

/// The five small price-pane overlay paints, IN THIS EXACT SUB-ORDER (z-order is
/// load-bearing): indicator overlays -> C2a %-lines (`overlay_lines`) -> secondary-axis
/// lines (`secondary_lines`) -> user drawings (`state.overlays`) -> last-price dashed hline.
/// Was `draw`'s five inline blocks immediately after the series paint dispatch (chart
/// refactor PR-7), moved verbatim. `overlay_lines` comes from the caller's still-inline
/// %-compare-overlay block (computed BEFORE the bounds single-write, for the auto_y fold);
/// `secondary_lines` comes from [`compute_secondary_axis_lines`] (this module). Both are
/// already in plot-space (`overlay_lines` shares the primary's Percent space, `secondary_lines`
/// is pre-remapped into `[ty0, ty1]`), so neither is re-mapped here — only `state.overlays`
/// (raw user-drawn points) and the last-price close are mapped through `map`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_price_overlays(
    plot_ui: &mut egui_plot::PlotUi,
    indicators: &[Active],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    overlay_lines: &[(Color32, Vec<f64>, usize)],
    secondary_lines: &[(Color32, Vec<f64>, usize)],
    shared_lines: &[(Color32, Vec<f64>, usize)],
    state: &ChartState,
    show_last_price: bool,
    up_s_col: Color32,
    down_s_col: Color32,
) {
    // indicator overlays (lines / bands / dots on the price scale)
    for a in indicators.iter().filter(|a| a.visible && a.is_overlay()) {
        render_overlay(plot_ui, a, x0, x1, &map, &state.bars);
    }

    // C2a compare overlays: one %-line per series, drawn AFTER the primary candles
    // + indicator overlays. `seg_line` breaks the line at NaN gaps and applies the
    // same ±2-bar visible filter the indicator overlays use; the values are already
    // in the shared Percent plot-space (NOT re-mapped). EMPTY ⇒ nothing drawn.
    for (color, pvs, base) in overlay_lines {
        seg_line(plot_ui, pvs, *color, 1.5, egui_plot::LineStyle::Solid, x0, x1, *base);
    }

    // C2b Task 7: secondary-axis (Right) overlays — already remapped into the
    // primary's plot-space ([ty0, ty1]), so `seg_line` plots them directly on
    // their own scale (the matching labeled right price gutter is built in
    // `chart::draw`'s Plot builder). EMPTY ⇒ nothing drawn (byte-identical).
    for (color, pvs, base) in secondary_lines {
        seg_line(plot_ui, pvs, *color, 1.5, egui_plot::LineStyle::Solid, x0, x1, *base);
    }

    // Absolute-shared-axis overlays: closes already `map`-ped into the PRIMARY's plot-space
    // (Linear/Log), so `seg_line` plots them directly on the SAME absolute-price axis as the
    // primary candles — no re-map. EMPTY ⇒ nothing drawn (byte-identical default).
    for (color, pvs, base) in shared_lines {
        seg_line(plot_ui, pvs, *color, 1.5, egui_plot::LineStyle::Solid, x0, x1, *base);
    }

    for (name, pts) in &state.overlays {
        let pts: Vec<[f64; 2]> = pts
            .iter()
            .copied()
            .filter(|p| p[0] >= x0 - 1.0 && p[0] <= x1 + 1.0)
            .map(|p| [p[0], map(p[1])])
            .collect();
        if pts.len() >= 2 {
            plot_ui.line(egui_plot::Line::new(name.clone(), PlotPoints::from(pts)).width(1.5));
        }
    }

    if let (true, Some(lastbar)) = (show_last_price, state.bars.last()) {
        let col = if lastbar.c >= lastbar.o { up_s_col } else { down_s_col };
        plot_ui.hline(
            HLine::new("", map(lastbar.c))
                .color(col) // direction-colored, matching vike's live last-price line
                .width(1.0)
                .style(egui_plot::LineStyle::dashed_loose())
                .allow_hover(false),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::SeriesInput;
    use crate::model::ChartState;

    /// A closed [`ChartState`] over `closes` on a fixed 1-minute ot grid starting at `base`.
    /// `l`/`h` bracket each close by ±1.0 so the visible OHLC extent is distinguishable from
    /// the close line.
    fn st(base: i64, closes: &[f64]) -> ChartState {
        let mut s = ChartState::default();
        s.bars = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                t: i as f64,
                ot: base + i as i64 * 60_000,
                o: c,
                h: c + 1.0,
                l: c - 1.0,
                c,
                v: 1.0,
            })
            .collect();
        s.closed_len = closes.len();
        s.refresh_caches();
        s
    }

    fn scale_map(sym: &str, a: ScaleAssign) -> IndexMap<String, ScaleAssign> {
        let mut m = IndexMap::new();
        m.insert(sym.to_string(), a);
        m
    }

    /// Linear mode: a SharedLinear compare's line = its ABSOLUTE closes (map is identity),
    /// reindexed by ot onto the primary's index domain; raw_ext = the compare's visible
    /// low/high (NOT the primary's).
    #[test]
    fn shared_axis_linear_maps_absolute_closes_and_reports_raw_extent() {
        let prim = st(1_700_000_000_000, &[100.0, 101.0, 102.0, 103.0]);
        // Same ot grid so reindex is identity; a HIGHER magnitude so the extent clearly
        // expands beyond the primary's [99, 104].
        let cmp = st(1_700_000_000_000, &[200.0, 205.0, 210.0, 208.0]);
        let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::RED }];
        let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
        let scales = scale_map("CMP", ScaleAssign::SharedLinear);
        let map = |y: f64| y; // Linear identity
        let mut legend = Vec::new();
        let out = compute_shared_axis_lines(
            &prim.bars,
            &overlays,
            &empty_panes,
            &scales,
            0.0,
            3.0,
            ScaleMode::Linear,
            &map,
            &mut legend,
        );
        assert_eq!(out.lines.len(), 1, "one shared line");
        let (color, pvs, base) = &out.lines[0];
        assert_eq!(*color, Color32::RED);
        assert_eq!(*base, 0);
        // absolute closes, mapped (identity)
        assert_eq!(pvs, &vec![200.0, 205.0, 210.0, 208.0]);
        // raw_ext = compare visible low (200-1) .. high (210+1)
        assert_eq!(out.raw_ext, Some((199.0, 211.0)));
        // legend carries the absolute LAST close (208.0) as a PRICE (is_pct=false)
        assert_eq!(legend, vec![(Color32::RED, "CMP".to_string(), 208.0, false)]);
    }

    /// Log mode: the line values are `log10(close)`, and a non-positive low is excluded from
    /// the raw extent so it can never inject a NaN into the primary's autofit bounds.
    #[test]
    fn shared_axis_log_uses_log10_and_drops_nonpositive_low_from_extent() {
        let prim = st(1_700_000_000_000, &[100.0, 100.0, 100.0]);
        // A compare whose FIRST bar's low would be <= 0 (c=0.5 → l=-0.5): its mapped low is
        // NaN, so it must be dropped from raw_ext; its close line still plots log10(0.5).
        let cmp = st(1_700_000_000_000, &[0.5, 10.0, 100.0]);
        let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::GREEN }];
        let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
        let scales = scale_map("CMP", ScaleAssign::SharedLinear);
        let map = |y: f64| y.log10();
        let mut legend = Vec::new();
        let out = compute_shared_axis_lines(
            &prim.bars,
            &overlays,
            &empty_panes,
            &scales,
            0.0,
            2.0,
            ScaleMode::Log,
            &map,
            &mut legend,
        );
        let (_, pvs, _) = &out.lines[0];
        // closes mapped through log10 (bar 0 close = 0.5 → negative, still finite)
        assert!((pvs[0] - 0.5_f64.log10()).abs() < 1e-12);
        assert!((pvs[2] - 100.0_f64.log10()).abs() < 1e-12);
        // raw_ext excludes bar 0 (l=-0.5 maps to NaN); bars 1&2 give l in [9, 99], h in [11, 101]
        let (lo, hi) = out.raw_ext.unwrap();
        assert_eq!((lo, hi), (9.0, 101.0));
        assert!(lo.is_finite() && hi.is_finite());
    }

    /// Percent/Indexed primary mode: a SharedLinear compare renders NOTHING (absolute price
    /// has no mapping onto a rebased axis) — empty lines, no extent, no legend.
    #[test]
    fn shared_axis_is_noop_in_percent_and_indexed_mode() {
        let prim = st(1_700_000_000_000, &[100.0, 101.0]);
        let cmp = st(1_700_000_000_000, &[200.0, 205.0]);
        let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::RED }];
        let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
        let scales = scale_map("CMP", ScaleAssign::SharedLinear);
        let map = |y: f64| y;
        for mode in [ScaleMode::Percent, ScaleMode::Indexed] {
            let mut legend = Vec::new();
            let out = compute_shared_axis_lines(
                &prim.bars,
                &overlays,
                &empty_panes,
                &scales,
                0.0,
                1.0,
                mode,
                &map,
                &mut legend,
            );
            assert!(out.lines.is_empty(), "{mode:?}: no shared line");
            assert_eq!(out.raw_ext, None);
            assert!(legend.is_empty());
        }
    }

    /// Non-SharedLinear pins (the default Percent, plus Right) are skipped by the shared path,
    /// and a symbol moved to its own pane is skipped too ⇒ empty result (byte-identical default).
    #[test]
    fn shared_axis_skips_non_sharedlinear_and_own_pane_symbols() {
        let prim = st(1_700_000_000_000, &[100.0, 101.0]);
        let cmp = st(1_700_000_000_000, &[200.0, 205.0]);
        let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::RED }];
        let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
        let map = |y: f64| y;
        // default (absent) ⇒ Percent, Right ⇒ secondary — both skipped here.
        for a in [ScaleAssign::Percent, ScaleAssign::Right, ScaleAssign::Left] {
            let scales = scale_map("CMP", a);
            let mut legend = Vec::new();
            let out = compute_shared_axis_lines(
                &prim.bars,
                &overlays,
                &empty_panes,
                &scales,
                0.0,
                1.0,
                ScaleMode::Linear,
                &map,
                &mut legend,
            );
            assert!(out.lines.is_empty(), "{a:?} must not render on the shared axis");
            assert_eq!(out.raw_ext, None);
        }
        // Even a SharedLinear pin is skipped when the symbol has its own pane.
        let mut own_pane: IndexMap<String, PaneKey> = IndexMap::new();
        own_pane.insert("CMP".to_string(), PaneKey::Series(0));
        let scales = scale_map("CMP", ScaleAssign::SharedLinear);
        let mut legend = Vec::new();
        let out = compute_shared_axis_lines(
            &prim.bars,
            &overlays,
            &own_pane,
            &scales,
            0.0,
            1.0,
            ScaleMode::Linear,
            &map,
            &mut legend,
        );
        assert!(out.lines.is_empty(), "own-pane symbol is not a price-pane shared overlay");
    }

    /// Empty overlays ⇒ empty result (the default at every non-compare call site).
    #[test]
    fn shared_axis_empty_overlays_is_noop() {
        let prim = st(1_700_000_000_000, &[100.0, 101.0]);
        let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
        let empty_scales: IndexMap<String, ScaleAssign> = IndexMap::new();
        let map = |y: f64| y;
        let mut legend = Vec::new();
        let out = compute_shared_axis_lines(
            &prim.bars,
            &[],
            &empty_panes,
            &empty_scales,
            0.0,
            1.0,
            ScaleMode::Linear,
            &map,
            &mut legend,
        );
        assert!(out.lines.is_empty());
        assert_eq!(out.raw_ext, None);
        assert!(legend.is_empty());
    }

    /// The secondary-axis path must NOT emit a line for a SharedLinear symbol (the skip
    /// tightening) — else the compare would double-render on both the primary and a secondary
    /// axis. A Right pin on a DIFFERENT symbol still renders, proving the path still works.
    #[test]
    fn secondary_axis_skips_sharedlinear_but_still_handles_right() {
        let prim = st(1_700_000_000_000, &[100.0, 101.0, 102.0]);
        let cmp_shared = st(1_700_000_000_000, &[200.0, 205.0, 210.0]);
        let cmp_right = st(1_700_000_000_000, &[50.0, 55.0, 60.0]);
        let overlays = [
            SeriesInput { symbol: "SHARED", state: &cmp_shared, color: Color32::RED },
            SeriesInput { symbol: "RIGHT", state: &cmp_right, color: Color32::BLUE },
        ];
        let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
        let mut scales: IndexMap<String, ScaleAssign> = IndexMap::new();
        scales.insert("SHARED".to_string(), ScaleAssign::SharedLinear);
        scales.insert("RIGHT".to_string(), ScaleAssign::Right);
        let mut legend = Vec::new();
        let sec = Cell::new((0.0, 0.0, 0.0, 0.0));
        let out = compute_secondary_axis_lines(
            &prim.bars,
            &overlays,
            &empty_panes,
            &scales,
            0.0,
            2.0,
            0.0,
            10.0,
            &mut legend,
            &sec,
        );
        // exactly ONE secondary line — the Right symbol; SHARED (SharedLinear) is absent.
        assert_eq!(out.len(), 1, "only the Right pin rides the secondary axis");
        assert_eq!(out[0].0, Color32::BLUE);
        assert!(legend.iter().all(|(_, s, _, _)| s == "RIGHT"));
    }
}
