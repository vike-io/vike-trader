//! Level-of-detail (LOD) decimation — LOD Phase 1's pure core. When a chart is zoomed out far
//! enough that the visible bar count exceeds the available screen columns, painting every bar is
//! wasted work and renders as visual mush; [`lod_decimate`] collapses the visible slice down to
//! roughly one min-max OHLC bar per screen column before it reaches a painter. Task 2 (not this
//! file) wires the result into `render.rs`'s per-style painters.
//!
//! No egui/GUI dependency here — this is a pure `&[Bar] -> Cow<[Bar]>` transform, unit-tested in
//! isolation.

use crate::interact::visible_slice;
use crate::model::Bar;
use std::borrow::Cow;

/// Decimate the visible slice (`x0..=x1` mapped through the SAME bounds
/// [`crate::interact::visible_slice`] uses) to at most `n_columns` min-max OHLC bars.
///
/// **IDENTITY** (zero-copy `Cow::Borrowed`): when the visible bar count is `<= n_columns`, or
/// `n_columns == 0` (nothing to divide the range into — treated as "don't decimate"), this
/// returns EXACTLY what [`crate::interact::visible_slice`] returns. `lod_decimate` calls that
/// helper directly rather than re-deriving its clamping/rounding, so normal-zoom rendering
/// through `lod_decimate` is byte-identical to today's pre-LOD `vis(bars, x0, x1)` render path —
/// the load-bearing guarantee this function exists to preserve.
///
/// **DECIMATE** (`Cow::Owned`): when the visible count exceeds `n_columns`, the visible range is
/// split into `n_columns` contiguous, near-equal buckets (sizes differ by at most one bar — the
/// standard balanced contiguous partition; every bucket is non-empty because this branch only
/// runs when visible count > n_columns). Each bucket folds to one `Bar`:
/// - `t`, `ot` — the bucket's FIRST bar's index/open-time (a decimated candle "opens" when its
///   first constituent bar does).
/// - `o` — the bucket's first bar's open.
/// - `h` / `l` — naive `f64::max`/`f64::min` folds over the whole bucket (order-independent, so a
///   plain fold is fine here — no compensated-summation concern the way there would be for a
///   running total).
/// - `c` — the bucket's last bar's close.
/// - `v` — SUMMED over the bucket. This is the one field the brief left as a pick: summing (not
///   carrying a single constituent bar's volume) matches the standard OHLCV downsampling
///   convention — a decimated candle's volume is the total volume traded across every bar it
///   stands in for.
///
/// Never panics: empty `bars`, `x0 >= x1` / zero-width range, `n_columns == 0`, and out-of-range
/// `x0`/`x1` all fall out of `visible_slice`'s own clamping (possibly to an empty slice) rather
/// than needing special-casing here.
pub fn lod_decimate<'a>(bars: &'a [Bar], x0: f64, x1: f64, n_columns: usize) -> Cow<'a, [Bar]> {
    let vis = visible_slice(bars, x0, x1);
    let n = vis.len();
    if n_columns == 0 || n <= n_columns {
        return Cow::Borrowed(vis);
    }
    // Balanced contiguous partition into `n_columns` near-equal buckets: bucket `i` covers
    // `[i*n/n_columns, (i+1)*n/n_columns)` (integer floor division). Sizes differ by at most
    // one bar; every bucket is non-empty here because this branch only runs when `n >
    // n_columns`; the final boundary lands exactly on `n` (`n_columns * n / n_columns == n`
    // exactly, no truncation loss).
    let mut out = Vec::with_capacity(n_columns);
    for i in 0..n_columns {
        let start = i * n / n_columns;
        let end = (i + 1) * n / n_columns;
        let bucket = &vis[start..end];
        let first = bucket[0];
        let last = bucket[bucket.len() - 1];
        let h = bucket.iter().map(|b| b.h).fold(f64::MIN, f64::max);
        let l = bucket.iter().map(|b| b.l).fold(f64::MAX, f64::min);
        let v: f64 = bucket.iter().map(|b| b.v).sum();
        out.push(Bar { t: first.t, ot: first.ot, o: first.o, h, l, c: last.c, v });
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic bars with distinct per-bar o/c/v and a NON-monotonic h/l pattern (a
    /// triangular wave that peaks/troughs at the midpoint of every 100-bar block) so a
    /// min/max-over-bucket assertion genuinely exercises the fold across the whole bucket
    /// instead of trivially matching whichever bar happens to be first or last.
    fn mk_bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let fi = i as f64;
                let local = (i % 100) as i64;
                let tri = (50 - (local - 50).abs()) as f64; // 0 at block edges, peaks at 50 mid-block
                Bar {
                    t: fi,
                    ot: 1_700_000_000_000 + i as i64 * 60_000,
                    o: fi,
                    h: 1_000.0 + tri,
                    l: -1_000.0 - tri,
                    c: fi + 0.5,
                    v: 1.0 + (i % 5) as f64,
                }
            })
            .collect()
    }

    /// Test-local mirror of the "vis()" visible-range helper: calls the REAL
    /// [`crate::interact::visible_slice`] directly (not a reimplementation of its
    /// bounds/clamping/rounding), so this genuinely tests `lod_decimate`'s identity path against
    /// the ground truth rather than against a second copy of the same logic that could drift
    /// independently.
    fn vis_slice(bars: &[Bar], x0: f64, x1: f64) -> &[Bar] {
        visible_slice(bars, x0, x1)
    }

    #[test]
    fn identity_below_budget_borrows_exact_visible_slice() {
        let bars = mk_bars(10); // t=0..9
        let out = lod_decimate(&bars, 2.0, 7.0, 100); // budget 100 >> visible count -> identity
        assert!(matches!(out, Cow::Borrowed(_)));
        let expected = vis_slice(&bars, 2.0, 7.0);
        assert_eq!(&*out, expected);
        // genuine zero-copy: same backing memory, not merely equal values.
        assert_eq!(out.as_ptr(), expected.as_ptr());
    }

    #[test]
    fn decimates_to_minmax_buckets_above_budget() {
        let bars = mk_bars(1000);
        let out = lod_decimate(&bars, 0.0, 1000.0, 10); // 1000 visible -> 10 buckets of 100
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out.len(), 10);

        // bucket 0 spans bars 0..100.
        assert_eq!(out[0].t, bars[0].t);
        assert_eq!(out[0].ot, bars[0].ot);
        assert_eq!(out[0].o, bars[0].o);
        assert_eq!(out[0].c, bars[99].c);
        assert_eq!(out[0].h, bars[..100].iter().map(|b| b.h).fold(f64::MIN, f64::max));
        assert_eq!(out[0].l, bars[..100].iter().map(|b| b.l).fold(f64::MAX, f64::min));
        assert_eq!(out[0].v, bars[..100].iter().map(|b| b.v).sum::<f64>());
        // the block's h/l peak strictly inside the bucket (index 50), not at either edge --
        // proves the fold actually scans the whole bucket rather than just first/last.
        assert_eq!(out[0].h, bars[50].h);
        assert_eq!(out[0].l, bars[50].l);

        // bucket 9 (last) spans bars 900..1000, same shape.
        assert_eq!(out[9].t, bars[900].t);
        assert_eq!(out[9].c, bars[999].c);
        assert_eq!(out[9].h, bars[900..1000].iter().map(|b| b.h).fold(f64::MIN, f64::max));
        assert_eq!(out[9].l, bars[900..1000].iter().map(|b| b.l).fold(f64::MAX, f64::min));
        assert_eq!(out[9].v, bars[900..1000].iter().map(|b| b.v).sum::<f64>());
    }

    #[test]
    fn degenerate_ranges_do_not_panic() {
        let bars = mk_bars(5);

        // x0 == x1 (zero-width range): visible_slice's own +/-1-bar margin still yields a
        // couple of bars here -- this asserts no panic AND exact agreement with vis(), not a
        // hand-guessed length (the margin is visible_slice's contract, not lod_decimate's).
        let out = lod_decimate(&bars, 0.0, 0.0, 10);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(&*out, vis_slice(&bars, 0.0, 0.0));

        // n_columns == 0 -> treated as identity/degenerate, no div-by-zero, regardless of range.
        let out = lod_decimate(&bars, -5.0, 100.0, 0);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(&*out, &bars[..]);

        // empty bars -> empty, whatever the range/budget.
        let out = lod_decimate(&[], 0.0, 10.0, 5);
        assert!(out.is_empty());

        // out-of-range x0/x1 entirely past the data -> empty, no panic.
        let out = lod_decimate(&bars, 500.0, 600.0, 3);
        assert!(out.is_empty());
    }
}
