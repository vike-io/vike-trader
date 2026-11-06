//! The price pane's `plot.show` content closure (x range, scale, bounds, paints) and the sub-panes.

use super::extent::{map_pad_view, y_raw_ext};
use super::fmt::fmt_datetime;
use super::overlay::Fit;
use super::style::ChartStyle;
use super::tick_guard::TickGuard;
use super::{Nav, SeriesInput, price_render, subpanes};
use crate::indicators::Active;
use crate::interact::{
    AutoYEvent, FollowLive, follow_shift, next_auto_y, resolve_ty, visible_slice, x_drag_zoom,
    y_drag_zoom,
};
use crate::model::{Bar, ChartState};
use crate::options::IndicatorDialog;
use crate::panes::{MoveTarget, PaneFractions, PaneKey};
use crate::render::oscillator::reindex_by_ot;
use crate::render::overlays::{fold_overlay_extent, overlay_visible_range};
use crate::scale::{self, ScaleAssign, ScaleMode, ScaleView};
use crate::sync::{SyncIn, nearest_index_by_ts, ts_range_to_index_bounds};
use egui::{Color32, Vec2};
use std::cell::Cell;
use vike_ui_theme::value::chart;

/// What the price pane's content closure hands back to [`draw`]: every field is fire-once and read
/// after `plot.show` returns — the declare-before / mutate-inside pattern the closure always used,
/// with the locals gathered into one value so the closure's three steps can share them.
#[derive(Default)]
pub(super) struct PriceOut {
    /// Set when the requested `scale` was downgraded this frame by the sticky fallback.
    pub(super) scale_fallback: Option<&'static str>,
    pub(super) hovered: Option<[f64; 4]>,
    pub(super) hover_xt: Option<(f64, i64)>,
    /// Sync seam (task B7) behavior 4: the ghost crosshair's `(t, ot)` — no local hover this frame
    /// (`hover_xt` is `None`) but the sync leader has one, mirrored via `nearest_index_by_ts` over
    /// the SAME rendered `series` this pane drew. Resolved in the closure, not after `show`, so
    /// the time axis' tick-label guard knows the ghost's time tag before it formats its labels.
    pub(super) ghost_xt: Option<(f64, i64)>,
    pub(super) hover_y: Option<f64>,
    /// C2a/C2b compare overlays: (color, symbol, last-visible value, is_percent) per
    /// overlay, filled inside the price closure and read by the OHLC legend after
    /// `.show()` returns. `is_percent == true` ⇒ the value is a %-change (shared-% overlay,
    /// C2a) rendered as `+d.dd%`; `false` ⇒ an ABSOLUTE close (a Pin-to-Right overlay,
    /// Task 7 review Minor-2) rendered as a plain price — a compact readout of the
    /// Right line's latest value alongside its now-labeled secondary axis. Stays EMPTY
    /// when `overlays` is empty → the legend is unchanged.
    pub(super) overlay_legend: Vec<(Color32, String, f64, bool)>,
    pub(super) interacted: bool,
}

/// Everything the price pane's content closure READS but never mutates — the frame's inputs, the
/// resolved default bounds and the shared cells. All `Copy` (shared references and scalars), the
/// shape [`subpanes::SubPaneCtx`] has, so `draw` builds it once and each step destructures what it
/// uses.
#[derive(Clone, Copy)]
pub(super) struct PricePane<'a> {
    pub(super) state: &'a ChartState,
    pub(super) style: ChartStyle,
    pub(super) series: &'a [Bar],
    /// `owned.is_none()` — the raw (untransformed) series gate for the cached-`y_ext` fast path.
    pub(super) owned_is_none: bool,
    pub(super) indicators: &'a [Active],
    pub(super) overlays: &'a [SeriesInput<'a>],
    pub(super) series_pane_of: &'a indexmap::IndexMap<String, PaneKey>,
    pub(super) series_scale: &'a indexmap::IndexMap<String, ScaleAssign>,
    pub(super) nav_in: Option<Nav>,
    pub(super) sync: Option<SyncIn>,
    pub(super) chart_rect: egui::Rect,
    pub(super) fx_min: f64,
    pub(super) fx_max: f64,
    pub(super) fy_min: f64,
    pub(super) fy_max: f64,
    pub(super) y_lo: f64,
    pub(super) last_t: Option<f64>,
    pub(super) requested_scale: ScaleMode,
    pub(super) requested_invert: bool,
    pub(super) margin_top: f32,
    pub(super) margin_bottom: f32,
    pub(super) auto_y: bool,
    pub(super) c: &'a crate::colors::ChartColors,
    pub(super) color_bars_prev_close: bool,
    pub(super) gpu_candles:
        Option<&'a dyn Fn(Vec<crate::render::candles::CandleInstance>, egui::Rect) -> egui::Shape>,
    pub(super) footprint: Option<&'a [vike_orderflow::FootprintBar]>,
    pub(super) of_tick_size: f64,
    pub(super) profile_on: bool,
    pub(super) show_last_price: bool,
    pub(super) scale_cell: &'a Cell<(ScaleView, f64)>,
    pub(super) sec_axis_cell: &'a Cell<(f64, f64, f64, f64)>,
    /// The price axis' and the time axis' tick-label guards: this closure tells them which chips
    /// `draw` is about to paint (see [`TickGuard::set_chips`]).
    pub(super) y_guard: &'a TickGuard,
    pub(super) x_guard: &'a TickGuard,
}

/// What the price pane's scale/overlay step resolves for the bounds-and-paint step: the visible
/// slice, the Percent anchor, the effective mode and view, and the C2a %-normalized compare lines.
pub(super) struct PriceScale<'a> {
    vis_now: &'a [Bar],
    anchor: f64,
    eff_mode: ScaleMode,
    view: ScaleView,
    overlay_lines: Vec<(Color32, Vec<f64>, usize)>,
}

/// The sub-panes' fire-once outputs, harvested into [`ChartActions`] by [`draw`].
#[derive(Default)]
pub(super) struct SubPaneOut {
    pub(super) volume_remove: bool,
    pub(super) cvd_toggle: bool,
    pub(super) remove_uid: Option<u64>,
    /// C1 Task 4: the study uid + destination the user picked from a pane header's
    /// ••• "Move to" menu this frame (fire-once, last click wins).
    pub(super) move_study: Option<(u64, MoveTarget)>,
    /// Feature #1: the pane ↑/↓ reorder the user picked from ANY reorderable sub-pane
    /// header this frame (or `None`) — harvested into `ChartActions::reorder_pane`.
    pub(super) reorder_pane: Option<(PaneKey, bool)>,
}

/// The price pane's `plot.show` content closure, as its three steps in the original order: the x
/// range (interaction), the scale and compare overlays, then the one bounds write and the paints.
pub(super) fn price_pane<'a>(
    plot_ui: &mut egui_plot::PlotUi<'a>,
    pp: &PricePane<'a>,
    follow: &mut FollowLive,
    po: &mut PriceOut,
) {
    let (b0, cx0, cx1, x_dragged) = price_x_range(plot_ui, pp, follow, po);
    let ps = price_scale_and_overlays(pp, follow, po, cx0, cx1);
    price_bounds_and_paint(plot_ui, pp, follow, po, ps, b0, cx0, cx1, x_dragged);
}

/// Price pane, step 1 — the tracked x range `(cx0, cx1)` for this frame: follow-live and history
/// seeds, the nav buttons, wheel zoom/pan, the bottom gutter's queued drag and the sync leader's
/// injected range. Returns the frame-start bounds, the range and whether a gutter drag applied.
pub(super) fn price_x_range(
    plot_ui: &mut egui_plot::PlotUi<'_>,
    pp: &PricePane<'_>,
    follow: &mut FollowLive,
    po: &mut PriceOut,
) -> (egui_plot::PlotBounds, f64, f64, bool) {
    let PricePane { series, nav_in, last_t, fx_min, fx_max, chart_rect, sync, .. } = *pp;
    // CRITICAL egui_plot semantics: set_plot_bounds is DEFERRED — it queues a
    // modification applied after this closure, while plot_bounds() keeps
    // returning the frame-START bounds. So the desired x-range is tracked
    // explicitly in (cx0, cx1) across the steps below, and every bounds write
    // uses it; reading plot_bounds() after a set would silently re-impose the
    // stale range (the "one giant bar" bug).
    let b0 = plot_ui.plot_bounds();
    let (mut cx0, mut cx1) = (b0.min()[0], b0.max()[0]);
    let len_now = series.len();

    // Sync seam (task B7) behavior 3: this frame's interaction signal,
    // resolved as early as possible in the closure so behavior 5 below
    // can gate on it. `plot_ui.response()` is set at `PlotUi`
    // construction (before this closure runs), so it already reads the
    // SAME value `resp.response` exposes once `.show()` returns further
    // down — no one-frame lag needed. `follow.pending_{x,y}_drag`
    // likewise already hold whatever a gutter interact rect queued LAST
    // frame, unconsumed at this point (the two blocks below read+reset
    // them). NOTE: this egui/egui_plot pin has no `raw_scroll_delta`
    // field — `smooth_scroll_delta` is the scroll signal egui_plot
    // itself reads for its own wheel-pan (see plot.rs's `allow_scroll`
    // handling) and what the Trade window's ladder scroll keys off
    // (`vike_panels::trade::ladder`, as the deleted DOM's did).
    let (scroll, zoom_delta) = plot_ui.ctx().input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
    po.interacted = plot_ui.response().dragged()
        || (plot_ui.response().hovered() && (scroll != Vec2::ZERO || zoom_delta != 1.0))
        || nav_in.is_some()
        || follow.pending_x_drag != 0.0
        || follow.pending_y_drag != 0.0;

    // A history SEED (first bars, a backfill jump, or a shrink on symbol
    // swap) re-establishes the Reset view: the live WS forming bar can render
    // frames before the REST backfill lands, and follow-shift would otherwise
    // "preserve" that 2-bar-wide pre-seed view forever. Live appends move at
    // most a bar or two per frame — those get the pinned-edge shift. The
    // reset stays pending (re-asserted) until the post-frame bounds confirm
    // it landed.
    if len_now > 0
        && (follow.last_len == 0 || len_now > follow.last_len + 2 || len_now < follow.last_len)
    {
        follow.pending_seed = true;
        // Final-review fix: a symbol/timeframe swap must not carry a
        // STICKY scale-fallback latch (interact.rs `resolve_scale`) over
        // from the old series — otherwise a chart that latched to Linear
        // on the old symbol stays Linear on the new one with a stale
        // hint until manually re-toggled. `resolve_scale` (below, this
        // frame) re-evaluates the requested mode from scratch against
        // the NEW data and re-latches only if that data genuinely fails.
        follow.fallback_latched = false;
    }
    if follow.pending_seed {
        cx0 = fx_min;
        cx1 = fx_max;
        // T5: a reload abandons whatever the user was doing in the OLD
        // domain (symbol/timeframe) — auto-fit resumes rather than
        // stranding a manual y-range; the actual y-write forcing lives at
        // `force_fresh_ty`/`resolve_ty` below (this flip only affects
        // FUTURE frames, since `auto_y` above was captured before this
        // closure ran — same one-frame-lag pattern as the gutter dbl-click).
        follow.auto_y = next_auto_y(follow.auto_y, AutoYEvent::Seed);
    } else if follow.on && len_now > follow.last_len {
        // follow-live: bars appended → keep the view pinned to the right edge
        if let Some(lt) = last_t {
            (cx0, cx1) = follow_shift(cx0, cx1, lt);
        }
    }
    follow.last_len = len_now;

    // nav buttons operate on the tracked range (not deferred zoom helpers)
    if let Some(n) = nav_in {
        let w = cx1 - cx0;
        let c = (cx0 + cx1) / 2.0;
        match n {
            Nav::ZoomIn => {
                cx0 = c - w / (2.0 * 1.3);
                cx1 = c + w / (2.0 * 1.3);
            }
            Nav::ZoomOut => {
                cx0 = c - w * 1.3 / 2.0;
                cx1 = c + w * 1.3 / 2.0;
            }
            Nav::PanLeft => {
                cx0 -= w * 0.2;
                cx1 -= w * 0.2;
            }
            Nav::PanRight => {
                cx0 += w * 0.2;
                cx1 += w * 0.2;
            }
            Nav::Reset => {
                cx0 = fx_min;
                cx1 = fx_max;
                follow.on = true;
                // T5: Nav::Reset is shared by the ⟳ Reset nav button and
                // the bottom-gutter double-click — both re-engage auto_y
                // (design spec §2: "Double-click price axis / Auto:
                // re-engage auto_y").
                follow.auto_y = next_auto_y(follow.auto_y, AutoYEvent::Reset);
            }
            Nav::AutoY => {
                // Final-review fix: the price-scale "Auto" button is
                // Y-ONLY per the TradingView oracle — re-fit the y-axis
                // without touching horizontal pan/zoom (cx0/cx1) or
                // follow-live (`follow.on`). Setting `auto_y` true here
                // is enough; the normal autofit block below re-fits y
                // next frame — no direct bounds write here.
                follow.auto_y = next_auto_y(follow.auto_y, AutoYEvent::Reset);
            }
        }
    }

    // Feature #3 (TradingView parity): wheel / two-finger zoom. egui_plot's
    // own scroll-pan is disabled above, so here a VERTICAL scroll zooms the
    // time axis around the cursor (like TradingView's wheel), and a
    // HORIZONTAL two-finger scroll pans. Pinch still zooms via `allow_zoom`.
    // Any scroll is a manual interaction, so follow-live releases (like a
    // drag/nav); `interacted` already counts this scroll (see above).
    // Feature #3 fix: fire when the pointer is over ANY pane (price OR an
    // indicator sub-pane), not only the price plot — so scroll-zoom works
    // over the indicator panes too. Center falls back to the view middle
    // when `pointer_coordinate()` is None (pointer over a sub-pane).
    let hover_in_chart =
        plot_ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| chart_rect.contains(p));
    if hover_in_chart && scroll != Vec2::ZERO {
        let w = cx1 - cx0;
        if scroll.y != 0.0 && w > 0.0 {
            // Zoom center = cursor's time index (fall back to view center).
            let center = plot_ui
                .pointer_coordinate()
                .map(|p| p.x)
                .filter(|x| x.is_finite())
                .unwrap_or(cx0 + w / 2.0)
                .clamp(cx0, cx1);
            // Scroll up (positive Δy) → zoom IN. Clamp one event to ≤2× and
            // never zoom past ~3 bars wide.
            let factor = (1.0 - 0.0015 * scroll.y as f64).clamp(0.5, 2.0);
            let new_w = (w * factor).max(3.0);
            let left_frac = (center - cx0) / w;
            cx0 = center - new_w * left_frac;
            cx1 = cx0 + new_w;
            follow.on = false;
        }
        if scroll.x != 0.0 {
            // Horizontal two-finger scroll pans the time axis.
            let px_w = plot_ui.response().rect.width().max(1.0) as f64;
            let dx = -(scroll.x as f64) / px_w * (cx1 - cx0);
            cx0 += dx;
            cx1 += dx;
            follow.on = false;
        }
    }

    // Chart-UX bundle T4: apply the bottom gutter's queued x-drag (from the
    // interact rect added AFTER `plot.show` LAST frame — see `bottom_frame`
    // below) to THIS frame's tracked x-range. Native axis-zoom-drag is
    // disabled on every pane above, so this is now the only way a gutter
    // drag moves the x-range; `x_drag_zoom` pins the right (live) edge
    // exactly, so this never fights follow-live.
    let x_dragged = follow.pending_x_drag != 0.0;
    if x_dragged {
        (cx0, cx1) = x_drag_zoom(cx0, cx1, follow.pending_x_drag);
        follow.pending_x_drag = 0.0;
    }

    // Sync seam (task B7) behavior 5: an inbound range from the sync
    // leader. X-only (like a manual pan/nav), applied at the same stage
    // so scale resolution + y-autofit below resolve against the NEW
    // range — and late enough to override a same-frame follow-live shift
    // (the whole point: a follower window tracks the leader's range even
    // while it would otherwise auto-follow-live locally). Skipped
    // entirely on any frame this window itself interacted (a live local
    // pan/zoom/nav/gutter-drag always wins over a racing/stale injected
    // range) — `interacted` was already resolved above, before this
    // frame's Nav/gutter-drag application, so it reflects exactly that.
    if !po.interacted
        && let Some((rx0, rx1)) =
            sync.and_then(|s| s.range_ts).and_then(|r| ts_range_to_index_bounds(series, r))
        && ((rx0 - cx0).abs() > 1e-9 || (rx1 - cx1).abs() > 1e-9)
    {
        cx0 = rx0;
        cx1 = rx1;
        follow.on = false; // identical semantics to a manual pan
    }
    (b0, cx0, cx1, x_dragged)
}

/// Price pane, step 2 — the frame's effective scale mode, view and Percent anchor (resolved ONCE
/// from the just-finalized range, sticky fallback included), then the C2a %-normalized compare
/// overlays built in that scale's plot space.
pub(super) fn price_scale_and_overlays<'a>(
    pp: &PricePane<'a>,
    follow: &mut FollowLive,
    po: &mut PriceOut,
    cx0: f64,
    cx1: f64,
) -> PriceScale<'a> {
    let PricePane {
        state,
        series,
        owned_is_none,
        overlays,
        series_pane_of,
        series_scale,
        requested_scale,
        requested_invert,
        scale_cell,
        ..
    } = *pp;
    // Chart-UX bundle T2: resolve the EFFECTIVE scale mode + Percent anchor
    // ONCE per frame, from THIS frame's just-finalized (cx0, cx1) — before
    // any mapped use below (autofit, single-write, series drawing, axis
    // labels via `scale_cell`). STICKY fallback (spec §1, latched in
    // `FollowLive::resolve_scale`): once the visible data fails
    // `supports()` for the requested Log/Percent, the chart stays Linear
    // — even across later pans back over supported data — until the user
    // re-toggles the requested mode. Never a per-frame flip.
    let vis_now = visible_slice(series, cx0, cx1);
    let raw_lo_for_mode = y_raw_ext(vis_now).map(|(lo, _)| lo).unwrap_or(f64::NAN);
    // Percent anchor = close of the first VISIBLE bar, index from cx0 (same
    // truncating index math the autofit overlay fold below uses), clamped
    // to a CLOSED bar — never the still-forming last bar, whose price
    // mutates every tick.
    let closed_n = if owned_is_none { state.closed_len.min(series.len()) } else { series.len() };
    // T3 carry-over fix: NaN (not 0.0) signals "no closed bar yet" to
    // `resolve_scale`'s latch — distinct from a genuine zero-close bar
    // from real data, which DOES latch (`scale::has_data`). Using 0.0
    // here would be indistinguishable from that genuine-zero case.
    // Hoisted out of the `else` so the C2a overlay below can rebase at the SAME bar.
    // `saturating_sub` is byte-identical to the old `closed_n - 1` whenever this index
    // is actually read (closed_n != 0), and can't underflow when hoisted past that guard.
    let first_idx = (cx0.max(0.0) as usize).min(closed_n.saturating_sub(1));
    let anchor =
        if closed_n == 0 { f64::NAN } else { series.get(first_idx).map(|b| b.c).unwrap_or(0.0) };
    let eff_mode = follow.resolve_scale(requested_scale, raw_lo_for_mode, anchor);
    // Invert is orthogonal to the fallback latch (always "supported"), so it
    // rides through unchanged onto the resolved effective mode.
    let view = ScaleView::new(eff_mode, requested_invert);
    scale_cell.set((view, anchor));
    // Hint only when genuinely LATCHED (real data failed the mode). A
    // transient no-data frame also degrades eff_mode but self-heals on the
    // next frame with data — hinting there would be wrong on both cause
    // ("non-positive prices") and remedy ("until re-selected").
    po.scale_fallback = if !follow.fallback_latched {
        None
    } else {
        Some(match requested_scale {
            ScaleMode::Log => {
                "log scale unavailable (non-positive prices) \u{2014} showing Linear until re-selected"
            }
            ScaleMode::Percent => {
                "percent scale unavailable (no anchor price) \u{2014} showing Linear until re-selected"
            }
            ScaleMode::Indexed => {
                "indexed scale unavailable (no anchor price) \u{2014} showing Linear until re-selected"
            }
            // resolve_scale only ever latches Log/Percent/Indexed; Linear always supports.
            ScaleMode::Linear => unreachable!("Linear never falls back"),
        })
    };
    // --- C2a %-normalized compare overlays (empty ⇒ this whole block is a no-op) ---
    // For each overlay series: reindex its bars onto the PRIMARY's visible index
    // domain by open-time (T1, floor/as-of), rebase to its OWN first-visible close as
    // 0% (T2 `pct_change`), and carry the per-index %-values (gaps = NaN, which
    // `seg_line` breaks the line at). These %-values live directly in the primary's
    // Percent plot-space — the primary's own candles map through `pct_change` too
    // (`ScaleMode::Percent::map`), so the two share one % axis WITHOUT re-wrapping
    // through `map` (that carries the PRIMARY's anchor — double-rebasing a 2nd symbol
    // to it is meaningless). Gated on `eff_mode == Percent`: in Linear/Log a %-line
    // can't share the primary's raw/log axis, and (per the design) overlays are only
    // meaningful in compare mode. Built once here for BOTH the autofit fold (below,
    // pre-`set_plot_bounds`) and the render pass (after the candles) so the two agree.
    let overlay_lines: Vec<(Color32, Vec<f64>, usize)> =
        if eff_mode == ScaleMode::Percent && !overlays.is_empty() {
            let (olo, ohi) = overlay_visible_range(series.len(), cx0, cx1);
            let mut out = Vec::with_capacity(overlays.len());
            for s in overlays {
                // C2b: a compare symbol moved to its OWN pane is NOT a price-pane
                // %-overlay (it renders as an absolute-price line in the series
                // sub-pane loop instead) — skip it here so it never double-renders.
                // On the default empty `series_pane_of` this skips nothing, so the
                // overlay build (render + autofit + legend) is byte-identical.
                if series_pane_of.contains_key(s.symbol) {
                    continue;
                }
                // C2b Task 7: a Right/Left-pinned overlay renders on its OWN
                // absolute axis (the secondary block after ty0/ty1 below), not
                // as a %-line here — skip it. Default (empty `series_scale`) ⇒
                // every overlay is `Percent` ⇒ skips nothing ⇒ byte-identical.
                if !matches!(
                    series_scale.get(s.symbol).copied().unwrap_or_default(),
                    ScaleAssign::Percent
                ) {
                    continue;
                }
                let reidx = reindex_by_ot(series, &s.state.bars, olo, ohi);
                // Overlay anchor = the overlay bar aligned (by ot) to the PRIMARY's own
                // anchor bar (`first_idx`), so both series read 0% at the identical
                // left-edge bar rather than the ~2-bar-earlier render-window edge (`olo`).
                // `reidx` is indexed from `olo`; fall back to the first non-None entry when
                // the overlay has a leading gap at the primary's anchor bar (or all-None).
                let anchor_off = first_idx.saturating_sub(olo).min(reidx.len().saturating_sub(1));
                let Some(a_idx) = reidx
                    .get(anchor_off)
                    .copied()
                    .flatten()
                    .or_else(|| reidx.iter().flatten().next().copied())
                else {
                    continue;
                };
                let oa = s.state.bars[a_idx].c;
                let pvs: Vec<f64> = reidx
                    .iter()
                    .map(|o| match o {
                        Some(j) => scale::pct_change(s.state.bars[*j].c, oa),
                        None => f64::NAN,
                    })
                    .collect();
                // Legend value = the LAST visible (non-gap) %-value (rightmost
                // bar) — the TRUE percent, taken BEFORE any invert flip.
                if let Some(&last_pv) = pvs.iter().rev().find(|v| !v.is_nan()) {
                    po.overlay_legend.push((s.color, s.symbol.to_string(), last_pv, true));
                }
                // These %-values live directly in the primary's Percent plot-
                // space (they bypass `map`), so with invert on they must be
                // negated to match the flipped primary candles + the flipped
                // ty0/ty1 the autofit fold below uses. `!invert` moves the SAME
                // Vec ⇒ byte-identical.
                let pvs = if requested_invert {
                    pvs.into_iter().map(|v| if v.is_nan() { v } else { -v }).collect()
                } else {
                    pvs
                };
                out.push((s.color, pvs, olo));
            }
            out
        } else {
            Vec::new()
        };
    PriceScale { vis_now, anchor, eff_mode, view, overlay_lines }
}

/// Price pane, step 3 — the absolute-axis compare overlays, the ONE authoritative bounds write
/// (autofit, bounds-space migration, gutter y-drag), the secondary-axis lines, every per-style
/// paint in z-order, and the hover record.
pub(super) fn price_bounds_and_paint<'a>(
    plot_ui: &mut egui_plot::PlotUi<'a>,
    pp: &PricePane<'a>,
    follow: &mut FollowLive,
    po: &mut PriceOut,
    ps: PriceScale<'_>,
    b0: egui_plot::PlotBounds,
    cx0: f64,
    cx1: f64,
    x_dragged: bool,
) {
    let PricePane {
        state,
        style,
        series,
        owned_is_none,
        indicators,
        overlays,
        series_pane_of,
        series_scale,
        nav_in,
        sync,
        fy_min,
        fy_max,
        y_lo,
        margin_top,
        margin_bottom,
        auto_y,
        c,
        color_bars_prev_close,
        gpu_candles,
        footprint,
        of_tick_size,
        profile_on,
        show_last_price,
        sec_axis_cell,
        y_guard,
        x_guard,
        ..
    } = *pp;
    let PriceScale { vis_now, anchor, eff_mode, view, overlay_lines } = ps;
    // The frame's series mapper — carries the invert flip via `view.map`, so
    // every candle/line/overlay painted through `&map` flips together.
    let map = move |y: f64| view.map(y, anchor);
    // --- Absolute-shared-axis compare overlays (empty ⇒ no-op) -------------------------
    // Each `ScaleAssign::SharedLinear` compare renders at its TRUE ABSOLUTE price on the
    // PRIMARY (Linear/Log) axis, sharing the one scale via the primary's own `map`. Built
    // here (before the bounds write) so its raw visible extents can fold into the auto_y
    // autofit below, exactly like the Percent overlays — but through absolute price, not a
    // rebased %. `compute_shared_axis_lines` self-gates to Linear/Log; in Percent/Indexed
    // mode it returns empty (no sensible absolute mapping onto a rebased axis). EMPTY
    // `overlays` / no SharedLinear pin ⇒ empty lines + `raw_ext: None` ⇒ byte-identical.
    let shared = price_render::compute_shared_axis_lines(
        series,
        overlays,
        series_pane_of,
        series_scale,
        cx0,
        cx1,
        eff_mode,
        &map,
        &mut po.overlay_legend,
    );

    // ONE authoritative bounds write per frame. y: visible-range autofit over
    // bars + overlay lines (Bollinger bands etc.) when engaged; otherwise the
    // frame-start y (or the Reset fit on Reset) — carried through the
    // bounds-space migration below when the effective MODE just flipped.
    // Chart-UX bundle T5: Reset AND a pending history seed both force the
    // fresh full-range y-write (mirroring the x-range force above) —
    // `resolve_ty` is the pure fn under test (interact.rs); a reload must
    // never leave a stale manual y-range (possibly computed for an
    // entirely different symbol's price domain, or a different
    // ScaleMode's space) sitting on screen. Every other frame (a plain
    // pan, wheel-zoom, or follow-live x-shift) passes the frame-start
    // bounds through bit-exact.
    let force_fresh_ty = matches!(nav_in, Some(Nav::Reset)) || follow.pending_seed;
    let (mut ty0, mut ty1) =
        resolve_ty(force_fresh_ty, (b0.min()[1], b0.max()[1]), (fy_min, fy_max));
    // Bounds-space migration (T2 §1): egui_plot persists y-bounds numerically
    // in whatever space they were last written. Reset/seed already recomputed
    // ty0/ty1 fresh in the CURRENT mode above (mirrors map_pad), so neither
    // ever needs conversion; the passthrough case does, when the MODE (not
    // just the anchor — a Percent re-anchor alone does NOT convert) differs
    // from last frame's AND auto_y is off (auto_y overwrites ty0/ty1 fresh
    // below regardless, so it needs no conversion either).
    let mut scale_migrated = false;
    if !force_fresh_ty && let Some((prev_view, prev_anchor_bits)) = follow.last_scale {
        // Migrate when the VIEW (mode OR invert) changed — an invert toggle
        // alone keeps the mode but must still negate the persisted range.
        if prev_view != view && !auto_y {
            let prev_anchor = f64::from_bits(prev_anchor_bits);
            (ty0, ty1) = scale::convert_bounds_view(prev_view, view, prev_anchor, anchor, ty0, ty1);
            scale_migrated = true;
        }
    }

    // Chart-UX bundle T4: apply the right gutter's queued y-drag (from the
    // interact rect added AFTER `plot.show` LAST frame) to THIS frame's
    // tracked mapped-space y-range — consumed HERE, before autofit, so a
    // manual drag lands on this frame's write even though `auto_y` was
    // already flipped false by the interact rect that queued it (autofit
    // below would otherwise clobber ty0/ty1 the instant it runs).
    let y_dragged = follow.pending_y_drag != 0.0;
    if y_dragged {
        (ty0, ty1) = y_drag_zoom(ty0, ty1, follow.pending_y_drag);
        follow.pending_y_drag = 0.0;
    }
    if auto_y && let Some((mut lo, mut hi)) = y_raw_ext(vis_now) {
        if owned_is_none {
            // overlay lines are index-aligned to the RAW bars, so include
            // them only for untransformed styles. `fold_overlay_extent` skips
            // Pattern overlays — their series are ±100/0 signals, not prices.
            let i0 = cx0.max(0.0) as usize;
            let take = (cx1.max(0.0).ceil() as usize).saturating_sub(i0) + 1;
            (lo, hi) = fold_overlay_extent(indicators, i0, take, lo, hi);
        }
        // state.overlays polylines (drawings) — mapped and included in
        // autofit extents (T2 §1), regardless of style: unlike indicator
        // overlays these aren't index-aligned to `series`, so no
        // owned-style gate.
        for pts in state.overlays.values() {
            for p in pts.iter().filter(|p| p[0] >= cx0 - 1.0 && p[0] <= cx1 + 1.0) {
                lo = lo.min(p[1]);
                hi = hi.max(p[1]);
            }
        }
        // Absolute-shared-axis compares: fold their RAW visible extents (Linear/Log
        // only; `raw_ext` is pre-filtered to finite-mapped bars) into the primary's
        // autofit BEFORE `map_pad_view`, so the padding is computed over the combined
        // absolute-price span and a shared compare's line is never clipped. `None`
        // (no shared compare, or Percent/Indexed mode) ⇒ this is a no-op ⇒ the auto_y
        // result is byte-identical to today.
        if let Some((slo, shi)) = shared.raw_ext {
            lo = lo.min(slo);
            hi = hi.max(shi);
        }
        (ty0, ty1) = map_pad_view(view, anchor, lo, hi, margin_top, margin_bottom);
        // C2a: keep the compare overlays (already in Percent plot-space, same as
        // the just-mapped ty0/ty1) inside the y-extent so a compare line isn't
        // clipped in % mode. EMPTY `overlay_lines` (no overlays, or not Percent)
        // ⇒ this fold is a no-op, so the auto_y result is byte-identical to today.
        for (_, pvs, _) in &overlay_lines {
            for &pv in pvs.iter().filter(|v| !v.is_nan()) {
                ty0 = ty0.min(pv);
                ty1 = ty1.max(pv);
            }
        }
    }
    if follow.pending_seed
        || auto_y
        || nav_in.is_some()
        || (cx0, cx1) != (b0.min()[0], b0.max()[0])
        || scale_migrated
        || y_dragged
        || x_dragged
    {
        plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max([cx0, ty0], [cx1, ty1]));
    }
    follow.last_scale = Some((view, anchor.to_bits()));
    let (x0, x1) = (cx0, cx1);

    // C2b Task 7: secondary-axis (Right) compare overlays — computed by
    // `price_render::compute_secondary_axis_lines` (chart refactor PR-7); see that fn's
    // doc for the full behavior. Reads only the finalized (x0, x1, ty0, ty1).
    let secondary_lines = price_render::compute_secondary_axis_lines(
        series,
        overlays,
        series_pane_of,
        series_scale,
        x0,
        x1,
        ty0,
        ty1,
        &mut po.overlay_legend,
        sec_axis_cell,
    );

    // Per-style series paint dispatch (LOD decimation + candles/bars/line/… + the GPU
    // candle seam) — `price_render::paint_price_series` (chart refactor PR-7).
    price_render::paint_price_series(
        plot_ui,
        style,
        series,
        state,
        vis_now,
        x0,
        x1,
        y_lo,
        &map,
        c,
        color_bars_prev_close,
        gpu_candles,
        footprint,
        of_tick_size,
    );

    // Volume-profile overlay (SP2, T4) — `price_render::paint_volume_profile` (chart
    // refactor PR-7); see that fn's doc for the full behavior.
    price_render::paint_volume_profile(
        plot_ui,
        profile_on,
        footprint,
        state,
        x0,
        x1,
        &map,
        of_tick_size,
    );

    // The five small fixed-order overlay paints (indicator overlays -> C2a %-lines ->
    // secondary-axis lines -> user drawings -> last-price dashed hline) —
    // `price_render::paint_price_overlays` (chart refactor PR-7); paint z-order is
    // load-bearing — see that fn's doc.
    price_render::paint_price_overlays(
        plot_ui,
        indicators,
        x0,
        x1,
        &map,
        &overlay_lines,
        &secondary_lines,
        &shared.lines,
        state,
        show_last_price,
        c,
    );

    if let Some(pt) = plot_ui.pointer_coordinate() {
        // crosshair is PAINTED below in screen space (exact Qt DashLine [4,2]); here we only
        // snap to the hovered bar + record the position.
        let i = pt.x.round() as i64;
        if let Some(bar) = state.bars.iter().find(|bb| bb.t.round() as i64 == i) {
            po.hovered = Some([bar.o, bar.h, bar.l, bar.c]);
            po.hover_xt = Some((bar.t, bar.ot));
        }
        po.hover_y = Some(pt.y);
    }
    po.ghost_xt = if po.hover_xt.is_none() {
        sync.and_then(|s| s.crosshair_ts)
            .and_then(|ts| nearest_index_by_ts(series, ts).map(|i| (series[i].t, series[i].ot)))
    } else {
        None
    };

    // The chips `draw` paints on the axes once this closure returns, handed to the guards NOW:
    // egui_plot formats the axes' labels after this closure, and a label under a chip is left
    // undrawn (`tick_guard`). Each is the one its painter in `overlay.rs` paints, at the same value
    // and with the same `Fit`.
    let last_chip = state
        .bars
        .last()
        .filter(|_| show_last_price)
        .map(|b| y_guard.y_chip(view.map(b.c, anchor), Fit::PinOutside));
    let price_tag = po
        .hover_y
        .map(|py| y_guard.y_chip(py, Fit::Hide { strip: chart::PRICE_TAG_HIDDEN_STRIP_H }));
    y_guard.set_chips([last_chip, price_tag]);
    let time_tag = po.hover_xt.or(po.ghost_xt);
    x_guard.set_chips([
        time_tag.map(|(t, ot)| x_guard.time_chip(t, &fmt_datetime(ot, state.tz()))),
        None,
    ]);
}

/// Draw phase — the ordered sub-pane render dispatch (chart single-max default): iterate the
/// authored `present` order (skip index 0 = Price, drawn above) and dispatch each pane by KIND at its
/// OWN position. Volume/CVD/Study are peers now — their top-to-bottom order is whatever `present`
/// says, not a fixed Volume→CVD→studies→series sequence. Each fn draws its header + plot, sets
/// `bottom_frame` when it's the last pane (carries the shared time axis), and draws the separator
/// that follows it (unless last). Returns the per-pane fire-once outputs.
#[expect(clippy::too_many_arguments)]
pub(super) fn draw_sub_panes(
    ui: &mut egui::Ui,
    cx: &subpanes::SubPaneCtx<'_>,
    panes: &mut PaneFractions,
    bottom_frame: &mut egui::Rect,
    present: &[PaneKey],
    state: &ChartState,
    series: &[Bar],
    footprint: Option<&[vike_orderflow::FootprintBar]>,
    footprint_gen: u64,
    indicators: &[Active],
    study_pane_of: &indexmap::IndexMap<u64, PaneKey>,
    indicator_dialog: &mut IndicatorDialog,
    visible_study_panes: &[PaneKey],
    visible_series: &[(PaneKey, &SeriesInput<'_>)],
) -> SubPaneOut {
    let mut out = SubPaneOut::default();
    for (pos, &pane_key) in present.iter().enumerate() {
        match pane_key {
            // The price plot is drawn above; it is never a sub-pane.
            PaneKey::Price => {}
            PaneKey::Volume => subpanes::draw_volume_pane(
                ui,
                cx,
                panes,
                bottom_frame,
                pos,
                state,
                &mut out.volume_remove,
                &mut out.reorder_pane,
            ),
            PaneKey::Cvd => subpanes::draw_cvd_pane(
                ui,
                cx,
                panes,
                bottom_frame,
                pos,
                state,
                footprint,
                footprint_gen,
                &mut out.cvd_toggle,
                &mut out.reorder_pane,
            ),
            PaneKey::Study(_) => {
                subpanes::draw_one_study_pane(
                    ui,
                    cx,
                    panes,
                    bottom_frame,
                    pos,
                    pane_key,
                    visible_study_panes,
                    indicators,
                    study_pane_of,
                    indicator_dialog,
                    &mut out.remove_uid,
                    &mut out.move_study,
                    &mut out.reorder_pane,
                );
            }
            PaneKey::Series(_) => {
                if let Some(si) = visible_series.iter().position(|(k, _)| *k == pane_key) {
                    let s = visible_series[si].1;
                    subpanes::draw_one_series_pane(
                        ui,
                        cx,
                        panes,
                        bottom_frame,
                        pos,
                        pane_key,
                        s,
                        series,
                    );
                }
            }
        }
    }
    out
}
