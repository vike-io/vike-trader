//! `prior_work_summary`: silent with nothing to say, its ranking, its scoping and its top-k cap.

use super::support::trial;
use crate::ledger::learning::{Learning, LearningScope, LearningStore};
use crate::ledger::summary::{PROMPT_TOP_K, prior_work_summary};
use crate::ledger::trial::TrialLedger;

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
