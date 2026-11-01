//! Ledger grounding: the prompt, the `record_learning` tool's gate, and the best-effort contract.

use serde_json::json;

use super::support::{
    CROSS, CapturingClient, prior_accepted, run_with_ledger, seeded, submit, submit_call,
};
use crate::agent::develop_strategy_with_ledger;
use crate::client::{FakeClient, FakeTurn, ToolCall};
use crate::ledger::learning::{Learning, LearningScope};
use crate::ledger::trial::TrialLedger;
use crate::ledger::{self, LedgerPaths};

/// Payoff (a): the grounding block naming a prior trial's Sharpe, its explanation, and the
/// matching learnings reaches the model — and the user's own request is preserved beneath it.
#[test]
fn prompt_includes_prior_trials() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let mut t = prior_accepted(0, 0.05, 1.23);
    t.explanation = "ema pullback".into();
    ledger::store::save_trials(&TrialLedger { trials: vec![t] }, &paths.trials);
    ledger::store::append_learning(
        &paths.learnings,
        Learning {
            ts_ms: 1,
            scope: LearningScope::Global,
            text: "fees dominate below 20 trades".into(),
        },
    );

    let client = CapturingClient::default();
    let _ = run_with_ledger("make an sma cross", store.as_ref(), &client, &paths);
    let prompt = client.prompt();
    assert!(prompt.contains("1.23"), "the prior trial's OOS Sharpe must be named: {prompt}");
    assert!(prompt.contains("ema pullback"), "…and what it was: {prompt}");
    assert!(prompt.contains("fees dominate below 20 trades"), "the learning: {prompt}");
    assert!(prompt.contains("make an sma cross"), "the user's request survives: {prompt}");

    // Control — no ledger means the prompt is the caller's, verbatim.
    let bare = CapturingClient::default();
    let _ = develop_strategy_with_ledger(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &bare,
        2,
        0.3,
        None,
    );
    assert_eq!(bare.prompt(), "make an sma cross");
}

/// A fresh ledger has nothing to say, so the prompt stays byte-identical to the ledger-less
/// one — grounding must not inject an empty header on a slice with no history.
#[test]
fn empty_ledger_leaves_the_prompt_untouched() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let client = CapturingClient::default();
    let _ = run_with_ledger("hello", store.as_ref(), &client, &paths);
    assert_eq!(client.prompt(), "hello");
}

/// The `record_learning` tool: advertised only WITH a ledger, writes a slice-scoped note, and
/// does not consume a repair attempt.
#[test]
fn record_learning_tool_is_gated_on_the_ledger_and_writes_the_note() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let client = CapturingClient::scripted(vec![
        ToolCall {
            id: "l".into(),
            name: "record_learning".into(),
            input: json!({"scope":"slice","text":"rsi mean-reversion overtrades here"}),
        },
        submit_call(CROSS),
    ]);
    let r = run_with_ledger("x", store.as_ref(), &client, &paths);
    assert!(client.tools.borrow().iter().any(|t| t == "record_learning"));
    assert!(r.accepted, "the submission after the note still runs");
    assert_eq!(r.attempts, 1, "a learning call must not burn a repair attempt");

    let notes = ledger::store::load_learnings(&paths.learnings);
    assert_eq!(notes.learnings.len(), 1);
    assert_eq!(notes.learnings[0].text, "rsi mean-reversion overtrades here");
    assert_eq!(
        notes.learnings[0].scope,
        LearningScope::Slice {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
        }
    );

    // Ledger-less: the tool is not offered at all.
    let bare = CapturingClient::default();
    let _ = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &bare,
        2,
        0.3,
        None,
    );
    assert_eq!(*bare.tools.borrow(), vec!["submit_strategy".to_string()]);
}

/// An unwritable ledger directory must cost the note/trial, never the run — the best-effort
/// contract the whole module is built on.
#[test]
fn an_unwritable_ledger_never_fails_the_loop() {
    let (d, store) = seeded();
    // A path whose PARENT does not exist: every read misses and every write errors.
    let missing = d.path().join("no").join("such").join("dir");
    let paths = LedgerPaths::under(&missing);
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = run_with_ledger("x", store.as_ref(), &client, &paths);
    assert!(r.accepted, "the authoring loop must survive an unavailable ledger");
    assert!(!paths.trials.exists());
}
