use super::*;
use vike_model::events::{Event, OrderSubmitted};

#[test]
fn event_channel_delivers_events_without_a_core() {
    let (sender, mut rx) = event_channel(4);
    sender
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: "c1".into(),
            ts: 7,
        }))
        .expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(Event::OrderSubmitted(e))) => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.ts, 7);
        }
        other => panic!("expected OrderSubmitted ingest, got {other:?}"),
    }
}

use vike_model::events::AccountState;

fn account_state(venue: &str) -> Event {
    Event::AccountState(AccountState {
        venue: venue.into(),
        balances: vec![("USDT".to_string(), 100.0)],
        ts: 1,
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
        ts: 1,
        route_key: None,
    })
}

fn liquidation(venue: &str) -> Event {
    Event::PositionLiquidated(vike_model::events::PositionLiquidated {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        qty: 1.0,
        liq_price: 90.0,
        fee: 0.1,
        ts: 1,
        trade_id: "l1".into(),
        route_key: None,
    })
}

fn sent(sender: &EventSender, rx: &mut mpsc::Receiver<Ingest>, ev: Event) -> AccountState {
    sender.blocking_send(ev).expect("receiver alive");
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

/// **…and the DEFAULT account's lane stamps NOTHING.** `vike_mount::account_route_key` renders
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

/// A COID-LESS venue-tagged payload is stamped, and there are THREE of them — this is the
/// funding half.
///
/// It was missed when the stamp was written, on the argument that "every other venue-tagged
/// payload names a symbol and the symbol is exact". The symbol stopped being an account key
/// when two accounts of one venue were allowed onto one instrument, and a funding payment
/// carries no client-order-id to fall back on — so an unstamped one folded into the venue's
/// DEFAULT engine and debited the wrong account's balance.
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
/// CLOSES the position and books the realized PnL, so an unstamped frame flattened the default
/// account's book on the strength of a labelled account's liquidation.
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

/// The three stamped payloads are the ones with NO client-order-id, and `Event::Fill` is
/// deliberately outside that set: `vike_core`'s `route_event` resolves it through the
/// submit-time `coid_venue` map, exactly, for every order this process placed — so a key here
/// would be redundant AND would touch the one wire shape the frozen parity fixtures pin.
#[test]
fn a_routed_lane_leaves_every_other_event_untouched() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    let ev = Event::OrderSubmitted(OrderSubmitted { client_order_id: "c9".into(), ts: 3 });
    scoped.blocking_send(ev.clone()).expect("receiver alive");
    match rx.blocking_recv() {
        Some(Ingest::Event(got)) => assert_eq!(got, ev),
        other => panic!("expected the event verbatim, got {other:?}"),
    }
}

/// A payload that ALREADY names an account keeps its own key: the stamp is an answer to
/// "nobody said", never an override of somebody who did.
#[test]
fn a_lane_does_not_overwrite_a_key_the_payload_already_carries() {
    let (plain, mut rx) = event_channel(4);
    let scoped = plain.routed("binance#alt");
    let mut ev = account_state("binance");
    if let Event::AccountState(a) = &mut ev {
        a.route_key = Some("binance#other".into());
    }
    assert_eq!(sent(&scoped, &mut rx, ev).route_key, Some("binance#other".into()));
}
