use super::*;
use crate::order::OrderStatus as S;

/// ⚠ **The picker and the applier must be ONE predicate**
/// (`OrderStatus::CAN_RECEIVE_LIQUIDATION`'s doc has the history): none of the five statuses the
/// old negated skip set admitted may be selectable, since a newest-first pick of one shadows the
/// order actually force-closed.
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

/// ⚠ `CAN_RECEIVE_LIQUIDATION` and `MODIFIABLE` are byte-identical TODAY and deliberately SEPARATE
/// constants (different questions). This pins the coincidence so it is OBSERVED: if one set ever
/// moves, this fails and the author decides whether the other follows, instead of a shared name
/// deciding for them.
#[test]
fn the_liquidation_and_modifiable_sets_coincide_today_and_that_is_pinned_not_shared() {
    assert_eq!(
        OrderStatus::CAN_RECEIVE_LIQUIDATION,
        OrderStatus::MODIFIABLE,
        "these coincide today; if one moves, decide about the other rather than merging them"
    );
}
