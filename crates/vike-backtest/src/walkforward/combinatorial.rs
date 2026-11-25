//! The COMBINATORIAL PURGED walk-forward (López de Prado, *Advances in Financial Machine Learning*,
//! ch. 12): an opt-in third protocol beside [`super::runner::run_walkforward`] and
//! [`super::runner::run_walkforward_optimized`], over the splits
//! [`vike_analytics::validation::combinatorial_purged_splits`] cuts.
//!
//! A walk-forward tests ONE path through history: each window is traded once, by what was chosen
//! on the bars before it. The combinatorial walk cuts the bars into `N` groups, and for every
//! choice of `k` of them as the test set (`C(N, k)` splits) it searches the other groups and trades
//! the winner on the `k` it left out. Each group is then traded `C(N - 1, k - 1)` times, by winners
//! chosen on different training sets, and those results recombine into `C(N - 1, k - 1)` complete
//! out-of-sample PATHS over the whole history. The report is the paths, so a reader sees how far
//! the out-of-sample answer moves with the training set instead of one number.
//!
//! # How each piece reuses the walk
//!
//! - The pre-flight is the walk's (`super::runner::one_series_preflight`): ONE series, every guard a
//!   plain run has. A tick profile cuts its groups on the seed-bar grid of its one prepared tape
//!   (`super::ticks`); both lanes walk the cut through one loop (`combinatorial_paths`).
//! - A split's TRAINING set is the groups it does not test, less the `embargo_bars` after each test
//!   group (the splitter's rule). Those bars are not contiguous, and a strategy must never trade
//!   across a hole, so a candidate runs on each contiguous training SEGMENT on its own, fresh at
//!   `engine.cash`, and scores the bar-weighted mean of the objective over the segments. One
//!   segment that cannot be scored makes the candidate unrankable (`NaN`), the walk's rule for a
//!   failed point, and a split in which every candidate failed fails the run.
//! - Each test GROUP runs on its own with the split's winner, fresh at `engine.cash`.
//! - Path `j` takes, for every group, the `j`-th split (in the splitter's order) that tested it, and
//!   stitches the groups in time order with the walk's own fold ([`super::walk_forward_over_windows`]),
//!   so a path's report means exactly what a walk-forward report means.
//!
//! [`WalkforwardWindowSearch::None`] is the CONTROL here as in the walk: every split trades the
//! profile's own parameters, so a group's result is the same in every split and every path is the
//! one fixed-parameter answer the searched paths are read against.
//!
//! # Each path's curve is THINNED, its numbers are not
//!
//! The answer crosses the wire as one frame (capped at `vike_hub_protocol::frame::MAX_FRAME_LEN`)
//! and is saved whole as the run's `report.json`, and whole curves are paths x bars floats: the ten
//! paths of a 6 / 3 cut over a year of minute bars pass the cap. So both doors thin every path's
//! `equity_curve` with `vike_runs::decimate` (a run record's rule, the last sample always kept),
//! AFTER the fold computed every number from the whole curve: a path's `overall` and `steps`
//! results and its profitable step share are the whole curve's.
//! [`CombinatorialWalkforwardReport::equity_curve_stride`] says whether a curve was thinned.
//!
//! The budget is the ANSWER's, not a path's ([`curve_samples_per_path`]):
//! [`CURVE_SAMPLES_ACROSS_PATHS`] shared by the paths, never more than a run record's
//! `vike_runs::MAX_EQUITY_SAMPLES` a path and never fewer than [`MIN_CURVE_SAMPLES_PER_PATH`]. And
//! a cut makes at most [`MAX_OUT_OF_SAMPLE_RUNS`] group runs (paths x groups, also the steps the
//! paths carry), so the largest answer is bounded too: at most 560 paths (`groups = 17`,
//! `test_groups = 4`), so at most 280,560 samples and 10,000 steps. The tests:
//! `a_year_of_minute_bars_passes_the_frame_cap_whole_and_fits_thinned` (10 paths, MEASURED 96.5 MB
//! whole, 3.6 MB thinned) and `four_hundred_sixty_two_paths_fit_the_frame_cap_thinned` (12 / 6 over
//! 12,000 minute bars, 105.1 MB whole, 7.6 MB thinned, under an eighth of the cap).
//!
//! # A path's steps carry their COMPACT results
//!
//! Each step of a path is a [`vike_analytics::walkforward::WalkforwardResultsStep`], and its two
//! long-form blocks, `extended` and `closed_trades`, are `None` ("not recorded") on this answer,
//! while each path's `overall` keeps its `extended`. Paths x groups of them do not fit one frame:
//! MEASURED, a group's closed-trade block alone was 17.6 KB, which put the 462-path answer at
//! 115 MB thinned, past the cap. The plain walk's few steps keep both.
//!
//! OPT-IN by construction: a profile asks for it with `[walkforward]`'s form (d)
//! (`groups`/`test_groups`, read by [`run_walkforward_combinatorial_profile`]; the backtesthub's
//! `RunWalkforwardProfile` routes such a profile here and answers `CombinatorialWalkforwardReport`),
//! a caller holding a cut with [`run_walkforward_combinatorial`]. A table without those keys walks
//! as it always did: both walk drivers, their window forms and their reports are untouched.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use vike_analytics::validation::{WalkforwardSplit, combinatorial_purged_splits, group_bounds};
use vike_analytics::walkforward::WalkforwardResults;
use vike_data::HistoryStore;
use vike_model::run_request::replay::ReplayKind;

use super::runner::{
    WalkRun, WalkScore, WalkforwardWindowSearch, labelled, one_series_preflight,
    refuse_a_failed_read,
};
use super::windows::{WalkforwardStep, combinatorial_splits_on};
use super::{WalkforwardWindowOutcome, walk_forward_over_splits};
use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::refusals::ProfileRefusal;
use crate::run::report::periods_per_year;
use crate::run::{
    cancelled_run, is_cancelled, refuse_after_failed_properties, run_backtest_over_bars,
};
use crate::search::objective::SearchObjective;
use crate::search::paramscan::grid::evaluation_scored_point_over_bars;
use crate::search::paramscan::{ParamscanPoint, expand_paramscan, map_bounded};

/// The most out-of-sample group runs one cut may make: `C(N, k) * k`, which is also paths x groups,
/// the windows its answer carries. Refused past it, before any split is enumerated: the answer's
/// size and the walk's run time both grow with it, and `groups = 12`, `test_groups = 6` (462 paths)
/// is 5,544.
pub const MAX_OUT_OF_SAMPLE_RUNS: usize = 10_000;

/// The curve samples the paths of one answer share: ten paths keep a run record's whole
/// `vike_runs::MAX_EQUITY_SAMPLES` each (about 3.6 MB of answer).
pub const CURVE_SAMPLES_ACROSS_PATHS: usize = 200_000;

/// The fewest samples a thinned path keeps, enough to draw it: it binds past 400 paths.
pub const MIN_CURVE_SAMPLES_PER_PATH: usize = 500;

/// The cap `vike_runs::decimate` thins each of `paths` curves to (the module doc's budget).
fn curve_samples_per_path(paths: usize) -> usize {
    (CURVE_SAMPLES_ACROSS_PATHS / paths.max(1))
        .clamp(MIN_CURVE_SAMPLES_PER_PATH, vike_runs::MAX_EQUITY_SAMPLES)
}

/// `C(N - 1, k - 1) * N` for `groups = N`, `test_groups = k` (`1 <= k < N`), counted no further
/// than `MAX_OUT_OF_SAMPLE_RUNS + 1`, so a cut of a million groups costs no time and no overflow.
fn out_of_sample_runs(groups: usize, test_groups: usize) -> usize {
    let (n, r) = (groups - 1, test_groups - 1);
    let r = r.min(n - r);
    let mut paths: usize = 1;
    for i in 1..=r {
        // C(n - r + i, i) from C(n - r + i - 1, i - 1): exact in integers, and rising with `i`.
        paths = paths.saturating_mul(n - r + i) / i;
        if paths.saturating_mul(groups) > MAX_OUT_OF_SAMPLE_RUNS {
            break;
        }
    }
    paths.saturating_mul(groups).min(MAX_OUT_OF_SAMPLE_RUNS + 1)
}

/// How the bars are cut: `groups` (`N`) contiguous groups, `test_groups` (`k`, `1 <= k < N`) of
/// them tested by each split, and `embargo_bars` dropped from training after each test group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombinatorialSplits {
    pub groups: usize,
    pub test_groups: usize,
    pub embargo_bars: usize,
}

/// What [`run_walkforward_combinatorial`] answers: the cut, how many splits were searched and
/// traded (`C(N, k)`), and the `C(N - 1, k - 1)` out-of-sample paths, each the whole walk-forward
/// results over every group (its steps are the groups, each with what its split chose), its
/// curve thinned (the module doc's "Each path's curve is THINNED, its numbers are not").
#[derive(Debug, Clone, Serialize)]
pub struct CombinatorialWalkforwardReport {
    pub groups: usize,
    pub test_groups: usize,
    pub embargo_bars: usize,
    pub splits: usize,
    /// The stride `vike_runs::decimate` kept every path's `equity_curve` at: `1` means each curve is
    /// whole, so a statistic recomputed from it is exact.
    pub equity_curve_stride: usize,
    /// How many samples each path's WHOLE curve had: one per bar of the series, the same for every
    /// path, so one stride holds for all of them.
    pub equity_curve_len: usize,
    pub paths: Vec<WalkforwardResults>,
}

/// The profile's own combinatorial walk: the cut from `[walkforward]`'s form (d)
/// (`groups`, `test_groups`, `embargo`, [`super::windows::combinatorial_splits`]), the search from
/// its `search` and the ranking from its `rank_by`, as the stitched walk reads them. What the
/// backtesthub's `RunWalkforwardProfile` runs for a profile that declares form (d).
///
/// Refused as [`run_walkforward_combinatorial`] is, and also for a profile with no `[walkforward]`
/// table or one declaring another window form.
pub fn run_walkforward_combinatorial_profile(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<CombinatorialWalkforwardReport, HarnessError> {
    let config = profile.walkforward.as_ref().ok_or_else(|| {
        HarnessError::from(ProfileRefusal::WalkforwardTableMissingForCombinatorial)
    })?;
    let search = config.window_search()?;
    let objective = config.rank_metric()?.objective();
    let cut_of =
        |anchor: i64, step: WalkforwardStep<'_>| combinatorial_splits_on(config, anchor, step);
    combinatorial_walk(profile, store, &cut_of, search, &objective, cancel).map(thinned)
}

/// How a cut is resolved once its axis is known: from the axis' first step and its width (a profile's `embargo`
/// is a duration, which needs both).
pub(super) type CutOf<'a> =
    dyn Fn(i64, WalkforwardStep<'_>) -> Result<CombinatorialSplits, HarnessError> + 'a;

/// Walk `profile` over every combinatorial split and return its paths (the module doc is the
/// protocol). `search` is the walk's own choice: [`WalkforwardWindowSearch::Sweep`] scores the
/// profile's `[paramscan]` grid on each split's training segments with `objective`;
/// [`WalkforwardWindowSearch::None`] is the control.
///
/// Refused before any engine runs: `groups < 2`, `test_groups` outside `1..groups`, a cut of more
/// than [`MAX_OUT_OF_SAMPLE_RUNS`] group runs, more groups than bars, a search with no `[paramscan]` table, and everything the walk's pre-flight refuses. A
/// set `cancel` stops it at the next split or candidate with the walk's cancelled refusal.
pub fn run_walkforward_combinatorial(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    splits: CombinatorialSplits,
    search: WalkforwardWindowSearch,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<CombinatorialWalkforwardReport, HarnessError> {
    combinatorial_walk(profile, store, &|_, _| Ok(splits), search, objective, cancel).map(thinned)
}

/// `report` with every path's curve thinned by `vike_runs::decimate` to
/// [`curve_samples_per_path`], and nothing else touched: every number was computed from the whole
/// curve before this runs.
fn thinned(mut report: CombinatorialWalkforwardReport) -> CombinatorialWalkforwardReport {
    let cap = curve_samples_per_path(report.paths.len());
    for path in &mut report.paths {
        report.equity_curve_len = path.equity_curve.len();
        let (curve, stride) = vike_runs::decimate(&path.equity_curve, cap);
        path.equity_curve = curve;
        report.equity_curve_stride = stride;
    }
    report
}

/// The body of both doors, answering every path's WHOLE curve ([`thinned`] thins them):
/// `cut_of` resolves the cut once the axis is known (a profile's `embargo` is a duration, which
/// needs the series' first bar). A tick profile walks its own lane (`super::ticks`).
fn combinatorial_walk(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cut_of: &CutOf<'_>,
    search: WalkforwardWindowSearch,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<CombinatorialWalkforwardReport, HarnessError> {
    refuse_a_searched_cut_without_a_grid(profile, search)?;
    if profile.data.kind == ReplayKind::Tick {
        return super::ticks::tick_combinatorial(profile, store, cut_of, search, objective, cancel);
    }
    let mut held = one_series_preflight(profile, &store, cancel)?;
    let (symbol, bars) = held.series.pop().expect("one_series_preflight hands exactly one series");
    let anchor = bars.first().map_or(0, |b| b.timestamp);
    let splits = cut_of(anchor, WalkforwardStep::Interval(&profile.data.interval))?;
    let CombinatorialSplits { groups, test_groups, .. } = splits;
    refuse_an_unrunnable_cut(groups, test_groups)?;
    if bars.len() < groups {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardGroupsMoreThanBars {
            groups,
            symbol: symbol.clone(),
            bars: bars.len(),
        }));
    }

    let bounds = group_bounds(bars.len(), groups);
    let points = expand_paramscan(profile)?;
    // A group's engine starts afresh and decides nothing before the warm-up has passed, so a group no
    // longer than it is a test run of zeros that reads as a result. A searched cut trades a grid
    // point, so its warm-up is the point's that warms up least.
    let warmup = super::runner::walk_warmup(profile, search.searches().then_some(&points[..]))?;
    let at_point = super::runner::warmup_point_words(search.searches());
    let shortest = bounds.iter().map(|&(start, end)| end - start).min().unwrap_or(0);
    if shortest <= warmup {
        let need = warmup.saturating_add(1);
        let most = bars.len() / need;
        let fix = if most >= 2 {
            format!("Cut at most {most} groups, or widen data.from/data.to")
        } else {
            format!(
                "No cut fits: two groups need {} bars; widen data.from/data.to",
                need.saturating_mul(2)
            )
        };
        return Err(HarnessError::from(ProfileRefusal::WalkforwardGroupsShorterThanWarmup {
            groups,
            bars: bars.len(),
            symbol: symbol.clone(),
            shortest,
            warmup,
            at_point: at_point.to_string(),
            fix,
        }));
    }
    let properties_error = Arc::default();
    // Each candidate's training segment and each test group is a view of the bars held once
    // (`vike_simulator::BarSeriesShared`): no run copies a bar.
    // Each candidate keeps its own failed read, which `combinatorial_paths` keeps in grid order (`WalkScore`); the
    // walk's own slot is its test groups'.
    let score = |point: &ParamscanPoint, segment: Range<usize>| {
        let read: Arc<std::sync::OnceLock<String>> = Arc::default();
        let (row, score) = evaluation_scored_point_over_bars(
            profile,
            &store,
            vec![(symbol.clone(), bars.slice(segment))],
            &held.funding,
            &held.conversions,
            objective,
            &point.profile,
            point.overrides.clone(),
            &read,
        );
        (row, score, read.get().cloned())
    };
    let run = |run_profile: &ProfileBacktest, group: Range<usize>| {
        run_backtest_over_bars(
            run_profile,
            &store,
            vec![(symbol.clone(), bars.slice(group))],
            &held.funding,
            &held.conversions,
            &properties_error,
        )
    };
    let lane = CombinatorialLane {
        bars: Some(&bars[..]),
        len: bars.len(),
        score: &score,
        run: &run,
        data_warnings: &held.data_warnings,
    };
    let report = combinatorial_paths(profile, splits, &bounds, &points, search, cancel, lane)?;
    refuse_after_failed_properties(&properties_error)?;
    Ok(report)
}

/// The refusal of a SEARCHED cut over a profile with no `[paramscan]` table (both lanes).
fn refuse_a_searched_cut_without_a_grid(
    profile: &ProfileBacktest,
    search: WalkforwardWindowSearch,
) -> Result<(), HarnessError> {
    if search.searches() && !profile.is_paramscan() {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardCombinatorialSearchWithoutGrid));
    }
    Ok(())
}

/// The refusals of a cut that cannot be walked, whatever its axis: fewer than two groups, `test_groups`
/// outside `1..groups`, and more than [`MAX_OUT_OF_SAMPLE_RUNS`] group runs (both lanes).
pub(super) fn refuse_an_unrunnable_cut(
    groups: usize,
    test_groups: usize,
) -> Result<(), HarnessError> {
    if groups < 2 || test_groups == 0 || test_groups >= groups {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardCombinatorialCutUnrunnable {
            groups,
            test_groups,
        }));
    }
    let runs = out_of_sample_runs(groups, test_groups);
    if runs > MAX_OUT_OF_SAMPLE_RUNS {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardCombinatorialTooManyRuns {
            groups,
            test_groups,
            runs,
        }));
    }
    Ok(())
}

/// How a cut is walked over the axis its groups index (a bar series, a tick walk's seed-bar grid): `len` steps,
/// `score` one grid point on a training segment, `run` a profile over a test group (each fresh at
/// `engine.cash`), and the warnings every path carries.
pub(super) struct CombinatorialLane<'a> {
    /// The axis when it is a bar series, for each step's window times (`walk_forward_over_splits`); `None` on a grid.
    pub(super) bars: Option<&'a [vike_marketdata::Bar]>,
    pub(super) len: usize,
    pub(super) score: &'a SegmentScore<'a>,
    pub(super) run: &'a WalkRun<'a>,
    pub(super) data_warnings: &'a [String],
}

/// One grid point scored on one training segment of a cut (the evaluation fold's row and steering score).
pub(super) type SegmentScore<'a> = dyn Fn(&ParamscanPoint, Range<usize>) -> WalkScore + Sync + 'a;

/// Every split of the cut searched (or not) and traded through `lane`, and the paths: the module doc's protocol,
/// and the ONE loop both lanes' combinatorial walks run. `bounds` are the groups ([`group_bounds`] over the axis).
pub(super) fn combinatorial_paths(
    profile: &ProfileBacktest,
    splits: CombinatorialSplits,
    bounds: &[(usize, usize)],
    points: &[ParamscanPoint],
    search: WalkforwardWindowSearch,
    cancel: Option<&AtomicBool>,
    lane: CombinatorialLane<'_>,
) -> Result<CombinatorialWalkforwardReport, HarnessError> {
    let CombinatorialSplits { groups, test_groups, embargo_bars } = splits;
    let cut = combinatorial_purged_splits(lane.len, groups, test_groups, embargo_bars);
    // A searched split scores its grid on its training bars, and an embargo can leave it none: the
    // search would then "fail" every candidate with no cause to name.
    if search.searches()
        && let Some(split) = cut.iter().position(|(train, _)| train.is_empty())
    {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardEmbargoLeavesNoTraining {
            embargo_bars,
            split,
            groups,
            test_groups,
            bars: lane.len,
        }));
    }
    // `outcomes[g]`: the test run of every split that tested group `g`, in the splitter's order.
    let mut outcomes: Vec<Vec<WalkforwardWindowOutcome>> =
        (0..groups).map(|_| Vec::new()).collect();
    // The first candidate in grid order that could not read (`WalkScore`): splits run in order, and in one the
    // points' answers come back in grid order whichever the pool finished first.
    let mut failed_read: Option<String> = None;

    for (split, (train, test)) in cut.iter().enumerate() {
        if is_cancelled(cancel) {
            return Err(cancelled_run());
        }
        let winner = match search {
            WalkforwardWindowSearch::None => None,
            WalkforwardWindowSearch::Sweep => {
                let segments = contiguous_runs(train);
                let mut scored =
                    map_bounded(points.iter().enumerate().collect::<Vec<_>>(), |(i, point)| {
                        if is_cancelled(cancel) {
                            return (i, f64::NAN, None, None);
                        }
                        let (mut weighted, mut weight) = (0.0, 0.0);
                        for segment in &segments {
                            let (row, score, read) = (lane.score)(point, segment.clone());
                            if !score.is_finite() {
                                return (i, f64::NAN, row.error, read);
                            }
                            weighted += score * segment.len() as f64;
                            weight += segment.len() as f64;
                        }
                        (i, weighted / weight, None, None)
                    });
                let cause = scored.iter().find_map(|(_, _, error, _)| error.clone());
                if failed_read.is_none() {
                    failed_read = scored.iter().find_map(|(.., read)| read.clone());
                }
                scored.sort_by(|a, b| vike_analytics::metrics::cmp_scores_desc(a.1, b.1));
                match scored.first() {
                    Some(&(i, score, ..)) if score.is_finite() => Some(i),
                    _ => {
                        if is_cancelled(cancel) {
                            return Err(cancelled_run());
                        }
                        let cause =
                            cause.map_or(String::new(), |c| format!(": the first said {c}"));
                        return Err(HarnessError::Data(format!(
                            "combinatorial walk-forward: every one of the {} candidate(s) failed \
                             on the training segments of split {split}{cause}",
                            points.len()
                        )));
                    }
                }
            }
        };
        if is_cancelled(cancel) {
            return Err(cancelled_run());
        }
        let chosen_params = winner.map(|i| points[i].overrides.clone());
        let run_profile = winner.map_or(profile, |i| &points[i].profile);
        for (group, &(start, end)) in bounds.iter().enumerate() {
            // `test` is sorted, and a group is tested whole or not at all.
            if test.binary_search(&start).is_err() {
                continue;
            }
            let result = (lane.run)(run_profile, start..end).map_err(|e| {
                HarnessError::Data(format!(
                    "combinatorial walk-forward: the point split {split} selected failed on its \
                     test group {group}: {}",
                    e.message()
                ))
            })?;
            outcomes[group]
                .push(WalkforwardWindowOutcome { result, chosen_params: chosen_params.clone() });
        }
    }
    refuse_a_failed_read(failed_read)?;

    // Every group was tested by the same number of splits, C(N - 1, k - 1): that is the path count.
    // Path `j` drains the `j`-th outcome of every group, in time order.
    let path_count = outcomes[0].len();
    let mut outcomes: Vec<_> = outcomes.into_iter().map(Vec::into_iter).collect();
    let windows: Vec<WalkforwardSplit> = bounds
        .iter()
        .map(|&(start, end)| WalkforwardSplit {
            train_start: start,
            train_end: start,
            test_start: start,
            test_end: end,
        })
        .collect();
    let mut paths = Vec::with_capacity(path_count);
    for _ in 0..path_count {
        let mut group = 0;
        let report = walk_forward_over_splits(
            lane.bars,
            &windows,
            profile.engine.cash,
            periods_per_year(profile),
            |_| {
                let outcome = outcomes[group].next().expect("each group has one outcome per path");
                group += 1;
                outcome
            },
        );
        let mut path = labelled(report, profile, lane.data_warnings.to_vec());
        // The module doc's "A path's steps carry their COMPACT results".
        for step in &mut path.steps {
            step.results.extended = None;
            step.results.closed_trades = None;
        }
        paths.push(path);
    }
    Ok(CombinatorialWalkforwardReport {
        groups,
        test_groups,
        embargo_bars,
        splits: cut.len(),
        equity_curve_stride: 1,
        equity_curve_len: lane.len,
        paths,
    })
}

/// The ascending `indices` as maximal runs of consecutive values.
fn contiguous_runs(indices: &[usize]) -> Vec<Range<usize>> {
    let mut runs: Vec<Range<usize>> = Vec::new();
    for &i in indices {
        match runs.last_mut() {
            Some(run) if run.end == i => run.end = i + 1,
            _ => runs.push(i..i + 1),
        }
    }
    runs
}

#[path = "combinatorial_tests.rs"]
#[cfg(test)]
mod combinatorial_tests;
