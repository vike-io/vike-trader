//! Submit, bracket and OTO/OCO drive tests, including the adversarial hostile-venue twins.

use super::*;

#[test]
fn submit_empty_coid_is_minted_nonempty_respected() {
    let mut c = test_core();
    let minted = c.apply_intent(OrderIntent::Submit(market_req("")), 0);
    assert_eq!(minted.len(), 1);
    assert!(!minted[0].is_empty(), "empty coid must be minted");
    assert_eq!(c.engine.client.submissions[0].client_order_id, minted[0]);

    let kept = c.apply_intent(OrderIntent::Submit(market_req("mine")), 0);
    assert_eq!(kept, vec!["mine".to_string()]);
    assert_eq!(c.engine.client.submissions[1].client_order_id, "mine");
}

#[test]
fn halted_gate_denies_submit_no_client_call() {
    let mut c = test_core();
    c.engine.trading_state = TradingState::Halted;
    c.apply_intent(OrderIntent::Submit(market_req("c1")), 0);
    assert!(c.engine.client.submissions.is_empty(), "RiskGate veto: nothing reaches the client");
}

#[test]
fn bracket_mints_three_linked_coids_but_holds_the_exits() {
    // Live-runtime OTO/OCO: a bracket still mints THREE linked coids, but only the OTO ENTRY
    // goes live to the venue — its protective stop-loss / take-profit are HELD off the venue
    // (the emulation) until the entry fills, so a naked exit can never trigger first.
    let mut c = test_core();
    let spec = vike_model::BracketSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    assert_eq!(coids.len(), 3, "three coids: entry + stop-loss + take-profit");
    assert_eq!(c.engine.client.submissions.len(), 1, "only the OTO entry reaches the venue");
    let venue_entry = &c.engine.client.submissions[0];
    assert_eq!(venue_entry.client_order_id, coids[0]);
    // the venue sees a PLAIN order — the OTO/OCO linkage lives in the CORE's book, not the wire
    assert_eq!(venue_entry.contingency_type, None, "the entry reaches the venue link-free");
    assert!(venue_entry.linked_order_ids.is_empty() && venue_entry.parent_order_id.is_none());
    // the entry IS recorded as an active OTO leg in the core's contingency book
    assert!(!c.contingency.is_empty() && !c.contingency.is_held(&coids[0]));
    // the two exits are held pending the entry fill, not at the venue
    assert_eq!(c.held_orders.len(), 2, "stop-loss + take-profit held");
    assert!(c.held_orders.contains_key(&coids[1]) && c.held_orders.contains_key(&coids[2]));
    assert!(c.contingency.is_held(&coids[1]) && c.contingency.is_held(&coids[2]));
}

// ---- live-runtime OTO/OCO drive (submit-hold + fill-drive) ----

fn bracket_spec() -> vike_model::BracketSpec {
    vike_model::BracketSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    }
}

/// Fold the [`OrderAccepted`, `Fill`, `OrderFilled`] sequence for `coid` into the core exactly as
/// a venue delivers a full fill — the `OrderFilled` drives any contingency (`drive_contingency_
/// on_fill`). Same helper shape the mid-expansion fill test uses (`fill_events`).
fn feed_full_fill(c: &mut CoreThread<RecordingClient>, coid: &str, side: i32, qty: f64, px: f64) {
    for ev in fill_events(coid, "sim", "BTCUSDT", side, qty, px) {
        c.dispatch(Ingest::Event(ev));
    }
}

/// Walk `coid` from `Initialized` to `Accepted` exactly as a real adapter does — the emitter
/// split's synchronous `[OrderSubmitted, OrderAccepted]` pair.
///
/// ⚠ REQUIRED before cancelling/expiring a resting order in these tests. `OrderCanceled` is
/// legal only from `Accepted`/`Triggered`/`PartiallyFilled`/`PendingCancel` — NEVER from
/// `Initialized` — and `RecordingClient` emits nothing of its own, so an order it "submitted"
/// sits at `Initialized` until a test says otherwise. Same fixture gap `fill_events` had: while
/// the contingency drive ignored the fold's verdict, cancelling an `Initialized` order still
/// drove the cascade, so the shortcut was invisible.
fn accept(c: &mut CoreThread<RecordingClient>, coid: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
        client_order_id: coid.to_string(),
        ts: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderAccepted(vike_model::events::OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(format!("v-{coid}").into()),
        ts: 0,
    })));
}

#[test]
fn oto_entry_fill_releases_both_held_exits() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    assert_eq!(c.engine.client.submissions.len(), 1, "only the entry is live pre-fill");
    // OTO: the entry's fill arms + releases both protective exits to the venue.
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let submitted: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
    assert!(
        submitted.contains(&sl) && submitted.contains(&tp),
        "both exits released: {submitted:?}"
    );
    assert!(c.held_orders.is_empty(), "nothing left held once the parent filled");
    assert!(
        !c.contingency.is_held(&sl) && !c.contingency.is_held(&tp),
        "the released exits are armed (active) in the book"
    );
}

#[test]
fn oco_stop_fill_cancels_the_take_profit() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms sl + tp
    feed_full_fill(&mut c, &sl, -1, 2.0, 95.0); // OCO direction 1: the stop fills
    assert!(c.engine.client.cancels.contains(&tp), "the stop's fill cancels the take-profit");
    assert!(!c.engine.client.cancels.contains(&sl), "the filled leg is never self-canceled");
    assert!(c.contingency.is_empty(), "the whole group resolved");
}

#[test]
fn oco_take_profit_fill_cancels_the_stop() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms sl + tp
    feed_full_fill(&mut c, &tp, -1, 2.0, 110.0); // OCO direction 2: the take-profit fills
    assert!(c.engine.client.cancels.contains(&sl), "the take-profit's fill cancels the stop");
    assert!(c.contingency.is_empty(), "the whole group resolved");
}

// ---- ADVERSARIAL: the contingency drive under a HOSTILE venue --------------------------
//
// The threat model and the conventions for this class of test are stated once, in
// `crates/vike-exec/tests/engine/hostile_venue_fold.rs`'s module doc. In short: TLS is verified, so
// this is not a man-in-the-middle — the actor is the VENUE ITSELF, returning well-formed frames
// with fabricated contents.
//
// These are the adversarial twins of the legitimate drive tests directly above. The property is
// narrow and total: an event the ENGINE DROPPED must drive NOTHING. Before the `Fold` verdict
// gated the drive, `contingency_terminal` classified the event ALONE, so the core law "invalid
// transitions are dropped" did not extend to the bracket/OCO/OTO machinery at all.

/// The bare `FillEvent` out of `fill_events` for `coid` — the wrap's embedded copy.
fn bare_fill(coid: &str, side: i32, qty: f64, px: f64) -> vike_model::events::FillEvent {
    fill_events(coid, "sim", "BTCUSDT", side, qty, px)
        .into_iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .expect("fill_events yields a bare Fill")
}

// ⚠ WHERE THE ASYMMETRY ACTUALLY IS — this is what makes the drive reachable with a coid the
// FSM refuses, and the first two attempts at these tests were VACUOUS for missing it.
//
// A bracket's protective exits are HELD: `apply_intent` puts them in `held_orders` + the
// contingency book and deliberately never calls `submit_order` for them, so they are NOT in
// `ExecutionEngine::registry`. That is the gap: the CONTINGENCY BOOK knows those coids while the
// ORDER REGISTRY does not. A fabricated terminal naming a held exit is therefore dropped by the
// fold (`dropped_unknown_coid` moves) AND was still driven by the contingency machinery.
//
// Forging a coid that exists in NEITHER (`"{tp}-FORGED"`) proves nothing — `on_fill` on an
// unknown coid is a no-op whether or not the gate is there, so such a test passes with the fix
// reverted. Always mutation-check an adversarial test; a green one may simply be inert.

/// A fabricated `OrderFilled` naming the HELD take-profit. The FSM refuses it (that coid was
/// never registered), but the contingency book knows it — so before the gate it ran the OCO
/// sibling-cancel and **silently destroyed the held STOP-LOSS**, leaving the bracket with no
/// downside protection to release when the entry eventually fills.
#[test]
fn a_fabricated_fill_on_a_held_exit_does_not_destroy_its_oco_sibling() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (sl, tp) = (coids[1].clone(), coids[2].clone());
    assert_eq!(c.held_orders.len(), 2, "precondition: both exits held");
    assert!(!c.engine.registry.contains_key(&tp), "precondition: a held exit is UNREGISTERED");

    c.dispatch(Ingest::Event(Event::OrderFilled(vike_model::events::OrderFilled {
        client_order_id: tp.clone(),
        fill: bare_fill(&tp, -1, 2.0, 110.0),
        ts: 1,
    })));

    assert!(c.engine.dropped_unknown_coid > 0, "the fold must have REFUSED the forged fill");
    assert!(
        c.held_orders.contains_key(&sl),
        "the STOP-LOSS must survive a refused fill — losing it leaves the bracket unprotected"
    );
    assert!(c.held_orders.contains_key(&tp), "and the take-profit too");
    assert_eq!(c.held_orders.len(), 2, "the group is untouched");
    assert!(c.contingency.is_held(&sl) && c.contingency.is_held(&tp), "book untouched");
}

/// The terminal-without-fill half of the same hole: a fabricated `OrderCanceled` naming a held
/// exit ran `drive_contingency_on_terminal`, which drops that leg from the held map AND the
/// book — so the take-profit simply VANISHES and is never released when the entry fills.
#[test]
fn a_fabricated_cancel_on_a_held_exit_does_not_remove_it_from_the_bracket() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: tp.clone(),
        reason: "forged".to_string().into(),
        ts: 1,
    })));

    assert!(c.engine.dropped_unknown_coid > 0, "the fold must have REFUSED the forged cancel");
    assert!(c.held_orders.contains_key(&tp), "the take-profit must NOT be dropped");
    assert!(c.contingency.is_held(&tp), "and must still be in the book");

    // ...and it is still there to be released when the entry genuinely fills.
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let submitted: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
    assert!(submitted.contains(&tp), "the take-profit still releases: {submitted:?}");
    assert!(submitted.contains(&sl), "and so does the stop-loss");
}

/// THE MUTATION SENTINEL for the three tests above: a gate that suppressed EVERYTHING would
/// pass all of them and fail only this. An event the engine ACCEPTS must drive exactly what it
/// drove before — the fix is "a dropped event drives nothing", never "drive less".
#[test]
fn an_accepted_terminal_still_drives_the_contingency_exactly_as_before() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

    // OTO: a REAL entry fill still releases both held exits to the venue.
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let submitted: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
    assert!(submitted.contains(&sl) && submitted.contains(&tp), "both released: {submitted:?}");
    assert!(c.held_orders.is_empty(), "nothing left held");

    // OCO: a REAL stop fill still cancels the take-profit.
    feed_full_fill(&mut c, &sl, -1, 2.0, 95.0);
    assert!(c.engine.client.cancels.contains(&tp), "the real fill still cancels the sibling");
    assert!(c.contingency.is_empty(), "the group still resolves");
}

#[test]
fn oto_entry_terminating_unfilled_drops_the_held_exits() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let entry = coids[0].clone();
    assert_eq!(c.held_orders.len(), 2, "sl + tp held");
    // The entry is canceled before it ever fills — its held children can never arm, so the
    // cascade drops them (they were never at the venue, so nothing to cancel there). It has to
    // REACH the venue first: a cancel is only legal on an accepted order (see `accept`).
    accept(&mut c, &entry);
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: entry,
        reason: "user".to_string().into(),
        ts: 0,
    })));
    assert!(c.held_orders.is_empty(), "held exits dropped when the parent terminates unfilled");
    assert!(c.contingency.is_empty(), "no orphaned linkage left behind");
}

#[test]
fn plain_order_is_inert_no_contingency_state_or_cancels() {
    let mut c = test_core();
    // a link-free order: submitted straight through, nothing enters the contingency book.
    let coids = c.apply_intent(OrderIntent::Submit(market_req("p1")), 0);
    assert_eq!(coids, vec!["p1".to_string()]);
    assert_eq!(c.engine.client.submissions.len(), 1, "plain order goes live immediately");
    assert!(c.contingency.is_empty() && c.held_orders.is_empty(), "no contingency state");
    // its fill drives nothing (the byte-identical no-bracket path).
    feed_full_fill(&mut c, "p1", 1, 1.0, 100.0);
    assert!(c.engine.client.cancels.is_empty(), "a plain fill cancels nothing");
    assert!(c.contingency.is_empty());
}

#[test]
fn denied_bracket_entry_drops_its_held_exits_not_orphans_them() {
    // The CRITICAL leak: a bracket entry vetoed by the RiskGate (synchronous `OrderDenied`)
    // must cascade-drop its held exits. Before the fix they were orphaned forever — never armed
    // (no fill ever comes), never removed, re-captured in every Snap.
    let mut c = test_core();
    c.engine.trading_state = TradingState::Halted; // the RiskGate vetoes every new order
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    assert_eq!(coids.len(), 3, "coids are still minted+returned");
    assert!(c.engine.client.submissions.is_empty(), "Halted: nothing reaches the venue");
    assert!(c.held_orders.is_empty(), "the denied entry's held exits are DROPPED, not orphaned");
    assert!(c.contingency.is_empty(), "no orphaned contingency linkage survives the denial");
}

#[test]
fn cancel_a_held_bracket_exit_drops_it_and_never_hits_the_venue() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (sl, tp) = (coids[1].clone(), coids[2].clone());
    assert_eq!(c.held_orders.len(), 2);
    c.apply_intent(OrderIntent::Cancel(sl.clone()), 0);
    assert!(
        !c.held_orders.contains_key(&sl) && !c.contingency.contains(&sl),
        "the canceled held exit is gone from both the held map and the book"
    );
    assert!(c.held_orders.contains_key(&tp), "its sibling stays held");
    assert!(c.engine.client.cancels.is_empty(), "a held cancel never reaches the venue");
}

#[test]
fn modify_a_held_bracket_exit_updates_the_terms_it_is_released_with() {
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, tp) = (coids[0].clone(), coids[2].clone());
    c.apply_intent(
        OrderIntent::Modify {
            client_order_id: tp.clone(),
            new_qty: Some(5.0),
            new_price: Some(115.0),
        },
        0,
    );
    let held = c.held_orders.get(&tp).expect("still held");
    assert_eq!(held.qty, 5.0);
    assert_eq!(held.price, Some(115.0));
    // and those updated terms are exactly what get released to the venue when the entry fills
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
    let released = c
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.client_order_id == tp)
        .expect("the take-profit was released");
    assert_eq!(released.qty, 5.0, "the release carries the modified qty");
    assert_eq!(released.price, Some(115.0), "and the modified price");
}

#[test]
fn a_released_exit_dying_unfilled_keeps_the_surviving_sibling() {
    // KNOB OFF (the default): a protective exit that dies unfilled (venue reject/cancel/expire)
    // cleans its OWN stale book entry, but the surviving OCO sibling is KEPT — a position that
    // just lost one protective leg should retain whatever protection it still has. This is the
    // inert default of `CoreConfig::oco_cancel_sibling_on_dead_exit` (`test_core` builds a
    // default config); the knob-ON twin below flips it.
    let mut c = test_core();
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms + releases sl + tp
    accept(&mut c, &sl); // the released stop-loss reaches the venue and rests there
    // the venue cancels the released stop-loss without it ever filling
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: sl.clone(),
        reason: "venue".to_string().into(),
        ts: 0,
    })));
    assert!(!c.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
    assert!(c.contingency.contains(&tp), "the surviving take-profit is KEPT (protection stays)");
    assert!(!c.engine.client.cancels.contains(&tp), "the sibling is NOT auto-canceled");
}

#[test]
fn a_released_exit_dying_unfilled_cancels_the_sibling_when_the_knob_is_on() {
    // KNOB ON (`CoreConfig::oco_cancel_sibling_on_dead_exit = true`): the same dead released
    // stop-loss now ALSO cancels its surviving OCO take-profit AND cleans its book entry — the
    // fully-flat-book deployment choice. Everything up to the death is identical to the OFF
    // twin above; only the sibling's fate differs.
    let mut c =
        core_with(CoreConfig { oco_cancel_sibling_on_dead_exit: true, ..Default::default() });
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms + releases sl + tp to the venue
    accept(&mut c, &sl); // the released stop-loss reaches the venue and rests there
    // the venue cancels the released stop-loss without it ever filling
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: sl.clone(),
        reason: "venue".to_string().into(),
        ts: 0,
    })));
    assert!(!c.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
    assert!(
        !c.contingency.contains(&tp),
        "knob ON: the surviving take-profit is CANCELED and cleaned from the book"
    );
    assert!(
        c.engine.client.cancels.contains(&tp),
        "knob ON: the surviving OCO sibling is canceled at the venue"
    );
    assert!(c.contingency.is_empty(), "the whole group resolved — the book is left flat");
}
