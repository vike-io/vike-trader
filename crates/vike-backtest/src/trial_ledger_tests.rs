use super::*;
use serde_json::json;

fn an_identity() -> SearchIdentity {
    SearchIdentity {
        profile_fnv1a64: "0123456789abcdef".to_string(),
        profile_name: Some("sweep-demo".to_string()),
        store: "/proj/market_data/hist".to_string(),
        store_data: "series bar demo BTCUSDT - 1h commits 2 c-one c-two\n".to_string(),
        build: Some("abc1234".to_string()),
        method: "tpe".to_string(),
        rank_by: "sharpe".to_string(),
        seed: Some(7),
        budget: Some(64),
    }
}

fn a_record(n: usize, score: f64) -> TrialRecord {
    TrialRecord {
        n,
        overrides: vec![
            ("fast".to_string(), toml::Value::Integer(8)),
            ("slow".to_string(), toml::Value::Float(34.5)),
        ],
        score,
        metrics: Some(json!({ "sharpe": 1.25, "n_trades": 42 })),
        error: None,
    }
}

fn a_document(overfit: Option<OverfitStats>) -> TrialsDocument {
    TrialsDocument {
        schema: TRIAL_LEDGER_SCHEMA,
        run_id: "1756000000-1-0".to_string(),
        keep_trials: "scalars".to_string(),
        identity: an_identity(),
        evaluated: 2,
        reused: 0,
        failed: 0,
        unreadable: 0,
        superseded: 0,
        trials: vec![a_record(0, 1.5), a_record(1, 0.5)],
        overfit,
    }
}

/// ⚠ **A search that did not opt in writes the document it always wrote.** The statistics are
/// ADDITIVE, so a reader of `report.json` — the `trials` verb, `runs diff`,
/// `crates/vike-backtest/tests/search_persist_cli.rs` — sees no new key at all unless the
/// matrix was actually measured. This is the `skip_serializing_if` half of the field's doc,
/// held as a test rather than as a claim.
#[test]
fn a_search_without_statistics_grows_no_key() {
    let json = serde_json::to_string(&a_document(None)).unwrap();
    assert!(!json.contains("overfit"), "no key when there is nothing to report: {json}");
}

fn some_stats() -> OverfitStats {
    OverfitStats {
        buckets: 512,
        requested_buckets: 512,
        trials: 64,
        excluded: 2,
        splits: 16,
        pbo: Some(0.34),
        effective_n: Some(11.5),
        deflated_sharpe: Some(0.72),
        observed_sharpe: Some(0.081),
        observations: 512,
        verdict: "medium".to_string(),
    }
}

/// …and the `serde(default)` half: a document written before the field existed still reads.
///
/// ⚠ The key is REMOVED from a document that HAD one, and the removal is asserted as a
/// precondition. Starting from `a_document(None)` would have proved nothing — that document
/// never carries the key, so the removal would be a no-op and the test would pass against a
/// REQUIRED field just as happily.
#[test]
fn a_document_written_before_the_field_existed_still_deserializes() {
    let mut value = serde_json::to_value(a_document(Some(some_stats()))).unwrap();
    assert!(
        value.as_object_mut().expect("a JSON object").remove("overfit").is_some(),
        "precondition: there was a key to remove"
    );
    let back: TrialsDocument = serde_json::from_value(value).expect("an older document reads");
    assert!(back.overfit.is_none(), "and reads as ABSENT rather than as a zeroed block");
}

/// ⚠ **The gateable spellings, pinned.** `vike-cli backtest gate --fail-if` resolves a
/// criterion as a DOTTED LEAF PATH into this document, so these four key names ARE the
/// operator-facing surface — renaming one silently breaks every `--fail-if` naming it, with no
/// compile error anywhere, because that verb never mentions this type.
#[test]
fn the_statistics_are_reachable_at_the_paths_a_gate_names() {
    let stats = some_stats();
    let doc = serde_json::to_value(a_document(Some(stats.clone()))).unwrap();
    for (path, want) in [
        ("pbo", 0.34),
        ("effective_n", 11.5),
        ("deflated_sharpe", 0.72),
        ("observed_sharpe", 0.081),
    ] {
        assert_eq!(
            doc["overfit"][path].as_f64(),
            Some(want),
            "`overfit.{path}` is what a --fail-if criterion names"
        );
    }
    assert_eq!(doc["overfit"]["verdict"], "medium");
    let back: TrialsDocument = serde_json::from_value(doc).unwrap();
    assert_eq!(back.overfit.as_ref(), Some(&stats), "and the block round-trips whole");
}

/// An UNCOMPUTABLE statistic is `null`, which `crates/vike-cli/src/cmd/runs/jsondoc.rs`'s
/// `number_at` reports as "carries no finite number" — never `0.0`, which under
/// `vike_analytics::overfit::overfit_verdict`'s thresholds would read as "no overfit".
#[test]
fn an_uncomputable_statistic_is_null_rather_than_zero() {
    let stats = OverfitStats {
        buckets: 16,
        requested_buckets: 512,
        trials: 1,
        excluded: 40,
        splits: 16,
        pbo: None,
        effective_n: Some(1.0),
        deflated_sharpe: None,
        observed_sharpe: None,
        observations: 16,
        verdict: "low".to_string(),
    };
    let doc = serde_json::to_value(a_document(Some(stats))).unwrap();
    assert!(doc["overfit"]["pbo"].is_null(), "null, not 0.0");
    assert!(doc["overfit"]["deflated_sharpe"].is_null());
    assert!(
        doc["overfit"].as_object().unwrap().contains_key("pbo"),
        "the KEY is still present, so a gate names a null leaf rather than a missing one"
    );
}

/// The ledger is APPEND-ONLY and one line per trial — the whole reason a 512-trial TPE search
/// does not rewrite a growing document 512 times.
#[test]
fn trials_append_one_line_each_and_read_back_in_order() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    append_trial(dir, &a_record(0, 1.5)).unwrap();
    append_trial(dir, &a_record(1, 0.5)).unwrap();
    append_trial(dir, &a_record(2, 2.5)).unwrap();

    let text = std::fs::read_to_string(dir.join(TRIALS_FILE)).unwrap();
    assert_eq!(text.lines().count(), 3, "one LINE per trial, not one document: {text:?}");
    assert!(
        !text.contains("\n  "),
        "compact JSON per line — a pretty-printed record would break the format: {text:?}"
    );

    let back = read_trials(dir).unwrap();
    assert!(back.unreadable.is_empty(), "nothing unreadable: {:?}", back.unreadable);
    assert_eq!(
        back.trials.iter().map(|t| t.n).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "EVALUATION order is the file's order and is preserved on the way back"
    );
    assert_eq!(back.trials[0].overrides, a_record(0, 1.5).overrides, "toml values round-trip");
    assert_eq!(back.trials[2].score, 2.5);
}

/// ⚠ `null` means UNRANKABLE and nothing else. `ParamscanRow`'s `ser_opt_score` writes `null` for
/// BOTH a non-finite score and a skipped `None`, which is the ambiguity this document must not
/// inherit — so the field is a plain `f64` whose `null` deserializes back to `NaN`.
#[test]
fn an_unrankable_score_round_trips_as_null_and_comes_back_nan() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    append_trial(dir, &a_record(0, f64::NAN)).unwrap();

    let text = std::fs::read_to_string(dir.join(TRIALS_FILE)).unwrap();
    assert!(text.contains("\"score\":null"), "a NaN score writes null: {text:?}");

    let back = read_trials(dir).unwrap();
    assert!(back.trials[0].score.is_nan(), "…and reads back as NaN, not as a missing field");
}

/// A crashed writer leaves a truncated final line. Losing the whole ledger over it is exactly
/// the failure JSON Lines exists to avoid, so the line is REPORTED and the rest survives.
#[test]
fn a_truncated_final_line_costs_that_line_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    append_trial(dir, &a_record(0, 1.0)).unwrap();
    append_trial(dir, &a_record(1, 2.0)).unwrap();

    let path = dir.join(TRIALS_FILE);
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{\"n\":2,\"overri");
    std::fs::write(&path, text).unwrap();

    let back = read_trials(dir).unwrap();
    assert_eq!(back.trials.len(), 2, "the two whole lines survive");
    assert_eq!(back.unreadable.len(), 1, "the torn one is reported, not swallowed");
    assert_eq!(back.unreadable[0].0, 2, "…by its 0-based LINE index, so it can be found");
}

/// A missing ledger is `Missing`, not an empty success — the same distinction `read_manifest`
/// makes, for the same reason: "no trials yet" and "this run kept none" have different fixes.
#[test]
fn an_absent_ledger_is_missing_rather_than_empty() {
    let root = tempfile::tempdir().unwrap();
    match read_trials(root.path()) {
        Err(RunReadError::Missing { path }) => {
            assert!(path.ends_with(TRIALS_FILE), "names the file it looked for: {path:?}");
        }
        other => panic!("expected Missing, got {other:?}"),
    }
}

/// A re-evaluated trial appends a SECOND line carrying the same `n` (a resume re-runs anything
/// its warm cache missed). Append order decides: the LAST line for an `n` is the answer.
#[test]
fn a_repeated_n_is_superseded_by_the_later_line() {
    let first = a_record(1, 1.0);
    let mut second = a_record(1, 9.0);
    second.metrics = Some(json!({ "sharpe": 9.0 }));
    let (kept, superseded) = latest_by_n(vec![a_record(0, 0.5), first, second]);

    assert_eq!(superseded, 1, "one line was superseded and the count says so");
    assert_eq!(kept.len(), 2);
    assert_eq!(kept.iter().map(|t| t.n).collect::<Vec<_>>(), vec![0, 1], "sorted by n");
    assert_eq!(kept[1].score, 9.0, "the LATER line wins");
}

/// The header is written BEFORE the search runs, which is what an interrupted run has and a
/// manifest (written last) does not.
#[test]
fn a_search_header_round_trips() {
    let root = tempfile::tempdir().unwrap();
    let header = SearchHeader {
        schema: TRIAL_LEDGER_SCHEMA,
        run_id: "1756000000-1-0".to_string(),
        keep_trials: "scalars".to_string(),
        trials_file: TRIALS_FILE.to_string(),
        identity: an_identity(),
        resumed_from: None,
    };
    write_search_header(root.path(), &header).unwrap();
    let back = read_search_header(root.path()).unwrap();
    assert_eq!(back, header);
}

/// Every field of the identity is compared, and a difference NAMES itself — a resume that
/// refused with a bare "the search changed" would leave an operator guessing which knob.
#[test]
fn an_identity_difference_names_the_field() {
    let a = an_identity();
    let mut b = an_identity();
    b.seed = Some(8);
    let diffs = a.differences(&b);
    assert_eq!(diffs.len(), 1, "one field differs: {diffs:?}");
    assert!(diffs[0].contains("seed") && diffs[0].contains('7') && diffs[0].contains('8'));
    assert!(a.differences(&an_identity()).is_empty(), "and an equal identity differs in nothing");
}

/// ⚠ An UNREADABLE profile can never match anything, including another unreadable one. Without
/// this, two runs whose profile could not be re-read would compare EQUAL and a resume would
/// reuse trials from a search it cannot prove was the same.
#[test]
fn an_unreadable_profile_hash_never_matches() {
    let mut a = an_identity();
    let mut b = an_identity();
    a.profile_fnv1a64 = PROFILE_UNREADABLE.to_string();
    b.profile_fnv1a64 = PROFILE_UNREADABLE.to_string();
    assert!(!a.differences(&b).is_empty(), "unreadable never equals unreadable");
}

/// ⚠ **THE REVERT-PROOF for the data witness.** Drop `store_data` from [`SearchIdentity`] or
/// from [`SearchIdentity::differences`] and this test goes red: two searches over the SAME
/// profile, store PATH, method, seed and budget, differing only in what the store held, would
/// compare EQUAL — which is the silent wrong answer this type's doc describes. The refusal must
/// also SAY it was the data, because "the search changed" sends an operator to the wrong knob.
#[test]
fn a_store_whose_contents_moved_is_a_named_difference() {
    let before = an_identity();
    let mut after = an_identity();
    after.store_data.push_str("series bar demo ETHUSDT - 1h commits 1 c-three\n");

    assert_eq!(before.store, after.store, "the store PATH is identical — only the data moved");
    let diffs = before.differences(&after);
    assert_eq!(diffs.len(), 1, "exactly one field differs: {diffs:?}");
    assert!(diffs[0].starts_with("data:"), "…and it is NAMED as the data: {diffs:?}");
    assert!(
        diffs[0].contains("backfill") || diffs[0].contains("re-fetch"),
        "the message says what MOVES a store, so an operator knows what to look for: {diffs:?}"
    );
    assert!(
        before.differences(&an_identity()).is_empty(),
        "and an unchanged store is not a difference — otherwise no resume could ever happen"
    );
}

/// ⚠ An UNREADABLE data witness can never match anything, including another unreadable one —
/// the same rule as [`PROFILE_UNREADABLE`] and for the same reason. A store that could not be
/// inventoried is evidence of nothing, and two of them are not evidence of sameness.
#[test]
fn an_unreadable_data_witness_never_matches() {
    let mut a = an_identity();
    let mut b = an_identity();
    a.store_data = DATA_UNREADABLE.to_string();
    b.store_data = DATA_UNREADABLE.to_string();
    assert!(!a.differences(&b).is_empty(), "unreadable never equals unreadable");
    assert!(a.differences(&b)[0].starts_with("data:"), "and it is named: {:?}", a.differences(&b));
}

/// The BINARY is an input to every score — a pull that changes fill logic, sizing or fees makes
/// a cached score an answer the current engine would not give. `unset` on both sides is the
/// standalone engine's ordinary state and must NOT be a difference; `unset` against a real
/// commit must be.
#[test]
fn a_different_build_is_a_difference_and_two_unknown_builds_are_not() {
    let mut newer = an_identity();
    newer.build = Some("def5678".to_string());
    let diffs = an_identity().differences(&newer);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("build") && diffs[0].contains("abc1234"), "{diffs:?}");

    let mut a = an_identity();
    let mut b = an_identity();
    a.build = None;
    b.build = None;
    assert!(
        a.differences(&b).is_empty(),
        "a binary that cannot name its build contributes NO witness — making that a refusal \
             would make the standalone engine unable to resume at all"
    );
    assert!(!a.differences(&an_identity()).is_empty(), "…but unset against a commit differs");
}

/// The hash is a value, so it is pinned as one. FNV-1a 64 over the empty input is its offset
/// basis, and one byte of change moves it.
#[test]
fn the_profile_hash_is_stable_and_sensitive() {
    assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325", "the FNV-1a 64 offset basis");
    assert_ne!(fnv1a64_hex(b"cash = 10000.0"), fnv1a64_hex(b"cash = 10001.0"));
    assert_eq!(fnv1a64_hex(b"abc"), fnv1a64_hex(b"abc"), "and it is a function of the bytes");
}

/// ⚠ The two file names this module adds to a run directory must not collide with the ones
/// `vike_model::runs` writes — that module publishes `RESERVED_FILES` so a producer with
/// artifacts of its own can check against ONE roster rather than a list it maintains itself,
/// and a search parent is exactly such a producer.
#[test]
fn the_search_documents_do_not_collide_with_the_run_records_own_files() {
    for name in [TRIALS_FILE, SEARCH_FILE] {
        assert!(
            !vike_model::runs::RESERVED_FILES.contains(&name),
            "{name} collides with a file the run record itself writes"
        );
    }
}
