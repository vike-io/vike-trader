//! What one `orderStatus` answer becomes, from the reply's shape to the events. The `{order, status,
//! statusTimestamp}` row is the WS `orderUpdates` row's shape as the venue's docs give it, hand-written:
//! no `orderStatus` reply of a closed order has been captured off the wire (decision 0171 owes that read).

use super::*;
use serde_json::json;

fn symbology() -> Symbology {
    let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]});
    Symbology::from_meta(&meta, &json!({"tokens": [], "universe": []}))
}

fn reply(status: &str) -> Value {
    json!({"status": "order", "order": {
        "order": {"coin": "BTC", "side": "B", "limitPx": "50000.0", "sz": "0.0", "origSz": "0.01",
                  "oid": 77, "cloid": "0xabc", "timestamp": 1000},
        "status": status, "statusTimestamp": 2000}})
}

fn events_of(read: Result<Value, VenueApiError>) -> Vec<Event> {
    confirm_events("c1", 5, read, true, &symbology())
}

fn accepted_77(e: &Event) -> bool {
    matches!(e, Event::OrderAccepted(a)
        if a.order_client_id == "c1" && a.venue_order_id.as_deref() == Some("77"))
}

/// The case this exists for: the order rests, the WS missed its `open` row, and the answer keeps it
/// from the watchdog's backstop reject.
#[test]
fn an_open_order_is_accepted_under_its_order_client_id_and_oid() {
    for word in ["open", "resting"] {
        let events = events_of(Ok(reply(word)));
        assert!(matches!(&events[..], [e] if accepted_77(e)), "{word}: {events:?}");
    }
}

/// A closed order is the accept and its terminal, on the order client id (the row carries the hash) and, for the
/// fill, under the engine's symbol.
#[test]
fn a_closed_order_is_the_accept_and_its_terminal() {
    let events = events_of(Ok(reply("filled")));
    match &events[..] {
        [a, Event::OrderFilled(f)] if accepted_77(a) => {
            assert_eq!(f.order_client_id, "c1");
            assert_eq!(f.fill.order_client_id, "c1", "the fill must carry the order client id too");
            assert_eq!(f.fill.symbol.as_str(), "BTCUSDC.P", "the coin BTC under its user name");
            assert_eq!(f.fill.trade_id.as_str(), "77", "the wrap's dedup key is the oid");
        }
        other => panic!("filled: {other:?}"),
    }
    for word in ["canceled", "reduceOnlyCanceled", "scheduledCancel"] {
        let events = events_of(Ok(reply(word)));
        match &events[..] {
            [a, Event::OrderCanceled(c)] if accepted_77(a) => {
                assert_eq!(c.order_client_id, "c1", "{word}");
                assert_eq!(c.reason, word);
            }
            other => panic!("{word}: {other:?}"),
        }
    }
    let events = events_of(Ok(reply("triggered")));
    assert!(matches!(&events[..], [a, Event::OrderTriggered(_)] if accepted_77(a)), "{events:?}");
}

/// Refused by the venue: the terminal alone, because a refused order was never accepted.
#[test]
fn a_refused_order_is_the_terminal_reject_alone() {
    let events = events_of(Ok(reply("tickRejected")));
    match &events[..] {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "c1");
            assert_eq!(r.reason, "tickRejected");
        }
        other => panic!("{other:?}"),
    }
}

/// A word this crate does not know proves the order exists, and nothing more.
#[test]
fn a_status_word_not_known_here_is_the_accept_alone() {
    for word in ["replaced", "somethingNewTheVenueAdded"] {
        let events = events_of(Ok(reply(word)));
        assert!(matches!(&events[..], [e] if accepted_77(e)), "{word}: {events:?}");
    }
}

/// Decision 0171's rule: `unknownOid` is the terminal reject for an order whose POST returned, and
/// nothing while it may still be in flight (or after a restart, which empties the ledger).
#[test]
fn absent_rejects_only_a_dispatched_order() {
    match &events_of(Ok(json!({"status": "unknownOid"})))[..] {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "c1");
            assert!(r.reason.contains("venue confirms order absent"), "{}", r.reason);
        }
        other => panic!("{other:?}"),
    }
    let undispatched =
        confirm_events("c1", 5, Ok(json!({"status": "unknownOid"})), false, &symbology());
    assert!(undispatched.is_empty(), "{undispatched:?}");
}

/// A read that failed, and an answer of no shape this crate knows, say nothing about the order, even
/// for one whose POST returned.
#[test]
fn a_failed_read_and_a_wrong_shape_say_nothing() {
    assert!(events_of(Err(VenueApiError { code: 429, msg: "throttled".into() })).is_empty());
    for body in [
        json!(null),
        json!({}),
        json!({"status": "order"}),
        json!({"status": "order", "order": {"status": "open"}}),
        json!({"status": "order", "order": {"status": "open", "order": {"coin": "BTC"}}}),
        json!({"status": "order", "order": {"order": {"oid": 77}}}),
        json!({"status": "somethingElse"}),
    ] {
        assert!(events_of(Ok(body.clone())).is_empty(), "{body}");
    }
}

/// The ledger the exec thread marks is the one the confirm reads: a confirm handed a second, empty one
/// would stay silent on absent for good, and nothing else would notice.
#[test]
fn the_client_builds_one_ledger_and_hands_it_to_both_sides() {
    let client = include_str!("client.rs");
    assert_eq!(client.matches("DispatchLedger::default()").count(), 1);
    assert!(client.contains("ledger.clone(),"), "the confirm's handle");
    assert!(client.contains(" ledger,"), "the exec thread's handle");
    assert!(!include_str!("confirm.rs").contains("DispatchLedger::default()"));
}

#[test]
fn the_read_tells_the_three_answers_apart() {
    assert!(read_status(json!({"status": "unknownOid"})).unwrap().is_none());
    assert!(read_status(reply("open")).unwrap().is_some());
    assert_eq!(read_status(json!({})).unwrap_err().code, E_TIMEOUT_AMBIGUOUS);
}
