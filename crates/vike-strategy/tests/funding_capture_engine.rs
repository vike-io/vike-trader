//! `FundingCapture` END-TO-END gate: the funding-harvest reference strategy folded through the
//! REAL [`StrategyEngine`] over a funding-carrying bar series.
//!
//! The engine sits ABOVE `vike-strategy`, so these run here (the pure tests stay beside the
//! strategy). Each pins the one observable proving the funding-COLLECTING side: net funding
//! RECEIVED (`funding_paid`, received-positive) ends above zero, under both signs of the rate.

use vike_model::Bar;
use vike_sim::{EngineParams, StrategyEngine};
use vike_strategy::FundingCapture;

const SYM: &str = "BTCUSDT";

fn bar(ts: i64, close: f64, funding: Option<f64>) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding,
        bid: None,
        ask: None,
        symbol: Some(SYM.into()),
    }
}

/// Run `strat` over eight one-minute bars each carrying `funding`.
fn run_with_funding(strat: FundingCapture, funding: f64) -> vike_analytics::BacktestResult {
    let bars: Vec<Bar> = (0..8).map(|i| bar(60_000 * i, 100.0, Some(funding))).collect();
    StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, EngineParams::default()).run()
}

#[test]
fn runs_end_to_end_and_collects_positive_funding_as_a_short() {
    // POSITIVE funding on every bar ⇒ the collecting side is SHORT. A market order from bar i's
    // on_bar fills in bar i+1's fill phase, BEFORE that bar's funding charge, so from the second
    // funding bar on the short is charged NEGATIVE funding (a receipt).
    let result = run_with_funding(FundingCapture { threshold: 0.0, qty: 5.0, symbol: None }, 0.01);
    assert_eq!(result.equity_curve.len(), 8, "runs every bar");
    assert!(
        result.funding_paid > 0.0,
        "a short under positive funding must NET-RECEIVE funding, got funding_paid = {}",
        result.funding_paid
    );
}

#[test]
fn negative_funding_pays_when_forced_long_but_collecting_side_is_long() {
    // Sanity mirror: with NEGATIVE funding the collecting side is LONG, and a held long under
    // negative funding also NET-RECEIVES (funding_paid > 0). Proves the sign flip end-to-end.
    let result = run_with_funding(FundingCapture { threshold: 0.0, qty: 4.0, symbol: None }, -0.01);
    assert!(
        result.funding_paid > 0.0,
        "a long under negative funding must NET-RECEIVE funding, got funding_paid = {}",
        result.funding_paid
    );
}
