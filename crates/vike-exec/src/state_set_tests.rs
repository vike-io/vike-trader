use super::*;

const ALL: &[OrderStatus] = &[
    S::Initialized,
    S::Submitted,
    S::Accepted,
    S::Triggered,
    S::PartiallyFilled,
    S::Filled,
    S::Canceled,
    S::Rejected,
    S::Denied,
    S::Expired,
    S::PendingCancel,
    S::Liquidated,
    S::Emulated,
    S::Released,
];

/// PIN: `is_live()` is NOT `!is_terminal()` — they disagree on exactly `Liquidated`, BY DESIGN
/// (`Liquidated` is a live perp force-close excluded from `is_terminal`, yet never worth a
/// cancel/modify). Do not "simplify" one into the other.
#[test]
fn is_live_is_not_the_negation_of_is_terminal_they_disagree_on_liquidated() {
    assert!(
        !OrderStatus::Liquidated.is_terminal(),
        "Liquidated is non-terminal BY DESIGN (perp force-close is a live state)"
    );
    assert!(
        !OrderStatus::Liquidated.is_live(),
        "Liquidated is NOT live for the cancel-send gate — never worth canceling/modifying"
    );
    for s in ALL {
        let expected = !s.is_terminal() && *s != OrderStatus::Liquidated;
        assert_eq!(
            s.is_live(),
            expected,
            "{}: is_live must equal !is_terminal for every status EXCEPT Liquidated",
            s.as_str()
        );
    }
}

/// PIN: the cancel-SEND gate (`is_live`) and the FSM's `OrderCanceled` allowed-from set
/// (`can_receive_cancel`) answer DIFFERENT questions and are intentionally different sets:
/// INITIALIZED/SUBMITTED/EMULATED/RELEASED are live (a cancel may be worth sending) yet may NOT
/// receive `OrderCanceled` — the documented gap being a venue-honored cancel of a SUBMITTED
/// order (OrderCanceled → InvalidOrderTransition → dropped, masked by in-order WS delivery).
/// This test fails if the two sets are ever merged.
#[test]
fn cancel_send_gate_and_can_receive_cancel_are_distinct_sets() {
    // The FSM allowed-from set, exactly.
    assert_eq!(
        OrderStatus::CAN_RECEIVE_CANCEL,
        &[S::Accepted, S::Triggered, S::PartiallyFilled, S::PendingCancel],
        "OrderCanceled allowed-from set must not change in a naming-only refactor"
    );
    // Strictly narrower than the send gate: everything cancelable is live…
    for s in OrderStatus::CAN_RECEIVE_CANCEL {
        // The method's POSITIVE direction: every member of its own set must answer true, so a
        // body mutated to `false` cannot pass by satisfying only the negative assertions below.
        assert!(
            s.can_receive_cancel(),
            "{}: can_receive_cancel must return true for every status in CAN_RECEIVE_CANCEL",
            s.as_str()
        );
        assert!(s.is_live(), "{}: can_receive_cancel ⊆ is_live", s.as_str());
    }
    // …but NOT vice versa — the designed divergence, per status.
    for s in [S::Initialized, S::Submitted, S::Emulated, S::Released] {
        assert!(s.is_live(), "{}: live (cancel worth sending)", s.as_str());
        assert!(
            !s.can_receive_cancel(),
            "{}: yet may NOT receive OrderCanceled (the sets must stay distinct)",
            s.as_str()
        );
    }
}

/// The modifiable set, exactly — {ACCEPTED, TRIGGERED, PARTIALLY_FILLED}; a naming-only
/// refactor must not change membership.
#[test]
fn modifiable_set_membership_is_pinned() {
    assert_eq!(OrderStatus::MODIFIABLE, &[S::Accepted, S::Triggered, S::PartiallyFilled]);
    for s in ALL {
        assert_eq!(s.is_modifiable(), OrderStatus::MODIFIABLE.contains(s), "{}", s.as_str());
    }
}
