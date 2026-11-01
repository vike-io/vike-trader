//! What an order's events SAY, through the real exec client over `fake_ig` (a hand-written loopback gateway, never IG):
//! a fill carries a real time even when the order stated none, and a market IG charges commission on is refused
//! instead of being booked at a commission of 0.
//!
//! * `OrderRequest::ts` is 0 for an order from the command line or the control door, and every event of that submit
//!   (the FILL included) used to carry 0 = 1970: three `exec_fill` rows of the demo account in the hist store.
//! * A SHARES market is charged commission by IG (share dealing and share CFDs), which no mapper of this client
//!   reads; a fill of it would book `commission: 0.0` without a word.

use serde_json::json;
use vike_model::events::Event;

use crate::fake_ig::Route;
use crate::order_currency::submit_at;

const EURUSD: &str = "CS.D.EURUSD.MINI.IP";

/// The `ts` of the events one submit emits, when it has one.
fn ts_of(ev: &Event) -> Option<i64> {
    match ev {
        Event::OrderSubmitted(e) => Some(e.ts),
        Event::OrderAccepted(e) => Some(e.ts),
        Event::OrderRejected(e) => Some(e.ts),
        Event::Fill(e) => Some(e.ts),
        Event::OrderFilled(e) => Some(e.ts),
        _ => None,
    }
}

fn market_of(class: &str, currency: &str) -> Route {
    Route::ok(
        "GET /markets/CS.D.EURUSD",
        json!({ "instrument": { "epic": EURUSD, "type": class, "currencies": [{ "code": currency }] } }),
    )
}

/// An order that states no time is stamped when the bridge handled it, and that includes its fill.
///
/// MUTATION PROOF: make `exec.rs`'s `event_ts` return its argument - every `ts` below is 0.
#[test]
fn an_order_that_states_no_time_is_filled_at_a_real_one() {
    let before = vike_model::now_ms();
    let (events, _ig) = submit_at(EURUSD, Some(market_of("CURRENCIES", "USD")), None, 0);
    let after = vike_model::now_ms();
    assert!(events.iter().any(|e| matches!(e, Event::Fill(_))), "no fill: {events:?}");
    let stamps: Vec<i64> = events.iter().filter_map(ts_of).collect();
    assert!(stamps.len() >= 4, "submitted, accepted and the fill pair: {events:?}");
    for ts in &stamps {
        assert!(
            (before..=after).contains(ts),
            "ts {ts} is not within [{before}, {after}]: {events:?}"
        );
    }
}

/// An order that states its time (a strategy's, stamped with the engine clock) keeps it on every event.
#[test]
fn an_order_that_states_a_time_keeps_it() {
    let (events, _ig) = submit_at(EURUSD, Some(market_of("CURRENCIES", "USD")), None, 1_234);
    assert!(events.iter().any(|e| matches!(e, Event::Fill(_))), "no fill: {events:?}");
    assert!(events.iter().filter_map(ts_of).all(|ts| ts == 1_234), "{events:?}");
}

/// A SHARES market is refused before anything is POSTed, in words that name the commission.
#[test]
fn a_shares_market_is_refused_before_anything_is_posted() {
    let (events, ig) = submit_at(EURUSD, Some(market_of("SHARES", "USD")), None, 5);
    let reasons: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "{events:?}");
    assert!(
        reasons[0].contains("SHARES") && reasons[0].contains("commission"),
        "the refusal names the class and the commission: {}",
        reasons[0]
    );
    assert_eq!(ig.count("POST /positions/otc"), 0, "nothing may reach the deal endpoint");
}
