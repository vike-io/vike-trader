//! Time-series cross-validation splitters (anti-overfitting).
//!
//! Port of vike-trader-app `analysis/validation.py` (López de Prado, *Advances in
//! Financial ML*, ch. 7). Embargo removes a window *after* each test block from
//! training (point-in-time observations, so no pre-block label-overlap purge).

use std::collections::HashSet;

/// Walk-forward train-window mode. `Anchored` = expanding window (`train_start` always 0);
/// `Rolling` = fixed-width train of one chunk immediately preceding the test window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkMode {
    Anchored,
    Rolling,
}

/// One walk-forward split. `train_end == test_start`; train is `[train_start, train_end)`,
/// test is `[test_start, test_end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    pub train_start: usize,
    pub train_end: usize,
    pub test_start: usize,
    pub test_end: usize,
}

/// Walk-forward splits. Python `walk_forward_splits` (the enum removes its `ValueError` path).
pub fn walk_forward_splits(n: usize, n_splits: usize, mode: WalkMode) -> Vec<Split> {
    let chunk = n / (n_splits + 1);
    let mut splits = Vec::with_capacity(n_splits);
    for s in 1..=n_splits {
        let test_start = s * chunk;
        let test_end = if s == n_splits { n } else { (s + 1) * chunk };
        let train_start = match mode {
            WalkMode::Anchored => 0,
            WalkMode::Rolling => test_start.saturating_sub(chunk),
        };
        splits.push(Split { train_start, train_end: test_start, test_start, test_end });
    }
    splits
}

fn group_bounds(n: usize, n_groups: usize) -> Vec<(usize, usize)> {
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

/// All `C(n_groups, n_test_groups)` train/test paths with per-test-group embargo.
/// Python `combinatorial_purged_splits`.
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
    let mut idx: Vec<usize> = (0..k).collect();
    loop {
        out.push(idx.clone());
        let mut i = k;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if idx[i] != i + n - k {
                break;
            }
        }
        idx[i] += 1;
        for j in (i + 1)..k {
            idx[j] = idx[j - 1] + 1;
        }
    }
}

/// Time-based purged walk-forward over an hourly index, using this file's half-open `Split`
/// convention: train is `[train_start, train_end)`, test is `[test_start, test_end)`.
///
/// The sibling of [`walk_forward_splits`], which divides a series into a FIXED NUMBER of splits.
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
/// leaves train and test contiguous with no skipped row, matching `walk_forward_splits` above.
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
) -> Vec<Split> {
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
        out.push(Split { train_start, train_end, test_start, test_end });
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
        let a = walk_forward_splits(100, 4, WalkMode::Anchored);
        assert_eq!(a.len(), 4);
        assert_eq!(a[0], Split { train_start: 0, train_end: 20, test_start: 20, test_end: 40 });
        // last split's test_end == n
        assert_eq!(a[3], Split { train_start: 0, train_end: 80, test_start: 80, test_end: 100 });
        let r = walk_forward_splits(100, 4, WalkMode::Rolling);
        // rolling: train_start = test_start - chunk
        assert_eq!(r[3], Split { train_start: 60, train_end: 80, test_start: 80, test_end: 100 });
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
        assert_eq!(s[0], Split { train_start: 0, train_end: 24, test_start: 29, test_end: 39 });
        assert_eq!(
            *s.last().unwrap(),
            Split { train_start: 60, train_end: 84, test_start: 89, test_end: 99 }
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
