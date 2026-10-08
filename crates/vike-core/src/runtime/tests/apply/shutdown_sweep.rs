//! The opt-in cancel-on-shutdown sweep: what it cancels, and what it never touches.

use super::*;

// ── the opt-in shutdown sweep (`CoreConfig::cancel_orders_on_shutdown`) ──────────────────
//
// The wiring — that the core's TEARDOWN calls this when the flag is on and not when it is off —
// is proved end to end through a real `spawn_core` in
// `crates/vike-core/tests/wiring/shutdown_cancel_policy.rs`. These two prove what the sweep DOES, from
// in-crate where the client's recorded cancels are directly readable.

/// The sweep cancels every resting order, naming each one to the client.
#[test]
fn the_shutdown_sweep_cancels_every_resting_order() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
    c.apply_intent(OrderIntent::Submit(market_req("rest-2")), 0);
    // `RecordingClient` emits nothing of its own, so both orders sit non-terminal — which is
    // exactly the state a resting order is in when a daemon is stopped.
    assert!(c.engine.client.cancels.is_empty(), "precondition: nothing cancelled yet");

    c.cancel_resting_on_shutdown();

    let mut cancelled = c.engine.client.cancels.clone();
    cancelled.sort();
    assert_eq!(cancelled, vec!["rest-1".to_string(), "rest-2".to_string()]);
    assert!(
        c.recent.iter().any(|m| m.contains("shutdown") && m.contains("positions untouched")),
        "the operator is told what the stop did — and what it did NOT do: {:?}",
        c.recent
    );
}

/// MUTATION SENTINEL: an empty book must not manufacture a cancel, and must not leave a note
/// claiming one happened. A sweep that unconditionally logged would pass the test above.
#[test]
fn the_shutdown_sweep_is_a_no_op_when_nothing_is_resting() {
    let mut c = test_core();
    c.cancel_resting_on_shutdown();
    assert!(c.engine.client.cancels.is_empty(), "nothing rested, so nothing is cancelled");
    assert!(
        !c.recent.iter().any(|m| m.contains("shutdown: cancelled")),
        "and no note claims otherwise: {:?}",
        c.recent
    );
}

/// It CANCELS; it does not FLATTEN. A stop must not decide on its own to realize PnL — closing a
/// position is `MarketExit`, an operator action. Pinned because "cancel on shutdown" is one
/// short step from "go flat on shutdown" in a reader's head.
#[test]
fn the_shutdown_sweep_never_closes_a_position() {
    let mut c = test_core();
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
    let submitted_before = c.engine.client.submissions.len();

    c.cancel_resting_on_shutdown();

    assert_eq!(c.engine.client.cancels, vec!["rest-1".to_string()], "the order is cancelled");
    assert_eq!(
        c.engine.client.submissions.len(),
        submitted_before,
        "but NO closing order is sent — the position survives the stop"
    );
    assert_eq!(c.engine.position_size_of("BTCUSDT", "BOTH"), 2.0, "the position is untouched");
}
