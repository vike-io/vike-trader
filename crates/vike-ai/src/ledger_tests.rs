use super::*;

fn trial(i: usize, accepted: bool) -> Trial {
    Trial {
        ts_ms: 1_000 + i as i64,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        code: format!("code{i}"),
        explanation: format!("try {i}"),
        accepted,
        attempts: 1,
        problems: if accepted { vec![] } else { vec!["did not trade".into()] },
        oos_sharpe: i as f64 / 10.0,
        n_trades: i,
        oos_equity_curve: vec![100.0, 101.0, 100.5],
        deflated_sharpe: 0.0,
        // Exact-division form on purpose: `i as f64 / 100.0` rounds once to the nearest f64,
        // so the fixture's values are the same bits as the `0.01`/`0.03` literals the
        // assertions below compare against (`0.01 * 3.0` is NOT bit-equal to `0.03`).
        sr_per_obs: i as f64 / 100.0,
    }
}

#[test]
fn trial_ledger_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(TRIALS_FILE);
    let ledger = TrialLedger { trials: vec![trial(1, true), trial(2, false)] };
    save_trials(&ledger, &p);
    assert_eq!(load_trials(&p), ledger, "disk round-trip is field-for-field lossless");
}

#[test]
fn learnings_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(LEARNINGS_FILE);
    let store = LearningStore {
        learnings: vec![
            Learning { ts_ms: 1, scope: LearningScope::Global, text: "g".into() },
            Learning {
                ts_ms: 2,
                scope: LearningScope::Slice {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                },
                text: "s".into(),
            },
        ],
    };
    save_learnings(&store, &p);
    assert_eq!(load_learnings(&p), store);
}

#[test]
fn trial_ledger_missing_file_is_default() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("nope.json");
    assert_eq!(load_trials(&p), TrialLedger::default());
    assert_eq!(load_learnings(&p), LearningStore::default());
}

#[test]
fn trial_ledger_corrupt_file_is_default() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(TRIALS_FILE);
    std::fs::write(&p, "{ not valid json ]").unwrap();
    assert_eq!(load_trials(&p), TrialLedger::default(), "corrupt file must not panic or error");
    // …and the loop must still be able to append over it.
    append_trial(&p, trial(7, true));
    assert_eq!(load_trials(&p).trials.len(), 1);
}

#[test]
fn trial_ledger_prunes_to_cap() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(TRIALS_FILE);
    let mut ledger = TrialLedger { trials: (0..600).map(|i| trial(i, false)).collect() };
    prune(&mut ledger);
    assert_eq!(ledger.trials.len(), MAX_TRIALS);
    // Newest kept, oldest dropped: 600 in, the surviving window is [100, 600).
    assert_eq!(ledger.trials.first().unwrap().code, "code100");
    assert_eq!(ledger.trials.last().unwrap().code, "code599");
    save_trials(&ledger, &p);
    assert_eq!(load_trials(&p).trials.len(), MAX_TRIALS);
}

#[test]
fn trial_ledger_prunes_curves() {
    // 120 accepted trials, each with a curve -> only the newest MAX_CURVE_ROWS keep one.
    let mut ledger = TrialLedger { trials: (0..120).map(|i| trial(i, true)).collect() };
    prune(&mut ledger);
    let with_curve: Vec<&Trial> =
        ledger.trials.iter().filter(|t| !t.oos_equity_curve.is_empty()).collect();
    assert_eq!(with_curve.len(), MAX_CURVE_ROWS);
    assert_eq!(with_curve.first().unwrap().code, format!("code{}", 120 - MAX_CURVE_ROWS));
    assert_eq!(with_curve.last().unwrap().code, "code119");
    // The curve-pruned rows keep every scalar — crucially `sr_per_obs`, so they still count
    // toward a later call's deflation trial set.
    let pruned = &ledger.trials[3]; // i=3: non-zero scalars, so this is not a vacuous check
    assert!(pruned.oos_equity_curve.is_empty());
    assert_eq!(pruned.sr_per_obs, 0.03);
    assert_eq!(pruned.n_trades, 3);
    assert_eq!(ledger.prior_sharpes_for("binance", "BTCUSDT", "1m").len(), 120);
}

#[test]
fn prune_counts_only_accepted_rows_toward_the_curve_budget() {
    // Interleaved accept/reject: the curve budget is spent on ACCEPTED rows only, so a run of
    // rejected trials at the tail must not consume it.
    let mut ledger = TrialLedger { trials: (0..200).map(|i| trial(i, i % 2 == 0)).collect() };
    prune(&mut ledger);
    assert_eq!(
        ledger.trials.iter().filter(|t| !t.oos_equity_curve.is_empty()).count(),
        MAX_CURVE_ROWS
    );
    assert!(ledger.trials.iter().all(|t| t.oos_equity_curve.is_empty() || t.accepted));
}

#[test]
fn learnings_cap() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(LEARNINGS_FILE);
    for i in 0..(MAX_LEARNINGS + 25) {
        append_learning(
            &p,
            Learning { ts_ms: i as i64, scope: LearningScope::Global, text: format!("n{i}") },
        );
    }
    let store = load_learnings(&p);
    assert_eq!(store.learnings.len(), MAX_LEARNINGS);
    assert_eq!(store.learnings.first().unwrap().text, "n25", "oldest dropped first");
    assert_eq!(store.learnings.last().unwrap().text, format!("n{}", MAX_LEARNINGS + 24));
}

#[test]
fn learning_text_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(LEARNINGS_FILE);
    // A multibyte string longer than the cap: truncation must land on a char boundary, so the
    // stored text is still valid UTF-8 and strictly shorter than the input.
    let text = "é".repeat(MAX_LEARNING_TEXT); // 2 bytes each -> 2 KiB
    append_learning(&p, Learning { ts_ms: 1, scope: LearningScope::Global, text });
    let stored = &load_learnings(&p).learnings[0].text;
    assert!(stored.len() <= MAX_LEARNING_TEXT);
    assert_eq!(stored.chars().count(), MAX_LEARNING_TEXT / 2, "cut on a char boundary");
    assert!(stored.chars().all(|c| c == 'é'));
    // A short note is stored verbatim.
    assert_eq!(truncate_text("short", MAX_LEARNING_TEXT), "short");
}

#[test]
fn prior_sharpes_are_slice_scoped_and_accepted_only() {
    let mut ledger = TrialLedger::default();
    ledger.trials.push(trial(1, true));
    ledger.trials.push(trial(2, false)); // rejected -> excluded
    let mut other = trial(3, true);
    other.symbol = "ETHUSDT".into(); // different slice -> excluded
    ledger.trials.push(other);
    let mut nan = trial(4, true);
    nan.sr_per_obs = f64::NAN; // non-finite -> excluded (would poison sample_variance)
    ledger.trials.push(nan);
    assert_eq!(ledger.prior_sharpes_for("binance", "BTCUSDT", "1m"), vec![0.01]);
}

/// The guard's own contract, pinned directly: adding the `warn` must not have turned it into a
/// filter that lets a NaN through (which would brick the file) or that perturbs a finite value
/// (`sr_per_obs` feeds deflation, so a rounded pass-through is a silent statistical change).
#[test]
fn finite_or_zero_replaces_non_finite_and_passes_finite_through_unchanged() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(finite_or_zero(bad), 0.0, "{bad} must still be substituted, not stored");
    }
    for ok in [0.0, -0.0, 1.25, -3.5, f64::MIN, f64::MAX, f64::MIN_POSITIVE] {
        // Bit equality, not `==`: `-0.0 == 0.0` would make the pass-through check vacuous.
        assert_eq!(finite_or_zero(ok).to_bits(), ok.to_bits(), "{ok} passes through verbatim");
    }
}

/// The never-brick guard: a run whose statistics came back non-finite must still produce a
/// ledger that RELOADS. `serde_json` writes NaN as `null`, and `null` does not deserialize
/// back into an `f64` — so without `finite_or_zero` one such run would silently discard every
/// trial ever recorded on the next load.
#[test]
fn non_finite_statistics_do_not_brick_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(TRIALS_FILE);
    let r = AgentResult {
        accepted: true,
        attempts: 1,
        oos_sharpe: f64::NAN,
        deflated_sharpe: f64::INFINITY,
        n_trades: 3,
        oos_equity_curve: vec![100.0, f64::NAN, 101.0],
        ..Default::default()
    };
    append_trial(&p, Trial::from_result(42, "binance", "BTCUSDT", "1m", &r));
    let back = load_trials(&p);
    assert_eq!(back.trials.len(), 1, "the file must reload, not parse-fail into Default");
    let t = &back.trials[0];
    assert_eq!(t.oos_sharpe, 0.0);
    assert_eq!(t.deflated_sharpe, 0.0);
    assert_eq!(t.sr_per_obs, 0.0);
    assert!(t.oos_equity_curve.is_empty(), "a curve with a NaN carries no usable moments");
    assert_eq!(t.n_trades, 3, "the non-float scalars are untouched");
}

#[test]
fn summary_is_empty_without_matching_trials_or_learnings() {
    let s = prior_work_summary(
        &TrialLedger::default(),
        &LearningStore::default(),
        "binance",
        "BTCUSDT",
        "1m",
        PROMPT_TOP_K,
    );
    assert!(s.is_empty(), "nothing to say -> the prompt stays byte-identical");
}

#[test]
fn summary_names_sharpes_notes_and_rejection_reasons() {
    let mut ledger = TrialLedger::default();
    let mut good = trial(1, true);
    good.oos_sharpe = 1.25;
    good.n_trades = 7;
    good.explanation = "sma cross 5/20".into();
    ledger.trials.push(good);
    ledger.trials.push(trial(2, false));
    let store = LearningStore {
        learnings: vec![Learning {
            ts_ms: 1,
            scope: LearningScope::Global,
            text: "spreads widen 00:00-04:00 UTC".into(),
        }],
    };
    let s = prior_work_summary(&ledger, &store, "binance", "BTCUSDT", "1m", PROMPT_TOP_K);
    assert!(s.contains("1.25"), "the accepted trial's OOS Sharpe: {s}");
    assert!(s.contains("sma cross 5/20"));
    assert!(s.contains("did not trade"), "the rejection reason: {s}");
    assert!(s.contains("spreads widen 00:00-04:00 UTC"), "the global learning: {s}");
    // Accepted ranks above rejected.
    assert!(s.find("ACCEPTED").unwrap() < s.find("REJECTED").unwrap());
}

#[test]
fn summary_hides_another_slices_learning() {
    let store = LearningStore {
        learnings: vec![Learning {
            ts_ms: 1,
            scope: LearningScope::Slice {
                venue: "binance".into(),
                symbol: "ETHUSDT".into(),
                interval: "1m".into(),
            },
            text: "eth-only note".into(),
        }],
    };
    let mut ledger = TrialLedger::default();
    ledger.trials.push(trial(1, true));
    let s = prior_work_summary(&ledger, &store, "binance", "BTCUSDT", "1m", PROMPT_TOP_K);
    assert!(!s.contains("eth-only note"));
}

#[test]
fn summary_caps_at_top_k() {
    let ledger = TrialLedger { trials: (0..20).map(|i| trial(i, true)).collect() };
    let s = prior_work_summary(
        &ledger,
        &LearningStore::default(),
        "binance",
        "BTCUSDT",
        "1m",
        PROMPT_TOP_K,
    );
    assert_eq!(s.matches("- ACCEPTED").count(), PROMPT_TOP_K);
}
