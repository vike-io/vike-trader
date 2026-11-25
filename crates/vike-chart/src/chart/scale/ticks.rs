//! The y-axis tick generator: nice 1/2/5·10^k values in RAW price space, sized to a tick budget
//! and emitted mapped through a [`ChartScaleMapping`] ([`nice_ticks_view`]). Why its logarithms
//! come from the `libm` crate, and its powers of ten from `vike_math::powers::power_of_ten`, is
//! `super`'s module doc.

use super::{ChartScaleMapping, ChartScaleMode};

/// One y-axis tick: mapped position + fabricated MAPPED step_size (egui_plot
/// culls/fades by step_size in plot units) + the raw price it labels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartScaleTick {
    pub mapped: f64,
    pub step_mapped: f64,
    pub raw: f64,
}

/// Nice 1/2/5·10^k values covering `[lo, hi]`, sized to the `max_ticks`
/// budget. Used directly for Linear (raw space) and for Percent (percent
/// space, then unmapped back to raw by the caller). Sizing is fence-post
/// (`max_ticks - 1` divisions), which can emit one extra value at
/// `max_ticks == 1`; the shared tail of [`nice_ticks_view`] enforces the hard cap. The step is
/// Heckbert's ceiling (`vike_math::steps::nice_step`, rounded `Up`), so the tick count stays within
/// the budget; the guard below keeps its input positive and finite.
fn linear_nice_values(lo: f64, hi: f64, max_ticks: usize) -> Vec<f64> {
    let range = hi - lo;
    if !range.is_finite() || range <= 0.0 || max_ticks == 0 {
        return Vec::new();
    }
    // max_ticks - 1 "divisions" between the first and last tick keeps the
    // emitted count within budget (fence-post: N ticks = N-1 divisions).
    let divisions = max_ticks.saturating_sub(1).max(1) as f64;
    let step =
        vike_math::steps::nice_step(range / divisions, vike_math::steps::NiceStepRounding::Up);
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
/// ⚠ Second DECADE CLIFF site (the twin of `vike_math::steps::nice_step`'s), and here the quantizer
/// is an `as i32` cast rather than a bare `.floor()`: `k_min`/`k_max` are the INTEGER decade bounds of
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
        // The exact power of ten, never `libm::pow(10.0, k as f64)` (the general transcendental path
        // for an exact integer power). `10f64.powi(n)` was MEASURED bit-identical on MSVC and glibc for
        // every n in -22..=22 (the sweep hashed `a87aa0b0905b4eac` on each), and `power_of_ten` gives
        // those same bits. Past ±22 no power of ten is exact, so such a decade draws no line rather
        // than one whose position depends on the box (no axis a chart shows reaches it).
        let Some(base) = vike_math::powers::power_of_ten(k) else { continue };
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
/// A non-inverted view (`ChartScaleMapping::new(mode, false)`) maps exactly as
/// `mode.map`, the pre-invert generator.
///
/// With the invert modifier, the RAW tick POSITIONS (`.raw`) are
/// generated identically (same 1/2/5·10^k prices), and only each tick's plotted
/// `.mapped` is passed through `view.map` — so an inverted axis shows the SAME
/// price gridlines, just flipped top-to-bottom. `step_mapped` stays an absolute
/// mapped distance (invert preserves distances), so egui_plot's step-size culling
/// is unaffected.
pub fn nice_ticks_view(
    view: ChartScaleMapping,
    raw_lo: f64,
    raw_hi: f64,
    anchor: f64,
    max_ticks: usize,
) -> Vec<ChartScaleTick> {
    let mode = view.mode;
    // Non-finite extents happen in practice (an unfilled ±inf extent fold);
    // they must yield no ticks, never a hang (Log's decade loop) or a
    // NaN step (`vike_math::steps::nice_step` answers NaN for a step that is not finite).
    if !raw_lo.is_finite()
        || !raw_hi.is_finite()
        || raw_hi <= raw_lo
        || max_ticks == 0
        || (matches!(mode, ChartScaleMode::Percent | ChartScaleMode::Indexed)
            && !anchor.is_finite())
    {
        return Vec::new();
    }

    let mut raws: Vec<f64> = match mode {
        ChartScaleMode::Linear => linear_nice_values(raw_lo, raw_hi, max_ticks),
        ChartScaleMode::Logarithmic => log_nice_values(raw_lo, raw_hi, max_ticks),
        // Percent and Indexed share one raw-candidate path: nice values in the
        // (linear-ish) mapped space, unmapped back to raw prices.
        ChartScaleMode::Percent | ChartScaleMode::Indexed => {
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
        ticks.push(ChartScaleTick { mapped: mapped[i], step_mapped, raw: raws[i] });
    }
    ticks
}
