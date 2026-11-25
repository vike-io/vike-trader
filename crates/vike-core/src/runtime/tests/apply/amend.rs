//! The unified amend of a resting order (`crate::runtime::apply::amend`, decision 0244), against a fake venue
//! (`RecordingClient` plus the events a test feeds): native where the venue amends in place, and
//! cancel-then-new where it cannot, in its two orderings (a reduce-only stop NEW-FIRST, a limit
//! CANCEL-FIRST), with the five outcomes each needs — replace ok, the new order refused, the cancel late,
//! the venue silent, a fill in the overlap.

use super::*;
use vike_execution::amend::{CANCEL_WAIT_MS, PLACE_WAIT_MS};
use vike_model::OrderStatus;
use vike_model::events::{OrderCanceled, OrderRejected, OrderSubmitted};

type Core = CoreThread<RecordingClient>;

/// A core of one `RecordingClient` engine on `venue`/`symbol`, the default config.
fn core_on(venue: &str, symbol: &str) -> Core {
    core_of(engine_on(venue, symbol, RecordingClient::default()), vec![], CoreConfig::default())
}

/// A core on a venue whose perpetual STOP cannot be amended in place (binance's Algo Service).
fn binance_core() -> Core {
    core_on("binance", "BTCUSDT.P")
}

/// A core on a venue with no amend at all (a polymarket order is only ever cancelled and placed again).
fn polymarket_core() -> Core {
    core_on("polymarket", "TOKEN")
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

/// Place `req` and walk it to ACCEPTED, as a venue does: the order client id.
fn place(c: &mut Core, req: Box<OrderRequest>) -> String {
    let order_client_id = c.apply_intent(OrderIntent::Submit(req), 0).remove(0);
    accept(c, &order_client_id);
    order_client_id
}

fn modify(c: &mut Core, order_client_id: &str, new_qty: Option<f64>, new_price: Option<f64>) {
    c.apply_intent(
        OrderIntent::Modify {
            order_client_id: order_client_id.to_string(),
            new_qty,
            new_price,
            new_trigger_price: None,
        },
        10,
    );
}

fn status(c: &Core, order_client_id: &str) -> Option<OrderStatus> {
    c.engine.registry.get(order_client_id).map(|mo| mo.status)
}

fn canceled(c: &mut Core, order_client_id: &str) {
    c.dispatch(Ingest::Event(Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: "venue".to_string().into(),
        timestamp: 0,
        post_only_would_cross: false,
        refusal_id: None,
    })));
}

fn rejected(c: &mut Core, order_client_id: &str, reason: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order_client_id.to_string(),
        reason: reason.to_string().into(),
        timestamp: 0,
        refusal_id: None,
    })));
}

/// The ring lines that mention `needle`.
fn said(c: &Core, needle: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

/// The order client id the replacement was placed under: the newest submission that is not `old`.
fn replacement(c: &Core, old: &str) -> String {
    c.engine
        .client
        .submissions
        .iter()
        .rev()
        .find(|r| r.order_client_id != old)
        .expect("a replacement was placed")
        .order_client_id
        .clone()
}

// ---- the trigger amend -------------------------------------------------------------------------

/// A reduce-only SELL stop-limit of 1 on `venue`'s `symbol`: trigger 90 000, limit 89 500.
fn stop_limit(venue: &str, symbol: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        order_type: vike_model::OrderType::StopLimit,
        price: Some(89_500.0),
        ..*stop(venue, symbol, true)
    })
}

/// A trigger amend rides the table's TRIGGER answer (`Engine::trigger_amend_mode_of`): in place
/// where it says native (a client that declares its own convention, the paper exchange: the client's
/// `modify_trigger`, no plain `modify`); refused in words, nothing sent, where the adapter amends the kind
/// in place but its trigger move is unproven (deribit's row); and where the venue cannot amend the kind
/// (binance's Algo Service) a NEW reduce-only stop-limit at the new trigger, placed first, keeping the limit.
#[test]
fn a_trigger_amend_is_sent_in_place_or_placed_again_at_the_new_trigger() {
    let move_trigger = |c: &mut Core, order_client_id: &str| {
        c.apply_intent(
            OrderIntent::Modify {
                order_client_id: order_client_id.to_string(),
                new_qty: None,
                new_price: None,
                new_trigger_price: Some(91_000.0),
            },
            10,
        );
    };
    let paper = RecordingClient {
        declared_amend_semantics: Some(vike_venues::AmendSemantics::InPlaceRemaining),
        ..RecordingClient::default()
    };
    let mut c = core_of(engine_on("deribit", "X", paper), vec![], CoreConfig::default());
    let s1 = place(&mut c, stop_limit("deribit", "X"));
    move_trigger(&mut c, &s1);
    assert_eq!(c.engine.client.trigger_modifies, vec![(s1, None, None, 91_000.0)]);
    assert!(c.engine.client.modifies.is_empty() && c.amends.is_empty());

    let mut c = core_on("deribit", "X");
    let s1 = place(&mut c, stop_limit("deribit", "X"));
    move_trigger(&mut c, &s1);
    assert!(c.engine.client.trigger_modifies.is_empty() && c.engine.client.modifies.is_empty());
    assert!(c.amends.is_empty() && c.engine.client.submissions.len() == 1, "nothing was sent");
    assert_eq!(said(&c, "cannot move a resting order's trigger").len(), 1, "refused in words");

    let mut c = binance_core();
    let s1 = place(&mut c, stop_limit("binance", "BTCUSDT.P"));
    move_trigger(&mut c, &s1);
    let new = replacement(&c, &s1);
    let placed = c.engine.client.submissions.last().expect("placed");
    assert_eq!(placed.order_client_id, new);
    assert_eq!(
        (placed.order_type.clone(), placed.price, placed.trigger_price, placed.reduce_only),
        (vike_model::OrderType::StopLimit, Some(89_500.0), Some(91_000.0), true)
    );
    assert!(c.engine.client.cancels.is_empty(), "a reduce-only stop-limit is placed new-first");
    assert!(c.engine.client.trigger_modifies.is_empty(), "nothing is amended in place");
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
    assert_ne!(new.order_client_id, s1, "a fresh order client id, minted by the node");
    assert!(
        c.engine.client.cancels.is_empty(),
        "the old stop stands until the new one is answered"
    );
    assert!(c.engine.client.modifies.is_empty(), "no native modify was attempted");
    assert_eq!(c.amends.len(), 1);
    let n = new.order_client_id.clone();

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

/// **On a SPOT (cash) symbol the new-first replacement is not a second sell.** Over a long of 2, a resting
/// reduce-only stop sell of 2 on binance (whose stop is cancel-then-new) moved to a new trigger: the replacement is
/// placed while the old stop is still live, and the engine's spot rule leaves the order it replaces out of the sells
/// already out (`Engine::replacing`), so it is NOT refused; the old stop is cancelled once the new one is
/// answered. A plain sell of 2 beside the resting stop is still refused in the rule's words.
#[test]
fn on_a_spot_symbol_a_reduce_only_stops_replacement_is_placed_and_a_second_sell_is_refused() {
    let mut c = core_on("binance", "BTCUSDT");
    c.engine.account.admit_margin_mode("BTCUSDT", Some(vike_model::MarginMode::Cash));
    c.engine.account.positions.insert(
        ("binance".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() },
    );
    c.engine.collect_applied_fills = true;
    let s1 =
        place(&mut c, Box::new(OrderRequest { qty: 2.0, ..*stop("binance", "BTCUSDT", true) }));
    modify(&mut c, &s1, None, Some(91_000.0));

    assert_eq!(c.engine.client.submissions.len(), 2, "the replacement was placed: {:?}", c.recent);
    let n = replacement(&c, &s1);
    assert!(c.engine.client.cancels.is_empty(), "new-first: the old stop still stands");
    assert!(c.engine.replacing.is_none(), "the link is cleared after the submit");
    accept(&mut c, &n);
    c.drive_amends(12);
    assert_eq!(c.engine.client.cancels, vec![s1.clone()], "the old stop is cancelled after");
    canceled(&mut c, &s1);
    c.drive_amends(13);
    assert!(c.amends.is_empty());
    assert_eq!(status(&c, &n), Some(OrderStatus::Accepted));

    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        20,
    );
    assert_eq!(c.engine.client.submissions.len(), 2, "the second sell never reached the venue");
    let denied: Vec<String> = c
        .engine
        .order_events
        .iter()
        .filter_map(|e| match &e.event.kind {
            vike_model::OrderEventKind::Denied { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        denied,
        ["a spot account cannot sell more than it holds: it has 0, this sells 2; nothing was sent"]
    );
}

/// The operator's FIRST order client id keeps naming the logical order: a cancel that names it reaches the live
/// replacement, and a second modify that names it amends the replacement.
#[test]
fn the_first_order_client_id_still_names_the_order_after_it_was_replaced() {
    let mut c = binance_core();
    let s1 = place(&mut c, stop("binance", "BTCUSDT.P", true));
    modify(&mut c, &s1, None, Some(91_000.0));
    let n = replacement(&c, &s1);
    accept(&mut c, &n);
    c.drive_amends(11);
    canceled(&mut c, &s1);
    c.drive_amends(12);
    assert!(c.amends.is_empty());

    // a second modify naming the FIRST order client id amends the live one (a second replacement)
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

    // and a cancel naming the first order client id cancels the live member of the chain
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
    for event in fill_events(&s1, "binance", "BTCUSDT.P", -1, 1.0, 90_000.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(event));
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
    let spec = vike_model::OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let order_client_ids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    let (entry, stop_loss, take_profit) =
        (order_client_ids[0].clone(), order_client_ids[1].clone(), order_client_ids[2].clone());
    accept(&mut c, &entry);
    for event in fill_events(&entry, "binance", "BTCUSDT.P", 1, 2.0, 100.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(event));
    }
    accept(&mut c, &stop_loss);
    accept(&mut c, &take_profit);

    modify(&mut c, &stop_loss, None, Some(96.0));
    let n = replacement(&c, &stop_loss);
    // until the replacement is answered the old stop is still the bracket's leg
    assert!(c.contingency.contains(&stop_loss) && !c.contingency.contains(&n));
    accept(&mut c, &n);
    c.drive_amends(11);
    assert!(
        c.contingency.contains(&n) && !c.contingency.contains(&stop_loss),
        "re-keyed once answered"
    );
    assert_eq!(c.contingency.siblings_of(&n), vec![take_profit.clone()]);
    canceled(&mut c, &stop_loss);
    c.drive_amends(12);
    assert!(c.amends.is_empty());

    // the take-profit fills: the bracket cancels the stop that is NOW the leg
    for event in
        fill_events(&take_profit, "binance", "BTCUSDT.P", -1, 2.0, 110.0).into_iter().skip(2)
    {
        c.dispatch(Ingest::Event(event));
    }
    assert!(c.engine.client.cancels.contains(&n), "{:?}", c.engine.client.cancels);
}

/// A bracket's stop-loss that has not reached the venue yet (held until the entry fills) is amended in
/// place, and what is amended is its TRIGGER: it used to take the price as a limit price and was
/// released at the trigger it was placed with.
#[test]
fn a_held_bracket_stop_takes_the_new_trigger_and_is_released_with_it() {
    let mut c = binance_core();
    let spec = vike_model::OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let order_client_ids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    let (entry, stop_loss) = (order_client_ids[0].clone(), order_client_ids[1].clone());
    modify(&mut c, &stop_loss, None, Some(93.0));
    let held = c.held_orders.get(&stop_loss).expect("still held");
    assert_eq!(held.trigger_price, Some(93.0), "the trigger moved");
    assert_eq!(held.price, None, "no limit price appeared on a stop");
    accept(&mut c, &entry);
    for event in fill_events(&entry, "binance", "BTCUSDT.P", 1, 2.0, 100.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(event));
    }
    let released = c
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.order_client_id == stop_loss)
        .expect("the stop-loss was released");
    assert_eq!(released.trigger_price, Some(93.0), "released at the amended trigger");
    assert_eq!(released.price, None);
}

/// A replacement the venue refuses leaves the bracket as it was: the old stop is still the leg, and the
/// refusal is not read as "the protective exit died unfilled".
#[test]
fn a_refused_bracket_stop_replacement_leaves_the_bracket_untouched() {
    let mut c = binance_core();
    let spec = vike_model::OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let order_client_ids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
    let (entry, stop_loss) = (order_client_ids[0].clone(), order_client_ids[1].clone());
    accept(&mut c, &entry);
    for event in fill_events(&entry, "binance", "BTCUSDT.P", 1, 2.0, 100.0).into_iter().skip(2) {
        c.dispatch(Ingest::Event(event));
    }
    accept(&mut c, &stop_loss);
    modify(&mut c, &stop_loss, None, Some(96.0));
    let n = replacement(&c, &stop_loss);
    rejected(&mut c, &n, "refused");
    c.drive_amends(11);
    assert!(
        c.contingency.contains(&stop_loss) && !c.contingency.contains(&n),
        "the old stop is still the bracket's leg"
    );
    assert!(said(&c, "died unfilled").is_empty(), "no false alarm: {:?}", c.recent);
    assert_eq!(status(&c, &stop_loss), Some(OrderStatus::Accepted));
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
        AmendRoute::Mount { index, tag, key } => {
            assert_eq!(index, 3);
            assert_eq!(tag.as_deref(), Some("ladder|1"));
            assert_eq!(key.as_deref(), Some("3|binance|BTCUSDT.P|ladder|1"));
        }
        other => panic!("expected a mount route, got {other:?}"),
    }
    // minted by a mount but carrying no tag: still the mount's, with no tag to repoint
    c.order_client_id_mount.insert("c-10".to_string(), 2);
    assert!(matches!(
        c.amend_route_of("c-10"),
        AmendRoute::Mount { index: 2, tag: None, key: None }
    ));
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
    assert_ne!(new.order_client_id, l1);

    accept(&mut c, &new.order_client_id);
    c.drive_amends(13);
    assert!(c.amends.is_empty());
    assert_eq!(c.engine.registry[&new.order_client_id].replaces.as_deref(), Some(l1.as_str()));
    assert!(!said(&c, "replaced by").is_empty(), "{:?}", c.recent);
}

/// A fill that beat the cancel: the order is done, nothing is placed twice, the words say it filled.
#[test]
fn a_limit_that_fills_before_its_cancel_lands_is_not_placed_again() {
    let mut c = polymarket_core();
    let l1 = place(&mut c, limit("polymarket", "TOKEN", 0.40));
    modify(&mut c, &l1, None, Some(0.45));
    for event in fill_events(&l1, "polymarket", "TOKEN", 1, 2.0, 0.40).into_iter().skip(2) {
        c.dispatch(Ingest::Event(event));
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
    c.contingency.insert(l1.clone(), None, None, vec!["x".to_string()]);
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
    let arm_id =
        c.emulated.cond_engine.keys().next().cloned().expect("the stop was armed by the core");
    modify(&mut c, &arm_id, None, Some(0.25));
    assert!(!said(&c, "a stop this node holds itself").is_empty(), "{:?}", c.recent);
    assert!(c.engine.client.modifies.is_empty() && c.amends.is_empty());
    assert!(c.emulated.cond_engine.contains_key(&arm_id), "the stop is still armed");
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
