//! The per-venue POLICY pins the perp bridges each ran over their OWN numbers (test-support only):
//! the startup leverage rule a `leverage_for` applies (aster, binance, bybit, okx) and the fixed-delay
//! pager a `page_pacer` builds (bybit, deribit, okx). Each bridge keeps its `#[test]` (name, doc, the
//! venue's constants and words) and hands them in, so every venue is still proven on its own code; only
//! the assertions are written once.

use std::time::Duration;

use vike_model::ProfileRisk;
use vike_model::rate_limits::PaceSample;

use crate::pacer::Pacer;

/// A perp bridge's `leverage_for` follows the operator's `[risk]` budget ([`crate::leverage`]): unset
/// (`None`, or a budget naming no leverage) posts `venue_default`, which must be `historical`, the
/// literal the venue posted before the budget was threaded in (`what` says where); a set number is
/// posted verbatim in both directions; and the posted number is the reciprocal of the initial-margin
/// fraction the RiskGate sizes against.
pub fn assert_leverage_follows_the_risk_budget(
    leverage_for: impl Fn(Option<&ProfileRisk>) -> f64,
    venue_default: f64,
    historical: f64,
    what: &str,
) {
    // Unset — both shapes of "the operator said nothing" — keeps the literal EXACTLY.
    assert_eq!(leverage_for(None), venue_default);
    assert_eq!(leverage_for(None), historical, "{what}");
    let silent = ProfileRisk { max_leverage: None, ..Default::default() };
    assert_eq!(leverage_for(Some(&silent)), historical);

    // Set — the venue account is configured with the operator's own number, in BOTH directions
    // (the two ways the old hardcode was dangerous).
    let ten = ProfileRisk { max_leverage: Some(10.0), ..Default::default() };
    assert_eq!(leverage_for(Some(&ten)), 10.0);
    let one = ProfileRisk { max_leverage: Some(1.0), ..Default::default() };
    assert_eq!(leverage_for(Some(&one)), 1.0);

    // ONE number governs both sides: what we POST is the reciprocal of the initial-margin fraction.
    assert_eq!(ten.im_requirement(), Some(0.1));
    assert_eq!(ten.im_requirement(), Some(1.0 / leverage_for(Some(&ten))));
    // The same invariant, via the shared helper the rule itself ships.
    assert!(crate::leverage::agrees_with_im_requirement(&ten, venue_default));
}

/// A `venue` that publishes no request budget pages on its FIXED `page_delay`: the pacer
/// `page_pacer` builds (the venue's whole pacing policy) measures but never moves the sleep, unseeded
/// or seeded with `seed` (a previous run's record), under any observation; nothing timed is no ETA.
pub fn assert_the_pager_never_changes_its_fixed_delay(
    page_pacer: impl Fn(Option<&PaceSample>) -> Pacer,
    page_delay: Duration,
    seed: &PaceSample,
    venue: &str,
) {
    let mut p = page_pacer(None);
    assert!(!p.is_discovered(), "{venue} publishes no budget to discover");
    assert_eq!(p.next_delay(), page_delay);
    assert_eq!(p.eta(1_000), None, "nothing timed ⇒ no ETA, not one guessed from the sleep");

    for _ in 0..4 {
        p.observe_request(None, Duration::from_millis(480));
    }
    assert_eq!(p.next_delay(), page_delay, "the measured 480ms must not move the sleep");
    assert!(!p.should_cool_down(), "and no budget ⇒ no target to cross");

    // A seed is likewise ETA-only: it can never make this pager faster.
    let mut seeded = page_pacer(Some(seed));
    assert_eq!(seeded.next_delay(), page_delay);
    seeded.observe_request(Some(9_999), Duration::from_secs(30));
    assert_eq!(seeded.next_delay(), page_delay, "nor can any observation, at any scale");
}
