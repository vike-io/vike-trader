//! The authoring loop's accept, repair and give-up paths, and the displayed Sharpe's annualization.

use vike_analytics::report::periods_per_year_for_interval;
use vike_analytics::{BacktestResult, metrics};
use vike_data::TimestampRange;

use super::support::{CROSS, CapturingClient, PERIODS_PER_YEAR, seeded, sim, submit, submit_call};
use crate::agent::{
    OutOfSampleSlice, develop_strategies_with_ledger, develop_strategy_with_ledger,
};
use crate::client::{FakeClient, FakeTurn};

/// ⚠ **A yen series is not scored as dollars.** The loop backtests on one dollar ledger with no
/// conversion rate, so a USDJPY slice would have the model told "Accepted. OOS Sharpe …" for numbers
/// wrong by the exchange rate. The symbol is refused BEFORE the model is asked (the capturing client
/// is never called) and the batch door refuses alike, `n` times, in the failed shape every caller
/// already renders. The slice here holds no USDJPY bars at all: the refusal comes first.
#[test]
fn a_foreign_quoted_symbol_is_refused_before_the_model_is_asked() {
    let (_d, store) = seeded();
    let client = CapturingClient::scripted(vec![submit_call(CROSS)]);
    let r = develop_strategy_with_ledger(
        "x",
        "oanda",
        "USDJPY",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        2,
        0.3,
        None,
    );
    assert!(!r.accepted);
    assert_eq!(r.attempts, 0, "no submission was processed");
    assert!(r.problems.iter().any(|p| p.contains("USDJPY (JPY)")), "{:?}", r.problems);
    assert!(client.tools.borrow().is_empty(), "the model was never called");

    let batch = develop_strategies_with_ledger(
        "x",
        "oanda",
        "EURGBP",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        3,
        2,
        0.3,
        None,
    );
    assert_eq!(batch.len(), 3);
    assert!(batch.iter().all(|r| !r.accepted && r.problems[0].contains("EURGBP (GBP)")));
    assert!(client.tools.borrow().is_empty(), "the model was never called for the batch either");
}

/// A coin-margined contract is refused the same way: the loop's simulator books size x price x
/// multiplier, so the Sharpe the model is told would be wrong by the price. `BTC-PERPETUAL` names it on
/// deribit, and the linear book of the same coin goes on to ask the model.
#[test]
fn an_inverse_contract_is_refused_before_the_model_is_asked() {
    let (_d, store) = seeded();
    let client = CapturingClient::scripted(vec![submit_call(CROSS)]);
    let r = develop_strategy_with_ledger(
        "x",
        "deribit",
        "BTC-PERPETUAL",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        2,
        0.3,
        None,
    );
    assert!(!r.accepted);
    assert_eq!(r.attempts, 0, "no submission was processed");
    assert!(r.problems.iter().any(|p| p.contains("BTC-PERPETUAL on deribit")), "{:?}", r.problems);
    assert!(client.tools.borrow().is_empty(), "the model was never called");

    let batch = develop_strategies_with_ledger(
        "x",
        "deribit",
        "ETH-PERPETUAL",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        3,
        2,
        0.3,
        None,
    );
    assert_eq!(batch.len(), 3);
    assert!(
        batch.iter().all(|r| !r.accepted && r.problems[0].contains("ETH-PERPETUAL on deribit"))
    );
    assert!(client.tools.borrow().is_empty(), "the model was never called for the batch either");
}

/// **A candidate is scored by the CALLER's backtester, over the held-out tail.** The loop hands it
/// the script and the last 30% of the 400 seeded bars (ts `60_000 * 281 ..= 60_000 * 400`), and
/// the result it reports is that backtester's, not an engine of its own: a planted answer comes back
/// as the candidate's trades and curve. A refusal is a problem the model is told to repair.
#[test]
fn the_callers_backtester_scores_the_candidate_over_the_held_out_slice() {
    let (_d, store) = seeded();
    let seen = std::cell::RefCell::new(Vec::new());
    let planted = |code: &str,
                   out_of_sample: OutOfSampleSlice<'_>|
     -> Result<BacktestResult, String> {
        let slice =
            format!("{}/{}/{}", out_of_sample.venue, out_of_sample.symbol, out_of_sample.interval);
        seen.borrow_mut().push((code.to_string(), slice, out_of_sample.range));
        Ok(BacktestResult {
            equity_curve: vec![100.0, 101.0, 100.5],
            n_trades: 3,
            ..Default::default()
        })
    };
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        &planted,
        2,
        0.3,
        None,
    );
    assert!(r.accepted);
    assert_eq!((r.n_trades, r.oos_equity_curve), (3, vec![100.0, 101.0, 100.5]));
    let range = TimestampRange::of(60_000 * 281, 60_000 * 400);
    assert_eq!(*seen.borrow(), vec![(CROSS.to_string(), "binance/BTCUSDT/1m".to_string(), range)]);

    let refusing = |_: &str, _: OutOfSampleSlice<'_>| -> Result<BacktestResult, String> {
        Err("no bars".to_string())
    };
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        &refusing,
        2,
        0.3,
        None,
    );
    assert!(!r.accepted);
    assert_eq!(r.problems, vec!["backtest refused: no bars".to_string()]);
}

#[test]
fn happy_path_accepts_a_valid_strategy() {
    let (_d, store) = seeded();
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        2,
        0.3,
        None,
    );
    assert!(r.accepted);
    assert_eq!(r.attempts, 1);
    assert!(r.n_trades > 0);
}

/// A broken submission, then a valid one, under `max_repairs = 2`.
fn broken_then_valid() -> crate::agent::AgentResult {
    let (_d, store) = seeded();
    let client =
        FakeClient::new(vec![submit("fn on_bar( {"), submit(CROSS), FakeTurn::Text("done".into())]);
    develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        2,
        0.3,
        None,
    )
}

#[test]
fn repair_path_recovers_after_a_compile_error() {
    let r = broken_then_valid();
    assert!(r.accepted);
    assert_eq!(r.attempts, 2);
    assert!(!r.problems.is_empty());
}

#[test]
fn give_up_after_max_repairs() {
    let (_d, store) = seeded();
    // Script MORE broken submissions than max_repairs allows -- if the cap were removed (or
    // merely decorative, as `_max_repairs` was pre-fix), every one of these 5 would be
    // compiled and `attempts` would end up at 5, not the capped 2.
    let max_repairs = 2;
    let client = FakeClient::new(vec![
        submit("fn on_bar( {"),
        submit("also broken ("),
        submit("still broken ("),
        submit("broken again ("),
        submit("broken once more ("),
        FakeTurn::Text("gave up".into()),
    ]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        max_repairs,
        0.3,
        None,
    );
    assert!(!r.accepted);
    assert_eq!(
        r.attempts, max_repairs,
        "attempts must be bounded by max_repairs, not by the 5-submission script"
    );
    assert!(!r.problems.is_empty());
}

#[test]
fn repair_path_still_accepts_on_the_last_allowed_attempt() {
    // max_repairs=2, broken-then-valid: the 2nd (valid) submission must still land inside the
    // cap and be accepted -- the cap must not be off-by-one against the happy/repair paths.
    let r = broken_then_valid();
    assert!(r.accepted);
    assert_eq!(r.attempts, 2);
}

/// The Copilot pane's displayed Sharpe follows the SLICE's interval, not a hardcoded 252.
///
/// This field is rendered as "OOS Sharpe" by `crates/vike-studio/src/panes/chat.rs`'s `summary_of`,
/// beside a Performance tab that annualizes off the picked slice. While this read 252 at every
/// interval, the two panes printed numbers `sqrt(1440) ≈ 37.9x` apart for one strategy on one
/// 1m slice, inside one window.
///
/// ⚠ The `assert_ne!` is what makes this bite. The equality alone compares two expressions that
/// would both move if the derivation regressed; only the inequality with the daily anchor fails
/// when the literal comes back. The precondition guards vacuity — `metrics::sharpe` returns
/// `0.0` at EVERY factor for a dispersion-free curve, which would satisfy the inequality for
/// the wrong reason.
#[test]
fn the_displayed_sharpe_annualizes_off_the_slice_interval_not_a_bare_252() {
    let (_d, store) = seeded();
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        &sim(store.as_ref()),
        2,
        0.3,
        None,
    );
    assert!(r.accepted, "the fixture must produce an accepted candidate to have a Sharpe");

    let at_daily = metrics::sharpe(&r.oos_equity_curve, PERIODS_PER_YEAR);
    assert!(
        at_daily != 0.0 && at_daily.is_finite(),
        "the fixture curve must have dispersion, else every factor yields 0.0: {at_daily}"
    );

    assert_eq!(
        r.oos_sharpe.to_bits(),
        metrics::sharpe(&r.oos_equity_curve, periods_per_year_for_interval("1m")).to_bits(),
        "the displayed Sharpe IS the OOS curve annualized at the 1m factor"
    );
    assert_ne!(
        r.oos_sharpe, at_daily,
        "...and NOT at the daily anchor — the regression this test exists to catch"
    );
}
