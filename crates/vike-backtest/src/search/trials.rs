//! The trial LEDGER's writer, and `--resume`'s warm cache — one type, because they are one seam.
//!
//! # What this is
//!
//! [`TrialRecorder`] is a [`SearchEvaluator`] that WRAPS another one. A searcher drives it exactly
//! as it drives `crate::harness::SearchEvaluatorStore`, and it does two things the wrapped evaluator does
//! not: it appends one [`crate::trial_ledger::TrialRecord`] per evaluation to the parent run's
//! ledger, and it answers from a warm cache instead of evaluating when it has seen a candidate
//! before.
//!
//! The DOCUMENTS it writes live in `crate::trial_ledger`, outside the search family entirely; this
//! module is the half that needs [`SearchCandidate`]/[`SearchEvaluation`] and therefore sits in it.
//!
//! # Why a decorator rather than a hook in `optimize`
//!
//! `crate::harness::optimize::optimize` is the ONE door and its doc says nothing may call
//! `Optimizer::search` directly: `accepts` runs there before any loop starts. A decorator
//! honours that untouched: the same `accepts`, the same loop. And it costs NO signature change
//! anywhere — a field on `crate::harness::SearchOutcome` would be 6 struct-literal edits, a field on
//! `crate::harness::ParamscanRow` 19, and a new required `Optimizer` method 5 impls.
//!
//! # Why this is all `--resume` needs
//!
//! Every one of the four methods drives a SEED-DETERMINISTIC loop. Given the same profile, the same
//! store, the same method and the same seed, a searcher proposes the same candidates in the same
//! order — so replaying the loop against a cache that answers with the ORIGINAL scores reproduces
//! the original trajectory exactly, and the only work performed is the evaluations that never
//! happened. Resuming a searcher's INTERNAL state would instead mean serializing
//! `crate::search::SearchEulerTrace` (which derives `Debug, Clone` only), `super::tpe::TpeOptimizer`'s
//! private `Vec<Trial>` and `super::genetic::SearchGenetic`'s local `pop`/`seen` — four methods,
//! five `impl Optimizer` edits, and a new failure mode per method.
//!
//! # ⚠ A FAILED trial is warmed like any other
//!
//! One rule rather than two. A trial that failed is a row whose score is `NaN`, and re-running it
//! is the case a resume most wants to skip. The cost is real and is declared rather than hidden: if
//! the original failures were TRANSIENT (a store that was briefly unreadable), a resume reproduces
//! them. [`TrialWarm::failed`] is carried so the caller's resume line can say how many reused
//! trials had failed, which is what makes that visible instead of silent.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::harness::optimize::{SearchCandidate, SearchEvaluation, SearchEvaluator};
use crate::harness::paramscan::{ParamscanRow, RankBy};
use crate::trial_ledger::TrialRecord;

/// What a REUSED row's `error` says.
///
/// ⚠ A reused row is not rendered on any path this workspace ships: `--resume` prints the LEDGER
/// rather than the `ParamscanReport` (`crates/vike-backtesthub/src/backtest_cli.rs`'s resume arm carries
/// that argument), precisely because `super::paramscan::ParamscanReport`'s `Display` branches on
/// `report: None` and would print a reused trial as `FAILED: …`. This string exists so that if some
/// future caller DOES render one, it says what it is instead of `(unknown error)`.
pub const REUSED_TRIAL: &str =
    "reused from an earlier trial — see `vike-backend backtesthub trials <id>`";

/// One warm-cache entry: what the earlier run scored this candidate, and whether it had failed.
///
/// ⚠ The entry carries the SCORE and not the report. A `BacktestReport` could now be rebuilt from a
/// ledger row (that type gained `Deserialize` with the run record), and it is deliberately not:
/// `profit_factor` writes `null` for BOTH of its house sentinels and reads back as `inf`, so a
/// reconstructed row would print a number the original run did not compute. The score is what the
/// SEARCHER steers on and is exact; the report is what the LEDGER already holds verbatim.
#[derive(Debug, Clone, Copy)]
pub struct TrialWarm {
    /// The steering score the earlier run recorded. `NaN` means unrankable.
    pub score: f64,
    /// Whether that trial carried an `error`.
    pub failed: bool,
}

/// What a search SPENT, as the recorder saw it.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrialRecorderTally {
    /// Candidates the searcher asked about — its budget spend, reused ones included.
    pub evaluated: usize,
    /// How many of those were answered from the warm cache.
    pub reused: usize,
    /// How many REUSED answers came from a trial that had failed.
    pub reused_failed: usize,
    /// How many freshly-evaluated trials failed.
    pub failed: usize,
    /// Ledger appends that did not land.
    pub write_failures: usize,
}

/// Build the warm cache from a ledger's records. A repeated key keeps the LAST record, matching
/// `crate::trial_ledger::latest_by_n`'s rule — the caller normally passes an already-collapsed
/// slice.
///
/// Keyed by the [`SearchCandidate`] itself — exact and total, because `vike_model::params::ParamValue`
/// compares a float by its BIT PATTERN. ORDER-SENSITIVE, deliberately:
/// `crate::harness::paramscan::profile_with_overrides` applies overrides in order, so two orderings
/// are not provably the same point, and a differing order is a cache MISS (a re-evaluation, never
/// a wrong answer); every searcher here emits a stable order, so the miss is unreachable in
/// practice. `0.0` and `-0.0` compare EQUAL under `f64`'s `==` and key DIFFERENTLY here: the same
/// direction, a miss.
pub fn warm_from(trials: &[TrialRecord]) -> HashMap<SearchCandidate, TrialWarm> {
    trials
        .iter()
        .map(|t| (t.overrides.clone(), TrialWarm { score: t.score, failed: t.error.is_some() }))
        .collect()
}

struct TrialRecorderState {
    next_n: usize,
    tally: TrialRecorderTally,
    first_write_error: Option<String>,
}

/// See this module's doc.
pub struct TrialRecorder<'a> {
    inner: &'a dyn SearchEvaluator,
    /// The parent run directory the ledger is appended into. `None` under `--keep-trials none`.
    ledger: Option<PathBuf>,
    warm: HashMap<SearchCandidate, TrialWarm>,
    state: Mutex<TrialRecorderState>,
}

impl<'a> TrialRecorder<'a> {
    /// Wrap `inner`, appending into `ledger` (when there is one) and answering from `warm`.
    pub fn new(
        inner: &'a dyn SearchEvaluator,
        ledger: Option<PathBuf>,
        warm: HashMap<SearchCandidate, TrialWarm>,
    ) -> Self {
        TrialRecorder {
            inner,
            ledger,
            warm,
            state: Mutex::new(TrialRecorderState {
                next_n: 0,
                tally: TrialRecorderTally::default(),
                first_write_error: None,
            }),
        }
    }

    /// What the search has spent so far. `Copy`, so a caller reads it once and passes it on.
    pub fn tally(&self) -> TrialRecorderTally {
        self.state.lock().expect("recorder tally").tally
    }

    /// The FIRST ledger-write failure, if any. One message rather than one per trial: a directory
    /// that stopped accepting writes fails every append, and an operator needs the reason once.
    pub fn first_write_error(&self) -> Option<String> {
        self.state.lock().expect("recorder tally").first_write_error.clone()
    }

    fn append(&self, n: usize, e: &SearchEvaluation) {
        let Some(dir) = self.ledger.as_deref() else {
            return;
        };
        let record = TrialRecord {
            n,
            overrides: e.row.overrides.clone(),
            score: e.score,
            // `.ok()` rather than `?`: a report that will not serialize costs its METRICS, never
            // the run. Persisting is additive — `crate::trial_ledger`'s module doc.
            metrics: e.row.report.as_ref().and_then(|r| serde_json::to_value(r).ok()),
            error: e.row.error.clone(),
        };
        if let Err(why) = crate::trial_ledger::append_trial(dir, &record) {
            let mut st = self.state.lock().expect("recorder state");
            st.tally.write_failures += 1;
            if st.first_write_error.is_none() {
                st.first_write_error = Some(why.to_string());
            }
        }
    }
}

/// The row a cache hit answers with. `report: None` because the warm cache carries a SCORE rather
/// than a report (see [`TrialWarm`]), and [`REUSED_TRIAL`] rather than `None` so the row is never
/// rendered as `(unknown error)`.
fn reused_row(overrides: SearchCandidate, score: f64) -> SearchEvaluation {
    SearchEvaluation {
        row: ParamscanRow {
            overrides,
            report: None,
            error: Some(REUSED_TRIAL.to_string()),
            score: Some(score),
        },
        score,
    }
}

impl SearchEvaluator for TrialRecorder<'_> {
    fn evaluate(&self, batch: Vec<SearchCandidate>) -> Vec<SearchEvaluation> {
        // ── 1. split, keeping every candidate's SLOT ────────────────────────────────────────────
        let mut out: Vec<Option<SearchEvaluation>> = Vec::with_capacity(batch.len());
        let mut misses: Vec<SearchCandidate> = Vec::new();
        let mut miss_slots: Vec<usize> = Vec::new();
        let mut hits: Vec<bool> = Vec::with_capacity(batch.len());
        for (i, c) in batch.iter().enumerate() {
            match self.warm.get(c) {
                Some(w) => {
                    out.push(Some(reused_row(c.clone(), w.score)));
                    hits.push(true);
                    let mut st = self.state.lock().expect("recorder state");
                    st.tally.reused += 1;
                    if w.failed {
                        st.tally.reused_failed += 1;
                    }
                }
                None => {
                    out.push(None);
                    hits.push(false);
                    miss_slots.push(i);
                    misses.push(c.clone());
                }
            }
        }

        // ── 2. evaluate the misses as ONE batch ─────────────────────────────────────────────────
        // ⚠ The inner evaluator sees ONE call with the misses in their original relative order, so
        // its index-order contract and the pool rule both hold. On a FRESH run `misses` IS the
        // original batch, which is what makes a recorded search byte-identical to an unrecorded
        // one. An ALL-HIT batch calls nothing: handing `SearchEvaluatorStore` an empty vector is a shape
        // worth not creating.
        let fresh = if misses.is_empty() { Vec::new() } else { self.inner.evaluate(misses) };
        assert_eq!(
            fresh.len(),
            miss_slots.len(),
            "a SearchEvaluator must answer one SearchEvaluation per candidate — see its doc"
        );
        for (slot, e) in miss_slots.iter().zip(fresh) {
            out[*slot] = Some(e);
        }

        // ── 3. assign n IN INPUT ORDER, and append only the misses ──────────────────────────────
        let mut answered: Vec<SearchEvaluation> = Vec::with_capacity(out.len());
        for (i, e) in out.into_iter().enumerate() {
            let e = e.expect("every slot was filled by step 2");
            let n = {
                let mut st = self.state.lock().expect("recorder state");
                let n = st.next_n;
                st.next_n += 1;
                st.tally.evaluated += 1;
                if !hits[i] && e.row.error.is_some() {
                    st.tally.failed += 1;
                }
                n
            };
            if !hits[i] {
                self.append(n, &e);
            }
            answered.push(e);
        }
        answered
    }

    fn rank_by(&self) -> RankBy {
        self.inner.rank_by()
    }
}

#[path = "trials_tests.rs"]
#[cfg(test)]
mod trials_tests;
