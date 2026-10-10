//! The trial LEDGER's writer, and `--resume`'s warm cache — one type, because they are one seam.
//!
//! # What this is
//!
//! [`TrialRecorder`] is a [`PointEvaluator`] that WRAPS another one. A searcher drives it exactly
//! as it drives `crate::harness::StoreEvaluator`, and it does two things the wrapped evaluator does
//! not: it appends one [`crate::trial_ledger::TrialRecord`] per evaluation to the parent run's
//! ledger, and it answers from a warm cache instead of evaluating when it has seen a candidate
//! before.
//!
//! The DOCUMENTS it writes live in `crate::trial_ledger`, outside the search family entirely; this
//! module is the half that needs [`Candidate`]/[`Evaluated`] and therefore sits in it.
//!
//! # Why a decorator rather than a hook in `optimize`
//!
//! `crate::harness::optimize::optimize` is the ONE door and its doc says nothing may call
//! `Optimizer::search` directly, because [`PointEvaluator::evaluate`]'s infallibility rests on
//! `require_overridable_params` having run first. A decorator honours that untouched: the same
//! preflight, the same `accepts`, the same loop. And it costs NO signature change anywhere — a
//! field on `crate::harness::SearchOutcome` would be 6 struct-literal edits, a field on
//! `crate::harness::ParamscanRow` 19, and a new required `Optimizer` method 5 impls.
//!
//! # Why this is all `--resume` needs
//!
//! Every one of the four methods drives a SEED-DETERMINISTIC loop. Given the same profile, the same
//! store, the same method and the same seed, a searcher proposes the same candidates in the same
//! order — so replaying the loop against a cache that answers with the ORIGINAL scores reproduces
//! the original trajectory exactly, and the only work performed is the evaluations that never
//! happened. Resuming a searcher's INTERNAL state would instead mean serializing
//! `crate::search::SearchTrace` (which derives `Debug, Clone` only), `super::tpe::TpeOptimizer`'s
//! private `Vec<Trial>` and `super::genetic::GeneticSearch`'s local `pop`/`seen` — four methods,
//! five `impl Optimizer` edits, and a new failure mode per method.
//!
//! # ⚠ A FAILED trial is warmed like any other
//!
//! One rule rather than two. A trial that failed is a row whose score is `NaN`, and re-running it
//! is the case a resume most wants to skip. The cost is real and is declared rather than hidden: if
//! the original failures were TRANSIENT (a store that was briefly unreadable), a resume reproduces
//! them. [`WarmTrial::failed`] is carried so the caller's resume line can say how many reused
//! trials had failed, which is what makes that visible instead of silent.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::harness::optimize::{Candidate, Evaluated, PointEvaluator};
use crate::harness::sweep::{ParamscanRow, RankBy};
use crate::trial_ledger::TrialRecord;

/// What a REUSED row's `error` says.
///
/// ⚠ A reused row is not rendered on any path this workspace ships: `--resume` prints the LEDGER
/// rather than the `ParamscanReport` (`crates/vike-backtest/src/backtest_cli.rs`'s resume arm carries
/// that argument), precisely because `super::sweep::ParamscanReport`'s `Display` branches on
/// `report: None` and would print a reused trial as `FAILED: …`. This string exists so that if some
/// future caller DOES render one, it says what it is instead of `(unknown error)`.
pub const REUSED_TRIAL: &str = "reused from an earlier trial — see `backtest trials <id>`";

/// One warm-cache entry: what the earlier run scored this candidate, and whether it had failed.
///
/// ⚠ The entry carries the SCORE and not the report. A `BacktestReport` could now be rebuilt from a
/// ledger row (that type gained `Deserialize` with the run record), and it is deliberately not:
/// `profit_factor` writes `null` for BOTH of its house sentinels and reads back as `inf`, so a
/// reconstructed row would print a number the original run did not compute. The score is what the
/// SEARCHER steers on and is exact; the report is what the LEDGER already holds verbatim.
#[derive(Debug, Clone, Copy)]
pub struct WarmTrial {
    /// The steering score the earlier run recorded. `NaN` means unrankable.
    pub score: f64,
    /// Whether that trial carried an `error`.
    pub failed: bool,
}

/// What a search SPENT, as the recorder saw it.
#[derive(Debug, Default, Clone, Copy)]
pub struct RecorderTally {
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

/// An exact, total key for a [`Candidate`].
///
/// ⚠ A `Candidate` cannot be a map key on its own: it is `Vec<(String, toml::Value)>` and
/// `toml::Value` derives `PartialEq, Clone, Debug` — no `Eq`, no `Hash`. Floats therefore go in as
/// their BIT PATTERN, which is the same rule `super::genetic`'s `key_of` and `crate::search`'s
/// `key_of` already use for the same reason.
///
/// ORDER-SENSITIVE, deliberately: `super::sweep::profile_with_overrides` applies overrides in
/// order, so two orderings are not provably the same point — and a differing order is a cache MISS,
/// which costs a re-evaluation rather than producing a wrong answer. Every searcher in this tree
/// emits its overrides in a stable order, so the miss is unreachable in practice.
///
/// ⚠ `0.0` and `-0.0` compare EQUAL under `==` and key DIFFERENTLY here. Same direction: a miss.
///
/// ⚠ **The two separators are ESCAPED, and the doc says so because the float, order and `-0.0`
/// cases are argued above and this one was silently omitted.** `=` and `;` are the segment
/// boundaries, so an unescaped string axis carrying either could forge a neighbouring segment and
/// two DIFFERENT candidates could key the same — a cache HIT answering with a score computed for
/// another point, which is the one direction this key must never fail in. Reachability is
/// effectively nil (one fixed axis set per search, and a ledger never crosses runs), so this is a
/// property held by construction rather than a bug that was hit; `\` escapes itself, `=` and `;`,
/// which is what makes the encoding injective.
pub fn candidate_key(c: &Candidate) -> String {
    use std::fmt::Write;
    /// `\` -> `\\`, `=` -> `\e`, `;` -> `\s`. Injective, so no two distinct inputs share an output.
    fn esc(out: &mut String, s: &str) {
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '=' => out.push_str("\\e"),
                ';' => out.push_str("\\s"),
                _ => out.push(ch),
            }
        }
    }
    let mut out = String::with_capacity(c.len() * 24);
    for (k, v) in c {
        esc(&mut out, k);
        out.push('=');
        match v {
            toml::Value::Float(f) => {
                let _ = write!(out, "f{:016x}", f.to_bits());
            }
            toml::Value::Integer(i) => {
                let _ = write!(out, "i{i}");
            }
            toml::Value::Boolean(b) => {
                let _ = write!(out, "b{b}");
            }
            toml::Value::String(s) => {
                out.push('s');
                esc(&mut out, s);
            }
            other => {
                out.push('o');
                esc(&mut out, &other.to_string());
            }
        }
        out.push(';');
    }
    out
}

/// Build the warm cache from a ledger's records. A repeated key keeps the LAST record, matching
/// `crate::trial_ledger::latest_by_n`'s rule — the caller normally passes an already-collapsed
/// slice.
pub fn warm_from(trials: &[TrialRecord]) -> HashMap<String, WarmTrial> {
    trials
        .iter()
        .map(|t| {
            (candidate_key(&t.overrides), WarmTrial { score: t.score, failed: t.error.is_some() })
        })
        .collect()
}

struct RecorderState {
    next_n: usize,
    tally: RecorderTally,
    first_write_error: Option<String>,
}

/// See this module's doc.
pub struct TrialRecorder<'a> {
    inner: &'a dyn PointEvaluator,
    /// The parent run directory the ledger is appended into. `None` under `--keep-trials none`.
    ledger: Option<PathBuf>,
    warm: HashMap<String, WarmTrial>,
    state: Mutex<RecorderState>,
}

impl<'a> TrialRecorder<'a> {
    /// Wrap `inner`, appending into `ledger` (when there is one) and answering from `warm`.
    pub fn new(
        inner: &'a dyn PointEvaluator,
        ledger: Option<PathBuf>,
        warm: HashMap<String, WarmTrial>,
    ) -> Self {
        TrialRecorder {
            inner,
            ledger,
            warm,
            state: Mutex::new(RecorderState {
                next_n: 0,
                tally: RecorderTally::default(),
                first_write_error: None,
            }),
        }
    }

    /// What the search has spent so far. `Copy`, so a caller reads it once and passes it on.
    pub fn tally(&self) -> RecorderTally {
        self.state.lock().expect("recorder tally").tally
    }

    /// The FIRST ledger-write failure, if any. One message rather than one per trial: a directory
    /// that stopped accepting writes fails every append, and an operator needs the reason once.
    pub fn first_write_error(&self) -> Option<String> {
        self.state.lock().expect("recorder tally").first_write_error.clone()
    }

    fn append(&self, n: usize, e: &Evaluated) {
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
/// than a report (see [`WarmTrial`]), and [`REUSED_TRIAL`] rather than `None` so the row is never
/// rendered as `(unknown error)`.
fn reused_row(overrides: Candidate, score: f64) -> Evaluated {
    Evaluated {
        row: ParamscanRow {
            overrides,
            report: None,
            error: Some(REUSED_TRIAL.to_string()),
            score: Some(score),
        },
        score,
    }
}

impl PointEvaluator for TrialRecorder<'_> {
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
        // ── 1. split, keeping every candidate's SLOT ────────────────────────────────────────────
        let mut out: Vec<Option<Evaluated>> = Vec::with_capacity(batch.len());
        let mut misses: Vec<Candidate> = Vec::new();
        let mut miss_slots: Vec<usize> = Vec::new();
        let mut hits: Vec<bool> = Vec::with_capacity(batch.len());
        for (i, c) in batch.iter().enumerate() {
            match self.warm.get(&candidate_key(c)) {
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
        // one. An ALL-HIT batch calls nothing: handing `StoreEvaluator` an empty vector is a shape
        // worth not creating.
        let fresh = if misses.is_empty() { Vec::new() } else { self.inner.evaluate(misses) };
        assert_eq!(
            fresh.len(),
            miss_slots.len(),
            "a PointEvaluator must answer one Evaluated per candidate — see its doc"
        );
        for (slot, e) in miss_slots.iter().zip(fresh) {
            out[*slot] = Some(e);
        }

        // ── 3. assign n IN INPUT ORDER, and append only the misses ──────────────────────────────
        let mut answered: Vec<Evaluated> = Vec::with_capacity(out.len());
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
