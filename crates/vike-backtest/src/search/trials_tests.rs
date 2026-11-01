use super::*;
use crate::harness::sweep::RankMetric;
use vike_model::params::ParamValue;

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
                    Some(ParamValue::Int(i)) => *i as f64,
                    Some(ParamValue::Float(f)) => *f,
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
    vec![(k.to_string(), ParamValue::Int(v))]
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
    warm.insert(cand("a", 1), WarmTrial { score: 1.0, failed: true });

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
    assert_eq!(back.trials[0].n, 1, "and the MISS kept index 1 — a hit still consumes an index");

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
    warm.insert(cand("a", 1), WarmTrial { score: 5.0, failed: false });
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
    assert!(!dir.path().join(crate::trial_ledger::TRIALS_FILE).exists(), "no ledger was written");
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

/// A [`Candidate`] is the warm cache's KEY as it stands (`ParamValue` is `Eq + Hash`), and the
/// key is EXACT and ORDER-SENSITIVE — floats compare by their bit pattern, which is the same rule
/// `crates/vike-backtest/src/search/genetic.rs`'s `key_of` and
/// `crates/vike-backtest/src/search.rs`'s `key_of` already use.
#[test]
fn a_candidate_key_separates_what_must_not_collide() {
    use std::collections::HashSet;
    let key = |c: Candidate| -> HashSet<Candidate> { HashSet::from([c]) };
    let f = |x: f64| vec![("a".to_string(), ParamValue::Float(x))];
    assert_eq!(key(f(1.0)), key(f(1.0)));
    assert_ne!(key(f(1.0)), key(f(1.0 + f64::EPSILON)), "one ULP apart");
    assert_ne!(
        key(f(0.0)),
        key(f(-0.0)),
        "⚠ 0.0 and -0.0 compare EQUAL but key DIFFERENTLY — a miss, which re-evaluates, is the \
             safe direction; a hit would answer with a score computed for the other value"
    );
    assert_ne!(
        key(vec![("a".to_string(), ParamValue::Int(1))]),
        key(f(1.0)),
        "an integer axis and a float axis are different spaces"
    );
    assert_ne!(
        key(vec![("a".to_string(), ParamValue::Int(1)), ("b".to_string(), ParamValue::Int(2))]),
        key(vec![("b".to_string(), ParamValue::Int(2)), ("a".to_string(), ParamValue::Int(1))]),
        "ORDER-sensitive on purpose: overrides are applied in order by `profile_with_overrides`, \
             and a differing order is a cache MISS rather than a wrong hit"
    );

    // ⚠ A STRING axis must not be able to forge a neighbour: two different candidates keying the
    // SAME would be a cache HIT answering with another point's score. The retired string key had
    // to ESCAPE its `=`/`;` separators to hold this; the structural key has no separators, and
    // these are the cases that once needed the escape.
    let s = |k: &str, v: &str| (k.to_string(), ParamValue::Str(v.to_string()));
    assert_ne!(
        key(vec![s("a", "x;b=sy")]),
        key(vec![s("a", "x"), s("b", "y")]),
        "a string axis cannot forge a segment boundary"
    );
    assert_ne!(
        key(vec![s("a=b", "c")]),
        key(vec![s("a", "b=c")]),
        "…nor move one, by carrying a separator in the KEY"
    );
    assert_ne!(
        key(vec![s("a", "x\\e")]),
        key(vec![s("a", "x=")]),
        "…nor collide through an escape sequence"
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
    assert!(!warm[&cand("a", 1)].failed);
    assert!(warm[&cand("a", 2)].failed);
    assert!(warm[&cand("a", 2)].score.is_nan());
}
