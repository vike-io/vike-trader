//! Visible-range extent & bucket-geometry helpers split out of `chart.rs`
//! (chart refactor PR-1): the price-pane y-extent + scale-space padding, the
//! plot-x-range -> bar-index/ts bounds resolvers (shared clamp shape), and
//! the orderflow bucket width / cell-pixel derivations. Bodies are verbatim.
//!
//! Chart refactor PR-2 (Block C) added [`default_bounds`]: the pure resolution
//! of the plot's default/Reset x/y bounds + the area/columns fill floor that
//! used to sit inline in [`crate::chart::draw`].

use crate::interact::FollowLive;
use crate::model::{Bar, ChartState};
use crate::scale::{self, ScaleMode, ScaleView};

/// Raw (min low, max high) over `bars`; `None` when empty.
pub(crate) fn y_raw_ext(bars: &[Bar]) -> Option<(f64, f64)> {
    if bars.is_empty() {
        return None;
    }
    Some((
        bars.iter().map(|b| b.l).fold(f64::INFINITY, f64::min),
        bars.iter().map(|b| b.h).fold(f64::NEG_INFINITY, f64::max),
    ))
}

/// Margin-padded y range (each side floored at 0.01) — the TV Canvas "Margins"
/// top/bottom percentages of the span. The default `5.0`/`5.0` reproduces the
/// former fixed 5% pad byte-for-byte (`pct as f64 / 100.0` computes the exact
/// same f64 as the old `0.05` literal, and the multiply order is preserved), so
/// a default-options chart is pixel-identical to before margins existed.
pub(crate) fn y_pad(lo: f64, hi: f64, top_pct: f32, bottom_pct: f32) -> (f64, f64) {
    let span = hi - lo;
    let top = (span * (top_pct as f64 / 100.0)).max(0.01);
    let bottom = (span * (bottom_pct as f64 / 100.0)).max(0.01);
    (lo - bottom, hi + top)
}

/// Map RAW (lo, hi) into the `view`'s plot space and margin-pad the result —
/// chart-UX bundle T2 §1: "y_raw_ext results ... mapped before y_pad" (the pad is
/// computed as a % of the MAPPED span, so it reads sanely in log/percent space
/// too). Defensively sorts the mapped pair: `Percent.map` can invert order for a
/// negative anchor (same precedent as `scale::nice_ticks`'s own sort).
///
/// Byte-identical to the pre-invert code when `view.invert == false` (`view.map`
/// is then exactly `mode.map`). With invert on, `view.map` negates the mapped
/// extents FIRST (so the re-sort puts the now-screen-top low prices before the
/// screen-bottom highs) and THEN `y_pad` applies `top_pct`/`bottom_pct` — meaning
/// the asymmetric top/bottom margins stay screen-relative through the flip (a
/// plain post-hoc negation of the non-inverted result would swap them).
pub(crate) fn map_pad_view(
    view: ScaleView,
    anchor: f64,
    lo: f64,
    hi: f64,
    top_pct: f32,
    bottom_pct: f32,
) -> (f64, f64) {
    let (a, b) = (view.map(lo, anchor), view.map(hi, anchor));
    let (mlo, mhi) = if a <= b { (a, b) } else { (b, a) };
    y_pad(mlo, mhi, top_pct, bottom_pct)
}

/// Sync seam (task B7) behavior 2: `ot` of the first/last visible bar in
/// `bars` (the RENDERED series — raw or style-transformed) from a resolved
/// plot x-range `(px0, px1)`. `i0`/`i1` are independently clamped (`i0`
/// floored-up and floored at 0; `i1` ceiled-down and capped at `len - 1`) per
/// the spec's exact formula, so a window narrower than one bar's spacing, or
/// one that has scrolled entirely past either end of the series, collapses
/// to `None` via the `i0 > i1` check rather than an out-of-bounds index.
pub(crate) fn visible_ts_bounds(bars: &[Bar], px0: f64, px1: f64) -> Option<(i64, i64)> {
    if bars.is_empty() {
        return None;
    }
    let last = (bars.len() - 1) as f64;
    let i0 = px0.ceil().max(0.0);
    let i1 = px1.floor().min(last);
    if i0 > i1 {
        return None;
    }
    Some((bars[i0 as usize].ot, bars[i1 as usize].ot))
}

/// Visible bar-index bounds `[i0, i1]` (inclusive) for the orderflow overlays that bucket by
/// price (the Footprint style + the volume-profile overlay, SP2 T4/T5), from a resolved plot
/// x-range. Same asymmetric ceil/floor-then-clamp shape as [`visible_ts_bounds`] above (`i0`
/// only floored at 0, `i1` only ceilinged at `last` — NOT both independently clamped to the same
/// side), so a visible window entirely off-history (panned past either edge) correctly fails
/// `i0f <= i1f` instead of aliasing onto bar 0 or the last bar. `None` for an empty series or a
/// sub-bar window.
pub(crate) fn orderflow_index_bounds(bars: &[Bar], x0: f64, x1: f64) -> Option<(usize, usize)> {
    if bars.is_empty() {
        return None;
    }
    let last = (bars.len() - 1) as f64;
    let i0f = x0.ceil().max(0.0);
    let i1f = x1.floor().min(last);
    (i0f <= i1f).then_some((i0f as usize, i1f as usize))
}

/// Orderflow tick_size (bucket width), shared by the Footprint style and the volume-profile
/// overlay (SP2 T4/T5) via ONE derivation so the two always agree for the same visible range:
/// caller-pinned (`of_tick_size > 0.0`, `ChartInputs::of_tick_size`) else derived from the RAW
/// price extent over `bars[i0..=i1]` via the same "nice" 1/2/5·10^k rounding the y-axis
/// gridlines use (`scale::nice_step_ceil`), sized to ~36 buckets across that extent. `0.0`
/// (out-of-bounds `i0`/`i1`, or a degenerate non-finite/zero-width extent) signals "no valid
/// bucket width this frame" to the caller, rather than a zero/garbage step.
pub(crate) fn orderflow_tick_size(bars: &[Bar], i0: usize, i1: usize, of_tick_size: f64) -> f64 {
    if of_tick_size > 0.0 {
        return of_tick_size;
    }
    if i0 > i1 || i1 >= bars.len() {
        return 0.0;
    }
    y_raw_ext(&bars[i0..=i1])
        .filter(|&(lo, hi)| lo.is_finite() && hi.is_finite() && hi > lo)
        .map(|(lo, hi)| scale::nice_step_ceil((hi - lo) / 36.0))
        .unwrap_or(0.0)
}

/// Pixel height of one `tick_size` row in the price pane's CURRENT screen-space transform (SP2
/// T5) — feeds the Footprint style's text-legibility gate (`orderflow::cell_text_legible`).
/// Multiplies the transform's y pixels-per-plot-unit directly by the RAW `tick_size`: exact for
/// Linear scale (`map` is the identity), a reasonable estimate for Log/Percent (whose per-price
/// pixel density varies by price, unlike Linear's) — adequate for a coarse legibility threshold,
/// not a plotted position. `PlotUi::transform()` reflects THIS frame's screen rect against the
/// bounds resolved as of the END of the PREVIOUS frame (`set_plot_bounds` calls made earlier in
/// THIS frame's closure only take effect next frame) — at most one frame stale, imperceptible for
/// a text on/off gate.
pub(crate) fn cell_px_for(transform: &egui_plot::PlotTransform, tick_size: f64) -> f32 {
    (transform.dpos_dvalue_y() * tick_size).abs() as f32
}

/// The plot's default/Reset bounds + the fill floor (chart refactor PR-2, Block C).
pub(crate) struct Bounds {
    /// Default x lower bound — always `-1.0` (the left margin Reset uses).
    pub(crate) fx_min: f64,
    /// Default x upper bound — `last.t + 1.0` (right margin), or `1.0` for an empty series.
    pub(crate) fx_max: f64,
    /// Default y lower bound, in the frame's effective scale space (5%-padded).
    pub(crate) fy_min: f64,
    /// Default y upper bound, in the frame's effective scale space (5%-padded).
    pub(crate) fy_max: f64,
    /// Unpadded RAW series min low — the area/columns fill floor (mapped internally
    /// by the render fns). `0.0` for an empty series.
    pub(crate) y_lo: f64,
    /// Whether `draw` should apply these as the plot's default bounds. `false` for an
    /// empty series: the `(-1, 1, 0, 1)` defaults are left untouched and the plot
    /// builder's `default_*_bounds`/`auto_bounds(false)` chain is skipped, exactly as
    /// the pre-extraction `if !series.is_empty()` guard did.
    pub(crate) apply: bool,
}

/// Resolve the plot's default/Reset [`Bounds`] (chart refactor PR-2, Block C) — a
/// verbatim extraction of `draw`'s pre-`show` default-bounds block (incl. the
/// former `y_extents` closure). `owned_is_none` is `draw`'s `owned.is_none()` (a
/// transform proxy suppresses the cached-`y_ext` fast path); `follow.peek_scale`
/// is the READ-ONLY scale peek (the state-advancing `resolve_scale` runs once per
/// frame inside the price closure — peeking here must not double-advance the latch).
///
/// "Default" == the FULL-series view (Reset sets cx0/cx1 to `fx_min`/`fx_max`), so
/// resolving the effective mode + anchor from the full extent + first bar's close is
/// exactly what the in-closure per-frame resolution would produce for a Reset frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn default_bounds(
    series: &[Bar],
    owned_is_none: bool,
    state: &ChartState,
    follow: &FollowLive,
    requested_scale: ScaleMode,
    invert: bool,
    margin_top_pct: f32,
    margin_bottom_pct: f32,
) -> Bounds {
    // Series y-extents (min low / max high). Raw series: the cached closed-prefix
    // fold (model.rs) extended with the forming bar — O(1) per frame. Transform
    // styles fall back to a fold over their (capped) owned series.
    let y_extents = || -> (f64, f64) {
        if owned_is_none && (state.y_ext.is_some() || state.closed_len == 0) {
            let (mut lo, mut hi) = state.y_ext.unwrap_or((f64::INFINITY, f64::NEG_INFINITY));
            for b in &state.bars[state.closed_len.min(state.bars.len())..] {
                lo = lo.min(b.l);
                hi = hi.max(b.h);
            }
            (lo, hi)
        } else {
            (
                series.iter().map(|b| b.l).fold(f64::INFINITY, f64::min),
                series.iter().map(|b| b.h).fold(f64::NEG_INFINITY, f64::max),
            )
        }
    };
    let (fx_min, mut fx_max, mut fy_min, mut fy_max) = (-1.0, 1.0, 0.0, 1.0);
    let mut y_lo = 0.0; // unpadded RAW series min low — the area/columns fill floor
    let apply = !series.is_empty();
    if apply {
        fx_max = series.last().map(|b| b.t).unwrap_or(1.0) + 1.0;
        let (lo, hi) = y_extents();
        y_lo = lo;
        // `peek_scale` (read-only) resolves the effective mode + anchor from the FULL
        // extent + first bar's close.
        let anchor0 = series.first().map(|b| b.c).unwrap_or(0.0);
        let outer_mode = follow.peek_scale(requested_scale, lo, anchor0);
        // Invert is orthogonal to the fallback (always "supported"), so it rides
        // through unchanged onto the resolved outer mode.
        (fy_min, fy_max) = map_pad_view(
            ScaleView::new(outer_mode, invert),
            anchor0,
            lo,
            hi,
            margin_top_pct,
            margin_bottom_pct,
        );
    }
    Bounds { fx_min, fx_max, fy_min, fy_max, y_lo, apply }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| Bar {
                t: i as f64,
                ot: 1_700_000_000_000 + i as i64 * 60_000,
                o: 1.0,
                h: 2.0,
                l: 0.5,
                c: 1.5,
                v: 10.0,
            })
            .collect()
    }

    #[test]
    fn y_autofit_pads_visible_extents() {
        let mut b = bars(10);
        b[3].l = 0.4; // visible min
        b[7].h = 3.0; // visible max
        let (rlo, rhi) = y_raw_ext(&b).unwrap();
        let (lo, hi) = y_pad(rlo, rhi, 5.0, 5.0);
        let pad = ((3.0 - 0.4) * 0.05_f64).max(0.01);
        assert_eq!(lo, 0.4 - pad);
        assert_eq!(hi, 3.0 + pad);
        // degenerate flat slice still gets the minimum pad
        let flat = vec![Bar { t: 0.0, ot: 0, o: 5.0, h: 5.0, l: 5.0, c: 5.0, v: 0.0 }];
        let (rlo, rhi) = y_raw_ext(&flat).unwrap();
        let (lo, hi) = y_pad(rlo, rhi, 5.0, 5.0);
        assert_eq!(lo, 5.0 - 0.01);
        assert_eq!(hi, 5.0 + 0.01);
        assert!(y_raw_ext(&[]).is_none());
    }

    // --- Sync seam (task B7) behavior 2: visible_ts_bounds clamp math ---

    #[test]
    fn visible_ts_bounds_interior_window() {
        let b = bars(10);
        // ceil(2.3) = 3, floor(6.7) = 6
        assert_eq!(visible_ts_bounds(&b, 2.3, 6.7), Some((b[3].ot, b[6].ot)));
        // exact integers pass through unchanged
        assert_eq!(visible_ts_bounds(&b, 2.0, 6.0), Some((b[2].ot, b[6].ot)));
    }

    #[test]
    fn visible_ts_bounds_clamps_both_edges() {
        let b = bars(10);
        assert_eq!(visible_ts_bounds(&b, -50.0, 500.0), Some((b[0].ot, b[9].ot)));
    }

    #[test]
    fn visible_ts_bounds_empty_series_is_none() {
        assert_eq!(visible_ts_bounds(&[], 0.0, 5.0), None);
    }

    #[test]
    fn visible_ts_bounds_sub_bar_window_is_none() {
        // ceil(5.1) = 6 > floor(5.4) = 5: a window narrower than one bar's
        // spacing, straddling no whole index — i0 > i1, not an empty series.
        let b = bars(10);
        assert_eq!(visible_ts_bounds(&b, 5.1, 5.4), None);
    }

    #[test]
    fn visible_ts_bounds_entirely_out_of_range_is_none() {
        let b = bars(10);
        assert_eq!(visible_ts_bounds(&b, 500.0, 600.0), None); // scrolled past the end
        assert_eq!(visible_ts_bounds(&b, -600.0, -500.0), None); // scrolled past the start
    }

    #[test]
    fn orderflow_index_bounds_normal_and_clamped_windows() {
        let b = bars(10);
        assert_eq!(orderflow_index_bounds(&b, 2.3, 6.7), Some((3, 6)));
        assert_eq!(orderflow_index_bounds(&b, 2.0, 6.0), Some((2, 6)));
        assert_eq!(orderflow_index_bounds(&b, -50.0, 500.0), Some((0, 9))); // clamped to [0, last]
    }

    #[test]
    fn orderflow_index_bounds_degenerate_windows_are_none() {
        let b = bars(10);
        assert_eq!(orderflow_index_bounds(&[], 0.0, 5.0), None); // empty series
        assert_eq!(orderflow_index_bounds(&b, 5.1, 5.4), None); // narrower than one bar's spacing
        assert_eq!(orderflow_index_bounds(&b, 500.0, 600.0), None); // scrolled past the end
        assert_eq!(orderflow_index_bounds(&b, -600.0, -500.0), None); // scrolled past the start
    }

    #[test]
    fn orderflow_tick_size_pinned_wins_over_derivation() {
        let b = bars(10); // fixture bars are flat h=2.0/l=0.5
        assert_eq!(orderflow_tick_size(&b, 0, 9, 0.25), 0.25);
        // a caller pin short-circuits BEFORE the bounds guard, even given an invalid window.
        assert_eq!(orderflow_tick_size(&b, 3, 2, 0.5), 0.5);
    }

    #[test]
    fn orderflow_tick_size_derives_the_nice_step_the_profile_overlay_used_to_inline() {
        let b = bars(10);
        let derived = orderflow_tick_size(&b, 0, 9, 0.0);
        assert_eq!(derived, scale::nice_step_ceil((2.0 - 0.5) / 36.0));
        assert_eq!(derived, 0.05);
    }

    #[test]
    fn orderflow_tick_size_degenerate_inputs_yield_zero() {
        let b = bars(10);
        assert_eq!(orderflow_tick_size(&b, 3, 2, 0.0), 0.0); // i0 > i1
        assert_eq!(orderflow_tick_size(&b, 0, 20, 0.0), 0.0); // i1 out of bounds
        assert_eq!(orderflow_tick_size(&[], 0, 0, 0.0), 0.0); // empty bars
    }

    #[test]
    fn cell_px_for_uses_transform_pixels_per_unit() {
        // 100 plot-y-units mapped onto a 400px-tall frame → 4 px/unit (sign-independent, abs'd
        // inside `cell_px_for` — `dpos_dvalue_y()` is negative because screen y grows downward
        // while plot y grows upward).
        let frame = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 400.0));
        let bounds = egui_plot::PlotBounds::from_min_max([0.0, 0.0], [10.0, 100.0]);
        let t = egui_plot::PlotTransform::new(frame, bounds, false);
        assert_eq!(cell_px_for(&t, 2.0), 8.0);
        assert_eq!(cell_px_for(&t, 0.0), 0.0);
    }

    // --- chart refactor PR-2 (Block C): default_bounds ---

    #[test]
    fn default_bounds_pads_the_series_extent_linear() {
        // Fixture bars: t = 0..9, l = 0.5, h = 2.0, c = 1.5. `owned_is_none = false`
        // takes the plain series-fold branch (no ChartState cache needed), and a
        // default `FollowLive` peeks Linear ⇒ map_pad == y_pad (Linear map is identity).
        let b = bars(10);
        let st = ChartState::default();
        let follow = FollowLive::default();
        let bnds = default_bounds(&b, false, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
        assert!(bnds.apply);
        assert_eq!(bnds.fx_min, -1.0);
        assert_eq!(bnds.fx_max, b.last().unwrap().t + 1.0); // 9.0 + 1.0
        assert_eq!(bnds.y_lo, 0.5); // unpadded min low = the fill floor
        let (elo, ehi) = y_pad(0.5, 2.0, 5.0, 5.0);
        assert_eq!(bnds.fy_min, elo);
        assert_eq!(bnds.fy_max, ehi);
    }

    #[test]
    fn y_pad_applies_asymmetric_top_bottom_margins() {
        // 20% top / 10% bottom of a span of 10 (lo=0, hi=10): top pad 2.0, bottom 1.0.
        let (lo, hi) = y_pad(0.0, 10.0, 20.0, 10.0);
        assert_eq!(lo, -1.0);
        assert_eq!(hi, 12.0);
        // 5.0/5.0 reproduces the former fixed 5% pad byte-for-byte.
        let (lo5, hi5) = y_pad(0.4, 3.0, 5.0, 5.0);
        let pad = ((3.0 - 0.4) * 0.05_f64).max(0.01);
        assert_eq!((lo5, hi5), (0.4 - pad, 3.0 + pad));
    }

    #[test]
    fn default_bounds_empty_series_keeps_defaults_and_skips_apply() {
        let st = ChartState::default();
        let follow = FollowLive::default();
        let bnds = default_bounds(&[], true, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
        assert!(!bnds.apply, "empty series ⇒ builder default-bounds chain is skipped");
        assert_eq!((bnds.fx_min, bnds.fx_max, bnds.fy_min, bnds.fy_max), (-1.0, 1.0, 0.0, 1.0));
        assert_eq!(bnds.y_lo, 0.0);
    }

    #[test]
    fn default_bounds_uses_cached_y_ext_fast_path() {
        // owned_is_none = true AND a seeded ChartState ⇒ the `y_ext` fast-path branch
        // of `y_extents` (cached closed-prefix extent) is taken; it must agree with the
        // raw series fold for an all-closed series.
        let b = bars(12);
        let mut st = ChartState::default();
        st.bars = b.clone();
        st.closed_len = b.len();
        st.refresh_caches();
        let follow = FollowLive::default();
        let via_cache = default_bounds(&b, true, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
        let via_fold = default_bounds(&b, false, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
        assert_eq!(via_cache.y_lo, via_fold.y_lo);
        assert_eq!((via_cache.fy_min, via_cache.fy_max), (via_fold.fy_min, via_fold.fy_max));
        assert_eq!(via_cache.y_lo, 0.5);
    }
}
