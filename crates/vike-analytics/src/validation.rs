//! Time-series cross-validation splitters (anti-overfitting).
//!
//! Port of vike-trader-app `analysis/validation.py` (López de Prado, *Advances in
//! Financial ML*, ch. 7). Embargo removes a window *after* each test block from
//! training (point-in-time observations, so no pre-block label-overlap purge).
//! [`purged_kfold_label_spans`] and [`combinatorial_purged_label_spans`] take label END bars and purge
//! every training label overlapping a test window; the others are unchanged by them.

use std::collections::HashSet;

/// Walk-forward train-window mode. `Anchored` = expanding window (`train_start` always 0);
/// `Rolling` = fixed-width train of one chunk immediately preceding the test window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkforwardMode {
    Anchored,
    Rolling,
}

/// One walk-forward split. `train_end == test_start`; train is `[train_start, train_end)`,
/// test is `[test_start, test_end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkforwardSplit {
    pub train_start: usize,
    pub train_end: usize,
    pub test_start: usize,
    pub test_end: usize,
}

/// Walk-forward splits. Python `walk_forward_splits` (the enum removes its `ValueError` path).
pub fn walkforward_splits(
    n: usize,
    n_splits: usize,
    mode: WalkforwardMode,
) -> Vec<WalkforwardSplit> {
    let chunk = n / (n_splits + 1);
    let mut splits = Vec::with_capacity(n_splits);
    for s in 1..=n_splits {
        let test_start = s * chunk;
        let test_end = if s == n_splits { n } else { (s + 1) * chunk };
        let train_start = match mode {
            WalkforwardMode::Anchored => 0,
            WalkforwardMode::Rolling => test_start.saturating_sub(chunk),
        };
        splits.push(WalkforwardSplit { train_start, train_end: test_start, test_start, test_end });
    }
    splits
}

/// The `n_groups` contiguous half-open `(start, end)` index groups [`purged_kfold_indices`] and
/// [`combinatorial_purged_splits`] cut `0..n` into, in order. Public so a caller that trades each
/// test GROUP of a combinatorial split on its own (`vike_backtest::walkforward::combinatorial`) reads
/// the groups these splits were cut from, not a second spelling of the cut.
pub fn group_bounds(n: usize, n_groups: usize) -> Vec<(usize, usize)> {
    (0..n_groups).map(|g| (g * n / n_groups, (g + 1) * n / n_groups)).collect()
}

/// k contiguous test folds; train excludes the test fold + an embargo window after it.
/// Python `purged_kfold_indices`.
pub fn purged_kfold_indices(n: usize, k: usize, embargo: usize) -> Vec<(Vec<usize>, Vec<usize>)> {
    let mut splits = Vec::with_capacity(k);
    for (t0, t1) in group_bounds(n, k) {
        let test: Vec<usize> = (t0..t1).collect();
        let embargo_end = (t1 + embargo).min(n);
        let train: Vec<usize> = (0..n).filter(|&i| i < t0 || i >= embargo_end).collect();
        splits.push((train, test));
    }
    splits
}

/// k contiguous test folds over LABELLED observations, PURGED by label span, then embargoed
/// (López de Prado, *Advances in Financial ML* ch. 7, snippet 7.3 `PurgedKFold`).
///
/// [`purged_kfold_indices`] treats each observation as a point; a label built over a window (a
/// triple-barrier or trend-scanning label, `crate::labels`) is not one, and a training label whose
/// window reaches into the test fold leaks the test outcome. Observation `i` is the label span
/// `label_spans[i] = (start, end)` (bar indices, `start <= end`), sorted by `start` as AFML requires.
///
/// Per fold, with the test window running from the test labels' smallest `start` to their largest
/// `end`, a training label is PURGED when it overlaps that window: `start < window_end` and
/// `end > window_start`. A label that ends exactly where the window starts, or starts exactly where
/// it ends, shares a price but no return and is kept, as snippet 7.3's `t1 <= t0` and
/// `searchsorted(maxT1)` keep it. Then the first `embargo` kept labels that start at or after the
/// window's end are dropped too. With zero-length spans `(i, i)` and `k <= n` this returns exactly
/// what [`purged_kfold_indices`] returns; with `k > n` a fold is empty, and an empty fold has no
/// window, so nothing is purged or embargoed for it (the point splitter still embargoes after it).
pub fn purged_kfold_label_spans(
    label_spans: &[(usize, usize)],
    k: usize,
    embargo: usize,
) -> Vec<(Vec<usize>, Vec<usize>)> {
    group_bounds(label_spans.len(), k)
        .into_iter()
        .map(|(t0, t1)| (purged_training(label_spans, &[(t0, t1)], embargo), (t0..t1).collect()))
        .collect()
}

/// The training labels of one split whose test groups are the contiguous index ranges `tested`: every
/// label outside them, less each one a test group's window PURGES (`start < window_end && end >
/// window_start`) and, per group, the first `embargo` kept labels starting at or after its window's
/// end ([`purged_kfold_label_spans`]' rule, applied to each group on its own). An empty group has no
/// window and drops nothing.
fn purged_training(
    label_spans: &[(usize, usize)],
    tested: &[(usize, usize)],
    embargo: usize,
) -> Vec<usize> {
    let windows: Vec<(usize, usize)> = tested
        .iter()
        .filter_map(|&(t0, t1)| {
            let spans = &label_spans[t0..t1];
            Some((spans.iter().map(|s| s.0).min()?, spans.iter().map(|s| s.1).max()?))
        })
        .collect();
    let mut embargoed = vec![0; windows.len()];
    let mut train = Vec::with_capacity(label_spans.len());
    for (i, &(start, end)) in label_spans.iter().enumerate() {
        if tested.iter().any(|&(t0, t1)| (t0..t1).contains(&i))
            || windows
                .iter()
                .any(|&(window_start, window_end)| start < window_end && end > window_start)
        {
            continue;
        }
        let mut dropped = false;
        for (count, &(_, window_end)) in embargoed.iter_mut().zip(&windows) {
            if start >= window_end && *count < embargo {
                *count += 1;
                dropped = true;
            }
        }
        if !dropped {
            train.push(i);
        }
    }
    train
}

/// All `C(n_groups, n_test_groups)` train/test paths with per-test-group embargo.
/// Python `combinatorial_purged_splits`.
///
/// ⚠ **Points only: NO label purge.** Each observation is a point, so nothing reaches into a test
/// group from before it and only an embargo follows each group. A label built over a window (the
/// combinatorial purged cross-validation of AFML ch. 12 over triple-barrier or trend-scanning labels)
/// leaks at every test group's left edge here: give its spans to [`combinatorial_purged_label_spans`].
pub fn combinatorial_purged_splits(
    n: usize,
    n_groups: usize,
    n_test_groups: usize,
    embargo: usize,
) -> Vec<(Vec<usize>, Vec<usize>)> {
    let bounds = group_bounds(n, n_groups);
    let mut splits = Vec::new();
    for combo in combinations(n_groups, n_test_groups) {
        let mut test: Vec<usize> = combo.iter().flat_map(|&g| bounds[g].0..bounds[g].1).collect();
        test.sort_unstable();
        let test_set: HashSet<usize> = test.iter().copied().collect();
        let mut purged: HashSet<usize> = HashSet::new();
        for &g in &combo {
            let end = bounds[g].1;
            purged.extend(end..(end + embargo).min(n));
        }
        let train: Vec<usize> =
            (0..n).filter(|i| !test_set.contains(i) && !purged.contains(i)).collect();
        splits.push((train, test));
    }
    splits
}

/// All `C(n_groups, n_test_groups)` train/test paths over LABELLED observations (AFML ch. 12's
/// combinatorial purged cross-validation): [`combinatorial_purged_splits`]' groups and paths, each
/// training set purged by label span and embargoed after each test group by
/// [`purged_kfold_label_spans`]' rule. `label_spans[i] = (start, end)` as there, sorted by `start`. With
/// zero-length spans `(i, i)` and `n_groups <= n` it returns exactly what
/// [`combinatorial_purged_splits`] returns.
pub fn combinatorial_purged_label_spans(
    label_spans: &[(usize, usize)],
    n_groups: usize,
    n_test_groups: usize,
    embargo: usize,
) -> Vec<(Vec<usize>, Vec<usize>)> {
    let bounds = group_bounds(label_spans.len(), n_groups);
    combinations(n_groups, n_test_groups)
        .into_iter()
        .map(|combo| {
            let tested: Vec<(usize, usize)> = combo.iter().map(|&g| bounds[g]).collect();
            let test: Vec<usize> = tested.iter().flat_map(|&(t0, t1)| t0..t1).collect();
            (purged_training(label_spans, &tested, embargo), test)
        })
        .collect()
}

/// All `C(n, k)` index combinations of `0..n` in lexicographic order.
/// `k == 0` -> one empty combo; `k > n` -> none. Reused by `overfit::pbo_cscv`.
pub(crate) fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    if k == 0 {
        return vec![vec![]];
    }
    if k > n {
        return vec![];
    }
    let mut out = Vec::new();
    let mut index: Vec<usize> = (0..k).collect();
    loop {
        out.push(index.clone());
        let mut i = k;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if index[i] != i + n - k {
                break;
            }
        }
        index[i] += 1;
        for j in (i + 1)..k {
            index[j] = index[j - 1] + 1;
        }
    }
}

/// Time-based purged walk-forward over an hourly index, using this file's half-open `WalkforwardSplit`
/// convention: train is `[train_start, train_end)`, test is `[test_start, test_end)`.
///
/// The sibling of [`walkforward_splits`], which divides a series into a FIXED NUMBER of splits.
/// This one is parameterised by DURATION instead, because a research protocol is specified that
/// way — "train on 14 days, validate the next 24 hours, step a day" — and converting that to a
/// split count depends on how much history happens to exist.
///
/// `purge_hours` is the gap between the end of train and the start of validation, and it exists
/// because the LABELS are forward-looking: a row at time `t` carries the return realised at
/// `t + H`, so the last `H` hours of any training window have labels that reach into the
/// validation window. Without the gap the model is trained on the answer. Set `purge_hours` to at
/// least the forecast horizon. Because `train_end`/`test_start` are half-open (exclusive)
/// boundaries here, `test_start - train_end == purge_hours` exactly — a `purge_hours` of `0`
/// leaves train and test contiguous with no skipped row, matching `walkforward_splits` above.
///
/// `expanding` anchors `train_start` at 0 and grows the window; otherwise the window slides at a
/// fixed length. Sliding is the more honest form for "the model adapts to the recent regime";
/// expanding gives later folds more history.
///
/// A zero `n_hours`, `train_hours`, `val_hours` or `step_hours` yields an empty vec rather than
/// panicking or looping forever; a trailing validation window that would run past `n_hours` is
/// dropped rather than truncated to fit.
pub fn purged_walk_forward_hours(
    n_hours: usize,
    train_hours: usize,
    val_hours: usize,
    step_hours: usize,
    purge_hours: usize,
    expanding: bool,
) -> Vec<WalkforwardSplit> {
    let mut out = Vec::new();
    if n_hours == 0 || train_hours == 0 || val_hours == 0 || step_hours == 0 {
        return out;
    }
    let mut train_start = 0usize;
    let mut train_end = train_hours;
    while let Some(test_start) = train_end.checked_add(purge_hours) {
        let Some(test_end) = test_start.checked_add(val_hours) else { break };
        if test_end > n_hours {
            break;
        }
        out.push(WalkforwardSplit { train_start, train_end, test_start, test_end });
        train_end += step_hours;
        if !expanding {
            train_start += step_hours;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_forward_anchored_vs_rolling() {
        // n=100, n_splits=4 -> chunk = 100/5 = 20. Python walk_forward_splits.
        let a = walkforward_splits(100, 4, WalkforwardMode::Anchored);
        assert_eq!(a.len(), 4);
        assert_eq!(
            a[0],
            WalkforwardSplit { train_start: 0, train_end: 20, test_start: 20, test_end: 40 }
        );
        // last split's test_end == n
        assert_eq!(
            a[3],
            WalkforwardSplit { train_start: 0, train_end: 80, test_start: 80, test_end: 100 }
        );
        let r = walkforward_splits(100, 4, WalkforwardMode::Rolling);
        // rolling: train_start = test_start - chunk
        assert_eq!(
            r[3],
            WalkforwardSplit { train_start: 60, train_end: 80, test_start: 80, test_end: 100 }
        );
    }

    #[test]
    fn purged_kfold_excludes_test_plus_embargo() {
        // n=10, k=2, embargo=1 -> groups (0,5),(5,10)
        let s = purged_kfold_indices(10, 2, 1);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].1, (0..5).collect::<Vec<_>>()); // test fold 0
        // train for fold 0: i<0 (none) or i>=min(5+1,10)=6 -> [6,7,8,9]
        assert_eq!(s[0].0, vec![6, 7, 8, 9]);
    }

    #[test]
    fn purged_kfold_label_spans_purges_overlaps_then_embargoes() {
        // groups (0,4), (4,8).
        let spans = [(0, 2), (1, 3), (2, 5), (4, 6), (5, 7), (6, 8), (8, 9), (9, 10)];
        // fold 0: window [0, 6]. (5,7) overlaps -> purged; (6,8) starts AT 6 -> kept, but it is
        // the first label at or after the window's end, so an embargo of 1 drops it.
        let s = purged_kfold_label_spans(&spans, 2, 1);
        assert_eq!(s[0], (vec![6, 7], vec![0, 1, 2, 3]));
        assert_eq!(purged_kfold_label_spans(&spans, 2, 0)[0].0, vec![5, 6, 7]);
        // fold 1: window [5, 10]. (4,6) overlaps -> purged; (2,5) ends AT 5 -> kept.
        assert_eq!(s[1], (vec![0, 1, 2], vec![4, 5, 6, 7]));
    }

    #[test]
    fn combinatorial_count_is_n_choose_k() {
        // C(4,2) = 6 paths
        let s = combinatorial_purged_splits(40, 4, 2, 0);
        assert_eq!(s.len(), 6);
    }

    #[test]
    fn combinations_basic() {
        assert_eq!(combinations(4, 2).len(), 6);
        assert_eq!(combinations(3, 0), vec![Vec::<usize>::new()]);
        assert!(combinations(2, 3).is_empty());
        assert_eq!(combinations(3, 2), vec![vec![0, 1], vec![0, 2], vec![1, 2]]);
    }

    // `purged_walk_forward_hours` — Split here is half-open (`train_end`/`test_start` are
    // exclusive bounds, per the struct doc and `walk_forward_anchored_vs_rolling` above), so the
    // purge gap is `test_start - train_end == purge_hours` exactly, not `purge_hours + 1`.

    #[test]
    fn purged_walk_forward_leaves_exactly_the_purge_gap_between_train_and_val() {
        let splits = purged_walk_forward_hours(100, 24, 10, 10, 5, false);
        assert!(!splits.is_empty());
        for s in &splits {
            assert_eq!(s.test_start - s.train_end, 5, "gap must equal purge_hours");
        }
    }

    #[test]
    fn the_exact_split_set_is_pinned_including_window_length_and_step() {
        let s = purged_walk_forward_hours(100, 24, 10, 10, 5, false);
        assert_eq!(s.len(), 7);
        assert_eq!(
            s[0],
            WalkforwardSplit { train_start: 0, train_end: 24, test_start: 29, test_end: 39 }
        );
        assert_eq!(
            *s.last().unwrap(),
            WalkforwardSplit { train_start: 60, train_end: 84, test_start: 89, test_end: 99 }
        );
    }

    #[test]
    fn the_step_advances_independently_of_the_validation_window_length() {
        // step 5, val 10: test_start must advance by 5 while each window stays 10 bars.
        let s = purged_walk_forward_hours(100, 24, 10, 5, 0, false);
        assert_eq!(s.len(), 14);
        for w in s.windows(2) {
            assert_eq!(w[1].test_start - w[0].test_start, 5, "step must drive the advance");
        }
        for sp in &s {
            assert_eq!(sp.test_end - sp.test_start, 10, "window length must be val_hours");
        }
    }

    #[test]
    fn a_sliding_window_keeps_a_constant_train_length() {
        let splits = purged_walk_forward_hours(200, 24, 10, 10, 0, false);
        assert!(!splits.is_empty());
        for s in &splits {
            assert_eq!(s.train_end - s.train_start, 24);
            assert_eq!(
                s.test_start, s.train_end,
                "purge_hours=0 must leave train and test contiguous"
            );
        }
    }

    #[test]
    fn an_expanding_window_anchors_the_train_start() {
        let splits = purged_walk_forward_hours(200, 24, 10, 10, 0, true);
        assert!(!splits.is_empty());
        for s in &splits {
            assert_eq!(s.train_start, 0);
        }
        assert!(splits.last().unwrap().train_end > splits[0].train_end);
    }

    #[test]
    fn a_series_too_short_for_one_fold_yields_none() {
        assert!(purged_walk_forward_hours(20, 24, 10, 10, 0, false).is_empty());
    }

    #[test]
    fn zero_valued_parameters_yield_empty_rather_than_looping() {
        assert!(purged_walk_forward_hours(0, 24, 10, 10, 0, false).is_empty(), "zero n_hours");
        assert!(purged_walk_forward_hours(100, 0, 10, 10, 0, false).is_empty(), "zero train_hours");
        assert!(purged_walk_forward_hours(100, 24, 0, 10, 0, false).is_empty(), "zero val_hours");
        assert!(
            purged_walk_forward_hours(100, 24, 10, 0, 0, false).is_empty(),
            "zero step must not loop"
        );
    }

    #[test]
    fn no_validation_window_runs_past_the_end_of_the_series() {
        let splits = purged_walk_forward_hours(100, 24, 10, 10, 5, false);
        assert!(!splits.is_empty());
        for s in &splits {
            assert!(s.test_end <= 100);
        }
    }
}
