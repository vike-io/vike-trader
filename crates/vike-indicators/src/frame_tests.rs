use super::*;
use crate::window::{WindowSpec, rolling_mean};
use std::assert_matches;

/// One hour, as unix seconds — the cadence this type's first caller runs on, and
/// a perfectly ordinary caller-side constant rather than anything this crate knows about.
const HOUR: i64 = 3_600;

/// Hour `n`, as unix seconds.
fn h(n: i64) -> i64 {
    n * HOUR
}

#[test]
fn an_off_grid_instant_is_not_a_valid_index_row_on_a_grid() {
    // 13:02:17 — the shape the research API's `start` bug produced. It is a perfectly legal
    // index row for a frame that declared NO grid, and refused by one that declared hourly.
    assert!(Frame::new(vec![("BTC".into(), 1_754_485_337)]).is_ok());
    assert!(Frame::on_grid(vec![("BTC".into(), 1_754_485_337)], HOUR).is_err());
    assert!(Frame::on_grid(vec![("BTC".into(), 1_754_485_200)], HOUR).is_ok());
}

#[test]
fn the_grid_is_the_callers_unit_not_seconds() {
    // The same instant in epoch MILLISECONDS — `vike_marketdata::Bar`'s own unit — on a
    // five-MINUTE grid. Nothing in this module knows what a second is.
    let ms = 1_754_485_200_000i64;
    assert!(Frame::on_grid(vec![("BTC".into(), ms)], 300_000).is_ok());
    assert!(Frame::on_grid(vec![("BTC".into(), ms + 1)], 300_000).is_err());
}

#[test]
fn a_non_positive_cadence_is_refused_rather_than_dividing_by_zero() {
    assert_eq!(Frame::on_grid(vec![], 0).unwrap_err(), FrameError::BadCadence(0));
    assert_eq!(Frame::on_grid(vec![], -3600).unwrap_err(), FrameError::BadCadence(-3600));
}

#[test]
fn a_frame_remembers_its_grid_so_a_derived_frame_is_held_to_it() {
    let f = Frame::on_grid(vec![("BTC".into(), h(0))], HOUR).unwrap();
    assert_eq!(f.cadence(), Some(HOUR));
    // A reindex onto an OFF-GRID target is refused by the cadence the frame carries — the
    // property that makes the grid structural rather than a convention the loader observed
    // once.
    assert!(f.reindex_onto(&[("BTC".to_string(), h(0) + 17)]).is_err());
    assert!(f.reindex_onto(&[("BTC".to_string(), h(1))]).is_ok());
    assert_eq!(f.reindex_onto(&[("BTC".to_string(), h(1))]).unwrap().cadence(), Some(HOUR));
    assert_eq!(Frame::new(vec![]).unwrap().cadence(), None);
}

#[test]
fn spans_are_derived_contiguous_and_half_open() {
    let f = Frame::new(vec![
        ("BTC".into(), h(0)),
        ("BTC".into(), h(1)),
        ("BTC".into(), h(2)),
        ("ETH".into(), h(0)),
        ("ETH".into(), h(1)),
    ])
    .unwrap();
    assert_eq!(f.spans().len(), 2);
    assert_eq!((f.spans()[0].start, f.spans()[0].end), (0, 3));
    assert_eq!((f.spans()[1].start, f.spans()[1].end), (3, 5));
    assert_eq!(f.spans()[0].asset, "BTC");
    assert_eq!(f.nrows(), 5);
    // ...and the same fact in the shape `per_group` takes, which is what `derive` hands it.
    assert_eq!(f.span_ranges(), vec![0..3, 3..5]);
}

#[test]
fn an_asset_split_into_two_runs_is_refused() {
    // The shape a rolling window must never see: ETH's rows either side of BTC's. This trips
    // the sort arm at row 1 (ETH sorts after BTC) — before the repeat is ever seen.
    assert!(
        Frame::new(vec![("ETH".into(), h(0)), ("BTC".into(), h(0)), ("ETH".into(), h(1)),])
            .is_err()
    );

    // The shape the test's NAME actually promises: BTC's two runs, with ETH's single run
    // sorted correctly between them. Sort order holds all the way to row 2, where BTC
    // reappears and sorts before ETH (the current last span) — the same sort arm, now
    // catching the split it exists to forbid rather than a merely-unsorted prefix.
    assert!(
        Frame::new(vec![("BTC".into(), h(0)), ("ETH".into(), h(0)), ("BTC".into(), h(1)),])
            .is_err()
    );
}

#[test]
fn a_duplicate_timestamp_is_refused_rather_than_deduped() {
    // Dedupe is a decision with a policy (first-wins, on a page overlap) and belongs to the
    // loader that knows why the duplicate exists.
    assert!(Frame::new(vec![("BTC".into(), h(3)), ("BTC".into(), h(3))]).is_err());
}

#[test]
fn timestamps_must_increase_inside_an_asset() {
    assert!(Frame::new(vec![("BTC".into(), h(4)), ("BTC".into(), h(3))]).is_err());
}

#[test]
fn assets_must_be_sorted() {
    assert!(Frame::new(vec![("ETH".into(), h(0)), ("BTC".into(), h(0))]).is_err());
}

#[test]
fn a_column_of_the_wrong_length_is_refused_and_so_is_a_silent_overwrite() {
    let mut f = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1))]).unwrap();
    assert!(f.push_col("x", vec![1.0]).is_err());
    assert!(f.push_col("x", vec![1.0, 2.0]).is_ok());
    assert!(f.push_col("x", vec![9.0, 9.0]).is_err(), "a silent overwrite is not an add");
}

#[test]
fn column_insertion_order_is_preserved() {
    // ⚠ Insertion order is deliberately NOT alphabetical — these were once real column names
    // whose ordering differed from their sort order, and neutral names that sorted as they
    // were inserted would let a hash- or BTree-backed container pass this by accident.
    let mut f = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
    for n in ["sig_c", "sig_a", "sig_b"] {
        f.push_col(n, vec![0.0]).unwrap();
    }
    assert_eq!(
        f.column_names().collect::<Vec<_>>(),
        vec!["sig_c", "sig_a", "sig_b"],
        "a hash map here would reorder a cross-column mean's f64 summation"
    );
}

#[test]
fn from_parts_validates_the_index_and_every_column_length() {
    let idx = vec![("BTC".to_string(), h(0)), ("BTC".to_string(), h(1))];
    let mut cols = Columns::new();
    cols.insert("a".to_string(), vec![1.0, 2.0]);
    assert!(Frame::from_parts(idx.clone(), cols).is_ok());

    let mut short = Columns::new();
    short.insert("a".to_string(), vec![1.0]);
    assert!(Frame::from_parts(idx, short).is_err());

    // The other half of "validates BOTH": an unsorted index is refused even when every
    // column length matches it.
    let bad_idx = vec![("ETH".to_string(), h(0)), ("BTC".to_string(), h(0))];
    let mut ok_lengths = Columns::new();
    ok_lengths.insert("a".to_string(), vec![1.0, 2.0]);
    assert!(Frame::from_parts(bad_idx, ok_lengths).is_err());
}

#[test]
fn from_parts_on_grid_carries_the_grid_onto_the_assembled_frame() {
    let mut cols = Columns::new();
    cols.insert("a".to_string(), vec![1.0]);
    let f = Frame::from_parts_on_grid(vec![("BTC".to_string(), h(2))], cols.clone(), HOUR).unwrap();
    assert_eq!(f.cadence(), Some(HOUR));
    assert!(Frame::from_parts_on_grid(vec![("BTC".to_string(), 61)], cols, HOUR).is_err());
}

#[test]
fn an_empty_frame_is_legal_and_has_no_spans() {
    let f = Frame::new(vec![]).unwrap();
    assert_eq!(f.nrows(), 0);
    assert!(f.spans().is_empty());
}

#[test]
fn col_for_returns_none_rather_than_panicking_on_a_span_that_does_not_fit_this_frame() {
    let mut f = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1))]).unwrap();
    f.push_col("x", vec![1.0, 2.0]).unwrap();
    // This frame has only 2 rows; a span read off a longer frame (or hand-built) must not
    // panic when handed to a shorter one.
    let oversized = AssetSpan { asset: "BTC".to_string(), start: 0, end: 999 };
    assert!(f.col_for("x", &oversized).is_none());
    // Inverted (start > end) — also representable since the fields are `pub`.
    let inverted = AssetSpan { asset: "BTC".to_string(), start: 5, end: 1 };
    assert!(f.col_for("x", &inverted).is_none());
}

/// `a.len()` rows of asset AAA followed by `b.len()` rows of BBB, one column named `x`.
fn two_asset_frame(a: &[f64], b: &[f64]) -> Frame {
    let mut idx = Vec::new();
    for i in 0..a.len() {
        idx.push(("AAA".to_string(), h(i as i64)));
    }
    for i in 0..b.len() {
        idx.push(("BBB".to_string(), h(i as i64)));
    }
    let mut f = Frame::new(idx).unwrap();
    let mut col = a.to_vec();
    col.extend_from_slice(b);
    f.push_col("x", col).unwrap();
    f
}

#[test]
fn a_rolling_window_cannot_reach_across_an_asset_boundary() {
    // AAA flat at 1.0 for 8 hours; BBB flat at 1000.0 for 8 hours, starting at row 8.
    let f = two_asset_frame(&[1.0; 8], &[1000.0; 8]);
    let out = f.derive("x", |s| rolling_mean(s, WindowSpec::indicator(4))).unwrap();

    // The discriminating assertion: BBB's warm-up RESTARTS at its own first row. A kernel run
    // over the whole column has a full 4-wide window at rows 8..10 — made of AAA's 1.0s and
    // BBB's 1000.0s — and writes 250.75 / 500.5 / 750.25 there instead of NaN.
    assert!(out[8].is_nan(), "row 8 must be BBB's warm-up, got {}", out[8]);
    assert!(out[9].is_nan(), "row 9 must be BBB's warm-up, got {}", out[9]);
    assert!(out[10].is_nan(), "row 10 must be BBB's warm-up, got {}", out[10]);
    assert_eq!(out[11], 1000.0, "BBB's first complete window is BBB's own values only");
    assert_eq!(out[7], 1.0, "BBB has not leaked backwards into AAA's last row");
    assert!(out[0].is_nan() && out[1].is_nan() && out[2].is_nan());
}

#[test]
fn one_assets_derived_column_does_not_depend_on_another_assets_values() {
    // The general property, which catches a spanning kernel even where warm-up alone would
    // not (lagged windows, rank, median). Only AAA changes between the two frames.
    let b = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
    let f1 = two_asset_frame(&[1.0; 8], &b);
    let f2 = two_asset_frame(&[-7.5; 8], &b);
    let k = |s: &[f64]| rolling_mean(s, WindowSpec::point_in_time(3));
    let o1 = f1.derive("x", k).unwrap();
    let o2 = f2.derive("x", k).unwrap();
    for i in 8..16 {
        assert_eq!(
            o1[i].to_bits(),
            o2[i].to_bits(),
            "row {i} of BBB moved when only AAA's values changed — the window spans the boundary"
        );
    }
}

#[test]
fn reordering_the_assets_does_not_change_either_ones_derived_values() {
    let a = [2.0, 4.0, 8.0, 16.0, 32.0, 64.0];
    let b = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0];
    let k = |s: &[f64]| rolling_mean(s, WindowSpec::indicator(3));
    let ab = two_asset_frame(&a, &b).derive("x", k).unwrap();
    let ba = two_asset_frame(&b, &a).derive("x", k).unwrap();
    for i in 0..6 {
        assert_eq!(ab[i].to_bits(), ba[6 + i].to_bits(), "a's row {i} changed with position");
        assert_eq!(ab[6 + i].to_bits(), ba[i].to_bits(), "b's row {i} changed with position");
    }
}

#[test]
fn a_kernel_that_returns_the_wrong_length_is_an_error_not_a_truncation() {
    // The Task-14 correction's whole reason for existing, now inherited from `per_group`: a
    // kernel that drops a row must fail loudly (shifting every later row of that asset by one
    // hour is the alternative), not pass quietly with a shorter slice silently copied back.
    let f = two_asset_frame(&[1.0; 4], &[2.0; 4]);
    let e = f.derive("x", |s| s[..s.len() - 1].to_vec()).unwrap_err();
    assert_matches!(
        e,
        FrameError::Kernel(WindowError::KernelLength { .. }),
        "expected the kernel-length refusal, got {e:?}"
    );
    assert!(e.to_string().contains('x'), "the message must name the source column: {e}");
}

#[test]
fn derive_scatters_each_pieces_result_back_into_its_own_rows() {
    let f = two_asset_frame(&[1.0, 2.0, 3.0], &[10.0, 20.0]);
    let out = f.derive("x", |s| s.iter().map(|v| v * 100.0).collect()).unwrap();
    assert_eq!(out, vec![100.0, 200.0, 300.0, 1000.0, 2000.0]);
}

#[test]
fn deriving_from_a_missing_column_is_an_error() {
    let f = two_asset_frame(&[1.0; 2], &[2.0; 2]);
    assert!(f.derive("nope", |s| s.to_vec()).is_err());
}

#[test]
fn derive_on_an_empty_frame_returns_an_empty_result() {
    let mut f = Frame::new(vec![]).unwrap();
    f.push_col("x", vec![]).unwrap();
    assert!(f.derive("x", |s| s.to_vec()).unwrap().is_empty());
}

#[test]
fn derive_on_a_single_asset_frame_applies_the_kernel_to_its_one_span() {
    // Pinning only — a single asset has ZERO discriminating power for the boundary property
    // (there is no second asset to leak into or from). The two-asset tests above are what
    // actually prove the boundary; this only proves `derive` still works with one span.
    let mut f =
        Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1)), ("BTC".into(), h(2))]).unwrap();
    f.push_col("x", vec![1.0, 2.0, 3.0]).unwrap();
    let out = f.derive("x", |s| rolling_mean(s, WindowSpec::indicator(2))).unwrap();
    assert!(out[0].is_nan());
    assert_eq!(out[1], 1.5);
    assert_eq!(out[2], 2.5);
}

#[test]
fn derive_handles_an_asset_with_exactly_one_row() {
    let f = two_asset_frame(&[5.0], &[7.0]);
    let out = f.derive("x", |s| s.iter().map(|v| v * 2.0).collect()).unwrap();
    assert_eq!(out, vec![10.0, 14.0]);
}

#[test]
fn push_derived_derives_then_appends_under_the_new_name() {
    let mut f = two_asset_frame(&[1.0, 2.0], &[10.0, 20.0]);
    f.push_derived("x2", "x", |s| s.iter().map(|v| v * 2.0).collect()).unwrap();
    assert_eq!(f.col("x2").unwrap(), &[2.0, 4.0, 20.0, 40.0]);
    // Same silent-overwrite guard as push_col, reached through the derived path.
    assert!(f.push_derived("x2", "x", |s| s.to_vec()).is_err());
}

#[test]
fn the_join_is_an_outer_union_with_nan_where_a_side_is_missing() {
    let mut l = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(2))]).unwrap();
    l.push_col("a", vec![1.0, 3.0]).unwrap();
    let mut r = Frame::new(vec![("BTC".into(), h(1)), ("BTC".into(), h(2))]).unwrap();
    r.push_col("b", vec![10.0, 20.0]).unwrap();

    let j = outer_join(&l, &r).unwrap();
    assert_eq!(j.nrows(), 3);
    assert_eq!(j.index().iter().map(|(_, t)| t / HOUR).collect::<Vec<_>>(), vec![0, 1, 2]);
    let a = j.col("a").unwrap();
    let b = j.col("b").unwrap();
    assert_eq!(a[0], 1.0);
    assert!(a[1].is_nan(), "hour 1 exists only on the right; a must be NaN, never 0.0");
    assert_eq!(a[2], 3.0);
    assert!(b[0].is_nan());
    assert_eq!((b[1], b[2]), (10.0, 20.0));
}

#[test]
fn the_join_interleaves_assets_correctly_and_keeps_spans_contiguous() {
    let mut l = Frame::new(vec![("BTC".into(), h(0)), ("ETH".into(), h(5))]).unwrap();
    l.push_col("a", vec![1.0, 2.0]).unwrap();
    let mut r = Frame::new(vec![("BTC".into(), h(1)), ("ETH".into(), h(4))]).unwrap();
    r.push_col("b", vec![9.0, 8.0]).unwrap();

    let j = outer_join(&l, &r).unwrap();
    assert_eq!(j.spans().len(), 2);
    assert_eq!((j.spans()[0].asset.as_str(), j.spans()[0].start, j.spans()[0].end), ("BTC", 0, 2));
    assert_eq!((j.spans()[1].asset.as_str(), j.spans()[1].start, j.spans()[1].end), ("ETH", 2, 4));
    assert!(j.index()[2].1 < j.index()[3].1, "ETH's hours came from opposite sides");
}

#[test]
fn left_columns_keep_their_order_and_precede_the_right_ones() {
    let mut l = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
    l.push_col("sig_c", vec![0.0]).unwrap();
    l.push_col("sig_a", vec![0.0]).unwrap();
    let mut r = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
    r.push_col("mid_price", vec![0.0]).unwrap();
    let j = outer_join(&l, &r).unwrap();
    assert_eq!(j.column_names().collect::<Vec<_>>(), vec!["sig_c", "sig_a", "mid_price"]);
}

#[test]
fn a_column_name_present_on_both_sides_is_an_error() {
    let mut l = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
    l.push_col("x", vec![1.0]).unwrap();
    let mut r = Frame::new(vec![("BTC".into(), h(0))]).unwrap();
    r.push_col("x", vec![2.0]).unwrap();
    assert!(outer_join(&l, &r).is_err(), "pandas would keep both and hand out whichever it found");
}

#[test]
fn joining_with_an_empty_frame_returns_the_other_side_intact() {
    let mut l = Frame::new(vec![("BTC".into(), h(0)), ("BTC".into(), h(1))]).unwrap();
    l.push_col("a", vec![1.0, 2.0]).unwrap();
    let j = outer_join(&l, &Frame::new(vec![]).unwrap()).unwrap();
    assert_eq!(j.col("a").unwrap(), &[1.0, 2.0]);
}

#[test]
fn two_different_grids_do_not_join_and_an_ungridded_side_adopts_the_other() {
    let hourly = Frame::on_grid(vec![("BTC".into(), h(0))], HOUR).unwrap();
    let five_min = Frame::on_grid(vec![("BTC".into(), h(0) + 300)], 300).unwrap();
    assert_eq!(
        outer_join(&hourly, &five_min).unwrap_err(),
        FrameError::CadenceMismatch { left: HOUR, right: 300 }
    );

    // An UNCONSTRAINED side takes the other's grid — and the union is then validated against
    // it, which is strictly more checking than the unconstrained side ever had. Its off-grid
    // row is what makes this refusal observable rather than assumed.
    let loose = Frame::new(vec![("BTC".into(), h(0) + 17)]).unwrap();
    assert!(outer_join(&hourly, &loose).is_err(), "the loose side's off-grid row must surface");
    let aligned = Frame::new(vec![("BTC".into(), h(1))]).unwrap();
    assert_eq!(outer_join(&hourly, &aligned).unwrap().cadence(), Some(HOUR));
}

#[test]
fn reindex_fills_absent_keys_with_nan_and_keeps_column_order() {
    let mut src = Frame::new(vec![("A".into(), h(0))]).unwrap();
    src.push_col("b", vec![1.0]).unwrap();
    src.push_col("a", vec![2.0]).unwrap();
    let grid = vec![("A".to_string(), h(0)), ("A".to_string(), h(1))];
    let out = src.reindex_onto(&grid).unwrap();
    assert_eq!(out.column_names().collect::<Vec<_>>(), vec!["b", "a"]);
    assert_eq!(out.col("b").unwrap()[0], 1.0);
    assert!(out.col("b").unwrap()[1].is_nan());
}

#[test]
fn the_union_grid_is_sorted_deduped_and_valid_as_a_frame_index() {
    let a = vec![("ETH".to_string(), h(1)), ("BTC".to_string(), h(0))];
    let b = vec![("BTC".to_string(), h(0)), ("BTC".to_string(), h(1))];
    let g = union_grid(&[&a, &b]);
    assert_eq!(
        g,
        vec![("BTC".to_string(), h(0)), ("BTC".to_string(), h(1)), ("ETH".to_string(), h(1))]
    );
    assert!(Frame::new(g).is_ok());
}

// ---- Columns ----------------------------------------------------------------------------

#[test]
fn columns_iterate_in_insertion_order_and_an_insert_replaces_in_place() {
    let mut c = Columns::with_capacity(3);
    c.insert("b", vec![1.0]);
    c.insert("a", vec![2.0]);
    c.insert("c", vec![3.0]);
    assert_eq!(c.keys().cloned().collect::<Vec<_>>(), vec!["b", "a", "c"]);
    // Replacing must NOT move the column: a cross-column mean's summation order is decided by
    // this order, so an insert that appended would silently change a downstream f64 result.
    assert_eq!(c.insert("a", vec![9.0]), Some(vec![2.0]));
    assert_eq!(c.keys().cloned().collect::<Vec<_>>(), vec!["b", "a", "c"]);
    assert_eq!(c["a"], vec![9.0]);
    assert_eq!(c.len(), 3);
}

#[test]
fn columns_lookup_answers_absence_rather_than_guessing() {
    let mut c = Columns::new();
    c.insert("x".to_string(), vec![1.0, 2.0]);
    assert!(c.contains_key("x") && !c.contains_key("y"));
    assert_eq!(c.get("x"), Some(&vec![1.0, 2.0]));
    assert_eq!(c.get("y"), None);
    c.get_mut("x").unwrap()[0] = 7.0;
    assert_eq!(c["x"][0], 7.0);
    assert!(Columns::new().is_empty());
}

#[test]
fn columns_iterate_by_reference_and_by_value_in_the_same_order() {
    let mut c = Columns::new();
    c.insert("p".to_string(), vec![1.0]);
    c.insert("q".to_string(), vec![2.0]);
    let by_ref: Vec<&String> = (&c).into_iter().map(|(k, _)| k).collect();
    assert_eq!(by_ref, vec!["p", "q"]);
    assert_eq!(c.values().cloned().collect::<Vec<_>>(), vec![vec![1.0], vec![2.0]]);
    let by_val: Vec<String> = c.into_iter().map(|(k, _)| k).collect();
    assert_eq!(by_val, vec!["p", "q"]);
}
