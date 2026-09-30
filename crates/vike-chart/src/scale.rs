//! Price-scale modes (chart UX bundle §1, `docs/superpowers/specs/2026-07-07-chart-ux-bundle-design.md`).
//!
//! `ScaleMode` is the mapping seam between raw price space (the source of
//! truth everywhere else — `ChartState`, y-extent caches, future drawings)
//! and the mapped plot space egui_plot actually renders. Every series y-value
//! passes through [`ScaleMode::map`] before reaching egui_plot; every
//! user-facing readback (crosshair tag, axis label) goes through
//! [`ScaleMode::unmap`]. This module owns only the pure mapping + nice-tick
//! math; wiring the seam through the draw sites is a later task (T2) against
//! these exact signatures.
//!
//! ## Why every logarithm and power here goes through the `libm` CRATE
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded; it requires NOTHING of
//! `log10`, `pow`, `exp`, `sin` or `cos`. An `f64` METHOD call (`y.log10()`, `10f64.powf(v)`)
//! reaches whatever libm the platform ships — glibc on the CI runners, the MSVC runtime on the
//! Windows dev box — and each is entitled to its own last bit. The `libm` crate is a pure-Rust
//! FDLIBM port that computes the same bits everywhere, so every transcendental in this module is
//! spelled `libm::log10(x)` / `libm::pow(10.0, k)` rather than as a method.
//!
//! **Honest priority: this is HYGIENE, not a known-live correctness bug.**
//! `crates/vike-chart/tests/tessellation_goldens.rs`'s
//! `a_one_ulp_price_perturbation_leaves_every_record_unchanged` nudges every fixture price by a
//! full `f64` ULP — roughly ten times the worst libm disagreement — and asserts all sixteen frame
//! records stay byte-identical; its own failure message calls a pass "a direct statement that a
//! libm difference cannot rebaseline this suite". Two things that gate does NOT cover, which is
//! the whole reason the conversion is still worth doing:
//!
//! * It perturbs INPUT PRICES, so it cannot probe the DECADE CLIFF. `nice_step_ceil` and
//!   `log_nice_values` both feed a logarithm straight into `.floor()`/`.round()`/`as i32`, and a
//!   last-bit disagreement either side of an exact power of ten flips that integer by one — a
//!   TEN-FOLD change in the chosen tick step or the decade loop's bounds, not a rounding wobble. A
//!   one-ULP price nudge lands nowhere near such a boundary, so the gate is green either way.
//! * It renders no PointFigure scenario (the sixteen goldens are candles/line/HeikinAshi/Renko),
//!   so `crates/vike-chart/src/render.rs`'s `draw_pnf` ellipse ring — this crate's other libm
//!   consumer — is on no golden at all.
//!
//! ⚠ ONE power is deliberately left as a method call: `log_nice_values`'s `10f64.powi(k)`. See
//! the comment at that site for the measurement.

/// How raw price maps to the plotted y value.
#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ScaleMode {
    #[default]
    Linear,
    /// log10 of price. Requires strictly positive prices — see [`ScaleMode::supports`].
    Log,
    /// percent change vs an anchor price: `(y / anchor - 1) * 100`.
    Percent,
    /// TradingView "Indexed to 100": rebase the visible series so the first-visible
    /// value reads 100, everything else relative — `100 * y / anchor`. Structurally
    /// the Percent twin (same first-visible `anchor`, same fallback guard: a
    /// zero/non-finite anchor degrades to Linear), differing only in the affine
    /// mapping and its plain-number axis labels (around 100, no `%`).
    Indexed,
}

impl ScaleMode {
    /// raw price -> mapped plot value. anchor used by Percent/Indexed only.
    ///
    /// Percent delegates to [`pct_change`] (DRY — verified bit-identical: every real call site
    /// reaches this with mode already narrowed to `Percent` via `effective_mode`/`resolve_scale`,
    /// which guarantees `anchor != 0.0 && anchor.is_finite()` per `supports()`, so `pct_change`'s
    /// degenerate-anchor guard is never actually exercised here — it only makes this total instead
    /// of producing inf/NaN if ever called out-of-contract). Indexed delegates to [`indexed`]
    /// for the same reason (one source of truth for the `100 * y / anchor` rebasing).
    ///
    /// Log spells its logarithm `libm::log10` rather than the `f64::log10` METHOD: the method
    /// reaches the platform's libm (glibc vs the MSVC runtime), which IEEE 754 does not require to
    /// be correctly rounded, so the same price could map to two different last bits on the two
    /// boxes. The `libm` crate's pure-Rust FDLIBM is identical everywhere. See the module doc for
    /// why this is hygiene rather than a live defect here, and why the tick generators below are
    /// the sites where a last bit actually matters.
    pub fn map(self, y: f64, anchor: f64) -> f64 {
        match self {
            ScaleMode::Linear => y,
            ScaleMode::Log => libm::log10(y),
            ScaleMode::Percent => pct_change(y, anchor),
            ScaleMode::Indexed => indexed(y, anchor),
        }
    }

    /// mapped plot value -> raw price.
    ///
    /// Log's inverse is `libm::pow(10.0, v)`, not `10f64.powf(v)`. The base being the literal 10
    /// is NOT enough to make this exact: `v` is an arbitrary runtime plot-space y (a pixel row's
    /// worth of log10 price, essentially never an integer), so the result is `10^2.8017…` and
    /// nothing about it is exactly representable — it goes through the platform's `pow`, which
    /// IEEE 754 leaves unconstrained. (The exactly-representable case is the INTEGER exponent, and
    /// that one is left alone — see `log_nice_values`'s `powi` comment.)
    pub fn unmap(self, v: f64, anchor: f64) -> f64 {
        match self {
            ScaleMode::Linear => v,
            ScaleMode::Log => libm::pow(10.0, v),
            ScaleMode::Percent => anchor * (1.0 + v / 100.0),
            ScaleMode::Indexed => anchor * v / 100.0,
        }
    }

    /// Whether this mode can represent the given raw extents (Log: lo > 0;
    /// Percent/Indexed: anchor != 0 — both rebase by dividing by the anchor).
    pub fn supports(self, raw_lo: f64, anchor: f64) -> bool {
        match self {
            ScaleMode::Linear => true,
            ScaleMode::Log => raw_lo > 0.0,
            ScaleMode::Percent | ScaleMode::Indexed => anchor != 0.0 && anchor.is_finite(),
        }
    }
}

/// A resolved price-scale transform: a [`ScaleMode`] plus the orthogonal
/// vertical-invert modifier (TradingView "Invert scale"). Invert is applied as a
/// FINAL negation of the mapped plot value, so ANY mode (Linear/Log/Percent/
/// Indexed) can be flipped top-to-bottom while egui_plot still renders normal
/// ascending (min < max) bounds. `invert == false` delegates byte-identically to
/// the bare [`ScaleMode`] methods — the whole render path is unchanged when invert
/// is off (the sole reason every seam site routes through `ScaleView` rather than
/// growing a second `invert` argument).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ScaleView {
    pub mode: ScaleMode,
    pub invert: bool,
}

impl ScaleView {
    pub const fn new(mode: ScaleMode, invert: bool) -> Self {
        ScaleView { mode, invert }
    }

    /// raw price -> mapped plot value, with the final invert flip.
    pub fn map(self, y: f64, anchor: f64) -> f64 {
        self.flip(self.mode.map(y, anchor))
    }

    /// mapped plot value -> raw price (un-flips first, then delegates to the mode).
    pub fn unmap(self, v: f64, anchor: f64) -> f64 {
        self.mode.unmap(self.flip(v), anchor)
    }

    /// Flip a value that is ALREADY in mapped plot-space (its own inverse: two
    /// flips cancel). Used both inside `map`/`unmap` and by the seam sites that
    /// hold pre-mapped values directly (Percent compare-overlay `pct_change`
    /// lines, crosshair/label readbacks). Identity when `invert == false`, so
    /// those sites stay byte-identical.
    pub fn flip(self, mapped: f64) -> f64 {
        if self.invert { -mapped } else { mapped }
    }
}

/// Percent-change of `value` vs `anchor` (a series' first-visible close): (value/anchor - 1) * 100.
/// anchor == 0 or non-finite -> 0.0 (no divide-by-zero / NaN leak). Standalone (not gated behind
/// `ScaleMode::supports`) so C2a's multi-symbol %-overlays can rebase a compare series without
/// going through the primary's scale-fallback-latch machinery; `ScaleMode::Percent::map` (above)
/// delegates here for the identical primary-axis rebasing (one source of truth).
pub fn pct_change(value: f64, anchor: f64) -> f64 {
    if anchor.is_finite() && anchor != 0.0 { (value / anchor - 1.0) * 100.0 } else { 0.0 }
}

/// Index-to-100 of `value` vs `anchor` (a series' first-visible close):
/// `100 * value / anchor`. anchor == 0 or non-finite -> 100.0 (the index base;
/// no divide-by-zero / NaN leak). The Indexed twin of [`pct_change`] — same
/// anchor role and same degenerate-guard discipline; `ScaleMode::Indexed::map`
/// delegates here so the `100 * value / anchor` rebasing has ONE source of truth.
pub fn indexed(value: f64, anchor: f64) -> f64 {
    if anchor.is_finite() && anchor != 0.0 { 100.0 * value / anchor } else { 100.0 }
}

/// How a compare series is scaled against the price pane. `Percent` (default) rebases
/// to first-visible on the shared % axis (C2a). `Right`/`Left` pin the series to a
/// secondary ABSOLUTE price axis on that side (its own price range, remapped into the
/// primary's plot-space via `remap_to_primary`). `SharedLinear` renders the series at its
/// TRUE ABSOLUTE price on the PRIMARY axis, sharing the one price scale — no rebasing, no
/// secondary axis. Its visible extents fold into the primary's y-autofit so it can't be
/// clipped. Only meaningful (and only rendered) when the primary's effective scale is
/// Linear or Log — an absolute-price overlay has no sensible mapping onto a Percent/Indexed
/// (rebased) primary axis. Despite the name it works on Log too (the compare's closes pass
/// through the primary's own `map`, which is `log10` in Log mode).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ScaleAssign {
    #[default]
    Percent,
    Right,
    Left,
    /// Absolute price on the PRIMARY (Linear/Log) axis, sharing the primary's scale +
    /// y-autofit. Added by the absolute-shared-axis feature.
    SharedLinear,
}

/// Linear-map a secondary series' absolute value `v` (in its own visible range
/// `sec_lo..=sec_hi`) into the primary's plot-space y-range `prim_lo..=prim_hi`, so a
/// `Right`/`Left`-pinned overlay tracks its OWN price range while sharing the one plot.
/// Degenerate `sec_lo == sec_hi` (flat/one-point range) -> the primary midpoint
/// `(prim_lo+prim_hi)/2` (no divide-by-zero / NaN).
pub fn remap_to_primary(v: f64, sec_lo: f64, sec_hi: f64, prim_lo: f64, prim_hi: f64) -> f64 {
    if sec_hi == sec_lo {
        return (prim_lo + prim_hi) * 0.5;
    }
    prim_lo + (v - sec_lo) / (sec_hi - sec_lo) * (prim_hi - prim_lo)
}

/// Inverse of [`remap_to_primary`]: map a value `pv` in the primary's plot-space
/// y-range `prim_lo..=prim_hi` back to the secondary series' OWN absolute range
/// `sec_lo..=sec_hi`. This is exactly what the secondary (Right/Left-pinned)
/// price-axis LABEL formatter needs: egui_plot hands each y grid mark in the
/// SHARED plot-space y (the primary's resolved range), and each must read out as
/// the compare series' real price. Exact round-trip with `remap_to_primary` at
/// the range endpoints (and, to f64 rounding, anywhere in between). Degenerate
/// `prim_lo == prim_hi` (flat/one-point primary range) -> `sec_lo` (no
/// divide-by-zero / NaN).
pub fn inverse_remap(pv: f64, sec_lo: f64, sec_hi: f64, prim_lo: f64, prim_hi: f64) -> f64 {
    if prim_hi == prim_lo {
        return sec_lo;
    }
    sec_lo + (pv - prim_lo) / (prim_hi - prim_lo) * (sec_hi - sec_lo)
}

/// Resolve the EFFECTIVE scale mode for a frame: `mode` when it `supports()`
/// the given visible raw extents, else the sticky Linear fallback (chart-UX
/// bundle T2 §1 — Log with non-positive visible lows, or Percent with a
/// zero/non-finite anchor, silently degrade to Linear rather than producing
/// NaN/garbage plot positions). Callers resolve this ONCE per frame, before
/// any mapped use, so there is no per-frame oscillation by construction.
pub fn effective_mode(mode: ScaleMode, raw_lo: f64, anchor: f64) -> ScaleMode {
    if mode.supports(raw_lo, anchor) { mode } else { ScaleMode::Linear }
}

/// Whether `(raw_lo, anchor)` reflects ACTUAL observed data for `mode`, as
/// opposed to an absence-of-data sentinel (chart-UX bundle T3 carry-over
/// fix): `FollowLive::resolve_scale`'s sticky-fallback latch (`interact.rs`)
/// must only engage from a genuine unsupported DATA value — e.g. `raw_lo`
/// finite but `<= 0.0` from real non-positive visible prices (Log), or a
/// finite `anchor` of exactly `0.0`/subnormal from a real closed bar's close
/// (Percent) — never from a data-absent frame: an empty visible slice (Log,
/// `raw_lo = NaN`) or no closed bar existing yet (Percent, `anchor = NaN`).
/// Callers pass `NaN` for exactly that data-absent case; any other value
/// (including a genuine non-positive `raw_lo` or a genuine zero `anchor`)
/// counts as data. Linear always "has data" — it never fails `supports()`,
/// so this is never consulted for it in practice.
pub fn has_data(mode: ScaleMode, raw_lo: f64, anchor: f64) -> bool {
    match mode {
        ScaleMode::Linear => true,
        ScaleMode::Log => raw_lo.is_finite(),
        ScaleMode::Percent | ScaleMode::Indexed => anchor.is_finite(),
    }
}

/// Convert a persisted MAPPED y-range from `old_mode`'s space to `new_mode`'s
/// space via `unmap_old -> map_new` (chart-UX bundle T2 §1 bounds-space
/// migration): egui_plot persists y-bounds numerically in whatever space they
/// were last written, so a mode flip must rewrite them or the view silently
/// reinterprets old numbers in the new space (e.g. a linear price range
/// treated as log10 exponents). Percent re-anchoring alone must NOT go
/// through this path — only genuine mode flips do. Defensively sorts the
/// output: `Percent.map` can invert order for a negative anchor (same
/// precedent as `nice_ticks`'s own sort).
pub fn convert_bounds(
    old_mode: ScaleMode,
    new_mode: ScaleMode,
    anchor_old: f64,
    anchor_new: f64,
    ty0: f64,
    ty1: f64,
) -> (f64, f64) {
    convert_bounds_view(
        ScaleView::new(old_mode, false),
        ScaleView::new(new_mode, false),
        anchor_old,
        anchor_new,
        ty0,
        ty1,
    )
}

/// [`convert_bounds`] over full [`ScaleView`]s: migrates persisted bounds when
/// EITHER the mode OR the invert flag changed (an invert toggle alone leaves the
/// mode identical but must still negate the persisted mapped range, else the same
/// data range would be reinterpreted in the flipped space and scroll off-screen).
/// The `unmap`/`map` round-trip through raw price space carries the flip on both
/// ends. Non-inverted views make this byte-identical to the plain `convert_bounds`.
pub fn convert_bounds_view(
    old_view: ScaleView,
    new_view: ScaleView,
    anchor_old: f64,
    anchor_new: f64,
    ty0: f64,
    ty1: f64,
) -> (f64, f64) {
    let raw0 = old_view.unmap(ty0, anchor_old);
    let raw1 = old_view.unmap(ty1, anchor_old);
    let m0 = new_view.map(raw0, anchor_new);
    let m1 = new_view.map(raw1, anchor_new);
    if m0 <= m1 { (m0, m1) } else { (m1, m0) }
}

/// One y-axis tick: mapped position + fabricated MAPPED step_size (egui_plot
/// culls/fades by step_size in plot units) + the raw price it labels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridTick {
    pub mapped: f64,
    pub step_mapped: f64,
    pub raw: f64,
}

/// Classic "nice numbers" step (Heckbert): rounds `raw_step` UP to the
/// nearest 1/2/5·10^k value — used to size Linear/Percent tick spacing so
/// the resulting tick count stays within the caller's `max_ticks` budget.
/// `pub(crate)`: also reused by `chart.rs`'s SP2 volume-profile overlay (T4) to derive a "nice"
/// bucket width from the visible price extent when the caller doesn't pin one
/// (`ChartInputs::of_tick_size <= 0.0`) — same rounding, one bucket instead of a tick series.
/// Callers must uphold the `debug_assert` precondition themselves (guard `raw_step > 0.0` before
/// calling, e.g. skip when the visible extent is degenerate).
///
/// ⚠ **This is a DECADE CLIFF, not a one-ULP wobble** — which is why both the logarithm and the
/// power are the deterministic `libm` crate's rather than the platform's. `.floor()` applied to a
/// logarithm quantizes it to an integer, so a last-bit disagreement between glibc and the MSVC
/// runtime matters only near an exact power of ten — but THERE it flips `exponent` by a whole one,
/// and `base` (and therefore the emitted tick spacing) by a factor of TEN. A `raw_step` of exactly
/// 100.0 whose `log10` comes back as `1.9999999999999998` on one box and `2.0` on the other yields
/// a step of 10 on one and 100 on the other, from identical inputs. Nothing downstream would flag
/// that as an error; the chart would simply carry a different gridline set on each platform.
pub(crate) fn nice_step_ceil(raw_step: f64) -> f64 {
    debug_assert!(raw_step > 0.0 && raw_step.is_finite());
    let exponent = libm::log10(raw_step).floor();
    let base = libm::pow(10.0, exponent);
    let fraction = raw_step / base;
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_fraction * base
}

/// Nice 1/2/5·10^k values covering `[lo, hi]`, sized to the `max_ticks`
/// budget. Used directly for Linear (raw space) and for Percent (percent
/// space, then unmapped back to raw by the caller). Sizing is fence-post
/// (`max_ticks - 1` divisions), which can emit one extra value at
/// `max_ticks == 1`; the shared tail of [`nice_ticks`] enforces the hard cap.
fn linear_nice_values(lo: f64, hi: f64, max_ticks: usize) -> Vec<f64> {
    let range = hi - lo;
    if !range.is_finite() || range <= 0.0 || max_ticks == 0 {
        return Vec::new();
    }
    // max_ticks - 1 "divisions" between the first and last tick keeps the
    // emitted count within budget (fence-post: N ticks = N-1 divisions).
    let divisions = max_ticks.saturating_sub(1).max(1) as f64;
    let step = nice_step_ceil(range / divisions);
    let eps = step * 1e-9;
    let first = (lo / step).ceil() * step;
    let mut out = Vec::new();
    let mut v = first;
    while v <= hi + eps {
        if v >= lo - eps {
            out.push(v);
        }
        v += step;
    }
    out
}

/// Candidate {1,2,5}·10^k raw values covering `[lo, hi]` (Log mode), thinned
/// to decades-only and then to every Nth decade if still over `max_ticks`.
///
/// ⚠ Second DECADE CLIFF site (the twin of [`nice_step_ceil`]'s), and here the quantizer is an
/// `as i32` cast rather than a bare `.floor()`: `k_min`/`k_max` are the INTEGER decade bounds of
/// the candidate loop, so a platform libm disagreeing on the last bit of `log10(lo)` at an exact
/// power of ten shifts the whole candidate set by a decade — a different gridline set on each box,
/// silently. Both logarithms therefore come from the deterministic `libm` crate.
fn log_nice_values(lo: f64, hi: f64, max_ticks: usize) -> Vec<f64> {
    // is_finite (not just NaN) is load-bearing: `libm::log10(hi).ceil() as i32`
    // saturates to i32::MAX for hi = +inf (documented Rust float->int cast
    // behavior), turning the candidate loop below into ~2.1e9 iterations.
    if !lo.is_finite() || lo <= 0.0 || !hi.is_finite() || hi <= lo || max_ticks == 0 {
        return Vec::new();
    }
    let k_min = libm::log10(lo).floor() as i32;
    let k_max = libm::log10(hi).ceil() as i32;
    let mults = [1.0, 2.0, 5.0];
    let mut all = Vec::new();
    for k in k_min..=k_max {
        // ⚠ DELIBERATELY a `powi` METHOD call, and it must STAY one — do not "finish the job" by
        // converting it alongside the logarithms above. `powi` takes an INTEGER exponent, and
        // MEASURED 2026-08-26 across the Windows dev box (MSVC) and the CI Linux runners (glibc),
        // `10f64.powi(n)` is BIT-IDENTICAL on both for every n in -22..=22 — the hash of the whole
        // sweep came back `a87aa0b0905b4eac` on each. That is not luck: powers of ten in that
        // range are exactly representable in `f64`, so there is no rounding for two libms to
        // disagree about. Rewriting it as `libm::pow(10.0, k as f64)` would buy nothing, and would
        // swap an exact integer power for the general transcendental path.
        let base = 10f64.powi(k);
        for &m in &mults {
            let val = m * base;
            let eps = val * 1e-9;
            if val >= lo - eps && val <= hi + eps {
                all.push(val);
            }
        }
    }
    all.sort_by(f64::total_cmp);
    all.dedup_by(|a, b| (*a - *b).abs() < a.abs().max(1.0) * 1e-9);

    // Sub-decade zoom: no {1,2,5}·10^k value falls inside [lo, hi] at all —
    // the COMMON case for a realistic single-instrument zoom level (e.g. BTC
    // $63,700-$64,200 straddles no decade value; the nearest are 50,000 and
    // 100,000). The decade-only grid is meant for multi-decade zoom-outs;
    // going empty here would blank the axis entirely (found via T2's
    // VIKE_SHOT smoke). Fall back to round numbers sized to the window
    // itself (same algorithm Linear uses) — still positioned via log10
    // mapping by the caller, so this is purely a candidate-value choice, not
    // a change to how ticks are placed. Deliberately NOT triggered by a
    // single-candidate result (`all.len() == 1`): that already has dedicated
    // coverage (`nice_ticks_single_tick_step_mapped_fallback_positive`) and
    // is a meaningfully different, legitimate case (exactly one decade value
    // in view) from "none at all".
    if all.is_empty() {
        return linear_nice_values(lo, hi, max_ticks);
    }

    if all.len() <= max_ticks {
        return all;
    }

    // Over budget: thin to decades only ({1}*10^k). `libm` for the same reason as the bounds
    // above: `.round()` on a logarithm is the decade quantizer, and this predicate is what
    // DECIDES membership — a last-bit disagreement at an exact power of ten changes which
    // candidates survive the thinning, not merely where they are drawn.
    let decades: Vec<f64> = all
        .into_iter()
        .filter(|&v| {
            let k = libm::log10(v).round();
            (v - libm::pow(10.0, k)).abs() < v * 1e-9
        })
        .collect();

    if decades.len() <= max_ticks {
        return decades;
    }

    // Still over budget: keep every Nth decade.
    let n = (decades.len() as f64 / max_ticks as f64).ceil().max(1.0) as usize;
    decades.into_iter().step_by(n).collect()
}

/// Nice ticks in RAW space (1/2/5·10^k for Linear/Log; nice percent steps for
/// Percent), emitted mapped. step_mapped = mapped distance to the next tick.
/// Non-inverted convenience wrapper over [`nice_ticks_view`] — byte-identical to
/// the pre-invert generator (the `ScaleView::new(mode, false)` map is exactly
/// `mode.map`, so every emitted `mapped`/`step_mapped`/`raw` is unchanged).
pub fn nice_ticks(
    mode: ScaleMode,
    raw_lo: f64,
    raw_hi: f64,
    anchor: f64,
    max_ticks: usize,
) -> Vec<GridTick> {
    nice_ticks_view(ScaleView::new(mode, false), raw_lo, raw_hi, anchor, max_ticks)
}

/// [`nice_ticks`] with the invert modifier: the RAW tick POSITIONS (`.raw`) are
/// generated identically (same 1/2/5·10^k prices), and only each tick's plotted
/// `.mapped` is passed through `view.map` — so an inverted axis shows the SAME
/// price gridlines, just flipped top-to-bottom. `step_mapped` stays an absolute
/// mapped distance (invert preserves distances), so egui_plot's step-size culling
/// is unaffected.
pub fn nice_ticks_view(
    view: ScaleView,
    raw_lo: f64,
    raw_hi: f64,
    anchor: f64,
    max_ticks: usize,
) -> Vec<GridTick> {
    let mode = view.mode;
    // Non-finite extents happen in practice (an unfilled ±inf extent fold);
    // they must yield no ticks, never a hang (Log's decade loop) or a
    // build-mode-dependent panic (nice_step_ceil's debug_assert backstop).
    if !raw_lo.is_finite()
        || !raw_hi.is_finite()
        || raw_hi <= raw_lo
        || max_ticks == 0
        || (matches!(mode, ScaleMode::Percent | ScaleMode::Indexed) && !anchor.is_finite())
    {
        return Vec::new();
    }

    let mut raws: Vec<f64> = match mode {
        ScaleMode::Linear => linear_nice_values(raw_lo, raw_hi, max_ticks),
        ScaleMode::Log => log_nice_values(raw_lo, raw_hi, max_ticks),
        // Percent and Indexed share one raw-candidate path: nice values in the
        // (linear-ish) mapped space, unmapped back to raw prices.
        ScaleMode::Percent | ScaleMode::Indexed => {
            let lo_p = mode.map(raw_lo, anchor);
            let hi_p = mode.map(raw_hi, anchor);
            let (lo_p, hi_p) = if lo_p <= hi_p { (lo_p, hi_p) } else { (hi_p, lo_p) };
            linear_nice_values(lo_p, hi_p, max_ticks)
                .into_iter()
                .map(|p| mode.unmap(p, anchor))
                .collect()
        }
    };
    // Percent's unmap can invert order when anchor < 0; Linear/Log are
    // already ascending. Sorting unconditionally keeps the invariant cheap
    // to guarantee rather than conditionally-correct.
    raws.sort_by(f64::total_cmp);
    raws.dedup_by(|a, b| (*a - *b).abs() < a.abs().max(1.0) * 1e-9);

    let eps = (raw_hi - raw_lo).abs() * 1e-9;
    let mut raws: Vec<f64> =
        raws.into_iter().filter(|&r| r >= raw_lo - eps && r <= raw_hi + eps).collect();
    // Hard max_ticks cap for ALL modes (the generators size to the budget but
    // fence-post rounding can overshoot by one, e.g. Linear at max_ticks=1).
    raws.truncate(max_ticks);
    if raws.is_empty() {
        return Vec::new();
    }

    // Positions through `view.map` (the ONLY invert-aware step): the flip negates
    // each mapped y; `step_mapped` reads absolute distances so it is unchanged.
    let mapped: Vec<f64> = raws.iter().map(|&r| view.map(r, anchor)).collect();
    let mut ticks = Vec::with_capacity(raws.len());
    for i in 0..raws.len() {
        let step_mapped = if i + 1 < mapped.len() {
            (mapped[i + 1] - mapped[i]).abs()
        } else if i > 0 {
            (mapped[i] - mapped[i - 1]).abs()
        } else {
            // Single-tick fallback: no neighbor to derive a step from.
            mapped[i].abs().max(1.0)
        };
        ticks.push(GridTick { mapped: mapped[i], step_mapped, raw: raws[i] });
    }
    ticks
}

// ⚠ The assertions below RE-DERIVE the module's own 1/2/5·10^k decomposition in order to check it
// (`log10().floor()` then a power of ten, to recover a mantissa). Those re-derivations go through
// the `libm` crate too, for a reason the production argument does not cover: a test that quantizes
// a PLATFORM logarithm is itself a decade cliff, so a mantissa recovered with `f64::log10` could
// disagree with the value the production code (now on `libm`) actually produced, and this suite
// would go red on Windows while staying green on the CI runners — a false failure hunting a real
// one. Keeping both sides on the same deterministic implementation removes that class outright.
#[path = "scale_tests.rs"]
#[cfg(test)]
mod scale_tests;
