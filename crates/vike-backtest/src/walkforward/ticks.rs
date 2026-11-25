//! The TICK lane of the walk-forward family (the owner's yes of 2026-11-20): ONE tape, read and prepared once for
//! the whole walk (`crate::run::tick_walk_tapes`), and every window, each training and validation half and every
//! grid point a training half scores, a run of the profile over that window's time range whose tape is a BORROWED
//! range of the walk's (`crate::run::tick_replay::tapes`'s module doc, "A walk's ONE tape"). Each such run is the
//! one a plain tick backtest of that range makes (`crate::run::run_backtest_over_tapes` of the window's profile:
//! its funding, conversions, inputs and refusals read over that range), so a window's answer is that range's
//! answer alone, digest for digest.
//!
//! - **The axis is the seed-bar grid**: `engine.seed_bar_interval_ms` steps from the open of the bucket holding
//!   `data.from` to the one holding `data.to` (a tick walk is bounded). Every window form resolves on it as on a
//!   bar series (`super::windows::resolve_windows_on`): `n_splits` into equal spans of time, a duration into whole
//!   seed bars (floored), a `bars` span a count of them; and every cut falls on a bucket's edge, so a window's
//!   seed bars are the ones its own events make. A step's range in the report is a seed-bar index.
//! - **A window is cut on whole timestamps**: binary-searched on the venue stamp, so events sharing one are never
//!   split between two windows.
//! - **The warm-up counts what the tick engine's does**, priced events (a quote or a trade): a window whose
//!   validation half (or, when the walk searches, training half; a combinatorial group) holds no more of them than
//!   the warm-up (`super::runner::walk_warmup`) is refused, as a bar window no longer than it is.
//! - **Refused in words**: more than one series, `[data.options]` legs, heard streams (`[[data.reference]]`,
//!   `[[data.mark]]`) and `engine.feed_latency` (each orders or makes events across a window's edge otherwise
//!   than a run of that window alone would), no `engine.seed_bar_interval_ms`, an open `data.from`/`data.to`.
//! - **Every form the bar lane walks**: the fixed walk (`super::runner::run_walkforward`), the searched walk
//!   (`[walkforward] search = "paramscan"`, through the one window loop `super::runner::walk_windows`) and the
//!   combinatorial walk (`super::combinatorial`, through `combinatorial_paths`).

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_analytics::TradingRecord;
use vike_analytics::validation::{WalkforwardMode, group_bounds};
use vike_analytics::walkforward::{WalkforwardResults, WalkforwardWindowTimes};
use vike_data::HistoryStore;
use vike_marketdata::interval::bar_open;

use super::combinatorial::{
    CombinatorialLane, CombinatorialSplits, CombinatorialWalkforwardReport, CutOf,
    combinatorial_paths, refuse_an_unrunnable_cut,
};
use super::runner::{
    WalkLane, WalkScore, WalkforwardWindowSearch, labelled, refuse_a_search_without_a_grid,
    walk_ledger, walk_warmup, walk_windows, walkforward_table, warmup_point_words,
};
use super::windows::{WalkforwardStep, resolve_windows_on};
use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::refusals::ProfileRefusal;
use crate::run::tick_replay::tapes::TickTapes;
use crate::run::{cancelled_run, is_cancelled, run_backtest_over_tapes, tick_walk_tapes};
use crate::search::objective::SearchObjective;
use crate::search::paramscan::grid::evaluation_scored_outcome;
use crate::search::paramscan::{ParamscanPoint, expand_paramscan};

/// A tick walk's one tape and the seed-bar grid its windows index (the module doc).
pub(super) struct TickWalk {
    tapes: TickTapes,
    /// The series' slot name, as the refusals name it.
    symbol: String,
    /// The walk's range, both ends inclusive.
    start: i64,
    end: i64,
    /// The grid: step `i` is `[anchor + i * step, anchor + (i + 1) * step)`, `len` of them.
    anchor: i64,
    step: i64,
    len: usize,
    /// What the data pre-flight found, said on the walk's `overall` results.
    data_warnings: Vec<String>,
}

impl TickWalk {
    /// Everything a tick walk establishes before its first window, then its ONE prepared tape. Refused: what the
    /// module doc lists, a symbol the ledger cannot book (`super::runner::walk_ledger`), an armed data gate's
    /// refusal, a failed read, and a range holding no quote or trade.
    fn preflight(
        profile: &ProfileBacktest,
        store: &Arc<dyn HistoryStore + Send + Sync>,
        cancel: Option<&AtomicBool>,
    ) -> Result<TickWalk, HarnessError> {
        let series = profile.data.resolved_series();
        if series.len() != 1 {
            return Err(HarnessError::from(ProfileRefusal::WalkforwardTickSeriesNotOne {
                series: series.len(),
            }));
        }
        if profile.data.options.is_some()
            || !profile.data.reference.is_empty()
            || !profile.data.mark.is_empty()
            || profile.engine.feed_latency
        {
            return Err(HarnessError::from(ProfileRefusal::WalkforwardTickEventsCrossWindowEdge));
        }
        let Some(step) = profile.engine.seed_bar_interval_ms.filter(|step| *step > 0) else {
            return Err(HarnessError::from(ProfileRefusal::WalkforwardTickSeedBarIntervalMissing));
        };
        let range = profile.range()?;
        let (Some(start), Some(end)) = (range.start, range.end) else {
            return Err(HarnessError::from(ProfileRefusal::WalkforwardTickRangeOpenEnded));
        };
        walk_ledger(profile)?;
        // ⚠ THE DATA PRE-FLIGHT, once per walk over the whole range every window is cut from, as the bar
        // walk's (each window's own run asks it again over its range).
        let data_warnings =
            crate::run::data_plan::enforce(profile, store.as_ref(), "the store this run opened")?;
        for line in &data_warnings {
            tracing::warn!("{line}");
        }
        if is_cancelled(cancel) {
            return Err(cancelled_run());
        }
        let tapes = tick_walk_tapes(profile, store)?;
        let symbol = series[0].leg_name();
        if tapes.walk_priced_events(start, end) == 0 {
            return Err(HarnessError::Data(format!(
                "no quotes or trades for {symbol} in the profile's range"
            )));
        }
        let anchor = bar_open(start, step);
        let len = usize::try_from((bar_open(end, step) - anchor) / step + 1).unwrap_or(usize::MAX);
        Ok(TickWalk { tapes, symbol, start, end, anchor, step, len, data_warnings })
    }

    /// The time range `[from, to]` (both inclusive) that `steps` of the grid cover, inside the walk's range.
    fn span(&self, steps: &Range<usize>) -> (i64, i64) {
        let at = |i: usize| {
            let i = i64::try_from(i).unwrap_or(i64::MAX);
            self.anchor.saturating_add(i.saturating_mul(self.step))
        };
        (at(steps.start).max(self.start), at(steps.end).saturating_sub(1).min(self.end))
    }

    /// `base` over `steps` of the grid alone: the plain tick backtest of that range (no walk, no grid).
    pub(super) fn window_profile(
        &self,
        base: &ProfileBacktest,
        steps: &Range<usize>,
    ) -> ProfileBacktest {
        let (from, to) = self.span(steps);
        let mut window = base.clone();
        window.data.from = from.to_string();
        window.data.to = to.to_string();
        window.walkforward = None;
        window.paramscan = None;
        window
    }

    /// The open times of the first and last seed bar of a window's two halves on the grid, the first held to the
    /// walk's start: what a bar walk reads off its bars (`super::window_times`). `None` for an empty test half, and
    /// no train pair for an empty or absent train half (a combinatorial group's).
    fn window_times(
        &self,
        test: &Range<usize>,
        train: Option<&Range<usize>>,
    ) -> Option<WalkforwardWindowTimes> {
        let open = |i: usize| {
            let i = i64::try_from(i).unwrap_or(i64::MAX);
            self.anchor.saturating_add(i.saturating_mul(self.step)).max(self.start)
        };
        let ends = |r: &Range<usize>| (!r.is_empty()).then(|| (open(r.start), open(r.end - 1)));
        let (test_start, test_end) = ends(test)?;
        let train = train.and_then(ends);
        Some(WalkforwardWindowTimes {
            train_start: train.map(|(start, _)| start),
            train_end: train.map(|(_, end)| end),
            test_start,
            test_end,
        })
    }

    /// The priced events `steps` of the grid hold.
    fn priced(&self, steps: &Range<usize>) -> usize {
        let (from, to) = self.span(steps);
        self.tapes.walk_priced_events(from, to)
    }

    /// `profile` run over `steps`, its tape a cut of the walk's.
    fn run(
        &self,
        store: &Arc<dyn HistoryStore + Send + Sync>,
        profile: &ProfileBacktest,
        steps: &Range<usize>,
    ) -> Result<TradingRecord, HarnessError> {
        run_backtest_over_tapes(
            &self.window_profile(profile, steps),
            Arc::clone(store),
            &self.tapes,
        )
    }

    /// `point` of `base`'s grid scored over `steps`: the search's evaluation fold over the window's run.
    fn score(
        &self,
        base: &ProfileBacktest,
        store: &Arc<dyn HistoryStore + Send + Sync>,
        objective: &SearchObjective,
        point: &ParamscanPoint,
        steps: &Range<usize>,
    ) -> WalkScore {
        let window = self.window_profile(&point.profile, steps);
        let outcome = run_backtest_over_tapes(&window, Arc::clone(store), &self.tapes);
        // A candidate that could not READ its data did not lose: the others ranked without it would name a
        // winner the walk never compared, so the loop refuses the walk in grid order, as the bar lane's.
        let read = match &outcome {
            Err(HarnessError::Data(why)) => Some(why.clone()),
            _ => None,
        };
        let (row, score) =
            evaluation_scored_outcome(base, objective, &window, point.overrides.clone(), outcome);
        (row, score, read)
    }
}

/// The tick lane's walk, fixed (`search` [`WalkforwardWindowSearch::None`]) or searched over `points`, each
/// window a run over a range of the walk's one tape (the module doc). The same refusals in the same order as the
/// bar lane's pre-flight, the warm-up counted in priced events.
pub(super) fn tick_walk(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    points: &[ParamscanPoint],
    search: WalkforwardWindowSearch,
    mode: WalkforwardMode,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<WalkforwardResults, HarnessError> {
    let config = walkforward_table(profile)?;
    // Before the tape is read: the profile alone says it.
    refuse_a_search_without_a_grid(profile, search)?;
    let walk = TickWalk::preflight(profile, &store, cancel)?;
    let seed = WalkforwardStep::SeedMs(walk.step);
    let windows = resolve_windows_on(config, walk.len, walk.anchor, seed, mode)?;
    if windows.is_empty() {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardTickNoWindows {
            seed_bars: walk.len,
            symbol: walk.symbol.clone(),
        }));
    }
    let searches = search.searches();
    let warmup = walk_warmup(profile, searches.then_some(points))?;
    for (i, w) in windows.iter().enumerate() {
        let test = walk.priced(&(w.test_start..w.test_end));
        let train = walk.priced(&(w.train_start..w.train_end));
        if test <= warmup || (searches && train <= warmup) {
            return Err(HarnessError::from(
                ProfileRefusal::WalkforwardTickWindowShorterThanWarmup {
                    i,
                    seed_bars: walk.len,
                    symbol: walk.symbol.clone(),
                    test,
                    training_clause: if searches {
                        format!(" and {train} in training")
                    } else {
                        String::new()
                    },
                    warmup,
                    at_point: warmup_point_words(searches).to_string(),
                    train_clause: if searches { " and walkforward.train" } else { "" }.to_string(),
                },
            ));
        }
    }

    let hold = |train: Range<usize>| train;
    let score = |train: &Range<usize>, point: &ParamscanPoint| {
        walk.score(profile, &store, objective, point, train)
    };
    let run =
        |run_profile: &ProfileBacktest, test: Range<usize>| walk.run(&store, run_profile, &test);
    let lane = WalkLane { bars: None, hold: &hold, score: &score, run: &run };
    let mut report = walk_windows(profile, &windows, points, search, cancel, lane)?;
    // The grid has no bars to read the window times off: they are its seed bars' opens, one step a window.
    for (step, w) in report.steps.iter_mut().zip(&windows) {
        let train = w.train_start..w.train_end;
        step.window_times = walk.window_times(&(w.test_start..w.test_end), Some(&train));
    }
    Ok(labelled(report, profile, walk.data_warnings))
}

/// The tick lane's combinatorial walk: the cut `cut_of` resolves on the walk's seed-bar grid, each group a run
/// over a range of its one tape (the module doc), the groups' warm-up counted in priced events.
pub(super) fn tick_combinatorial(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cut_of: &CutOf<'_>,
    search: WalkforwardWindowSearch,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<CombinatorialWalkforwardReport, HarnessError> {
    let walk = TickWalk::preflight(profile, &store, cancel)?;
    let splits = cut_of(walk.anchor, WalkforwardStep::SeedMs(walk.step))?;
    let CombinatorialSplits { groups, test_groups, .. } = splits;
    refuse_an_unrunnable_cut(groups, test_groups)?;
    if walk.len < groups {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardTickGroupsMoreThanSeedBars {
            groups,
            symbol: walk.symbol.clone(),
            seed_bars: walk.len,
        }));
    }
    let bounds = group_bounds(walk.len, groups);
    let points = expand_paramscan(profile)?;
    let warmup = walk_warmup(profile, search.searches().then_some(&points[..]))?;
    let fewest = bounds.iter().map(|&(start, end)| walk.priced(&(start..end))).min().unwrap_or(0);
    if fewest <= warmup {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardTickGroupsShorterThanWarmup {
            groups,
            seed_bars: walk.len,
            symbol: walk.symbol.clone(),
            fewest,
            warmup,
            at_point: warmup_point_words(search.searches()).to_string(),
        }));
    }
    let score = |point: &ParamscanPoint, segment: Range<usize>| {
        walk.score(profile, &store, objective, point, &segment)
    };
    let run =
        |run_profile: &ProfileBacktest, group: Range<usize>| walk.run(&store, run_profile, &group);
    let lane = CombinatorialLane {
        bars: None,
        len: walk.len,
        score: &score,
        run: &run,
        data_warnings: &walk.data_warnings,
    };
    let mut report = combinatorial_paths(profile, splits, &bounds, &points, search, cancel, lane)?;
    // A group's test dates off the grid, as the stitched walk's; a path's step has no train half of its own.
    for step in report.paths.iter_mut().flat_map(|path| path.steps.iter_mut()) {
        let (start, end) = step.test_range;
        step.window_times = walk.window_times(&(start..end), None);
    }
    Ok(report)
}

#[path = "ticks_tests.rs"]
#[cfg(test)]
mod ticks_tests;
