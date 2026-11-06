//! The post-only law at the paper book: a maker-only order that would TAKE liquidity when it
//! arrives is refused (`RejectCause::PostOnlyWouldCross`), nothing rests and nothing fills; one that
//! would rest, rests and fills later as a maker.
use super::*;
use vike_exec::ExecutionClient;
use vike_model::events::RejectCause;
use vike_model::{OrderRequest, OrderType, TimeInForce};

fn post_only(coid: &str, side: i32, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(price),
        time_in_force: TimeInForce::PostOnly,
        ..Default::default()
    }
}

fn drain(c: &mut PaperExecutionClient) -> Vec<Event> {
    std::iter::from_fn(|| c.poll_events()).collect()
}

fn book_at(close: f64) -> Bar {
    Bar::ohlcv(1_000, close, close, close, close, 10.0)
}

#[test]
fn a_crossing_post_only_buy_is_refused_and_nothing_rests() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.on_bar(&book_at(100.0));
    c.submit(&post_only("po1", 1, 101.0)); // a buy ABOVE the last price would lift it
    let events = drain(&mut c);
    let [Event::OrderSubmitted(_), Event::OrderRejected(r)] = events.as_slice() else {
        panic!("a crossing post-only order is Submitted then Rejected, got {events:?}");
    };
    assert_eq!(r.cause, Some(RejectCause::PostOnlyWouldCross));
    assert!(r.reason.contains("post-only buy at 101"), "{}", r.reason);
    assert!(c.pending.is_empty(), "a refused post-only order must not rest");
    // and the next bar fills nothing: it never existed
    c.on_bar(&Bar::ohlcv(2_000, 100.0, 105.0, 99.0, 100.0, 10.0));
    assert!(drain(&mut c).is_empty(), "no fill for an order that was refused");
    assert!(c.fills.lock().unwrap().is_empty());
}

#[test]
fn touching_the_price_crosses_for_both_sides() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.on_bar(&book_at(100.0));
    c.submit(&post_only("buy-touch", 1, 100.0));
    c.submit(&post_only("sell-touch", -1, 100.0));
    c.submit(&post_only("sell-through", -1, 99.0));
    let rejected = drain(&mut c)
        .into_iter()
        .filter(|e| matches!(e, Event::OrderRejected(r) if r.cause == Some(RejectCause::PostOnlyWouldCross)))
        .count();
    assert_eq!(rejected, 3);
}

#[test]
fn a_post_only_order_below_the_market_rests_and_fills_as_a_maker_later() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.on_bar(&book_at(100.0));
    c.submit(&post_only("po2", 1, 95.0)); // a buy under the market: a maker
    let events = drain(&mut c);
    assert!(
        matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
        "{events:?}"
    );
    assert_eq!(c.pending.len(), 1);
    // the market trades down through it: the resting order fills, at its limit, as a maker
    c.on_bar(&Bar::ohlcv(2_000, 100.0, 100.0, 90.0, 92.0, 10.0));
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert!(fills[0].is_maker);
}

#[test]
fn a_quoted_bar_is_judged_against_the_side_the_order_would_take() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let mut bar = book_at(100.0);
    bar.bid = Some(99.0);
    bar.ask = Some(101.0);
    c.on_bar(&bar);
    c.submit(&post_only("under-ask", 1, 100.5)); // below the ask: rests although above the last
    c.submit(&post_only("at-ask", 1, 101.0)); // lifts the ask: refused
    let events = drain(&mut c);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderAccepted(a) if a.client_order_id == "under-ask"))
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "at-ask"
            && r.cause == Some(RejectCause::PostOnlyWouldCross)))
    );
}

#[test]
fn before_the_first_bar_there_is_no_price_to_judge_and_the_order_rests() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&post_only("early", 1, 1_000_000.0));
    let events = drain(&mut c);
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
}

#[test]
fn a_post_only_market_order_is_refused_without_a_cause() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.on_bar(&book_at(100.0));
    let mut req = post_only("m1", 1, 100.0);
    req.order_type = OrderType::Market;
    req.price = None;
    c.submit(&req);
    let events = drain(&mut c);
    let Some(Event::OrderRejected(r)) = events.last() else { panic!("{events:?}") };
    assert_eq!(r.cause, None);
    assert!(r.reason.contains("needs a limit order"), "{}", r.reason);
}

#[test]
fn every_other_time_in_force_is_untouched_by_the_post_only_law() {
    // a plain GTC buy at 101 against a last price of 100 is a marketable limit: the paper book
    // accepts it exactly as before (it fills as the engine's limit does), post-only or not.
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.on_bar(&book_at(100.0));
    let mut req = post_only("gtc", 1, 101.0);
    req.time_in_force = TimeInForce::Gtc;
    c.submit(&req);
    let events = drain(&mut c);
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
}
