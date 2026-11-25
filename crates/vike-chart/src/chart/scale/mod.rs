//! Price-scale modes (chart UX bundle §1, `docs/superpowers/specs/2026-07-07-chart-ux-bundle-design.md`).
//!
//! `ChartScaleMode` is the mapping seam between raw price space (the source of
//! truth everywhere else — `ChartState`, y-extent caches, future drawings)
//! and the mapped plot space egui_plot actually renders. Every series y-value
//! passes through [`ChartScaleMode::map`] before reaching egui_plot; every
//! user-facing readback (crosshair tag, axis label) goes through
//! [`ChartScaleMode::unmap`]. This module owns only the pure mapping + nice-tick
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
//! `crates/vike-chart/tests/headless/tessellation_goldens.rs`'s
//! `a_one_ulp_price_perturbation_leaves_every_record_unchanged` nudges every fixture price by a
//! full `f64` ULP — roughly ten times the worst libm disagreement — and asserts all sixteen frame
//! records stay byte-identical; its own failure message calls a pass "a direct statement that a
//! libm difference cannot rebaseline this suite". Two things that gate does NOT cover, which is
//! the whole reason the conversion is still worth doing:
//!
//! * It perturbs INPUT PRICES, so it cannot probe the DECADE CLIFF. The Linear step
//!   (`crates/vike-math/src/steps.rs`'s `nice_step`) and `log_nice_values` both feed a logarithm
//!   straight into `.floor()`/`.round()`/`as i32`, and a last-bit disagreement either side of an
//!   exact power of ten flips that integer by one — a TEN-FOLD change in the chosen tick step or
//!   the decade loop's bounds, not a rounding wobble. A one-ULP price nudge lands nowhere near
//!   such a boundary, so the gate is green either way.
//! * It renders no PointFigure scenario (the fifteen goldens are candles/line/HeikinAshi/Renko),
//!   so `crates/vike-chart/src/chart/style/synthetic.rs`'s `draw_pnf` ellipse ring — this crate's other libm
//!   consumer — is on no golden at all.
//!
//! The decade loop's powers of ten (`log_nice_values`, in `ticks.rs`) are
//! `vike_math::powers::power_of_ten`: exact, the bits `10f64.powi(k)` was measured to give on both
//! platforms, and no answer past ±22. See the comment at that site.

/// How raw price maps to the plotted y value.
#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ChartScaleMode {
    #[default]
    Linear,
    /// log10 of price. Requires strictly positive prices — see `ChartScaleMode::supports`.
    Logarithmic,
    /// percent change vs an anchor price: `(y / anchor - 1) * 100`.
    Percent,
    /// TradingView "Indexed to 100": rebase the visible series so the first-visible
    /// value reads 100, everything else relative — `100 * y / anchor`. Structurally
    /// the Percent twin (same first-visible `anchor`, same fallback guard: a
    /// zero/non-finite anchor degrades to Linear), differing only in the affine
    /// mapping and its plain-number axis labels (around 100, no `%`).
    Indexed,
}

impl ChartScaleMode {
    /// raw price -> mapped plot value. anchor used by Percent/Indexed only.
    ///
    /// Percent delegates to `percent_change` (DRY — verified bit-identical: every real call site
    /// reaches this with mode already narrowed to `Percent` via `effective_mode`/`resolve_scale`,
    /// which guarantees `anchor != 0.0 && anchor.is_finite()` per `supports()`, so `percent_change`'s
    /// degenerate-anchor guard is never actually exercised here — it only makes this total instead
    /// of producing inf/NaN if ever called out-of-contract). Indexed delegates to `indexed`
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
            ChartScaleMode::Linear => y,
            ChartScaleMode::Logarithmic => libm::log10(y),
            ChartScaleMode::Percent => percent_change(y, anchor),
            ChartScaleMode::Indexed => indexed(y, anchor),
        }
    }

    /// mapped plot value -> raw price.
    ///
    /// Log's inverse is `libm::pow(10.0, v)`, not `10f64.powf(v)`. The base being the literal 10
    /// is NOT enough to make this exact: `v` is an arbitrary runtime plot-space y (a pixel row's
    /// worth of log10 price, essentially never an integer), so the result is `10^2.8017…` and
    /// nothing about it is exactly representable — it goes through the platform's `pow`, which
    /// IEEE 754 leaves unconstrained. (The exactly-representable case is the INTEGER exponent, and
    /// that one is `vike_math::powers::power_of_ten` — see `log_nice_values`' comment.)
    pub fn unmap(self, v: f64, anchor: f64) -> f64 {
        match self {
            ChartScaleMode::Linear => v,
            ChartScaleMode::Logarithmic => libm::pow(10.0, v),
            ChartScaleMode::Percent => anchor * (1.0 + v / 100.0),
            ChartScaleMode::Indexed => anchor * v / 100.0,
        }
    }

    /// Whether this mode can represent the given raw extents (Log: lo > 0;
    /// Percent/Indexed: anchor != 0 — both rebase by dividing by the anchor).
    pub(crate) fn supports(self, raw_lo: f64, anchor: f64) -> bool {
        match self {
            ChartScaleMode::Linear => true,
            ChartScaleMode::Logarithmic => raw_lo > 0.0,
            ChartScaleMode::Percent | ChartScaleMode::Indexed => {
                anchor != 0.0 && anchor.is_finite()
            }
        }
    }
}

/// A resolved price-scale transform: a [`ChartScaleMode`] plus the orthogonal
/// vertical-invert modifier (TradingView "Invert scale"). Invert is applied as a
/// FINAL negation of the mapped plot value, so ANY mode (Linear/Log/Percent/
/// Indexed) can be flipped top-to-bottom while egui_plot still renders normal
/// ascending (min < max) bounds. `invert == false` delegates byte-identically to
/// the bare [`ChartScaleMode`] methods — the whole render path is unchanged when invert
/// is off (the sole reason every seam site routes through `ChartScaleMapping` rather than
/// growing a second `invert` argument).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ChartScaleMapping {
    pub mode: ChartScaleMode,
    pub invert: bool,
}

impl ChartScaleMapping {
    pub const fn new(mode: ChartScaleMode, invert: bool) -> Self {
        ChartScaleMapping { mode, invert }
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
    /// hold pre-mapped values directly (Percent compare-overlay `percent_change`
    /// lines, crosshair/label readbacks). Identity when `invert == false`, so
    /// those sites stay byte-identical.
    pub fn flip(self, mapped: f64) -> f64 {
        if self.invert { -mapped } else { mapped }
    }
}

/// Percent-change of `value` vs `anchor` (a series' first-visible close): (value/anchor - 1) * 100.
/// anchor == 0 or non-finite -> 0.0 (no divide-by-zero / NaN leak). Standalone (not gated behind
/// `ChartScaleMode::supports`) so C2a's multi-symbol %-overlays can rebase a compare series without
/// going through the primary's scale-fallback-latch machinery; `ChartScaleMode::Percent::map` (above)
/// delegates here for the identical primary-axis rebasing (one source of truth).
pub(crate) fn percent_change(value: f64, anchor: f64) -> f64 {
    if anchor.is_finite() && anchor != 0.0 { (value / anchor - 1.0) * 100.0 } else { 0.0 }
}

/// Index-to-100 of `value` vs `anchor` (a series' first-visible close):
/// `100 * value / anchor`. anchor == 0 or non-finite -> 100.0 (the index base;
/// no divide-by-zero / NaN leak). The Indexed twin of [`percent_change`] — same
/// anchor role and same degenerate-guard discipline; `ChartScaleMode::Indexed::map`
/// delegates here so the `100 * value / anchor` rebasing has ONE source of truth.
pub(crate) fn indexed(value: f64, anchor: f64) -> f64 {
    if anchor.is_finite() && anchor != 0.0 { 100.0 * value / anchor } else { 100.0 }
}

/// How a compare series is scaled against the price pane. `Percent` (default) rebases
/// to first-visible on the shared % axis (C2a). `Right`/`Left` pin the series to a
/// secondary ABSOLUTE price axis on that side (its own price range, remapped into the
/// primary's plot-space via `remap_to_primary`). `Primary` renders the series at its
/// TRUE ABSOLUTE price on the PRIMARY axis, sharing the one price scale — no rebasing, no
/// secondary axis. Its visible extents fold into the primary's y-autofit so it can't be
/// clipped. Only meaningful (and only rendered) when the primary's effective scale is
/// Linear or Logarithmic — an absolute-price overlay has no sensible mapping onto a
/// Percent/Indexed (rebased) primary axis. On a Logarithmic primary the compare's closes pass
/// through the primary's own `map` (`log10`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ChartScaleAssignment {
    #[default]
    Percent,
    Right,
    Left,
    /// Absolute price on the primary price axis, on a Linear and a Logarithmic primary: the
    /// closes pass through the primary's own `map` and share its y-autofit. It parallels `Right`
    /// and `Left`, the secondary axes. The variant name is the stored `workspace.json` word
    /// (`WindowSnapshot::series_scale`).
    Primary,
}

/// Linear-map a secondary series' absolute value `v` (in its own visible range
/// `sec_lo..=sec_hi`) into the primary's plot-space y-range `prim_lo..=prim_hi`, so a
/// `Right`/`Left`-pinned overlay tracks its OWN price range while sharing the one plot.
/// Degenerate `sec_lo == sec_hi` (flat/one-point range) -> the primary midpoint
/// `(prim_lo+prim_hi)/2` (no divide-by-zero / NaN).
pub(crate) fn remap_to_primary(
    v: f64,
    sec_lo: f64,
    sec_hi: f64,
    prim_lo: f64,
    prim_hi: f64,
) -> f64 {
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
pub(crate) fn inverse_remap(pv: f64, sec_lo: f64, sec_hi: f64, prim_lo: f64, prim_hi: f64) -> f64 {
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
pub(crate) fn effective_mode(mode: ChartScaleMode, raw_lo: f64, anchor: f64) -> ChartScaleMode {
    if mode.supports(raw_lo, anchor) { mode } else { ChartScaleMode::Linear }
}

/// Whether `(raw_lo, anchor)` reflects ACTUAL observed data for `mode`, as
/// opposed to an absence-of-data sentinel (chart-UX bundle T3 carry-over
/// fix): `ChartFollowLive::resolve_scale`'s sticky-fallback latch (`interact.rs`)
/// must only engage from a genuine unsupported DATA value — e.g. `raw_lo`
/// finite but `<= 0.0` from real non-positive visible prices (Log), or a
/// finite `anchor` of exactly `0.0`/subnormal from a real closed bar's close
/// (Percent) — never from a data-absent frame: an empty visible slice (Log,
/// `raw_lo = NaN`) or no closed bar existing yet (Percent, `anchor = NaN`).
/// Callers pass `NaN` for exactly that data-absent case; any other value
/// (including a genuine non-positive `raw_lo` or a genuine zero `anchor`)
/// counts as data. Linear always "has data" — it never fails `supports()`,
/// so this is never consulted for it in practice.
pub(crate) fn has_data(mode: ChartScaleMode, raw_lo: f64, anchor: f64) -> bool {
    match mode {
        ChartScaleMode::Linear => true,
        ChartScaleMode::Logarithmic => raw_lo.is_finite(),
        ChartScaleMode::Percent | ChartScaleMode::Indexed => anchor.is_finite(),
    }
}

/// Convert a persisted MAPPED y-range from `old_view`'s space to `new_view`'s
/// space via `unmap_old -> map_new` (chart-UX bundle T2 §1 bounds-space
/// migration): egui_plot persists y-bounds numerically in whatever space they
/// were last written, so a mode flip must rewrite them or the view silently
/// reinterprets old numbers in the new space (e.g. a linear price range
/// treated as log10 exponents). Percent re-anchoring alone must NOT go
/// through this path — only genuine mode flips do. Defensively sorts the
/// output: `Percent.map` can invert order for a negative anchor (same
/// precedent as [`ticks::nice_ticks_view`]'s own sort).
///
/// It migrates persisted bounds when EITHER the mode OR the invert flag changed
/// (an invert toggle alone leaves the mode identical but must still negate the
/// persisted mapped range, else the same data range would be reinterpreted in the
/// flipped space and scroll off-screen). The `unmap`/`map` round-trip through raw
/// price space carries the flip on both ends. A pure mode flip is two
/// non-inverted views (`ChartScaleMapping::new(mode, false)`), whose map is exactly
/// `mode.map`.
pub(crate) fn convert_bounds_view(
    old_view: ChartScaleMapping,
    new_view: ChartScaleMapping,
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

pub mod ticks;

//⚠ The assertions below RE-DERIVE the module's own 1/2/5·10^k decomposition in order to check it
// (`log10().floor()` then a power of ten, to recover a mantissa). Those re-derivations go through
// the `libm` crate too, for a reason the production argument does not cover: a test that quantizes
// a PLATFORM logarithm is itself a decade cliff, so a mantissa recovered with `f64::log10` could
// disagree with the value the production code (now on `libm`) actually produced, and this suite
// would go red on Windows while staying green on the CI runners — a false failure hunting a real
// one. Keeping both sides on the same deterministic implementation removes that class outright.
#[cfg(test)]
mod tests;
