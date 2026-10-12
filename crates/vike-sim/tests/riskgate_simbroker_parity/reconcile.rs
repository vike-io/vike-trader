//! The agree-or-pin law: `check` over the matrix, the shared `reconcile` verdict, the classifier.

use super::matrix::matrix;
use super::sides::{live_verdict, sim_outcome};
use super::{Divergence, Row};

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

    // THE RECONCILIATION. The two engines must agree, or the row must say WHY not — and a pin
    // that no longer bites is just as much a failure as an unpinned divergence.
    reconcile(row.name, live_ok, &reason, sim_filled, pos_before, pos_after, row.divergence);
}

/// The shared RECONCILIATION verdict, used by BOTH the hand matrix ([`check`]) and the proptest
/// arms below. The two engines must AGREE (`live_ok == sim_filled`), unless the scenario is a
/// pinned [`Divergence`] (in which case they MUST differ). It panics on an unpinned divergence (a
/// real live-vs-backtest bug — exactly the class #458/#468/#479 this file exists to catch) or on a
/// pin that no longer bites. This is the single source of the agree-or-pin law; the proptest simply
/// feeds it a STRUCTURALLY-computed `expected` instead of a hand-declared one.
pub(super) fn reconcile(
    name: &str,
    live_ok: bool,
    reason: &str,
    sim_filled: bool,
    pos_before: f64,
    pos_after: f64,
    expected: Option<Divergence>,
) {
    match (live_ok == sim_filled, expected) {
        (true, None) | (false, Some(_)) => {}
        (true, Some(d)) => panic!(
            "[{name}] pinned divergence {d:?} NO LONGER BITES — both engines now agree \
             (live_ok={live_ok}, sim_filled={sim_filled}). If this was fixed on purpose, drop \
             the pin; the pin must never outlive the divergence."
        ),
        (false, None) => panic!(
            "[{name}] UNPINNED LIVE/BACKTEST DIVERGENCE: the gate says ok={live_ok} \
             (reason={reason:?}) but the backtest says filled={sim_filled} \
             (position {pos_before} -> {pos_after}). This is exactly the class of bug this file \
             exists to catch (#458/#468/#479). Do NOT silence it by editing the expectation: \
             either fix the production divergence in its own PR, or add a named `Divergence` \
             variant documenting why the two engines are allowed to differ here."
        ),
    }
}

/// Structural classifier for the FLOOR lane's ONE documented divergence,
/// [`Divergence::BelowMinReversal`]. With `reduce_only = false` (the proptest floor-lane setting)
/// `is_covered_reduce` collapses to `is_implicit_reduce`, so a REVERSAL — one that opposes the
/// position AND overshoots it (`|qty| > |position|`, flipping through flat) — is the only order
/// that is neither opening nor covered: the gate applies its floors to it (it opens the far side),
/// while `SimBroker::apply_fill` fills the flip WHOLE (direction-only). It therefore diverges
/// EXACTLY when such a reversal is below either armed floor, computed here with the gate's OWN
/// arithmetic (`order_notional`, strict `<`) — the same shared vocabulary the matrix speaks. An
/// opening or covered order never diverges (both sides agree), so this returns `None`; if they ever
/// disagree there, [`reconcile`] fires the unpinned-divergence panic — a real finding.
pub(super) fn expected_floor_divergence(row: &Row) -> Option<Divergence> {
    let reducing = vike_model::is_reducing_direction(row.side, row.position);
    let is_reversal = reducing && row.qty.abs() > row.position.abs();
    if !is_reversal {
        return None;
    }
    let below_qty = row.min_qty > 0.0 && row.qty.abs() < row.min_qty;
    let notional = vike_model::order_notional(row.qty, row.price, row.multiplier);
    let below_notional = row.min_notional > 0.0 && notional < row.min_notional;
    (below_qty || below_notional).then_some(Divergence::BelowMinReversal)
}

#[test]
fn riskgate_and_simbroker_agree_or_pin_every_divergence() {
    for row in matrix() {
        check(&row);
    }
}
