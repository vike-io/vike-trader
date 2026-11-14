use super::*;
use vike_model::events::{Event, OrderSubmitted};

#[test]
fn event_channel_delivers_events_without_a_core() {
    let (sender, mut rx) = event_channel(4);
    sender
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: "c1".into(),
            timestamp: 7,
        }))
        .expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::OrderSubmitted(e))) => {
            assert_eq!(e.order_client_id, "c1");
            assert_eq!(e.timestamp, 7);
        }
        other => panic!("expected OrderSubmitted ingest, got {other:?}"),
    }
}

use vike_model::events::AccountState;

fn account_state(venue: &str) -> Event {
    Event::AccountState(AccountState {
        venue: venue.into(),
        balances: vec![("USDT".to_string(), 100.0)],
        timestamp: 1,
        route_key: None,
    })
}

fn funding(venue: &str) -> Event {
    Event::Funding(vike_model::events::FundingEvent {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        funding_rate: 0.0001,
        amount: -1.25,
        mark_price: None,
        timestamp: 1,
        route_key: None,
        currency: None,
    })
}

fn liquidation(venue: &str) -> Event {
    Event::PositionLiquidated(vike_model::events::PositionLiquidated {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        qty: 1.0,
        liquidation_price: 90.0,
        fee: 0.1,
        timestamp: 1,
        trade_id: "l1".into(),
        route_key: None,
        fee_currency: None,
    })
}

fn sent(sender: &EventSender, rx: &mut mpsc::Receiver<Ingest>, event: Event) -> AccountState {
    sender.blocking_send(event).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::AccountState(a))) => a,
        other => panic!("expected an AccountState ingest, got {other:?}"),
    }
}

/// **The lane the mount hands a LABELLED account stamps its route key**, so `vike_core`'s
/// router can fold the snapshot into that account's own engine instead of the venue's default.
#[test]
fn a_routed_lane_stamps_a_labelled_accounts_route_key() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    assert_eq!(
        sent(&scoped, &mut rx, account_state("binance")).route_key,
        Some("binance#alt".into())
    );
}

/// **…and the DEFAULT account's lane stamps NOTHING.** `vike_model`'s `route_key_of` renders
/// the bare venue id for `AccountLabel::Default`, so this is the case every single-account box
/// is in — and an emitted key would change its journal bytes. The mount calls `routed`
/// unconditionally precisely because this case is inert.
#[test]
fn the_default_accounts_lane_stamps_nothing() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance");
    assert_eq!(
        sent(&scoped, &mut rx, account_state("binance")).route_key,
        None,
        "a key equal to the payload's own venue says nothing and must not reach the wire"
    );
}

/// An UNSCOPED lane — the one `CoreHandle::event_sender` hands out, and the one every test and
/// in-process producer holds — is byte-identical to its pre-feature self.
#[test]
fn an_unscoped_lane_is_inert() {
    let (plain, mut rx) = event_channel(4);
    assert_eq!(sent(&plain, &mut rx, account_state("binance")).route_key, None);
}

/// An order-client-id-LESS venue-tagged payload is stamped, and there are THREE of them — this is the
/// funding half: the symbol is not an account key (two accounts may share an instrument), so
/// an unstamped funding payment folds into the venue's DEFAULT engine and debits the wrong
/// account's balance.
#[test]
fn a_routed_lane_stamps_a_funding_payment() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    scoped.blocking_send(funding("binance")).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::Funding(f))) => {
            assert_eq!(f.route_key, Some("binance#alt".into()));
        }
        other => panic!("expected a Funding ingest, got {other:?}"),
    }
    // …and the DEFAULT account's lane still stamps nothing, so a single-account box's bytes
    // are unchanged on this payload too.
    let (plain, mut rx) = event_channel(4);
    let default_lane = plain.routed("binance");
    default_lane.blocking_send(funding("binance")).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::Funding(f))) => assert_eq!(f.route_key, None),
        other => panic!("expected a Funding ingest, got {other:?}"),
    }
}

/// …and the LIQUIDATION half, the more damaging of the two: `Account::apply_liquidation`
/// CLOSES the position and books the realized PnL, so an unstamped frame would flatten the
/// default account's book on a labelled account's liquidation.
#[test]
fn a_routed_lane_stamps_a_liquidation() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    scoped.blocking_send(liquidation("binance")).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::PositionLiquidated(p))) => {
            assert_eq!(p.route_key, Some("binance#alt".into()));
        }
        other => panic!("expected a PositionLiquidated ingest, got {other:?}"),
    }
    let (plain, mut rx) = event_channel(4);
    let default_lane = plain.routed("binance");
    default_lane.blocking_send(liquidation("binance")).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::PositionLiquidated(p))) => assert_eq!(p.route_key, None),
        other => panic!("expected a PositionLiquidated ingest, got {other:?}"),
    }
}

/// The three stamped payloads are the ones with NO order-client-id, and `Event::Fill` is
/// deliberately outside that set: `vike_core`'s `route_event` resolves it through the
/// submit-time `order_client_id_venue` map, exactly, for every order this process placed — so a key here
/// would be redundant AND would touch the one wire shape the frozen parity fixtures pin.
#[test]
fn a_routed_lane_leaves_every_other_event_untouched() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    let event =
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "c9".into(), timestamp: 3 });
    scoped.blocking_send(event.clone()).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(got)) => assert_eq!(got, event),
        other => panic!("expected the event verbatim, got {other:?}"),
    }
}

/// A payload that ALREADY names an account keeps its own key: the stamp is an answer to
/// "nobody said", never an override of somebody who did.
#[test]
fn a_lane_does_not_overwrite_a_key_the_payload_already_carries() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    let mut event = account_state("binance");
    if let Event::AccountState(a) = &mut event {
        a.route_key = Some("binance#other".into());
    }
    assert_eq!(sent(&scoped, &mut rx, event).route_key, Some("binance#other".into()));
}

// ── the gateway lane (journal v18): an event that came over a trade link, with its ring cursor ──

fn cursor(gateway_epoch: u64, seq: u64) -> EventRingCursor {
    EventRingCursor { gateway_epoch, sequence: seq, timestamp_ms: seq as i64 * 10 }
}

fn gateway(event: Event, route_key: &str, c: EventRingCursor) -> GatewayEvent {
    GatewayEvent { event, route_key: route_key.into(), cursor: c }
}

/// `blocking_send_gateway` delivers ONE `Ingest::GatewayEvent` (never an `Ingest::Event` beside
/// it: the event and its cursor are one record) and leaves the ring's account key as the producer
/// named it.
#[test]
fn a_gateway_send_delivers_one_record_with_the_event_and_its_cursor() {
    let (sender, mut rx) = event_channel(4);
    let event =
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".into(), timestamp: 7 });
    sender
        .blocking_send_gateway(gateway(event.clone(), "binance", cursor(5, 9)))
        .expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::GatewayEvent(g)) => {
            assert_eq!(g.event, event);
            assert_eq!(g.route_key.as_str(), "binance");
            assert_eq!(g.cursor, cursor(5, 9));
        }
        other => panic!("expected one GatewayEvent ingest, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "exactly one record per event");
}

/// The route-key stamp is the same rule on this door: a labelled account's order-client-id-less payload is
/// stamped, the default account's is not, and the ring key is left alone either way.
#[test]
fn a_gateway_send_stamps_the_payload_like_a_plain_send() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    scoped
        .blocking_send_gateway(gateway(funding("binance"), "binance#alt", cursor(1, 1)))
        .expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::GatewayEvent(g)) => {
            assert!(
                matches!(&g.event, Event::Funding(f) if f.route_key == Some("binance#alt".into()))
            );
            assert_eq!(g.route_key.as_str(), "binance#alt");
        }
        other => panic!("expected a GatewayEvent ingest, got {other:?}"),
    }
    let (plain, mut rx) = event_channel(4);
    let default_lane = plain.routed("binance");
    default_lane
        .blocking_send_gateway(gateway(funding("binance"), "binance", cursor(1, 2)))
        .expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::GatewayEvent(g)) => {
            assert!(matches!(&g.event, Event::Funding(f) if f.route_key.is_none()));
            assert_eq!(
                g.route_key.as_str(),
                "binance",
                "the default account's ring is keyed by its venue"
            );
        }
        other => panic!("expected a GatewayEvent ingest, got {other:?}"),
    }
}

/// The ONE cursor rule: inside an epoch the larger `seq` wins and a re-delivered straggler cannot
/// move the resume point back; across epochs the later observation wins whatever its `seq`.
#[test]
fn a_cursor_advances_by_sequence_inside_an_epoch_and_by_arrival_across_epochs() {
    let held = cursor(7, 10);
    assert_eq!(held.after(cursor(7, 11)), cursor(7, 11), "later in the ring");
    assert_eq!(held.after(cursor(7, 10)), cursor(7, 10), "the same position");
    assert_eq!(held.after(cursor(7, 4)), held, "a replayed straggler never rewinds the cursor");
    assert_eq!(held.after(cursor(8, 1)), cursor(8, 1), "a new epoch is a new ring: it wins");
    assert_eq!(
        cursor(9, 1).after(cursor(2, 500)),
        cursor(2, 500),
        "an epoch is random, not ordered: arrival decides, the number does not"
    );
}

/// `venue_event` answers "is this message a venue event?" for BOTH doors, and for nothing else.
#[test]
fn venue_event_reads_both_doors_and_nothing_else() {
    let event =
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".into(), timestamp: 7 });
    assert_eq!(Ingest::Event(event.clone()).venue_event(), Some(&event));
    let wrapped = Ingest::GatewayEvent(Box::new(gateway(event.clone(), "binance", cursor(1, 1))));
    assert_eq!(wrapped.venue_event(), Some(&event));
    assert_eq!(Ingest::Watchdog.venue_event(), None);
    assert_eq!(Ingest::Market.venue_event(), None);
    assert_eq!(Ingest::Command(Command::Shutdown).venue_event(), None);
}

fn link_update(up: bool) -> TradeLinkUpdate {
    TradeLinkUpdate {
        route_key: "binance".into(),
        venue: "binance".into(),
        up,
        at_timestamp_ms: 42,
    }
}

/// An account's link transition rides the event lane as its OWN variant (not an `Event`), carrying
/// the account it speaks for in the payload, whatever route key the handle itself holds.
#[test]
fn execution_link_delivers_the_transition_on_its_own_variant() {
    let (sender, mut rx) = event_channel(4);
    let labelled = sender.routed("binance:desk-2");
    sender.execution_link(link_update(false)).expect("receiver alive");
    labelled.execution_link(link_update(true)).expect("receiver alive");
    for up in [false, true] {
        match rx.blocking_recv() {
            Some(Ingest::TradeLink(u)) => assert_eq!(*u, link_update(up)),
            other => panic!("expected an ExecLink ingest, got {other:?}"),
        }
    }
}

/// A link transition is NOT a venue event: no reader of a journal may mistake it for one, and it
/// is classified by an arm of its own in `venue_event`.
#[test]
fn an_execution_link_is_not_a_venue_event() {
    assert_eq!(Ingest::TradeLink(Box::new(link_update(false))).venue_event(), None);
}

/// `venue_event` is EXHAUSTIVE over `Ingest` and names `ExecLink`: a `_ =>` arm would let the next
/// variant fall through unclassified (the compile error that forces a decision is the whole point).
/// The two answers are indistinguishable at run time, so the guard reads the source.
#[test]
fn venue_event_names_execution_link_and_has_no_wildcard_arm() {
    let src = include_str!("../lanes.rs");
    let from = src.find("pub fn venue_event").expect("venue_event exists");
    let body = &src[from..];
    let body = &body[..body.find("\n    }\n").expect("venue_event ends")];
    assert!(body.contains("Ingest::TradeLink(_)"), "ExecLink must be classified by name");
    assert!(!body.contains("_ =>"), "venue_event must stay exhaustive: no wildcard arm");
}
