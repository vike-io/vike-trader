use super::*;
use crate::order::OrderStatus as S;

/// ⚠ **The picker and the applier must be ONE predicate.**
///
/// "Which registry order is still alive enough to be the leg a liquidation force-closed?" used
/// to be answered in two places that disagreed: `transition`'s `OrderLiquidated` arm spelled
/// {ACCEPTED, TRIGGERED, PARTIALLY_FILLED} inline, while
/// `ExecutionEngine::coid_for_position` selected by NEGATION, skipping only
/// {Liquidated, Filled, Canceled}.
///
/// This asserts the five statuses that gap admitted — each one an order the picker WOULD have
/// returned and the FSM then refused. Because the scan is newest-first, any of them shadows the
/// ACCEPTED order that was actually force-closed, which then keeps its old status.
#[test]
fn the_picker_admits_nothing_the_fsm_refuses() {
    for s in [S::Submitted, S::PendingCancel, S::Rejected, S::Denied, S::Expired] {
        assert!(
            !s.can_receive_liquidation(),
            "{}: the OLD negated skip set admitted this, and the FSM refuses it — selecting on \
                 it shadows the real liquidated leg",
            s.as_str()
        );
    }
    // ...and the three that ARE legal still are, so this is not a blanket refusal.
    for s in [S::Accepted, S::Triggered, S::PartiallyFilled] {
        assert!(s.can_receive_liquidation(), "{}: must stay selectable", s.as_str());
    }
}

/// The constant IS the FSM's allowed-from row, verbatim — a naming-only refactor cannot move it.
#[test]
fn the_liquidation_set_matches_the_fsm() {
    assert_eq!(
        OrderStatus::CAN_RECEIVE_LIQUIDATION,
        &[S::Accepted, S::Triggered, S::PartiallyFilled],
        "OrderLiquidated allowed-from set must not change in a naming-only refactor"
    );
}

/// ⚠ `CAN_RECEIVE_LIQUIDATION` and `MODIFIABLE` are byte-identical TODAY and are deliberately
/// SEPARATE constants — they answer different questions ("may the venue force-close this?" vs
/// "may I amend this?"). This pins the coincidence so it is OBSERVED rather than assumed: if
/// one set ever moves, this test fails and the author decides whether the other should follow,
/// instead of a shared name silently deciding for them.
///
/// The same reasoning keeps `CAN_RECEIVE_CANCEL` apart from `is_live`, pinned above.
#[test]
fn the_liquidation_and_modifiable_sets_coincide_today_and_that_is_pinned_not_shared() {
    assert_eq!(
        OrderStatus::CAN_RECEIVE_LIQUIDATION,
        OrderStatus::MODIFIABLE,
        "these coincide today; if one moves, decide about the other rather than merging them"
    );
}
