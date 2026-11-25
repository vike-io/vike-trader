use super::*;
use AmendOrderStatus::{Filled, Gone, Partly, Pending, Refused, Resting, Unknown};
use vike_model::OrderType;

fn nf() -> AmendReplace {
    AmendReplace::begin(AmendPlacement::NewFirst, 0)
}

fn cf() -> AmendReplace {
    AmendReplace::begin(AmendPlacement::CancelFirst, 0)
}

fn req(order_type: OrderType, reduce_only: bool) -> OrderRequest {
    OrderRequest { order_type, reduce_only, ..Default::default() }
}

#[test]
fn only_a_reduce_only_trigger_order_goes_new_first() {
    assert_eq!(AmendPlacement::of(&req(OrderType::Stop, true)), AmendPlacement::NewFirst);
    assert_eq!(AmendPlacement::of(&req(OrderType::StopLimit, true)), AmendPlacement::NewFirst);
    assert_eq!(AmendPlacement::of(&req(OrderType::TakeProfit, true)), AmendPlacement::NewFirst);
    // an ENTRY stop, a limit (reduce-only or not): cancel first
    assert_eq!(AmendPlacement::of(&req(OrderType::Stop, false)), AmendPlacement::CancelFirst);
    assert_eq!(AmendPlacement::of(&req(OrderType::Limit, false)), AmendPlacement::CancelFirst);
    assert_eq!(AmendPlacement::of(&req(OrderType::Limit, true)), AmendPlacement::CancelFirst);
}

#[test]
fn the_status_reduction_covers_every_status() {
    use OrderStatus as S;
    for (status, want) in [
        (S::Initialized, Pending),
        (S::Submitted, Pending),
        (S::Accepted, Resting),
        (S::Triggered, Resting),
        (S::PendingCancel, Resting),
        (S::Emulated, Resting),
        (S::Released, Resting),
        (S::PartiallyFilled, Partly),
        (S::Filled, Filled),
        (S::Canceled, Gone),
        (S::Expired, Gone),
        (S::Liquidated, Gone),
        (S::Rejected, Refused),
        (S::Denied, Refused),
    ] {
        assert_eq!(AmendOrderStatus::of(Some(status)), want, "{status:?}");
    }
    assert_eq!(AmendOrderStatus::of(None), Unknown);
}

// ---- new-first (a reduce-only stop) -------------------------------------------------------------

/// REPLACE-OK: the new stop is answered, the old is cancelled, the old is gone: replaced, and at no
/// pass is the old order cancelled before the new one is in force.
#[test]
fn new_first_places_then_cancels_then_ends_replaced() {
    let mut r = nf();
    // the new stop is out, the venue has not answered
    assert_eq!(r.step(Resting, Pending, 1), AmendStep::WAIT);
    // answered: now (and only now) the old one is cancelled
    assert_eq!(r.step(Resting, Resting, 2), AmendStep::send(AmendAct::CancelOld));
    // the cancel is out, not yet confirmed
    assert_eq!(r.step(Resting, Resting, 3), AmendStep::WAIT);
    // confirmed
    assert_eq!(r.step(Gone, Resting, 4), AmendStep::end(AmendAct::Nothing, AmendEnd::Replaced));
}

/// REPLACE-NEW-REJECTED: the venue refuses the new stop; the old one is never touched.
#[test]
fn a_refused_new_stop_leaves_the_old_one_standing() {
    let mut r = nf();
    let step = r.step(Resting, Refused, 1);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::NewRefused { old_stands: true }));
    assert!(step.end.unwrap().old_stands());
    let words = step.end.unwrap().words("c1", "c1b", Some("price would trigger immediately"));
    assert!(words.contains("REFUSED") && words.contains("c1 STANDS"), "{words}");
    assert!(words.contains("price would trigger immediately"), "{words}");
}

/// REPLACE-CANCEL-LATE: the cancel of the old stop is not confirmed in time: BOTH rest, and the words
/// say which one to cancel by hand. Nothing is cancelled again by the machine.
#[test]
fn an_unconfirmed_cancel_of_the_old_stop_leaves_both_resting_and_says_so() {
    let mut r = nf();
    assert_eq!(r.step(Resting, Resting, 1), AmendStep::send(AmendAct::CancelOld));
    assert_eq!(r.step(Resting, Resting, 1 + CANCEL_WAIT_MS), AmendStep::WAIT);
    let step = r.step(Resting, Resting, 2 + CANCEL_WAIT_MS);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::OldNotCancelled));
    let words = step.end.unwrap().words("c1", "c1b", None);
    assert!(words.contains("BOTH rest") && words.contains("cancel c1 by hand"), "{words}");
}

/// VENUE-DOWN MID-REPLACE: the new stop is never answered. After the wait it is cancelled and the old
/// one stands.
#[test]
fn a_silent_venue_leaves_the_old_stop_standing_and_cancels_the_new_one() {
    let mut r = nf();
    assert_eq!(r.step(Resting, Pending, PLACE_WAIT_MS), AmendStep::WAIT);
    let step = r.step(Resting, Pending, PLACE_WAIT_MS + 1);
    assert_eq!(step, AmendStep::end(AmendAct::CancelNew, AmendEnd::NewSilent { old_stands: true }));
    // a registry that lost the new order has nothing to cancel
    let mut r = nf();
    let step = r.step(Resting, Unknown, PLACE_WAIT_MS + 1);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::NewSilent { old_stands: true }));
}

/// A FILL of the old stop during the overlap: the first to fill wins, the other is cancelled.
#[test]
fn a_fill_of_the_old_stop_during_the_overlap_cancels_the_new_one() {
    let mut r = nf();
    assert_eq!(
        r.step(Filled, Resting, 1),
        AmendStep::end(AmendAct::CancelNew, AmendEnd::OldFilled),
        "the old stop filled while the new one is in force: cancel the new one"
    );
    let mut r = nf();
    assert_eq!(
        r.step(Filled, Pending, 1),
        AmendStep::end(AmendAct::CancelNew, AmendEnd::OldFilled)
    );
    // ...also after the old one's cancel went out
    let mut r = nf();
    assert_eq!(r.step(Resting, Resting, 1), AmendStep::send(AmendAct::CancelOld));
    assert_eq!(
        r.step(Filled, Resting, 2),
        AmendStep::end(AmendAct::CancelNew, AmendEnd::OldFilled)
    );
}

/// The NEW stop fires (or is cancelled by a bracket's sibling) while the old one still rests: the pair
/// is one logical order, so the old one follows.
#[test]
fn a_new_stop_that_ends_takes_the_old_one_with_it() {
    let mut r = nf();
    assert_eq!(r.step(Resting, Gone, 1), AmendStep::end(AmendAct::CancelOld, AmendEnd::NewGone));
    let mut r = nf();
    assert_eq!(r.step(Resting, Resting, 1), AmendStep::send(AmendAct::CancelOld));
    assert_eq!(r.step(Resting, Gone, 2), AmendStep::end(AmendAct::CancelOld, AmendEnd::NewGone));
    // a new stop that FILLED (it fired at once) is in force: the old one is cancelled, replaced
    let mut r = nf();
    assert_eq!(r.step(Resting, Filled, 1), AmendStep::send(AmendAct::CancelOld));
    assert_eq!(r.step(Gone, Filled, 2), AmendStep::end(AmendAct::Nothing, AmendEnd::Replaced));
}

/// The old stop is cancelled and the new one is gone too: loud, nothing rests.
#[test]
fn both_gone_is_reported_as_nothing_resting() {
    let mut r = nf();
    assert_eq!(r.step(Resting, Resting, 1), AmendStep::send(AmendAct::CancelOld));
    // the new one disappears and the old one's cancel lands in the same pass
    let step = r.step(Gone, Gone, 2);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::BothGone));
    assert!(!step.end.unwrap().old_stands() && !step.end.unwrap().succeeded());
    assert!(step.end.unwrap().words("c1", "c1b", None).contains("NOTHING"));
}

/// The old stop was cancelled by someone else while the new one was still being placed.
#[test]
fn an_old_stop_cancelled_by_someone_else_ends_the_amend() {
    let mut r = nf();
    assert_eq!(r.step(Gone, Pending, 1), AmendStep::end(AmendAct::CancelNew, AmendEnd::OldGone));
}

// ---- cancel-first (a limit order, an entry stop) -------------------------------------------------

/// REPLACE-OK: cancel, wait for it to be gone, place the new one, wait for its answer.
#[test]
fn cancel_first_cancels_then_places_then_ends_replaced() {
    let mut r = cf();
    assert_eq!(r.step(Resting, Unknown, 1), AmendStep::WAIT, "the cancel is out");
    assert_eq!(r.step(Gone, Unknown, 2), AmendStep::send(AmendAct::SubmitNew));
    assert_eq!(r.step(Gone, Pending, 3), AmendStep::WAIT);
    assert_eq!(r.step(Gone, Resting, 4), AmendStep::end(AmendAct::Nothing, AmendEnd::Replaced));
}

/// A fill that beat the cancel: nothing is placed.
#[test]
fn a_limit_that_filled_before_its_cancel_is_not_placed_again() {
    let mut r = cf();
    let step = r.step(Filled, Unknown, 1);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::OldFilled));
    assert!(step.end.unwrap().words("c1", "", None).contains("FILLED"));
}

/// REPLACE-NEW-REJECTED, cancel-first: the old order IS gone, and the words say so.
#[test]
fn a_refused_new_limit_after_the_cancel_says_the_order_is_gone() {
    let mut r = cf();
    assert_eq!(r.step(Gone, Unknown, 1), AmendStep::send(AmendAct::SubmitNew));
    let step = r.step(Gone, Refused, 2);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::NewRefused { old_stands: false }));
    assert!(!step.end.unwrap().old_stands());
    let words = step.end.unwrap().words("c1", "c1b", Some("post-only would cross"));
    assert!(words.contains("GONE") && words.contains("post-only would cross"), "{words}");
}

/// REPLACE-CANCEL-LATE, cancel-first: the cancel is slow. The machine says so once, keeps waiting, and
/// still places the new order when the cancel finally lands (the order is not left half-moved).
#[test]
fn a_late_cancel_still_places_the_new_order_when_it_lands() {
    let mut r = cf();
    assert!(!r.take_cancel_advisory(CANCEL_WAIT_MS));
    assert_eq!(r.step(Resting, Unknown, CANCEL_WAIT_MS + 1), AmendStep::WAIT);
    assert!(r.take_cancel_advisory(CANCEL_WAIT_MS + 1), "the advisory is due once");
    assert!(!r.take_cancel_advisory(CANCEL_WAIT_MS + 2), "and only once");
    assert_eq!(r.step(Gone, Unknown, CANCEL_WAIT_MS + 3), AmendStep::send(AmendAct::SubmitNew));
}

/// A cancel that never lands is dropped after the give-up bound, and the words say what a late cancel
/// would mean.
#[test]
fn a_cancel_that_never_lands_is_given_up_with_words() {
    let mut r = cf();
    assert_eq!(r.step(Resting, Unknown, CANCEL_GIVE_UP_MS), AmendStep::WAIT);
    let step = r.step(Resting, Unknown, CANCEL_GIVE_UP_MS + 1);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::CancelUnconfirmed));
    assert!(step.end.unwrap().old_stands());
    assert!(step.end.unwrap().words("c1", "", None).contains("GONE"));
}

/// VENUE-DOWN MID-REPLACE, cancel-first: the cancel landed, the new order is never answered.
#[test]
fn a_silent_venue_after_the_cancel_is_reported_not_retried() {
    let mut r = cf();
    assert_eq!(r.step(Gone, Unknown, 1), AmendStep::send(AmendAct::SubmitNew));
    assert_eq!(r.step(Gone, Pending, 1 + PLACE_WAIT_MS), AmendStep::WAIT);
    let step = r.step(Gone, Pending, 2 + PLACE_WAIT_MS);
    assert_eq!(step, AmendStep::end(AmendAct::Nothing, AmendEnd::NewSilent { old_stands: false }));
    assert!(step.end.unwrap().words("c1", "c1b", None).contains("check the open orders"));
}

/// Every ending has words, and the three that claim the old order stands are exactly the ones whose
/// `old_stands` is true.
#[test]
fn every_ending_has_words_and_old_stands_matches_them() {
    for end in [
        AmendEnd::Replaced,
        AmendEnd::NewRefused { old_stands: true },
        AmendEnd::NewRefused { old_stands: false },
        AmendEnd::NewSilent { old_stands: true },
        AmendEnd::NewSilent { old_stands: false },
        AmendEnd::OldNotCancelled,
        AmendEnd::CancelUnconfirmed,
        AmendEnd::OldFilled,
        AmendEnd::OldGone,
        AmendEnd::NewGone,
        AmendEnd::BothGone,
    ] {
        let words = end.words("OLD", "NEW", None);
        assert!(!words.is_empty() && words.contains("OLD"), "{end:?}: {words}");
        if end.old_stands() {
            assert!(
                words.contains("STANDS")
                    || words.contains("BOTH rest")
                    || words.contains("left as it is"),
                "{end:?} says the old order stands, its words must: {words}"
            );
        }
    }
    // a partial fill of the old order is still an order that rests
    let mut r = cf();
    assert_eq!(r.step(Partly, Unknown, 1), AmendStep::WAIT);
}
