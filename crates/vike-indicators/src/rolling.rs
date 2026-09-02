//! PANEL-level rolling blocks: one [`crate::feature::ColumnFeature`] applied to EVERY column of a
//! [`crate::frame::Columns`] bag, per asset, with a renaming convention.
//!
//! # Where the boundary is between this module and its two neighbours
//!
//! * [`crate::window`] owns the arithmetic — one kernel over one `&[f64]`, plus
//!   [`crate::window::per_group`], which applies a kernel per group so a window cannot reach across
//!   an instrument boundary. **This module writes no window kernel of its own**; one home for that
//!   was the whole point of the kernel extraction and a second one here would be the drift it
//!   exists to prevent.
//! * [`crate::feature`] owns the streaming↔batch seam — a [`crate::feature::ColumnFeature`] is a
//!   batch kernel plus a truthful retained-history bound, and its streaming form is that same
//!   kernel over a bounded tail, so a live strategy cannot compute a different number from the one
//!   a study trained on.
//! * This module is the FAN-OUT over both: a panel's whole column bag × its asset spans, in one
//!   call, producing renamed columns in the input's own order.
//!
//! Every block below names a `ColumnFeature` and hands it to [`per_group_map`], so the numbers it
//! produces are the numbers `crates/vike-indicators/tests/feature_parity.rs` already gates
//! streaming-equals-batch — by construction rather than by two spellings that happen to agree.

use std::ops::Range;

use crate::feature::{ColumnFeature, WindowFeature, WindowStat};
use crate::frame::Columns;
use crate::window;

/// Cap on `|z|` for a BOUNDED input series, as `[`zscore_block`]` applies it.
///
/// A series bounded in `[-1, 1]` can pin near a bound for long stretches — MEASURED on the
/// research panel this block came from: `bias_Smart` held -0.983 ± 0.0005 for twenty consecutive
/// hours — which collapses the trailing standard deviation and explodes the ratio: observed `|z|`
/// up to 281. Those are DENOMINATOR failures, not large moves, and they dominate any effect size
/// measured on the feature. Clipping preserves ordering while bounding the range.
///
/// ⚠ It is deliberately NOT applied to every z-score. A z-score over an UNBOUNDED series (a price
/// return, an open-interest level) has no collapsing-denominator regime of this kind, and clipping
/// it would discard a real excursion — so [`crate::window::zscore`] takes the clip as an
/// `Option<f64>` and this constant is one caller's answer, not the module's.
///
/// `crates/vike-indicators/src/test_support.rs`'s `SHAPES` carries the `near-bound` generator
/// written for exactly this regime — values pinned within ~5e-4 of a bound WITHOUT being
/// bit-identical, so the constant-window shortcut does not fire and the ratio explodes instead.
pub const Z_CLIP: f64 = 6.0;

/// `x[i] - x[i - lag]`, NaN for the first `lag` rows. pandas `g - g.shift(lag)`.
pub fn lagged_diff(x: &[f64], lag: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for i in lag..x.len() {
        out[i] = x[i] - x[i - lag];
    }
    out
}

/// [`lagged_diff`] as a [`ColumnFeature`], so a live strategy can compute it one row at a time and
/// get the number the training matrix holds.
///
/// ⚠ This is NOT expressible as a [`WindowFeature`], and the difference is semantic rather than
/// cosmetic. A window kernel blanks its whole window when `min_periods` is not met, so one NaN
/// anywhere inside `lag + 1` rows makes the answer NaN. A lagged difference reads exactly TWO rows
/// and still answers a number when the rows between them are missing. Bending it into a window
/// preset would change what it computes; giving it its own two-line impl does not.
///
/// The reach is `lag + 1` and it is EXACT: the value at row `i` reads `x[i]` and `x[i - lag]`, so
/// the deepest row it touches is `lag` back and one fewer retained value would answer NaN where a
/// number belongs. `crate::test_support::assert_reach_is_tight` is what keeps that measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaggedDiff {
    /// How many rows back the subtrahend sits.
    pub lag: usize,
}

impl LaggedDiff {
    pub fn new(lag: usize) -> Self {
        Self { lag }
    }
}

impl ColumnFeature for LaggedDiff {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        lagged_diff(x, self.lag)
    }

    fn reach(&self) -> usize {
        self.lag.saturating_add(1)
    }
}

/// [`crate::window::per_group`] over EVERY column of `cols`, renaming as it goes. Output insertion
/// order MIRRORS the input's — that is what keeps a derived block's column order a pure function
/// of the input's.
///
/// `groups` are the panel's per-asset row ranges — [`crate::frame::Frame::span_ranges`] is where
/// they come from, and deriving them from the same index the columns were built on is what makes
/// them impossible to disagree with.
///
/// A block kernel returns one output value per input row by construction (every one this module
/// ships is a [`ColumnFeature`], whose contract says exactly that), so the `expect` below is a
/// genuine invariant rather than a shortcut: a mismatch here would be a bug in the kernel, not in
/// caller data — which is why this function's signature stays infallible rather than propagating a
/// `Result` nothing here can actually produce. The one thing a CALLER can get wrong — groups that
/// do not fit the columns — is caught by the same `expect`, naming the column.
pub fn per_group_map<K>(
    cols: &Columns,
    groups: &[Range<usize>],
    rename: impl Fn(&str) -> String,
    kernel: K,
) -> Columns
where
    K: Fn(&[f64]) -> Vec<f64>,
{
    let mut out = Columns::with_capacity(cols.len());
    for (name, values) in cols {
        let dst = rename(name);
        let v = window::per_group(values, groups, &dst, |s| kernel(s))
            .expect("per_group_map: a fixed-shape block kernel returned the wrong length");
        out.insert(dst, v);
    }
    out
}

/// `z{window}_<col>` — the POINT-IN-TIME z-score: the window EXCLUDES the current row (`lag = 1`),
/// divides by `n - 1` (`ddof = 1`, pandas' `rolling().std()` default), and clips to ±[`Z_CLIP`].
///
/// The NUMERATOR is `x[t]` itself, which is not lookahead — `t` is the present. The WINDOW is what
/// must not contain it, and [`crate::window::WindowSpec::point_in_time`]'s `lag = 1` is what
/// enforces that.
pub fn zscore_block(cols: &Columns, groups: &[Range<usize>], window: usize) -> Columns {
    let f = WindowFeature::point_in_time(WindowStat::ZScore { clip: Some(Z_CLIP) }, window);
    per_group_map(cols, groups, |n| format!("z{window}_{n}"), |x| f.vectorize(x))
}

/// `pr{window}_<col>` — the POINT-IN-TIME percentile rank.
///
/// ⚠ At `lag = 1` this ranks the PREVIOUS row's value inside the window ending there — NOT the
/// current value. That is [`crate::window::rolling_rank_pct`]'s stated behaviour ("this ranks the
/// value at the window's end") and it is what pandas' own
/// `g.shift(1).rolling(w, min_periods=w).rank(pct=True)` computes, because ranking a shifted
/// series ranks the shifted series' last element. Prose describing it as "the rank of the current
/// value" has been written more than once and is wrong; the kernel is the authority.
pub fn rank_pct_block(cols: &Columns, groups: &[Range<usize>], window: usize) -> Columns {
    let f = WindowFeature::point_in_time(WindowStat::RankPct, window);
    per_group_map(cols, groups, |n| format!("pr{window}_{n}"), |x| f.vectorize(x))
}

/// `d{lag}_<col>` — [`LaggedDiff`] per asset.
pub fn delta_block(cols: &Columns, groups: &[Range<usize>], lag: usize) -> Columns {
    let f = LaggedDiff::new(lag);
    per_group_map(cols, groups, |n| format!("d{lag}_{n}"), |x| f.vectorize(x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;

    /// One panel per `(asset, row count)`, on an hourly seconds grid.
    fn frame_of(assets: &[(&str, usize)]) -> Frame {
        let mut idx = Vec::new();
        for (a, n) in assets {
            for i in 0..*n {
                idx.push(((*a).to_string(), i as i64 * 3600));
            }
        }
        Frame::on_grid(idx, 3600).unwrap()
    }

    #[test]
    fn a_rolling_window_never_reaches_across_an_asset_boundary() {
        // ETH's first two rows have only 0 and 1 rows of ETH history, so a 3-window is NaN there
        // even though BTC's tail sits immediately above them in the same Vec.
        let f = frame_of(&[("BTC", 4), ("ETH", 4)]);
        let mut cols = Columns::new();
        cols.insert("bias_Whale".to_string(), vec![1.0, 2.0, 3.0, 4.0, 10.0, 20.0, 30.0, 40.0]);
        let z = zscore_block(&cols, &f.span_ranges(), 3);
        let c = &z["z3_bias_Whale"];
        assert!(c[4].is_nan() && c[5].is_nan() && c[6].is_nan(), "ETH warm-up leaked BTC history");
        assert!(c[3].is_finite() && c[7].is_finite());
    }

    #[test]
    fn a_missing_row_is_nan_and_not_the_clip_floor() {
        // The Task-1 regression, at panel level: one cohort absent at the last hour. Without
        // `zscore`'s NaN-current-value guard the clip INVENTS a value here — `f64::max` returns
        // the non-NaN operand, so a missing observation would reach a model as the most extreme
        // bearish reading the band allows.
        let f = frame_of(&[("BTC", 30)]);
        let mut v: Vec<f64> = (0..30).map(|i| (i as f64 * 0.13).sin()).collect();
        v[29] = f64::NAN;
        let mut cols = Columns::new();
        cols.insert("bias_3xSmart".to_string(), v);
        assert!(zscore_block(&cols, &f.span_ranges(), 24)["z24_bias_3xSmart"][29].is_nan());
    }

    #[test]
    fn every_z_value_is_inside_the_clip_band() {
        // ⚠ The history must be NEAR-flat, not FLAT. A perfectly flat window has `sd == 0.0`, so
        // every z is NaN and `is_nan() || <= Z_CLIP` is satisfied by the first disjunct on every
        // row — the assertion cannot fail and the clip is never exercised. The jitter keeps `sd`
        // tiny but non-zero, which is the collapsing-denominator shape that produces the huge
        // FINITE z this test exists to catch: at sd ~1e-9 the final row's z is ~1e9, and only the
        // clip brings it inside the band.
        let f = frame_of(&[("BTC", 60)]);
        let mut v: Vec<f64> = (0..59).map(|i| 0.42 + (i % 2) as f64 * 1e-9).collect();
        v.push(-0.98); // a jump after a near-flat stretch
        let mut cols = Columns::new();
        cols.insert("bias_Smart".to_string(), v);
        let out = zscore_block(&cols, &f.span_ranges(), 24);
        assert!(
            out["z24_bias_Smart"].iter().any(|x| x.abs() == Z_CLIP),
            "no value was actually clipped — the band is untested"
        );
        for (n, c) in out {
            for x in c {
                assert!(x.is_nan() || x.abs() <= Z_CLIP, "{n} = {x}");
            }
        }
    }

    #[test]
    fn delta_is_nan_for_the_first_lag_rows_of_each_asset() {
        let f = frame_of(&[("A", 2), ("B", 2)]);
        let mut cols = Columns::new();
        cols.insert("x".to_string(), vec![1.0, 5.0, 100.0, 101.0]);
        let d = delta_block(&cols, &f.span_ranges(), 1);
        assert!(
            d["d1_x"][0].is_nan() && d["d1_x"][2].is_nan(),
            "B's first row differenced A's last"
        );
        assert_eq!(d["d1_x"][1], 4.0);
        assert_eq!(d["d1_x"][3], 1.0);
    }

    #[test]
    fn a_block_preserves_its_input_column_order() {
        let f = frame_of(&[("A", 30)]);
        let mut cols = Columns::new();
        for n in ["bias_Whale", "bias_4xWhale", "bias_Shrimp"] {
            cols.insert(n.to_string(), vec![0.5; 30]);
        }
        assert_eq!(
            rank_pct_block(&cols, &f.span_ranges(), 24).keys().cloned().collect::<Vec<_>>(),
            vec!["pr24_bias_Whale", "pr24_bias_4xWhale", "pr24_bias_Shrimp"]
        );
    }

    #[test]
    fn per_group_map_scatters_each_pieces_result_back_into_its_own_rows() {
        let f = frame_of(&[("A", 3), ("B", 2)]);
        let mut cols = Columns::new();
        cols.insert("x".to_string(), vec![1.0, 2.0, 3.0, 10.0, 20.0]);
        let out = per_group_map(
            &cols,
            &f.span_ranges(),
            |n| format!("k_{n}"),
            |s| s.iter().map(|v| v * 100.0).collect(),
        );
        assert_eq!(out["k_x"], vec![100.0, 200.0, 300.0, 1000.0, 2000.0]);
    }

    #[test]
    #[should_panic(expected = "wrong length")]
    fn per_group_map_panics_rather_than_truncating_on_a_length_mismatch() {
        // The Task-14 correction's whole reason for existing: a kernel that drops a row must fail
        // loudly (shifting every later row of that asset by one hour is the alternative), not pass
        // quietly with a shorter slice silently copied back. This path is infallible by signature
        // BECAUSE no kernel it ships can reach it — so the refusal is a panic, and it is asserted
        // rather than assumed.
        let f = frame_of(&[("A", 4), ("B", 4)]);
        let mut cols = Columns::new();
        cols.insert("x".to_string(), vec![1.0; 8]);
        per_group_map(&cols, &f.span_ranges(), |n| n.to_string(), |s| s[..s.len() - 1].to_vec());
    }

    #[test]
    fn the_blocks_are_the_column_features_they_name_and_nothing_else() {
        // The blocks must be exactly `WindowFeature`/`LaggedDiff` applied per group, because that
        // is what puts them behind `tests/feature_parity.rs`'s streaming-equals-batch gate. A
        // block that grew arithmetic of its own would still pass every assertion above while
        // silently leaving that gate.
        let f = frame_of(&[("A", 300)]);
        let x: Vec<f64> = (0..300).map(|i| (i as f64 * 0.07).sin()).collect();
        let mut cols = Columns::new();
        cols.insert("x".to_string(), x.clone());
        let groups = f.span_ranges();

        let z = WindowFeature::point_in_time(WindowStat::ZScore { clip: Some(Z_CLIP) }, 24);
        let pr = WindowFeature::point_in_time(WindowStat::RankPct, 24);
        let d = LaggedDiff::new(4);
        for (block, direct) in [
            (zscore_block(&cols, &groups, 24)["z24_x"].clone(), z.vectorize(&x)),
            (rank_pct_block(&cols, &groups, 24)["pr24_x"].clone(), pr.vectorize(&x)),
            (delta_block(&cols, &groups, 4)["d4_x"].clone(), d.vectorize(&x)),
        ] {
            assert_eq!(block.len(), direct.len());
            for (i, (a, b)) in block.iter().zip(&direct).enumerate() {
                assert!(
                    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits(),
                    "row {i}: the block computed {a:?} where its own ColumnFeature answers {b:?}"
                );
            }
        }
    }

    #[test]
    fn a_lagged_difference_answers_across_a_gap_where_a_window_preset_would_blank() {
        // The reason `LaggedDiff` is its own impl rather than a `WindowFeature`. Row 4 reads rows
        // 4 and 0 only; rows 1..=3 are missing. A window preset with `min_periods = 5` blanks the
        // whole window and answers NaN.
        let x = [10.0, f64::NAN, f64::NAN, f64::NAN, 14.0];
        assert_eq!(LaggedDiff::new(4).vectorize(&x)[4], 4.0);
        assert!(
            WindowFeature::indicator(WindowStat::Mean, 5).vectorize(&x)[4].is_nan(),
            "the window preset must blank here — otherwise the two are interchangeable and this \
             impl has no reason to exist"
        );
        assert_eq!(LaggedDiff::new(4).reach(), 5);
    }
}
