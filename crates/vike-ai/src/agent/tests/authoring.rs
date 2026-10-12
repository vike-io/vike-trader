//! The authoring loop's accept, repair and give-up paths, and the displayed Sharpe's annualization.

use vike_analytics::metrics;
use vike_analytics::report::periods_per_year_for_interval;

use super::support::{CROSS, PPY, seeded, submit};
use crate::agent::develop_strategy;
use crate::client::{FakeClient, FakeTurn};

#[test]
fn happy_path_accepts_a_valid_strategy() {
    let (_d, store) = seeded();
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
    );
    assert!(r.accepted);
    assert_eq!(r.attempts, 1);
    assert!(r.n_trades > 0);
}

#[test]
fn repair_path_recovers_after_a_compile_error() {
    let (_d, store) = seeded();
    let client =
        FakeClient::new(vec![submit("fn on_bar( {"), submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
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
    let r = develop_strategy(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        max_repairs,
        0.3,
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
    let (_d, store) = seeded();
    let client =
        FakeClient::new(vec![submit("fn on_bar( {"), submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
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
    let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
    assert!(r.accepted, "the fixture must produce an accepted candidate to have a Sharpe");

    let at_daily = metrics::sharpe(&r.oos_equity_curve, PPY);
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
