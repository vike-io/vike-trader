//! The agreement law: `check` over the matrix, the shared `reconcile` verdict, the classifier.

use super::Row;
use super::matrix::matrix;
use super::sides::{live_verdict, sim_outcome};

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

fn check(row: &Row) {
    let (live_ok, reason) = live_verdict(row);
    let (pos_before, other_before, pos_after, _dropped) = sim_outcome(row);

    // HARNESS INVARIANT, the ACCOUNT lane's half: the second symbol must hold what the row says it
    // holds. Its setup leg crosses the same armed gate, and an account ceiling is exactly the knob
    // that can refuse it — so without this a row whose ceiling ate its own setup would reconcile a
    // scenario in which the account is EMPTY and report it as agreement.
    assert!(
        (other_before - row.other_position).abs() <= 1e-12,
        "[{}] HARNESS: the other symbol holds {other_before}, not the declared {}. The account \
         lane's setup leg was itself gated — raise the row's ceiling above what the setup needs.",
        row.name,
        row.other_position
    );

    // HARNESS INVARIANT: the backtest must actually be in the account state the row declares, or
    // the comparison is between two different scenarios. `gate_market_order` gates the SETUP
    // order too (deny-whole by default), so this is a live failure mode, not a theoretical one.
    assert!(
        (pos_before - row.position).abs() <= 1e-12,
        "[{}] HARNESS: the setup leg established position {pos_before}, not the declared {}. \
         The row's account state was silently gated (gate_market_order applies to the setup \
         order as well) — pick a position that fits the row's margin budget.",
        row.name,
        row.position
    );

    // The backtest "did the order execute" decision: the FULL requested signed qty reached the
    // position. A denied-whole (gate_market_order), truncated (clamp knob) or dropped
    // (below-floor) fill all answer `false`; the two gate shapes are pinned separately below.
    let expected_after = pos_before + row.side as f64 * row.qty;
    let sim_filled = (pos_after - expected_after).abs() <= 1e-12;

    assert_eq!(
        live_ok, row.live_ok,
        "[{}] LIVE verdict changed: expected ok={}, got ok={} (reason={reason:?})",
        row.name, row.live_ok, live_ok
    );
    assert_eq!(
        sim_filled, row.sim_filled,
        "[{}] BACKTEST outcome changed: expected filled={}, got filled={} \
         (position {pos_before} -> {pos_after}, expected {expected_after})",
        row.name, row.sim_filled, sim_filled
    );

    // THE RECONCILIATION. The two engines must agree.
    reconcile(row.name, live_ok, &reason, sim_filled, pos_before, pos_after);
}

/// The shared RECONCILIATION verdict, used by BOTH the hand matrix ([`check`]) and the proptest
/// arms below: the two engines must AGREE (`live_ok == sim_filled`). A divergence is a real
/// live-vs-backtest bug, exactly the class #458/#468/#479 this file exists to catch.
pub(super) fn reconcile(
    name: &str,
    live_ok: bool,
    reason: &str,
    sim_filled: bool,
    pos_before: f64,
    pos_after: f64,
) {
    assert_eq!(
        live_ok, sim_filled,
        "[{name}] LIVE/BACKTEST DIVERGENCE: the gate says ok={live_ok} (reason={reason:?}) but the \
         backtest says filled={sim_filled} (position {pos_before} -> {pos_after}). This is exactly \
         the class of bug this file exists to catch (#458/#468/#479). Do NOT silence it by editing \
         the expectation: fix the production divergence."
    );
}

/// The FLOOR lane's one order the two engines refuse differently. With `reduce_only = false` (the
/// proptest floor-lane setting) a REVERSAL
/// — one that opposes the position AND overshoots it (`|qty| > |position|`, flipping through flat) —
/// is the only order that is neither opening nor covered. Below either armed floor, computed here
/// with the gate's OWN arithmetic (`order_notional`, strict `<`), the gate denies it whole and
/// `OrderBrokerSimulator::apply_fill` fills its closing half alone, so the backtest ends FLAT.
pub(super) fn is_below_min_reversal(row: &Row) -> bool {
    let reducing = vike_model::is_reducing_direction(row.side, row.position);
    let is_reversal = reducing && row.qty.abs() > row.position.abs();
    if !is_reversal {
        return false;
    }
    let below_quantity = row.min_qty > 0.0 && row.qty.abs() < row.min_qty;
    let notional = vike_model::order_notional(row.qty, row.price, row.multiplier);
    let below_notional = row.min_notional > 0.0 && notional < row.min_notional;
    below_quantity || below_notional
}

#[test]
fn riskgate_and_simbroker_agree_on_every_row() {
    for row in matrix() {
        check(&row);
    }
}
