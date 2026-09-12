//! Chart interaction state: follow-live-edge (FollowLive) + the visible-range
//! slicing helpers every per-style renderer filters through, split out of
//! chart.rs (chart-UX bundle T0). No egui_plot painting here — `chart::draw`
//! owns the plot/bounds orchestration, `render.rs` owns the painting.

use crate::model::Bar;
use crate::scale::{ScaleMode, ScaleView};

/// Visible subslice of index-keyed bars (`t == index` by the render-model
/// contract, model.rs — true for raw AND reindexed transform series), keeping
/// the same ±1-bar margin as the old linear filter — O(1) per call instead of
/// an O(N) scan per series per frame.
pub(crate) fn visible_slice(bars: &[Bar], x0: f64, x1: f64) -> &[Bar] {
    if bars.is_empty() {
        return bars;
    }
    let n = bars.len();
    let hi_f = (x1 + 1.0).floor();
    let lo = (x0 - 1.0).ceil().clamp(0.0, n as f64) as usize;
    if hi_f < 0.0 || lo >= n {
        return &bars[0..0];
    }
    let hi = hi_f.min((n - 1) as f64) as usize;
    if lo > hi {
        return &bars[0..0];
    }
    &bars[lo..=hi]
}

pub(crate) fn vis(bars: &[Bar], x0: f64, x1: f64) -> impl Iterator<Item = &Bar> {
    visible_slice(bars, x0, x1).iter()
}

/// Follow-live-edge state: while engaged, appended bars keep the view pinned to
/// the right edge (TradingView behaviour). Panning/zooming away disengages;
/// Reset/Auto — or panning back to the edge — re-engages. `last_len` tracks the
/// series length between frames to detect appends.
pub struct FollowLive {
    pub on: bool,
    /// Visible-range y-autofit (the TradingView default): y bounds refit to the
    /// visible bars + overlay lines every frame; interactions are x-only while
    /// engaged so the fit never fights the user. A right-gutter drag disengages
    /// it (manual y-scale, chart-UX bundle T4); the price-gutter double-click,
    /// the Auto button/⟳ Reset nav, and a history seed/reload all re-engage it
    /// (T5) — every flip funnels through [`next_auto_y`]/[`AutoYEvent`], so the
    /// full transition table is one pure-fn test (see this module's tests).
    pub auto_y: bool,
    pub(crate) last_len: usize,
    /// A history seed was detected: keep asserting the Reset view until the
    /// post-frame bounds confirm it landed (egui_plot can stomp a bounds set
    /// with its placeholder for a frame on freshly-created plots). Also
    /// forces `auto_y` back on and the y-bounds write to the fresh full-range
    /// fit regardless of the PRIOR `auto_y` value (chart-UX bundle T5, via
    /// [`next_auto_y`]/[`resolve_ty`]) — a reload must never strand a manual
    /// y-range computed for a domain that may no longer exist.
    pub(crate) pending_seed: bool,
    /// Last frame's resolved (effective `ScaleMode`, Percent anchor as f64
    /// bits) — chart-UX bundle T2 bounds-space migration. egui_plot persists
    /// y-bounds numerically in whatever space they were last written; when
    /// the MODE (not just the anchor) differs from this across a frame while
    /// `auto_y` is off, `chart::draw` converts the persisted y-range via
    /// `scale::convert_bounds` instead of silently reinterpreting stale
    /// numbers in the new space. `None` before the first frame (nothing to
    /// convert from yet). The anchor is stored as bits rather than `f64` so
    /// the tuple stays a plain `Copy` value with no float-equality-comparison
    /// (clippy::float_cmp) temptation at the (rare) call sites that inspect it.
    /// The stored value is the full [`ScaleView`] (mode + invert), so an invert
    /// toggle alone — same mode — still trips the bounds-space migration below
    /// (it must negate the persisted mapped range so the same data stays in view).
    pub(crate) last_scale: Option<(ScaleView, u64)>,
    /// STICKY-fallback latch (chart-UX spec §1: an unsupported Log/Percent
    /// view "stays Linear with a status hint until the user re-toggles —
    /// never a per-frame flip"). Once a frame's visible data fails
    /// `supports()` for the requested mode, this latches and the chart stays
    /// Linear even if later pans move back over supported data; only a CHANGE
    /// of the requested mode (a user re-toggle — T3's UI writes a new
    /// `ChartInputs::scale`) clears it. Without the latch, panning across a
    /// y<=0 region would flip modes mid-pan, firing a bounds conversion on
    /// every flip.
    pub(crate) fallback_latched: bool,
    /// The requested mode the latch was evaluated against last frame — a
    /// change here IS the "user re-toggled" signal that clears the latch.
    pub(crate) last_requested: ScaleMode,
    /// Right-gutter vertical drag, queued in screen px (chart-UX bundle T4):
    /// the interact rect lives AFTER `plot.show` (needs this frame's
    /// resolved `frame` rect), so it can't write this frame's bounds
    /// directly — egui_plot's bounds mutation is single-write, tracked
    /// inside the closure (see chart.rs). Accumulated across a multi-frame
    /// drag; consumed (and zeroed) at the top of the NEXT frame's closure,
    /// before autofit, via [`y_drag_zoom`].
    pub(crate) pending_y_drag: f32,
    /// Bottom-gutter horizontal drag, queued likewise — consumed via
    /// [`x_drag_zoom`]. Same deferred-write reason as `pending_y_drag`.
    pub(crate) pending_x_drag: f32,
    /// Feature #1b (TradingView parity): price-pane maximize. While `true`,
    /// every sub-pane (volume / CVD / study / series) is hidden and the price
    /// plot fills the chart; the nav row's ⛶ button toggles it. Transient view
    /// state (not persisted), mirroring TradingView's per-pane maximize for the
    /// price pane.
    pub price_maximized: bool,
}

impl Default for FollowLive {
    fn default() -> Self {
        // charts follow the live edge and auto-fit y out of the box
        FollowLive {
            on: true,
            auto_y: true,
            last_len: 0,
            pending_seed: false,
            last_scale: None,
            fallback_latched: false,
            last_requested: ScaleMode::Linear,
            pending_y_drag: 0.0,
            pending_x_drag: 0.0,
            price_maximized: false,
        }
    }
}

impl FollowLive {
    /// Reset the live-follow view for a NEW series (a symbol or timeframe swap): re-engage
    /// follow-live + y-autofit and arm next frame's seed path so BOTH axes refit to the new
    /// data's full range, instead of inheriting the previous symbol's zoom/pan.
    ///
    /// Zeroing `last_len` is what forces it: `chart::draw`'s seed guard fires on
    /// `last_len == 0` (as on a fresh load), which resets `cx0/cx1` to the data extent and
    /// re-arms auto-y. Without this, a swap between two symbols of *similar* bar count slips
    /// past that guard (`len_now` within `last_len + 2`) and the stale view persists — the
    /// "new symbol doesn't auto-scale" bug. `price_maximized` is deliberately preserved (it's a
    /// view-chrome preference, not a per-series property).
    pub fn on_series_change(&mut self) {
        self.on = true;
        self.auto_y = true;
        self.last_len = 0; // arm the seed/reset path in `chart::draw` next frame
        self.pending_seed = false;
        self.fallback_latched = false;
    }

    /// Resolve the frame's EFFECTIVE scale mode with the sticky-fallback
    /// latch — the once-per-frame, state-advancing entry point (wraps the
    /// pure `scale::effective_mode`). Call exactly once per frame, with the
    /// frame's visible raw low + Percent anchor.
    ///
    /// T3 carry-over fix: the latch must only engage from a genuine
    /// unsupported value FROM REAL DATA (`scale::has_data`), never from a
    /// data-absent frame — an empty visible slice (Log: caller passes
    /// `raw_lo = NaN`) or no closed bar existing yet (Percent: caller passes
    /// `anchor = NaN`). A data-absent frame still DEGRADES to Linear for that
    /// frame (via `effective_mode`'s own `supports()` check — NaN never
    /// supports Log/Percent), it just doesn't latch, so the next frame with
    /// real supported data resumes the requested mode instead of being stuck.
    pub(crate) fn resolve_scale(
        &mut self,
        requested: ScaleMode,
        raw_lo: f64,
        anchor: f64,
    ) -> ScaleMode {
        if requested != self.last_requested {
            // user re-toggle: the ONLY thing that clears the latch (spec §1)
            self.last_requested = requested;
            self.fallback_latched = false;
        }
        let eff = crate::scale::effective_mode(requested, raw_lo, anchor);
        if !self.fallback_latched
            && eff != requested
            && crate::scale::has_data(requested, raw_lo, anchor)
        {
            self.fallback_latched = true;
        }
        if self.fallback_latched { ScaleMode::Linear } else { eff }
    }

    /// Read-only twin of [`Self::resolve_scale`]: what the effective mode
    /// would resolve to, WITHOUT advancing the latch. Used by the pre-frame
    /// default/Reset-bounds site in `chart::draw`, which needs the mode
    /// before the in-closure per-frame resolution runs — mutating there would
    /// double-advance the latch per frame.
    pub(crate) fn peek_scale(&self, requested: ScaleMode, raw_lo: f64, anchor: f64) -> ScaleMode {
        let latched = requested == self.last_requested && self.fallback_latched;
        if latched {
            ScaleMode::Linear
        } else {
            crate::scale::effective_mode(requested, raw_lo, anchor)
        }
    }
}

/// New `(min_x, max_x)` pinning the right edge to `last_t + 1` (the same margin
/// Reset uses for `fx_max`), preserving the current view width.
pub(crate) fn follow_shift(min_x: f64, max_x: f64, last_t: f64) -> (f64, f64) {
    let w = max_x - min_x;
    (last_t + 1.0 - w, last_t + 1.0)
}

/// Follow-live stays engaged while the view's right edge sits at/near the last
/// bar (half-bar tolerance) — the post-frame check that turns manual pans away
/// from the edge into a disengage and pans back into a re-engage.
pub(crate) fn follow_engaged(max_x: f64, last_t: f64) -> bool {
    max_x >= last_t - 0.5
}

/// Shared exponential-drag rate for [`y_drag_zoom`]/[`x_drag_zoom`] (chart-UX
/// bundle T4 spec: k = 0.005).
const K_DRAG: f64 = 0.005;

// Both drag-zoom factors below are `libm::exp`, not the `f64::exp` METHOD, and the reason is the
// one `crates/vike-chart/src/scale.rs`'s `nice_step_ceil` spells out at length: IEEE 754 requires
// `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `exp`, so the method reaches
// whichever libm the platform ships — glibc on the CI runners, the MSVC runtime on the Windows dev
// box — and the two are each entitled to a different last bit for the same pixel delta. The `libm`
// crate is a pure-Rust FDLIBM port that answers identically everywhere.
//
// Honest magnitude, so nobody reads more into this than it says: this pair is the MILD end of the
// survey. `exp` here scales a viewport half-width by a smooth factor and feeds no `.floor()`,
// `.round()` or integer cast, so a last-bit disagreement moves a bound by a last bit — a
// sub-pixel difference in where a gridline lands, not a different gridline. The conversion is
// worth making anyway because the alternative is a per-site judgement call about which
// transcendental is "allowed" to be platform-dependent, and because `y_drag_zoom`'s output is
// PERSISTED (the tracked mapped-space bounds), so its last bits compound across a session rather
// than being recomputed from scratch each frame.
//
// ⚠ `dy_px`/`dx_px` arrive as `f32` and are widened with `as f64` before the multiply. That
// widening is exact (every `f32` is an `f64`) and is NOT a libm concern — leave it alone.

/// y-range compress/expand around center: factor = exp(k * dy_px), k = 0.005
/// (chart-UX bundle T4). `(ty0, ty1)` are the tracked MAPPED-space (post-
/// `ScaleMode`) bounds — the fn itself is scale-agnostic, it just scales
/// whatever pair it's given around their midpoint. `dy_px` is the right
/// gutter's vertical drag delta in screen pixels; `dy_px == 0.0` is the exact
/// identity (used for the no-op/first-frame case).
pub fn y_drag_zoom(ty0: f64, ty1: f64, dy_px: f32) -> (f64, f64) {
    let center = (ty0 + ty1) / 2.0;
    let half = (ty1 - ty0) / 2.0;
    let factor = libm::exp(K_DRAG * dy_px as f64);
    let new_half = half * factor;
    (center - new_half, center + new_half)
}

/// x width scale anchored at the RIGHT edge: factor = exp(-k * dx_px), k =
/// 0.005 (chart-UX bundle T4). `cx1` (the right edge) is returned UNCHANGED
/// (bit-equal to the input) — only `cx0` moves, so the bottom gutter's
/// horizontal drag zooms time without shifting the live edge out from under
/// follow-live. `dx_px == 0.0` is the exact identity.
pub fn x_drag_zoom(cx0: f64, cx1: f64, dx_px: f32) -> (f64, f64) {
    let factor = libm::exp(-K_DRAG * dx_px as f64);
    let new_w = (cx1 - cx0) * factor;
    (cx1 - new_w, cx1)
}

// --- T5: manual-y state completeness — auto_y engage/disengage + the
// seed/Reset y-write source. Both pulled out as pure fns (rather than left as
// inline assignments scattered across `chart::draw`'s several call sites)
// because `draw` needs a live `egui::Ui`/`Plot` to run at all, so this is the
// only way to put the actual transition/write logic under test — the real
// call sites in chart.rs delegate here instead of duplicating the logic. ---

/// The four sites that can flip [`FollowLive::auto_y`]: one disengage (a
/// manual right-gutter drag) and three re-engage sites (price-gutter
/// double-click, the Auto button/`Nav::Reset`, and a history seed/reload) —
/// design spec §2: "Double-click price axis / Auto: re-engage auto_y".
#[derive(Clone, Copy, Debug)]
pub(crate) enum AutoYEvent {
    /// Right-gutter vertical drag: user takes manual control.
    Drag,
    /// Price-gutter double-click.
    DblClick,
    /// The ⟳ Reset nav button or the bottom-gutter double-click (both
    /// `Nav::Reset`), OR the price-scale Auto button (`Nav::AutoY`, Y-only —
    /// final-review fix: Auto no longer routes through `Nav::Reset`, but
    /// still reuses this same re-engage transition).
    Reset,
    /// A history reload (`pending_seed`): the old manual range belongs to a
    /// domain (symbol/timeframe) that may no longer exist.
    Seed,
}

/// Pure `auto_y` transition. `cur` is unused today — every event is an
/// unconditional set, not a toggle — but kept in the signature so this reads
/// as an actual state-transition table (and so a future current-state-
/// dependent event doesn't need a signature change).
pub(crate) fn next_auto_y(_cur: bool, ev: AutoYEvent) -> bool {
    match ev {
        AutoYEvent::Drag => false,
        AutoYEvent::DblClick | AutoYEvent::Reset | AutoYEvent::Seed => true,
    }
}

/// This frame's authoritative y-bounds SOURCE, before the queued-drag/autofit
/// steps compose on top of it: `Nav::Reset` and a pending history seed both
/// force the fresh full-range fit (`fresh` — `chart::draw`'s `(fy_min,
/// fy_max)`), discarding `prev` (the frame-start bounds) ENTIRELY, however
/// stale or wrong-space it is — a reload must never strand a manual y-range
/// computed for a different symbol's price domain (or even a different
/// `ScaleMode`'s space, from a stale mode-flip) on screen. Every other frame
/// (a plain pan, wheel-zoom, or follow-live x-shift) passes `prev` through
/// bit-exact, so a manual y-range survives an x-only interaction untouched.
pub(crate) fn resolve_ty(force_fresh: bool, prev: (f64, f64), fresh: (f64, f64)) -> (f64, f64) {
    if force_fresh { fresh } else { prev }
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
    fn on_series_change_arms_seed_refit_and_keeps_maximize() {
        // Simulate a chart the user zoomed on (auto_y off) with a settled bar count and the
        // price pane maximized. A symbol swap must re-arm the seed/reset path (last_len == 0),
        // re-engage follow + auto_y, clear the scale-fallback latch, and preserve maximize.
        let mut f =
            FollowLive { on: false, auto_y: false, price_maximized: true, ..Default::default() };
        f.last_len = 500;
        f.fallback_latched = true;
        f.pending_seed = false;

        f.on_series_change();

        assert!(f.on, "follow-live re-engaged");
        assert!(f.auto_y, "y-autofit re-engaged");
        assert_eq!(f.last_len, 0, "last_len zeroed → chart::draw seed guard fires next frame");
        assert!(!f.fallback_latched, "stale scale-fallback latch cleared for the new series");
        assert!(f.price_maximized, "price-pane maximize preserved (view chrome, not per-series)");
    }

    #[test]
    fn visible_slice_maps_x_range_to_indices() {
        let b = bars(100);
        // interior window: ±1 bar margin like the old filter
        let s = visible_slice(&b, 10.0, 20.0);
        assert_eq!(s.first().unwrap().t, 9.0);
        assert_eq!(s.last().unwrap().t, 21.0);
        // clamped at both ends
        let s = visible_slice(&b, -50.0, 5.0);
        assert_eq!(s.first().unwrap().t, 0.0);
        assert_eq!(s.last().unwrap().t, 6.0);
        let s = visible_slice(&b, 95.0, 500.0);
        assert_eq!(s.first().unwrap().t, 94.0);
        assert_eq!(s.last().unwrap().t, 99.0);
        // fully outside → empty; empty input → empty
        assert!(visible_slice(&b, 200.0, 300.0).is_empty());
        assert!(visible_slice(&b, -30.0, -10.0).is_empty());
        assert!(visible_slice(&[], 0.0, 10.0).is_empty());
    }

    #[test]
    fn visible_slice_equals_old_linear_filter() {
        let b = bars(64);
        for (x0, x1) in [(0.0, 63.0), (5.3, 9.9), (-2.0, 1.0), (62.5, 70.0), (31.0, 31.0)] {
            let new: Vec<f64> = visible_slice(&b, x0, x1).iter().map(|bb| bb.t).collect();
            let old: Vec<f64> =
                b.iter().filter(|bb| bb.t >= x0 - 1.0 && bb.t <= x1 + 1.0).map(|bb| bb.t).collect();
            assert_eq!(new, old, "mismatch at ({x0},{x1})");
        }
    }

    #[test]
    fn follow_shift_pins_right_edge_keeping_width() {
        let (nx0, nx1) = follow_shift(10.0, 60.0, 99.0);
        assert_eq!(nx1, 100.0); // last bar + 1 margin, same as Reset's fx_max
        assert_eq!(nx1 - nx0, 50.0); // width preserved
    }

    #[test]
    fn follow_engaged_hysteresis() {
        // at/near the live edge → engaged; panned away → disengaged
        assert!(follow_engaged(100.0, 99.0));
        assert!(follow_engaged(98.6, 99.0));
        assert!(!follow_engaged(90.0, 99.0));
    }

    // --- T2 review fixes: the scale fallback must be STICKY (spec §1: "stays
    // Linear with a status hint until the user re-toggles — never a per-frame
    // flip"), not recomputed per frame from the visible slice. ---

    #[test]
    fn scale_fallback_latches_and_stays_latched_across_pans() {
        let mut f = FollowLive::default();
        // supported Log frames → Log, no latch
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
        assert_eq!(f.resolve_scale(ScaleMode::Log, 50.0, 0.0), ScaleMode::Log);
        // pan into a y<=0 region → falls back to Linear AND latches
        assert_eq!(f.resolve_scale(ScaleMode::Log, -5.0, 0.0), ScaleMode::Linear);
        // pan BACK to a supported region → STAYS Linear (sticky — this is the
        // per-frame-flip hazard the spec forbids; a per-frame recompute would
        // return Log here)
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
    }

    #[test]
    fn scale_fallback_unlatches_only_on_requested_change() {
        let mut f = FollowLive::default();
        // latch via an unsupported Log view
        assert_eq!(f.resolve_scale(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
        // user re-toggles to Linear → latch clears
        assert_eq!(f.resolve_scale(ScaleMode::Linear, 100.0, 0.0), ScaleMode::Linear);
        // user re-toggles back to Log over a NOW-supported view → Log again
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
        // and an unsupported re-toggle re-latches immediately (Percent, anchor 0)
        assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, 0.0), ScaleMode::Linear);
        assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, 63_000.0), ScaleMode::Linear);
    }

    // --- T3 carry-over fix: the latch must NOT engage on a data-absent frame
    // (empty visible slice for Log, no closed bar yet for Percent) — only a
    // genuine unsupported value FROM real data may latch. Callers signal
    // "no data" with NaN (see `scale::has_data`). ---

    #[test]
    fn scale_fallback_does_not_latch_on_empty_visible_slice_log() {
        let mut f = FollowLive::default();
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
        // this frame's visible slice is empty -> raw_lo is NaN (no data): degrade
        // for the frame but must NOT latch.
        assert_eq!(f.resolve_scale(ScaleMode::Log, f64::NAN, 0.0), ScaleMode::Linear);
        // a later frame with real supported data resumes Log — proves no latch.
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
    }

    #[test]
    fn scale_fallback_latches_on_genuine_nonpositive_log_data() {
        let mut f = FollowLive::default();
        // real (finite) non-positive visible low -> genuine unsupported DATA -> latches.
        assert_eq!(f.resolve_scale(ScaleMode::Log, -5.0, 0.0), ScaleMode::Linear);
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
        // stays latched
    }

    #[test]
    fn scale_fallback_does_not_latch_on_no_closed_bar_percent() {
        let mut f = FollowLive::default();
        // no closed bar yet -> anchor is NaN (no data): degrade without latching.
        assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, f64::NAN), ScaleMode::Linear);
        // a later frame with a real closed-bar anchor resumes Percent — proves no latch.
        assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, 63_000.0), ScaleMode::Percent);
    }

    #[test]
    fn scale_fallback_peek_is_read_only() {
        let mut f = FollowLive::default();
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
        // peek over an unsupported view reports Linear but does NOT latch...
        assert_eq!(f.peek_scale(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
        assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
        // ...while peek after a real latch reports the latched Linear even
        // over a supported view
        assert_eq!(f.resolve_scale(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
        assert_eq!(f.peek_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
    }

    // --- T4 gutter drag math: y_drag_zoom / x_drag_zoom (pure, TDD) ---
    //
    // The two tests below RE-DERIVE the expected factor by evaluating the same `exp(±0.005 * d)`
    // the functions do, then compare within 1e-9. They call `libm::exp` for the same reason the
    // production sites now do — and here it is not merely hygiene: an expected value computed with
    // the `f64::exp` METHOD would come from the PLATFORM's libm while the value under test came
    // from the `libm` crate, so the assertion would be measuring the gap between two
    // implementations rather than checking the function. The 1e-9 tolerance is wide enough to
    // absorb that today, which is precisely why it would hide the mismatch instead of reporting
    // it; keeping both sides on one implementation removes the question.
    //
    // ⚠ These re-derivations also RESTATE the `0.005` rate as a literal rather than referencing
    // `K_DRAG`, deliberately — a test that reads the constant it is checking cannot catch a change
    // to it. That predates this conversion and is left as it stands.

    #[test]
    fn y_drag_zoom_dy_zero_is_identity() {
        let (n0, n1) = y_drag_zoom(3.0, 7.0, 0.0);
        assert_eq!(n0, 3.0);
        assert_eq!(n1, 7.0);
    }

    #[test]
    fn y_drag_zoom_preserves_center_and_applies_exp_factor() {
        let (ty0, ty1) = (10.0, 20.0); // center 15.0, half-width 5.0
        let dy = 40.0_f32;
        let (n0, n1) = y_drag_zoom(ty0, ty1, dy);
        let factor = libm::exp(0.005_f64 * dy as f64);
        let center = (n0 + n1) / 2.0;
        assert!((center - 15.0).abs() < 1e-9, "center drifted: {center}");
        let new_half = (n1 - n0) / 2.0;
        assert!(
            (new_half - 5.0 * factor).abs() < 1e-9,
            "factor mismatch: {new_half} vs {}",
            5.0 * factor
        );
    }

    #[test]
    fn y_drag_zoom_negative_dy_compresses() {
        // dy negative (drag up) -> factor < 1 -> narrower range, same center
        let (n0, n1) = y_drag_zoom(0.0, 10.0, -100.0);
        assert!(n1 - n0 < 10.0);
        assert!((n0 + n1 - 10.0).abs() < 1e-9);
    }

    #[test]
    fn x_drag_zoom_dx_zero_is_identity() {
        let (n0, n1) = x_drag_zoom(10.0, 50.0, 0.0);
        assert_eq!(n0, 10.0);
        assert_eq!(n1, 50.0);
    }

    #[test]
    fn x_drag_zoom_pins_right_edge_exactly_and_scales_width() {
        let (cx0, cx1) = (0.0, 100.0);
        let dx = 20.0_f32;
        let (n0, n1) = x_drag_zoom(cx0, cx1, dx);
        assert_eq!(n1, cx1, "right edge must be bit-equal to the input");
        let factor = libm::exp(-0.005_f64 * dx as f64);
        let new_w = n1 - n0;
        assert!(
            (new_w - 100.0 * factor).abs() < 1e-9,
            "width mismatch: {new_w} vs {}",
            100.0 * factor
        );
    }

    #[test]
    fn x_drag_zoom_positive_dx_narrows_negative_dx_widens() {
        let (cx0, cx1) = (0.0, 100.0);
        let (n0_pos, n1_pos) = x_drag_zoom(cx0, cx1, 50.0);
        assert!(n1_pos - n0_pos < 100.0);
        assert_eq!(n1_pos, cx1);
        let (n0_neg, n1_neg) = x_drag_zoom(cx0, cx1, -50.0);
        assert!(n1_neg - n0_neg > 100.0);
        assert_eq!(n1_neg, cx1);
    }

    // --- T5: auto_y state-transition table + the seed/Reset y-write source ---

    #[test]
    fn auto_y_state_transition_table() {
        // Every real call site in chart.rs funnels through `next_auto_y` (the
        // Nav::Reset arm, the pending_seed block, and the y-gutter drag/dbl-
        // click handlers) — this table doubles as the T5 audit: auto→manual
        // ONLY via Drag; manual→auto via DblClick, Reset (Auto button/⟳/
        // bottom-gutter dbl-click), or Seed (history reload).
        let cases = [
            (true, AutoYEvent::Drag, false),
            (false, AutoYEvent::Drag, false), // idempotent while already manual
            (false, AutoYEvent::DblClick, true),
            (true, AutoYEvent::DblClick, true), // idempotent while already auto
            (false, AutoYEvent::Reset, true),
            (true, AutoYEvent::Reset, true),
            (false, AutoYEvent::Seed, true),
            (true, AutoYEvent::Seed, true),
        ];
        for (cur, ev, expected) in cases {
            assert_eq!(next_auto_y(cur, ev), expected, "{cur} + {ev:?} -> expected {expected}");
        }
    }

    #[test]
    fn resolve_ty_seed_or_reset_discards_stale_prev_entirely() {
        // Fabricate a "stale, wrong-space" prev the way a real frame could:
        // a manual range from BEFORE a symbol swap, migrated into Log space
        // by T2's own convert_bounds — not even same-space staleness, but
        // numerically meaningless for the NEW series.
        let stale_linear = (56_700.0, 69_300.0); // BTC-ish manual range
        let stale_in_log_space = crate::scale::convert_bounds(
            ScaleMode::Linear,
            ScaleMode::Log,
            0.0,
            0.0,
            stale_linear.0,
            stale_linear.1,
        );
        let fresh = (0.0001, 0.0005); // a totally different (e.g. altcoin) fresh fit
        assert_eq!(resolve_ty(true, stale_in_log_space, fresh), fresh);
    }

    #[test]
    fn resolve_ty_non_seed_non_reset_preserves_prev_bit_exact() {
        let prev = (12.345, 67.891);
        let fresh = (0.0, 1.0);
        let (p0, p1) = resolve_ty(false, prev, fresh);
        // bit-exact, not just approximately equal — "the write must carry
        // (ty0, ty1) unchanged" (task brief).
        assert_eq!(p0.to_bits(), prev.0.to_bits());
        assert_eq!(p1.to_bits(), prev.1.to_bits());
    }

    #[test]
    fn follow_shift_x_only_leaves_resolve_ty_prev_untouched() {
        // Behavior 3: follow-live's x-shift (bars appended, view pinned to
        // the live edge) is an x-only interaction — resolve_ty's non-seed/
        // non-reset branch must return the SAME (ty0, ty1) chart.rs read at
        // frame-start, bit-exact, regardless of how far follow_shift moves
        // (cx0, cx1).
        let (cx0, cx1) = follow_shift(10.0, 60.0, 99.0); // width-preserving x pin
        assert_ne!((cx0, cx1), (10.0, 60.0)); // sanity: x really did move
        let manual_ty = (1_234.5, 1_250.0);
        let fresh = (0.0, 1.0); // must be ignored entirely on this path
        let (ty0, ty1) = resolve_ty(false, manual_ty, fresh);
        assert_eq!((ty0.to_bits(), ty1.to_bits()), (manual_ty.0.to_bits(), manual_ty.1.to_bits()));
    }

    #[test]
    fn resolve_ty_composes_with_queued_drag_using_fresh_never_stale() {
        // Edge case: a seed AND a queued gutter-drag delta landing on the
        // same frame (chart.rs consumes `pending_y_drag` AFTER this
        // resolution, see chart.rs ~702-706). The drag must zoom the FRESH
        // seeded range, never the stale prev — proven here by checking the
        // composed result only ever depends on `fresh`.
        let prev = (999_000.0, 999_999.0); // wildly stale, must be fully ignored
        let fresh = (10.0, 20.0);
        let dy = 40.0_f32;
        let (ty0, ty1) = resolve_ty(true, prev, fresh);
        let composed = y_drag_zoom(ty0, ty1, dy);
        let direct = y_drag_zoom(fresh.0, fresh.1, dy);
        assert_eq!(composed, direct);
    }
}
