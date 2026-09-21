//! The trial LEDGER's writer, and `--resume`'s warm cache — one type, because they are one seam.
//!
//! # What this is
//!
//! [`TrialRecorder`] is a [`PointEvaluator`] that WRAPS another one. A searcher drives it exactly
//! as it drives `super::StoreEvaluator`, and it does two things the wrapped evaluator does not: it
//! appends one [`crate::trial_ledger::TrialRecord`] per evaluation to the parent run's ledger, and
//! it answers from a warm cache instead of evaluating when it has seen a candidate before.
//!
//! The DOCUMENTS it writes live in `crate::trial_ledger`, outside the `hist-replay` gate; this
//! module is the half that needs [`Candidate`]/[`Evaluated`] and therefore sits under it.
//!
//! # Why a decorator rather than a hook in `optimize`
//!
//! `super::optimize::optimize` is the ONE door and its doc says nothing may call
//! `Optimizer::search` directly, because [`PointEvaluator::evaluate`]'s infallibility rests on
//! `require_overridable_params` having run first. A decorator honours that untouched: the same
//! preflight, the same `accepts`, the same loop. And it costs NO signature change anywhere — a
//! field on `super::SearchOutcome` would be 6 struct-literal edits, a field on `super::ParamscanRow`
//! 19, and a new required `Optimizer` method 5 impls.
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

use super::optimize::{Candidate, Evaluated, PointEvaluator};
use super::sweep::{ParamscanRow, RankBy};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::sweep::RankMetric;

    /// A `PointEvaluator` that scores a candidate by its first numeric value and RECORDS the
    /// batches it was handed — so a test can assert what the decorator passed DOWN, not just what
    /// came back up.
    struct Spy {
        seen: std::sync::Mutex<Vec<Vec<Candidate>>>,
    }

    impl Spy {
        fn new() -> Self {
            Spy { seen: std::sync::Mutex::new(Vec::new()) }
        }
        fn batches(&self) -> Vec<Vec<Candidate>> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl PointEvaluator for Spy {
        fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
            self.seen.lock().unwrap().push(batch.clone());
            batch
                .into_iter()
                .map(|c| {
                    let score = match c.first().map(|(_, v)| v) {
                        Some(toml::Value::Integer(i)) => *i as f64,
                        Some(toml::Value::Float(f)) => *f,
                        _ => f64::NAN,
                    };
                    Evaluated {
                        row: ParamscanRow {
                            overrides: c,
                            report: None,
                            error: Some("empty store".to_string()),
                            score: Some(score),
                        },
                        score,
                    }
                })
                .collect()
        }

        fn rank_by(&self) -> RankBy {
            RankBy::Metric(RankMetric::Sharpe)
        }
    }

    fn cand(k: &str, v: i64) -> Candidate {
        vec![(k.to_string(), toml::Value::Integer(v))]
    }

    /// ⚠ THE PROPERTY EVERYTHING ELSE RESTS ON. With an EMPTY warm cache the decorator must hand
    /// the inner evaluator the SAME vector it was given and return its answer untouched — same
    /// length, same order, same scores. A recorder that reordered, deduped or re-batched would
    /// break the byte-identical parallel/sequential property that `sort_scored_rows` being a STABLE
    /// sort over evaluation order provides, and no sweep test would say so.
    #[test]
    fn an_empty_cache_passes_the_batch_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new();
        let rec = TrialRecorder::new(&spy, Some(dir.path().to_path_buf()), HashMap::new());

        let batch = vec![cand("a", 3), cand("a", 1), cand("a", 2)];
        let out = rec.evaluate(batch.clone());

        assert_eq!(spy.batches(), vec![batch.clone()], "the INNER batch is the original, verbatim");
        assert_eq!(out.len(), 3);
        assert_eq!(
            out.iter().map(|e| e.score).collect::<Vec<_>>(),
            vec![3.0, 1.0, 2.0],
            "INPUT order, not score order — the trajectory is what a SearchOutcome carries"
        );
        let t = rec.tally();
        assert_eq!((t.evaluated, t.reused), (3, 0));
    }

    /// `n` is assigned in batch INPUT order and is the ledger's line index.
    #[test]
    fn n_is_the_evaluation_index_across_batches() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new();
        let rec = TrialRecorder::new(&spy, Some(dir.path().to_path_buf()), HashMap::new());

        rec.evaluate(vec![cand("a", 1), cand("a", 2)]);
        rec.evaluate(vec![cand("a", 3)]);

        let back = crate::trial_ledger::read_trials(dir.path()).unwrap();
        assert_eq!(
            back.trials.iter().map(|t| t.n).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "one counter across every batch, in input order"
        );
        assert_eq!(back.trials[2].score, 3.0);
    }

    /// A CACHE HIT costs no evaluation, still consumes an `n`, and writes NO new line — so a
    /// resumed search's ledger ends up identical to the uninterrupted one's.
    #[test]
    fn a_hit_is_not_evaluated_not_appended_and_still_consumes_its_index() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new();
        let mut warm = HashMap::new();
        warm.insert(candidate_key(&cand("a", 1)), WarmTrial { score: 1.0, failed: true });

        let rec = TrialRecorder::new(&spy, Some(dir.path().to_path_buf()), warm);
        let out = rec.evaluate(vec![cand("a", 1), cand("a", 2)]);

        assert_eq!(
            spy.batches(),
            vec![vec![cand("a", 2)]],
            "only the MISS reaches the inner evaluator"
        );
        assert_eq!(out.len(), 2, "…and the caller still gets one answer per candidate");
        assert_eq!(out[0].score, 1.0, "the hit answers with its CACHED score, in its own slot");
        assert_eq!(out[1].score, 2.0);
        assert_eq!(
            out[0].row.error.as_deref(),
            Some(REUSED_TRIAL),
            "a reused row says what it is rather than reading as `(unknown error)`"
        );

        let back = crate::trial_ledger::read_trials(dir.path()).unwrap();
        assert_eq!(back.trials.len(), 1, "the hit appended nothing");
        assert_eq!(
            back.trials[0].n, 1,
            "and the MISS kept index 1 — a hit still consumes an index"
        );

        let t = rec.tally();
        assert_eq!((t.evaluated, t.reused, t.reused_failed), (2, 1, 1));
    }

    /// ⚠ An ALL-HIT batch must not call the inner evaluator at all. Without the guard it would be
    /// called with an EMPTY vector, and `StoreEvaluator` would answer with an empty vector that the
    /// reassembly's length assertion then has to tolerate — a shape worth refusing at the source.
    #[test]
    fn an_all_hit_batch_never_reaches_the_inner_evaluator() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new();
        let mut warm = HashMap::new();
        warm.insert(candidate_key(&cand("a", 1)), WarmTrial { score: 5.0, failed: false });
        let rec = TrialRecorder::new(&spy, Some(dir.path().to_path_buf()), warm);

        let out = rec.evaluate(vec![cand("a", 1)]);
        assert!(spy.batches().is_empty(), "nothing was evaluated");
        assert_eq!(out[0].score, 5.0);
    }

    /// `--keep-trials none`: no ledger path, so no file — and everything else is unchanged.
    #[test]
    fn keeping_no_trials_writes_no_file_and_still_evaluates() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new();
        let rec = TrialRecorder::new(&spy, None, HashMap::new());
        let out = rec.evaluate(vec![cand("a", 1)]);

        assert_eq!(out.len(), 1);
        assert!(
            !dir.path().join(crate::trial_ledger::TRIALS_FILE).exists(),
            "no ledger was written"
        );
        assert_eq!(rec.tally().evaluated, 1);
    }

    /// A ledger that cannot be written is COUNTED and named ONCE, and the search runs on. Persisting
    /// is additive; a directory that stopped accepting writes must not cost the run.
    #[test]
    fn a_ledger_that_cannot_be_written_is_counted_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        let spy = Spy::new();
        let rec = TrialRecorder::new(&spy, Some(missing), HashMap::new());

        let out = rec.evaluate(vec![cand("a", 1), cand("a", 2)]);
        assert_eq!(out.len(), 2, "the run is unaffected");
        let t = rec.tally();
        assert_eq!(t.write_failures, 2);
        assert!(
            rec.first_write_error().is_some(),
            "…and the FIRST failure is kept so the caller names it once rather than twice per trial"
        );
    }

    /// The key is EXACT and ORDER-SENSITIVE. `toml::Value` derives `PartialEq, Clone, Debug` and
    /// NOT `Eq`/`Hash`, so a `Candidate` cannot be a map key — floats go in as their bit pattern,
    /// which is the same rule `crates/vike-backtest/src/harness/genetic.rs`'s `key_of` and
    /// `crates/vike-backtest/src/search.rs`'s `key_of` already use.
    #[test]
    fn the_candidate_key_separates_what_must_not_collide() {
        let f = |x: f64| vec![("a".to_string(), toml::Value::Float(x))];
        assert_eq!(candidate_key(&f(1.0)), candidate_key(&f(1.0)));
        assert_ne!(candidate_key(&f(1.0)), candidate_key(&f(1.0 + f64::EPSILON)), "one ULP apart");
        assert_ne!(
            candidate_key(&f(0.0)),
            candidate_key(&f(-0.0)),
            "⚠ 0.0 and -0.0 compare EQUAL but key DIFFERENTLY — a miss, which re-evaluates, is the \
             safe direction; a hit would answer with a score computed for the other value"
        );
        assert_ne!(
            candidate_key(&vec![("a".to_string(), toml::Value::Integer(1))]),
            candidate_key(&f(1.0)),
            "an integer axis and a float axis are different spaces"
        );
        assert_ne!(
            candidate_key(&vec![
                ("a".to_string(), toml::Value::Integer(1)),
                ("b".to_string(), toml::Value::Integer(2))
            ]),
            candidate_key(&vec![
                ("b".to_string(), toml::Value::Integer(2)),
                ("a".to_string(), toml::Value::Integer(1))
            ]),
            "ORDER-sensitive on purpose: overrides are applied in order by `profile_with_overrides`, \
             and a differing order is a cache MISS rather than a wrong hit"
        );

        // ⚠ A STRING axis carrying the separators must not be able to forge a neighbour's segment.
        // Unescaped, `a="x;b=sy"` and `a="x", b="y"` both render `a=sx;b=sy;` — two different
        // candidates keying the SAME, which is a cache HIT answering with another point's score.
        let s = |k: &str, v: &str| (k.to_string(), toml::Value::String(v.to_string()));
        assert_ne!(
            candidate_key(&vec![s("a", "x;b=sy")]),
            candidate_key(&vec![s("a", "x"), s("b", "y")]),
            "a string axis cannot forge a segment boundary"
        );
        assert_ne!(
            candidate_key(&vec![s("a=b", "c")]),
            candidate_key(&vec![s("a", "b=c")]),
            "…nor move one, by carrying the separator in the KEY"
        );
        assert_ne!(
            candidate_key(&vec![s("a", "x\\e")]),
            candidate_key(&vec![s("a", "x=")]),
            "and the escape itself is escaped, so the encoding stays injective"
        );
    }

    /// The warm cache is built from the ledger, failed trials INCLUDED — one rule, not two. The
    /// `failed` flag is carried so the resume line can say how many reused trials had failed, which
    /// is what makes a resume after a store outage visible rather than silent.
    #[test]
    fn the_warm_cache_carries_failures_and_flags_them() {
        let ok = TrialRecord {
            n: 0,
            overrides: cand("a", 1),
            score: 1.0,
            metrics: Some(serde_json::json!({ "sharpe": 1.0 })),
            error: None,
        };
        let bad = TrialRecord {
            n: 1,
            overrides: cand("a", 2),
            score: f64::NAN,
            metrics: None,
            error: Some("no data".to_string()),
        };
        let warm = warm_from(&[ok, bad]);
        assert_eq!(warm.len(), 2, "both are reusable — see this module's doc");
        assert!(!warm[&candidate_key(&cand("a", 1))].failed);
        assert!(warm[&candidate_key(&cand("a", 2))].failed);
        assert!(warm[&candidate_key(&cand("a", 2))].score.is_nan());
    }
}
