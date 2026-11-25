//! Monte Carlo PERMUTATION TESTS over a profile's bars (Masters, *Permutation and Randomization
//! Tests for Trading System Development*, 2020): would the strategy have scored as well on the
//! same price changes in a random order?
//!
//! Two protocols, one report ([`PermutationTestReport`]):
//!
//! - [`run_permutation_test`] runs the profile's OWN parameters on its real history and on `N`
//!   permuted ones ([`vike_analytics::permutation::permute_bars`]) and counts how often a permuted
//!   history scored at least as well. It tests the strategy as it stands.
//! - [`run_permutation_training`] re-runs the WHOLE optimisation (the profile's `[paramscan]` grid)
//!   on the real history and on each permuted one, and compares the BEST scores. It tests the
//!   procedure: an optimiser finds something on noise too, and this measures how good "something
//!   on noise" looks (Masters' permutation training). The real history's winner is in the report.
//!
//! The p-value is Masters' `(1 + k) / (N + 1)`, `k` the permuted histories that scored at or above
//! the real one: the real history is one of the `N + 1` orderings, so `N` permutations cannot give
//! less than `1 / (N + 1)`. A permuted run that could not be scored (`NaN`) counts as at or above,
//! because it is no evidence against the strategy.
//!
//! History `i` is permuted with the seed `seed + (i + 1) * 0x9E37_79B9_7F4A_7C15` (wrapping), so
//! the report is the seed's alone, whichever thread finished first, and two tests with
//! neighbouring seeds share no history.
//!
//! # What is reused, and what each run sees
//!
//! The pre-flight is the walk-forward family's (`crate::walkforward::runner`'s
//! `held_bars_preflight`): every guard a plain run has, and the refusal of a strategy that reads an
//! input series (decision 0271), since no permuted input exists. Each history is scored by the
//! search lane's own point scorer over held bars (`search::paramscan::grid`'s
//! `evaluation_scored_point_over_bars`) on its bounded pool ([`map_bounded`]): the plain test
//! scores its histories in parallel, permutation training scores each history's grid in parallel.
//!
//! ⚠ The funding schedule and the conversion pairs are the REAL history's, handed to every run:
//! they are facts of the clock (a rate at an instant), and a permuted price path has no other. A
//! funding charge on a permuted run is the real rate on the permuted position.
//!
//! Bar mode only. Several symbols are permuted by one draw (the time index moves jointly), which
//! needs them aligned; a misaligned set is refused before any run.
//!
//! A profile asks with its `[permutation]` table ([`run_permutation_profile`], what the
//! backtesthub's `RunBacktest` runs for such a profile and answers `PermutationReport`); a caller
//! holding its own objective calls the two doors above with arguments.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use vike_analytics::permutation::permute_bars;
use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_model::params::ParamValue;
use vike_model::run_request::replay::ReplayKind;
use vike_simulator::BarSeriesShared;

use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::refusals::ProfileRefusal;
use crate::run::{cancelled_run, is_cancelled};
use crate::search::objective::SearchObjective;
use crate::search::paramscan::grid::evaluation_scored_point_over_bars;
use crate::search::paramscan::{ParamscanPoint, expand_paramscan, map_bounded};
use crate::walkforward::runner::{HeldBars, held_bars_preflight};

/// What a permutation test answers. `real_params` is what the search chose on the real history
/// (`None` for the plain test, which chooses nothing); `permuted_scores[i]` is history `i`'s score
/// (its best one under permutation training); `at_or_above` counts those at or above `real_score`,
/// a `NaN` included, and `p_value = (1 + at_or_above) / (permuted_scores.len() + 1)`.
#[derive(Debug, Clone, Serialize)]
pub struct PermutationTestReport {
    pub trained: bool,
    pub seed: u64,
    pub real_score: f64,
    pub real_params: Option<Vec<(String, ParamValue)>>,
    pub permuted_scores: Vec<f64>,
    pub at_or_above: usize,
    pub p_value: f64,
    pub data_warnings: Vec<String>,
    pub strategy_file: String,
    pub strategy_sha256: String,
}

/// The profile's own permutation test, every knob read from its `[permutation]` table
/// (`permutations`, `seed`, `train`, `rank_by`): [`run_permutation_training`] when `train` is set,
/// [`run_permutation_test`] otherwise. Refused for a profile with no `[permutation]` table, and as
/// the door it reaches refuses.
pub fn run_permutation_profile(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<PermutationTestReport, HarnessError> {
    let config = profile
        .permutation
        .as_ref()
        .ok_or_else(|| HarnessError::from(ProfileRefusal::PermutationTableMissing))?;
    let objective = config.rank_metric()?.objective();
    // The table's seed is a `usize`, the unsigned integer the published profile schema models.
    let (permutations, seed) = (config.permutations, config.seed as u64);
    permutation_run(profile, &store, permutations, seed, &objective, cancel, config.train)
}

/// The plain test: the profile's own parameters on the real history and on `permutations` permuted
/// ones, each scored by `objective` (HIGHER is better, as everywhere a search ranks).
///
/// Refused before any run: zero permutations, a tick profile, a misaligned multi-symbol set, and
/// everything the pre-flight refuses. A real history that cannot be scored is refused (there is no
/// p-value without it); a set `cancel` stops it with the harness's cancelled refusal.
pub fn run_permutation_test(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    permutations: usize,
    seed: u64,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<PermutationTestReport, HarnessError> {
    permutation_run(profile, &store, permutations, seed, objective, cancel, false)
}

/// Permutation TRAINING: the profile's `[paramscan]` grid searched on the real history and on each
/// of `permutations` permuted ones, the best score of each compared. Refused as
/// [`run_permutation_test`] is, and also for a profile with no `[paramscan]` table.
pub fn run_permutation_training(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    permutations: usize,
    seed: u64,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<PermutationTestReport, HarnessError> {
    permutation_run(profile, &store, permutations, seed, objective, cancel, true)
}

/// The seed history `index` is permuted with (the module doc's rule).
fn history_seed(seed: u64, index: usize) -> u64 {
    seed.wrapping_add((index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// `held`'s series permuted by one draw from `seed`, under their own symbols (each bar keeps its
/// place's tag, so no grid point's run copies the history).
fn permuted(held: &HeldBars, seed: u64) -> Result<History, HarnessError> {
    let views: Vec<&[Bar]> = held.series.iter().map(|(_, bars)| &bars[..]).collect();
    let bars = permute_bars(&views, seed).map_err(|refusal| {
        HarnessError::from(ProfileRefusal::PermutationBarsRefused { refusal })
    })?;
    let symbols = held.series.iter().map(|(symbol, _)| symbol.clone());
    Ok(symbols.zip(bars.into_iter().map(BarSeriesShared::from)).collect())
}

/// One history a run scores: each series under its symbol.
type History = Vec<(String, BarSeriesShared)>;

/// The body of both doors; `train` picks the protocol.
fn permutation_run(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    permutations: usize,
    seed: u64,
    objective: &SearchObjective,
    cancel: Option<&AtomicBool>,
    train: bool,
) -> Result<PermutationTestReport, HarnessError> {
    if permutations == 0 {
        return Err(HarnessError::from(ProfileRefusal::PermutationPermutationsZero));
    }
    if profile.data.kind != ReplayKind::Bar {
        return Err(HarnessError::from(ProfileRefusal::PermutationBarModeOnly));
    }
    if train && !profile.is_paramscan() {
        return Err(HarnessError::from(ProfileRefusal::PermutationTrainingNeedsParamscan));
    }
    let held = held_bars_preflight(profile, store, "a permutation test", cancel)?;
    // A misaligned set is refused here, before the real run, by the first history's own permutation.
    permuted(&held, history_seed(seed, 0))?;

    let points = if train { expand_paramscan(profile)? } else { Vec::new() };
    // Each run keeps its own failed read, which `HistoryScorer::histories` keeps in a fixed order.
    let run =
        |run_profile: &ProfileBacktest, overrides: Vec<(String, ParamValue)>, history: History| {
            let read: Arc<std::sync::OnceLock<String>> = Arc::default();
            let (row, score) = evaluation_scored_point_over_bars(
                profile,
                store,
                history,
                &held.funding,
                &held.conversions,
                objective,
                run_profile,
                overrides,
                &read,
            );
            (score, row.error, read.get().cloned())
        };
    let scorer = HistoryScorer { profile, run: &run };
    let seeds: Vec<u64> = (0..permutations).map(|i| history_seed(seed, i)).collect();
    let history_of = |s: u64| permuted(&held, s).expect("alignment refused above, before any run");
    let (real_score, real_winner, permuted_scores) =
        scorer.histories(&points, held.series.clone(), &history_of, seeds, train, cancel)?;

    let at_or_above =
        permuted_scores.iter().filter(|score| score.is_nan() || **score >= real_score).count();
    Ok(PermutationTestReport {
        trained: train,
        seed,
        real_score,
        real_params: real_winner.map(|i| points[i].overrides.clone()),
        p_value: (1 + at_or_above) as f64 / (permuted_scores.len() + 1) as f64,
        permuted_scores,
        at_or_above,
        data_warnings: held.data_warnings.clone(),
        strategy_file: profile.one_strategy()?.file.clone(),
        strategy_sha256: profile.one_strategy()?.sha256().to_string(),
    })
}

/// One run of the profile's own parameters or of a grid point's profile over a history: its score (`NaN` when it
/// failed, the search lane's rule), the cause when it failed, and the read it could not make (its refusal).
type HistoryRun<'a> = dyn Fn(
        &ProfileBacktest,
        Vec<(String, ParamValue)>,
        History,
    ) -> (f64, Option<String>, Option<String>)
    + Sync
    + 'a;

/// A history's answer: its best score, the point that scored it, and the first failure's cause and the first read
/// that could not be made, both in grid order.
type HistoryScore = (f64, Option<usize>, Option<String>, Option<String>);

/// What scoring one history needs, borrowed once.
struct HistoryScorer<'a> {
    profile: &'a ProfileBacktest,
    run: &'a HistoryRun<'a>,
}

impl HistoryScorer<'_> {
    /// The real history scored, then each permuted one (`history_of` each of `seeds`), by the protocol `train`
    /// picks: the real score, the point it chose, and every permuted score in `seeds`' order. A run that could not
    /// READ refuses the test, and the read named is the FIRST in a fixed order (the real history, then each
    /// permuted one in `seeds`' order, and in one its grid's points in order), whichever run finished first.
    fn histories(
        &self,
        points: &[ParamscanPoint],
        real: History,
        history_of: &(dyn Fn(u64) -> History + Sync),
        seeds: Vec<u64>,
        train: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<(f64, Option<usize>, Vec<f64>), HarnessError> {
        let refuse_a_failed_read =
            |read: Option<String>| read.map_or(Ok(()), |why| Err(HarnessError::Data(why)));
        let (real_score, real_winner, cause, read) =
            if train { self.best(points, real, cancel) } else { self.plain(real) };
        if is_cancelled(cancel) {
            return Err(cancelled_run());
        }
        refuse_a_failed_read(read)?;
        if !real_score.is_finite() {
            let cause = cause.map_or(String::new(), |c| format!(": {c}"));
            return Err(HarnessError::Data(format!(
                "the real history could not be scored, so there is nothing to compare the permuted \
                 ones with{cause}"
            )));
        }
        let score_one = |permutation_seed: u64| -> (f64, Option<String>) {
            if is_cancelled(cancel) {
                return (f64::NAN, None);
            }
            let history = history_of(permutation_seed);
            let (score, _, _, read) =
                if train { self.best(points, history, cancel) } else { self.plain(history) };
            (score, read)
        };
        // The plain test fans its histories over the bounded pool; training fans each history's grid
        // (`HistoryScorer::best`), so the pool is never entered twice. Either way the answers come back in
        // `seeds`' order.
        let scored: Vec<(f64, Option<String>)> = if train {
            seeds.into_iter().map(score_one).collect()
        } else {
            map_bounded(seeds, score_one)
        };
        if is_cancelled(cancel) {
            return Err(cancelled_run());
        }
        refuse_a_failed_read(scored.iter().find_map(|(_, read)| read.clone()))?;
        Ok((real_score, real_winner, scored.into_iter().map(|(score, _)| score).collect()))
    }

    /// The profile's own parameters over `history`.
    fn plain(&self, history: History) -> HistoryScore {
        let (score, cause, read) = (self.run)(self.profile, Vec::new(), history);
        (score, None, cause, read)
    }

    /// Every grid point over `history`: the best score, the point that scored it, and the first
    /// failure's cause and failed read in grid order. `NaN` and no point when none could be scored.
    fn best(
        &self,
        points: &[ParamscanPoint],
        history: History,
        cancel: Option<&AtomicBool>,
    ) -> HistoryScore {
        let mut scored =
            map_bounded(points.iter().enumerate().collect::<Vec<_>>(), |(i, point)| {
                if is_cancelled(cancel) {
                    return (i, f64::NAN, None, None);
                }
                let (score, cause, read) =
                    (self.run)(&point.profile, point.overrides.clone(), history.clone());
                (i, score, cause, read)
            });
        let cause = scored.iter().find_map(|(_, _, cause, _)| cause.clone());
        let read = scored.iter().find_map(|(.., read)| read.clone());
        scored.sort_by(|a, b| vike_analytics::metrics::cmp_scores_desc(a.1, b.1));
        match scored.first() {
            Some(&(i, score, ..)) if score.is_finite() => (score, Some(i), cause, read),
            _ => (f64::NAN, None, cause, read),
        }
    }
}

#[path = "permutation_tests.rs"]
#[cfg(test)]
mod permutation_tests;
