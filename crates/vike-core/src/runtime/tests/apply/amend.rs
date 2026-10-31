//! The unified amend of a resting order (`crate::runtime::apply::amend`, decision 0244), against a fake venue
//! (`RecordingClient` plus the events a test feeds): native where the venue amends in place, and
//! cancel-then-new where it cannot, in its two orderings (a reduce-only stop NEW-FIRST, a limit
//! CANCEL-FIRST), with the five outcomes each needs — replace ok, the new order refused, the cancel late,
//! the venue silent, a fill in the overlap.

use super::*;
use vike_exec::amend::{CANCEL_WAIT_MS, PLACE_WAIT_MS};
use vike_model::OrderStatus;
use vike_model::events::{OrderCanceled, OrderRejected, OrderSubmitted};

type Core = CoreThread<RecordingClient>;

/// A core on a venue whose perpetual STOP cannot be amended in place (binance's Algo Service).
fn binance_core() -> Core {
    core_of(
        engine_on("binance", "BTCUSDT.P", RecordingClient::default()),
        vec![],
        CoreConfig::default(),
    )
}

/// A core on a venue with no amend at all (a polymarket order is only ever cancelled and placed again).
fn polymarket_core() -> Core {
    core_of(
        engine_on("polymarket", "TOKEN", RecordingClient::default()),
        vec![],
        CoreConfig::default(),
    )
}

fn stop(venue: &str, symbol: &str, reduce_only: bool) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        venue: venue.into(),
        symbol: symbol.into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(90_000.0),
        reduce_only,
        ..Default::default()
    })
}

fn limit(venue: &str, symbol: &str, price: f64) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 2.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        ..Default::default()
    })
}

/// Walk `coid` to ACCEPTED as a real adapter does (`OrderSubmitted`, then `OrderAccepted`).
fn accept(c: &mut Core, coid: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
        client_order_id: coid.to_string(),
        ts: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderAccepted(vike_model::events::OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(format!("v-{coid}").into()),
        ts: 0,
    })));
}

/// Place `req` and walk it to ACCEPTED, as a venue does: the coid.
fn place(c: &mut Core, req: Box<OrderRequest>) -> String {
    let coid = c.apply_intent(OrderIntent::Submit(req), 0).remove(0);
    accept(c, &coid);
    coid
}

fn modify(c: &mut Core, coid: &str, new_qty: Option<f64>, new_price: Option<f64>) {
    c.apply_intent(
        OrderIntent::Modify { client_order_id: coid.to_string(), new_qty, new_price },
        10,
    );
}

fn status(c: &Core, coid: &str) -> Option<OrderStatus> {
    c.engine.registry.get(coid).map(|mo| mo.status)
}

fn canceled(c: &mut Core, coid: &str) {
    c.dispatch(Ingest::Event(Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: "venue".to_string().into(),
        ts: 0,
    })));
}

fn rejected(c: &mut Core, coid: &str, reason: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
        client_order_id: coid.to_string(),
        ts: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderRejected(OrderRejected {
        cause: None,
        client_order_id: coid.to_string(),
        reason: reason.to_string().into(),
        ts: 0,
    })));
}

/// The ring lines that mention `needle`.
fn said(c: &Core, needle: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

/// The coid the replacement was placed under: the newest submission that is not `old`.
fn replacement(c: &Core, old: &str) -> String {
    c.engine
        .client
        .submissions
        .iter()
        .rev()
        .find(|r| r.client_order_id != old)
        .expect("a replacement was placed")
        .client_order_id
        .clone()
}

// ---- native ------------------------------------------------------------------------------------

/// A venue that amends in place gets today's path, byte for byte: the client's `modify`, no second
/// order, no amend in flight.
#[test]
fn a_native_modify_is_sent_as_a_modify_and_starts_no_amend() {
    let mut c = binance_core();
    let l1 = place(&mut c, limit("binance", "BTCUSDT.P", 50_000.0));
    modify(&mut c, &l1, None, Some(50_100.0));
    assert_eq!(c.engine.client.modifies.len(), 1, "the limit amends in place on binance");
    assert_eq!(c.engine.client.submissions.len(), 1, "no replacement");
    assert!(c.engine.client.cancels.is_empty());
    assert!(c.amends.is_empty());
}

/// The paper exchange declares its own convention, so it is judged by it: native for every kind,
/// whatever the venue string it is mounted under says.
#[test]
fn a_paper_client_is_native_for_every_kind_whatever_its_venue_string() {
    let client = RecordingClient {
        declared_amend_semantics: Some(vike_venues::AmendSemantics::InPlaceRemaining),
        ..Default::default()
    };
    let mut c = core_of(engine_on("polymarket", "TOKEN", client), vec![], CoreConfig::default());
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    assert_eq!(
        c.engine.client.modifies.len(),
        1,
        "native, although polymarket itself cannot amend"
    );
    assert!(c.amends.is_empty() && c.engine.client.cancels.is_empty());
}

// ---- new-first: a reduce-only stop on a venue that cannot move it ----------------------------------

/// REPLACE-OK. The new stop is placed FIRST; the old one is cancelled only once the new one is
/// answered, and is gone before the amend ends. At no point is the position without a stop.
#[test]
fn a_reduce_only_stop_is_placed_first_and_the_old_one_cancelled_after() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));

    // the new stop is out with the new trigger; the old one is untouched
    assert_eq!(c.engine.client.submissions.len(), 2);
    let new = &c.engine.client.submissions[1];
    assert_eq!(new.trigger_price, Some(91_000.0), "the amend's price is the TRIGGER of a stop");
    assert!(new.reduce_only && new.order_type == vike_model::OrderType::Stop);
    assert_ne!(new.client_order_id, s1, "a fresh coid, minted by the node");
    assert!(
        c.engine.client.cancels.is_empty(),
        "the old stop stands until the new one is answered"
    );
    assert!(c.engine.client.modifies.is_empty(), "no native modify was attempted");
    assert_eq!(c.amends.len(), 1);
    let n = new.client_order_id.clone();

    // not answered yet: still nothing cancelled
    c.drive_amends(11);
    assert!(c.engine.client.cancels.is_empty());

    // answered: now the old one is cancelled
    accept(&mut c, &n);
    c.drive_amends(12);
    assert_eq!(c.engine.client.cancels, vec![s1.clone()]);
    assert_eq!(status(&c, &s1), Some(OrderStatus::Accepted), "still resting until the venue says");

    // the venue confirms: the amend ends, both orders carry the lineage
    canceled(&mut c, &s1);
    c.drive_amends(13);
    assert!(c.amends.is_empty());
    assert_eq!(status(&c, &s1), Some(OrderStatus::Canceled));
    assert_eq!(status(&c, &n), Some(OrderStatus::Accepted));
    assert_eq!(c.engine.registry[&n].replaces.as_deref(), Some(s1.as_str()));
    assert_eq!(c.engine.registry[&s1].replaces.as_deref(), Some(s1.as_str()));
    assert_eq!(c.engine.registry[&n].request.trigger_price, Some(91_000.0));
    assert!(!said(&c, "replaced by").is_empty(), "{:?}", c.recent);
}

/// The operator's FIRST coid keeps naming the logical order: a cancel that names it reaches the live
/// replacement, and a second modify that names it amends the replacement.
#[test]
fn the_first_coid_still_names_the_order_after_it_was_replaced() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);
    accept(&mut c, &n);
    c.drive_amends(11);
    canceled(&mut c, &s1);
    c.drive_amends(12);
    assert!(c.amends.is_empty());

    // a second modify naming the FIRST coid amends the live one (a second replacement)
    modify(&mut c, &s1, None, Some(92_000.0));
    assert_eq!(c.engine.client.submissions.len(), 3);
    let n2 = replacement(&c, &n);
    assert_eq!(c.engine.client.submissions[2].trigger_price, Some(92_000.0));
    accept(&mut c, &n2);
    c.drive_amends(13);
    assert_eq!(c.engine.client.cancels.last(), Some(&n), "the old member was the live one, `{n}`");
    canceled(&mut c, &n);
    c.drive_amends(14);
    assert!(c.amends.is_empty());
    assert_eq!(
        c.engine.registry[&n2].replaces.as_deref(),
        Some(s1.as_str()),
        "the chain's root is kept"
    );

    // and a cancel naming the first coid cancels the live member of the chain
    c.apply_intent(OrderIntent::Cancel(s1.clone()), 20);
    assert_eq!(c.engine.client.cancels.last(), Some(&n2));
}

/// REPLACE-NEW-REJECTED. The venue refuses the new stop: the old one is never cancelled and keeps
/// resting, and the words say it STANDS and why.
#[test]
fn a_refused_replacement_stop_leaves_the_old_one_standing_and_says_why() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);

    rejected(&mut c, &n, "Order would immediately trigger");
    c.drive_amends(11);

    assert!(c.amends.is_empty(), "the amend ended");
    assert!(c.engine.client.cancels.is_empty(), "the old stop was never touched");
    assert_eq!(status(&c, &s1), Some(OrderStatus::Accepted));
    let words = said(&c, "STANDS");
    assert!(!words.is_empty(), "{:?}", c.recent);
    assert!(words[0].contains("Order would immediately trigger") && words[0].contains("REFUSED"));
}

/// REPLACE-CANCEL-LATE. The venue does not confirm the cancel of the old stop in time: BOTH rest and
/// the words name the one to cancel by hand. Nothing is cancelled again, nothing is lost.
#[test]
fn a_late_cancel_of_the_old_stop_leaves_both_resting_and_says_which_to_cancel() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);
    accept(&mut c, &n);
    c.drive_amends(11); // cancels the old one
    assert_eq!(c.engine.client.cancels, vec![s1.clone()]);

    c.drive_amends(11 + CANCEL_WAIT_MS + 1);

    assert!(c.amends.is_empty());
    assert_eq!(status(&c, &s1), Some(OrderStatus::Accepted));
    assert_eq!(status(&c, &n), Some(OrderStatus::Accepted));
    let words = said(&c, "BOTH rest");
    assert!(!words.is_empty(), "{:?}", c.recent);
    assert!(words[0].contains(&format!("cancel {s1} by hand")), "{}", words[0]);
}

/// VENUE-DOWN MID-REPLACE. The new stop is never answered: after the wait it is cancelled and the old
/// stop stands.
#[test]
fn a_silent_venue_leaves_the_old_stop_standing() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);

    c.drive_amends(10 + PLACE_WAIT_MS);
    assert_eq!(c.amends.len(), 1, "still waiting at the bound");
    c.drive_amends(10 + PLACE_WAIT_MS + 1);

    assert!(c.amends.is_empty());
    assert_eq!(c.engine.client.cancels, vec![n], "the unanswered replacement is cancelled");
    assert_eq!(status(&c, &s1), Some(OrderStatus::Accepted));
    assert!(!said(&c, "did not answer").is_empty(), "{:?}", c.recent);
}

/// A FILL of the old stop in the overlap: the first to fill wins, the replacement is cancelled.
#[test]
fn an_old_stop_that_fills_in_the_overlap_cancels_the_replacement() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);
    accept(&mut c, &n);
    // the old stop fires before its cancel lands
    for ev in fill_events(&s1, "binance", "BTCUSDT.P", -1, 1.0, 90_000.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(ev));
    }
    assert_eq!(status(&c, &s1), Some(OrderStatus::Filled));
    c.drive_amends(11);

    assert!(c.amends.is_empty());
    assert_eq!(
        c.engine.client.cancels,
        vec![n],
        "the replacement is cancelled, not the filled order"
    );
    assert!(!said(&c, "FILLED").is_empty(), "{:?}", c.recent);
}

/// The operator cancels the order while its stop is being moved: both halves go, nothing is left.
#[test]
fn cancelling_the_order_during_a_stop_amend_cancels_the_replacement_too() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);
    accept(&mut c, &n);

    c.apply_intent(OrderIntent::Cancel(s1.clone()), 11);
    c.drive_amends(12);

    assert!(c.amends.is_empty());
    assert!(c.engine.client.cancels.contains(&s1) && c.engine.client.cancels.contains(&n));
}

/// A second modify while one is in flight is refused in words and starts nothing.
#[test]
fn a_second_modify_during_an_amend_is_refused_in_words() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    modify(&mut c, &s1, None, Some(92_000.0));
    assert_eq!(c.engine.client.submissions.len(), 2, "no second replacement");
    assert_eq!(c.amends.len(), 1);
    assert!(!said(&c, "already in flight").is_empty(), "{:?}", c.recent);
}

/// A bracket's stop-loss is amended new-first and the bracket follows it: the take-profit's sibling is
/// the replacement, so a take-profit fill cancels the replacement.
#[test]
fn a_bracket_stop_is_replaced_without_breaking_its_link() {
    let mut c = binance_core();
    let spec = vike_model::BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    accept(&mut c, &entry);
    for ev in fill_events(&entry, "binance", "BTCUSDT.P", 1, 2.0, 100.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(ev));
    }
    accept(&mut c, &sl);
    accept(&mut c, &tp);

    modify(&mut c, &sl, None, Some(96.0));
    let n = replacement(&c, &sl);
    // until the replacement is answered the old stop is still the bracket's leg
    assert!(c.contingency.contains(&sl) && !c.contingency.contains(&n));
    accept(&mut c, &n);
    c.drive_amends(11);
    assert!(c.contingency.contains(&n) && !c.contingency.contains(&sl), "re-keyed once answered");
    assert_eq!(c.contingency.siblings_of(&n), vec![tp.clone()]);
    canceled(&mut c, &sl);
    c.drive_amends(12);
    assert!(c.amends.is_empty());

    // the take-profit fills: the bracket cancels the stop that is NOW the leg
    for ev in fill_events(&tp, "binance", "BTCUSDT.P", -1, 2.0, 110.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(ev));
    }
    assert!(c.engine.client.cancels.contains(&n), "{:?}", c.engine.client.cancels);
}

/// A bracket's stop-loss that has not reached the venue yet (held until the entry fills) is amended in
/// place, and what is amended is its TRIGGER: it used to take the price as a limit price and was
/// released at the trigger it was placed with.
#[test]
fn a_held_bracket_stop_takes_the_new_trigger_and_is_released_with_it() {
    let mut c = binance_core();
    let spec = vike_model::BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    let (entry, sl) = (coids[0].clone(), coids[1].clone());
    modify(&mut c, &sl, None, Some(93.0));
    let held = c.held_orders.get(&sl).expect("still held");
    assert_eq!(held.trigger_price, Some(93.0), "the trigger moved");
    assert_eq!(held.price, None, "no limit price appeared on a stop");
    accept(&mut c, &entry);
    for ev in fill_events(&entry, "binance", "BTCUSDT.P", 1, 2.0, 100.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(ev));
    }
    let released = c
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.client_order_id == sl)
        .expect("the stop-loss was released");
    assert_eq!(released.trigger_price, Some(93.0), "released at the amended trigger");
    assert_eq!(released.price, None);
}

/// A replacement the venue refuses leaves the bracket as it was: the old stop is still the leg, and the
/// refusal is not read as "the protective exit died unfilled".
#[test]
fn a_refused_bracket_stop_replacement_leaves_the_bracket_untouched() {
    let mut c = binance_core();
    let spec = vike_model::BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
    };
    let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    let (entry, sl) = (coids[0].clone(), coids[1].clone());
    accept(&mut c, &entry);
    for ev in fill_events(&entry, "binance", "BTCUSDT.P", 1, 2.0, 100.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(ev));
    }
    accept(&mut c, &sl);
    modify(&mut c, &sl, None, Some(96.0));
    let n = replacement(&c, &sl);
    rejected(&mut c, &n, "refused");
    c.drive_amends(11);
    assert!(
        c.contingency.contains(&sl) && !c.contingency.contains(&n),
        "the old stop is still the bracket's leg"
    );
    assert!(said(&c, "died unfilled").is_empty(), "no false alarm: {:?}", c.recent);
    assert_eq!(status(&c, &sl), Some(OrderStatus::Accepted));
}

/// A mount's order is replaced through the mount: the route reads the mount and the tag back out of the
/// tag registry's key (`{mount}|{venue}|{symbol}|{tag}`, the tag being the remainder and so free to
/// hold a `|`), and an order no mount minted is an engine's.
#[test]
fn the_route_of_a_mounts_order_is_read_from_its_tag_key() {
    use crate::runtime::apply::amend::AmendRoute;
    let mut c = binance_core();
    c.strategy_tags.insert("3|binance|BTCUSDT.P|ladder|1".to_string(), "c-9".to_string());
    match c.amend_route_of("c-9") {
        AmendRoute::Mount { idx, tag, key } => {
            assert_eq!(idx, 3);
            assert_eq!(tag.as_deref(), Some("ladder|1"));
            assert_eq!(key.as_deref(), Some("3|binance|BTCUSDT.P|ladder|1"));
        }
        other => panic!("expected a mount route, got {other:?}"),
    }
    // minted by a mount but carrying no tag: still the mount's, with no tag to repoint
    c.coid_mount.insert("c-10".to_string(), 2);
    assert!(matches!(c.amend_route_of("c-10"), AmendRoute::Mount { idx: 2, tag: None, key: None }));
    assert!(matches!(c.amend_route_of("operators-own"), AmendRoute::Engine));
}

// ---- cancel-first: a limit order on a venue with no amend --------------------------------------

/// REPLACE-OK. The old limit is cancelled first; only when the venue says it is gone is the new one
/// placed, and the words say the place in the queue is lost.
#[test]
fn a_limit_is_cancelled_first_and_placed_when_the_old_one_is_gone() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));

    assert_eq!(c.engine.client.cancels, vec![l1.clone()], "cancel first");
    assert_eq!(c.engine.client.submissions.len(), 1, "nothing placed yet");
    assert!(!said(&c, "PLACE IN THE QUEUE").is_empty(), "{:?}", c.recent);

    c.drive_amends(11); // the cancel is not confirmed: still nothing placed
    assert_eq!(c.engine.client.submissions.len(), 1);

    canceled(&mut c, &l1);
    c.drive_amends(12);
    assert_eq!(c.engine.client.submissions.len(), 2);
    let new = c.engine.client.submissions[1].clone();
    assert_eq!(new.price, Some(0.45));
    assert_eq!(new.qty, 2.0, "the same size");
    assert_ne!(new.client_order_id, l1);

    accept(&mut c, &new.client_order_id);
    c.drive_amends(13);
    assert!(c.amends.is_empty());
    assert_eq!(c.engine.registry[&new.client_order_id].replaces.as_deref(), Some(l1.as_str()));
    assert!(!said(&c, "replaced by").is_empty(), "{:?}", c.recent);
}

/// A fill that beat the cancel: the order is done, nothing is placed twice, the words say it filled.
#[test]
fn a_limit_that_fills_before_its_cancel_lands_is_not_placed_again() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    for ev in fill_events(&l1, "polymarket", "TOKEN", 1, 2.0, 0.40).into_iter().skip(2) {
        c.dispatch(Ingest::Event(ev));
    }
    c.drive_amends(11);
    assert!(c.amends.is_empty());
    assert_eq!(c.engine.client.submissions.len(), 1, "nothing was placed");
    assert!(!said(&c, "FILLED").is_empty(), "{:?}", c.recent);
}

/// REPLACE-NEW-REJECTED, cancel-first: the old order IS gone and the words say so in capitals.
#[test]
fn a_refused_new_limit_after_the_cancel_says_the_order_is_gone() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    canceled(&mut c, &l1);
    c.drive_amends(11);
    let n = replacement(&c, &l1);
    rejected(&mut c, &n, "price out of range");
    c.drive_amends(12);
    assert!(c.amends.is_empty());
    let words = said(&c, "GONE");
    assert!(!words.is_empty(), "{:?}", c.recent);
    assert!(words[0].contains("price out of range"), "{}", words[0]);
}

/// REPLACE-CANCEL-LATE, cancel-first: the cancel is slow. The order is placed anyway when the cancel
/// finally lands; before that, nothing is placed and the amend says it is still waiting.
#[test]
fn a_slow_cancel_still_places_the_new_limit_when_it_lands() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    c.drive_amends(10 + CANCEL_WAIT_MS + 1);
    assert_eq!(c.amends.len(), 1, "still waiting for the venue");
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert!(!said(&c, "not confirmed").is_empty(), "{:?}", c.recent);
    canceled(&mut c, &l1);
    c.drive_amends(10 + CANCEL_WAIT_MS + 2);
    assert_eq!(c.engine.client.submissions.len(), 2, "placed when the cancel landed");
}

/// VENUE-DOWN MID-REPLACE, cancel-first: the cancel landed, the new limit is never answered. Reported,
/// not retried (a second copy is the thing that must not happen).
#[test]
fn a_silent_venue_after_the_cancel_is_reported_and_not_retried() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    canceled(&mut c, &l1);
    c.drive_amends(11);
    c.drive_amends(11 + PLACE_WAIT_MS + 1);
    assert!(c.amends.is_empty());
    assert_eq!(c.engine.client.submissions.len(), 2, "one replacement, never a second");
    assert!(!said(&c, "check the open orders").is_empty(), "{:?}", c.recent);
}

/// The operator cancels a limit while it is being moved: it is not placed again.
#[test]
fn cancelling_a_limit_during_its_amend_places_nothing() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    c.apply_intent(OrderIntent::Cancel(l1.clone()), 11);
    canceled(&mut c, &l1);
    c.drive_amends(12);
    assert!(c.amends.is_empty());
    assert_eq!(
        c.engine.client.submissions.len(),
        1,
        "nothing was placed after the operator's cancel"
    );
}

/// A cancel-first amend of a bracket's leg would break the group's link, so it is refused in words.
#[test]
fn a_bracket_limit_exit_on_a_venue_with_no_amend_is_refused_in_words() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    // a recorded leg of a contingency group (a bracket's take-profit looks like this to the book)
    c.contingency.insert(l1.clone(), None, vec!["x".to_string()]);
    modify(&mut c, &l1, None, Some(0.45));
    assert!(c.amends.is_empty() && c.engine.client.cancels.is_empty());
    assert!(!said(&c, "leg of a bracket").is_empty(), "{:?}", c.recent);
}

/// A stop the NODE holds (the venue rests none) is named by its arm id and has no order in the registry:
/// a modify of it used to vanish without a word. It is refused, saying how to move it.
#[test]
fn a_modify_of_a_core_held_stop_is_refused_in_words_not_swallowed() {
    let mut c = polymarket_core();
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            side: -1,
            qty: 2.0,
            order_type: vike_model::OrderType::Stop,
            trigger_price: Some(0.30),
            reduce_only: true,
            ..Default::default()
        })),
        0,
    );
    let arm_id = c.cond_engine.keys().next().cloned().expect("the stop was armed by the core");
    modify(&mut c, &arm_id, None, Some(0.25));
    assert!(!said(&c, "a stop this node holds itself").is_empty(), "{:?}", c.recent);
    assert!(c.engine.client.modifies.is_empty() && c.amends.is_empty());
    assert!(c.cond_engine.contains_key(&arm_id), "the stop is still armed");
}

/// A modify naming neither a quantity nor a price is refused in words, on a cancel-then-new venue.
#[test]
fn a_modify_that_names_nothing_is_refused_in_words() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, None);
    assert!(c.amends.is_empty() && c.engine.client.cancels.is_empty());
    assert!(!said(&c, "neither a quantity nor a price").is_empty(), "{:?}", c.recent);
}

/// The new order is judged by the RiskGate like any modify: a veto is a non-terminal refusal and the
/// old order is never cancelled.
#[test]
fn a_replacement_the_gate_vetoes_leaves_the_old_order_alone() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    c.engine.gate =
        RiskGate::new(RiskLimits { max_notional_per_order: Some(1.0), ..RiskLimits::new() });
    modify(&mut c, &l1, Some(1000.0), None);
    assert!(c.amends.is_empty());
    assert!(c.engine.client.cancels.is_empty(), "a vetoed amend cancels nothing");
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert_eq!(status(&c, &l1), Some(OrderStatus::Accepted));
}
