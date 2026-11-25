//! The paper book's clock (decision 0285's amendment of 2026-11-16): where the venue's quotes reach it
//! they ARE the clock, so an order fills the moment the market reaches it and at the side that reached
//! it, and a `fill = "close"` strategy's bar order fills at that bar's close. The owner's four examples
//! on a 1-minute and a 1-hour bar: 12:00 opens 100.00 and closes 101.00; the next bar's first price is
//! 101.20, 300 ms after it opens (a buy pays the ask, so each price here is the ask, the bid 0.10 under).

use super::test_support::{bar, drain};
use super::*;
use vike_execution::ExecutionClient;
use vike_marketdata::BarSides;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

const SYMBOL: &str = "BTCUSDT";
const SPREAD: f64 = 0.10;
/// 12:00 UTC of the first day.
const NOON: i64 = 12 * 3_600_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;

fn book() -> ExecutionClientPaper {
    ExecutionClientPaper::new("binance", SYMBOL, 0.0, 0.0, 0.0)
}

fn order(id: &str, side: i32, kind: OrderType, price: Option<f64>, at: i64) -> OrderRequest {
    let stop = kind == OrderType::Stop;
    OrderRequest {
        order_client_id: id.into(),
        venue: "binance".into(),
        symbol: SYMBOL.into(),
        side,
        qty: 1.0,
        order_type: kind,
        price: price.filter(|_| !stop),
        trigger_price: price.filter(|_| stop),
        timestamp: at,
        ..Default::default()
    }
}

/// One quote whose ask is `ask`, the bid `SPREAD` under it.
fn quote(c: &mut ExecutionClientPaper, at: i64, ask: f64) {
    c.on_quote(SYMBOL, ask - SPREAD, ask, at);
}

/// The bar of the window `start .. start + width` whose quotes were `asks` (the first at its open),
/// closed as the node closes it: the mid's path, each side's path beside it.
fn close_bar(c: &mut ExecutionClientPaper, start: i64, asks: &[f64]) {
    let path = |shift: f64| {
        let px: Vec<f64> = asks.iter().map(|a| a - shift).collect();
        let high = px.iter().copied().fold(f64::MIN, f64::max);
        let low = px.iter().copied().fold(f64::MAX, f64::min);
        bar(start, px[0], high, low, *px.last().expect("one quote at least"))
    };
    let sides = BarSides { bid: path(SPREAD), ask: path(0.0) };
    c.on_bar_sides(&path(SPREAD / 2.0), Some(&sides));
}

/// `(price, timestamp)` of every fill so far.
fn fills(c: &ExecutionClientPaper) -> Vec<(f64, i64)> {
    c.fills.lock().unwrap().iter().map(|f| (f.price, f.timestamp)).collect()
}

/// The 12:00 bar of `width`, its quotes shown to the book as they came: 100.00 at the open, 100.60 at
/// :30 (`width` / 2), 101.00 at its last instant.
fn the_noon_bar(c: &mut ExecutionClientPaper, width: i64) {
    quote(c, NOON + 100, 100.00);
    quote(c, NOON + width / 2, 100.60);
    quote(c, NOON + width - 100, 101.00);
    close_bar(c, NOON, &[100.00, 100.60, 101.00]);
}

/// Example 1, `next_open`: the order a strategy places on the 12:00 close fills at 101.20, the first
/// price of the next bar, REPORTED the moment that price comes (300 ms into the bar), not at the next
/// bar's close one bar later.
#[test]
fn a_bar_order_fills_at_the_next_bars_first_price_the_moment_it_comes() {
    for width in [MINUTE, HOUR] {
        let mut c = book();
        the_noon_bar(&mut c, width);
        let next = NOON + width;
        c.submit(&order("s", 1, OrderType::Market, None, next));
        drain(&mut c);
        assert!(fills(&c).is_empty(), "{width}: it waits for the next bar's first price");
        quote(&mut c, next + 300, 101.20);
        assert_eq!(fills(&c), [(101.20, next + 300)], "{width}: at its first price, then");
        assert!(
            drain(&mut c).iter().any(|e| matches!(e, Event::OrderFilled(_))),
            "{width}: reported with that quote"
        );
    }
}

/// Example 1, `close`: the same order fills at the 12:00 close, 101.00, at the bar's end, the moment
/// the bar closes; `next_open` (above) fills it at 101.20 300 ms later. A sell takes the bid's close.
#[test]
fn a_close_strategys_bar_order_fills_at_that_bars_close_against_next_open() {
    for width in [MINUTE, HOUR] {
        let next = NOON + width;
        for (side, price) in [(1, 101.00), (-1, 101.00 - SPREAD)] {
            let mut close = book();
            the_noon_bar(&mut close, width);
            close.submit(&order("s", side, OrderType::Market, None, next));
            close.fill_at_close(&["s".to_string()], width);
            assert_eq!(fills(&close), [(price, next)], "{width} side {side}: the close");
            quote(&mut close, next + 300, 101.20);
            assert_eq!(fills(&close).len(), 1, "{width} side {side}: once");

            let mut next_open = book();
            the_noon_bar(&mut next_open, width);
            next_open.submit(&order("s", side, OrderType::Market, None, next));
            quote(&mut next_open, next + 300, 101.20);
            let first = if side > 0 { 101.20 } else { 101.20 - SPREAD };
            assert_eq!(fills(&next_open), [(first, next + 300)], "{width} side {side}: next open");
        }
    }
}

/// `fill_at_close` fills the orders it names and nothing else: a manual order resting beside them
/// keeps waiting for the quote that reaches it.
#[test]
fn the_close_fills_only_the_orders_it_names() {
    let mut c = book();
    the_noon_bar(&mut c, MINUTE);
    c.submit(&order("manual", 1, OrderType::Market, None, NOON + MINUTE));
    c.submit(&order("s", 1, OrderType::Market, None, NOON + MINUTE));
    c.fill_at_close(&["s".to_string()], MINUTE);
    assert_eq!(fills(&c), [(101.00, NOON + MINUTE)]);
    quote(&mut c, NOON + MINUTE + 300, 101.20);
    assert_eq!(fills(&c), [(101.00, NOON + MINUTE), (101.20, NOON + MINUTE + 300)]);
}

/// A `fill = "close"` strategy's bar order that reaches the book AFTER a quote of the next bar (a
/// sandboxed strategy's reply) still fills at its bar's close at the bar's end: the core holds the
/// book while it drains such orders (`hold_for_close`), so `submit` does not meet them at that quote.
#[test]
fn a_close_order_held_past_a_later_quote_fills_at_its_bars_close() {
    for width in [MINUTE, HOUR] {
        let next = NOON + width;
        let mut c = book();
        the_noon_bar(&mut c, width);
        quote(&mut c, next + 920, 101.20);
        c.hold_for_close(true);
        c.submit(&order("s", 1, OrderType::Market, None, next));
        c.hold_for_close(false);
        assert!(fills(&c).is_empty(), "{width}: not at the later quote");
        c.fill_at_close(&["s".to_string()], width);
        assert_eq!(fills(&c), [(101.00, next)], "{width}: the 12:00 close, at its end");
        // unheld, the same order meets the standing quote at once
        let mut unheld = book();
        the_noon_bar(&mut unheld, width);
        quote(&mut unheld, next + 920, 101.20);
        unheld.submit(&order("m", 1, OrderType::Market, None, next));
        assert_eq!(fills(&unheld), [(101.20, next + 920)], "{width}: unheld");
    }
}

/// The two bar stamps a book is shown, and the one END a close fill takes: a venue candle stamped at
/// its window's START (12:00 for [12:00, 12:01)) and a synthesizer's bar stamped at its END (12:01),
/// its sides stamped at the start either way (`QuoteSidePaths`), so both close fills land at 12:01.
#[test]
fn a_close_fill_is_stamped_at_the_bars_end_whichever_stamp_the_bar_carries() {
    for width in [MINUTE, HOUR] {
        let sides = |start: i64| BarSides {
            bid: bar(start, 99.9, 100.9, 99.9, 100.9),
            ask: bar(start, 100.0, 101.0, 100.0, 101.0),
        };
        for (convention, stamp) in [("start", NOON), ("end", NOON + width)] {
            let mut c = book();
            c.on_bar_sides(&bar(stamp, 99.95, 100.95, 99.95, 100.95), Some(&sides(NOON)));
            c.submit(&order("s", 1, OrderType::Market, None, NOON + width));
            c.fill_at_close(&["s".to_string()], width);
            assert_eq!(fills(&c), [(101.00, NOON + width)], "{width}, stamped at its {convention}");
        }
        // a candle no quote came beside is a venue candle: stamped at its start
        let mut bare = book();
        bare.on_bar(&bar(NOON, 100.0, 101.0, 99.0, 100.5));
        bare.submit(&order("s", 1, OrderType::Market, None, NOON + width));
        bare.fill_at_close(&["s".to_string()], width);
        assert_eq!(fills(&bare), [(100.5, NOON + width)], "{width}: a bare candle");
    }
}

/// Example 2: a stop-loss at 99.50 under a long SELLS at 99.50 the moment the bid reaches it, 20 s into
/// the next bar, not at a bar's open two bars later.
#[test]
fn a_stop_loss_sells_the_moment_the_bid_reaches_it() {
    for width in [MINUTE, HOUR] {
        let mut c = book();
        the_noon_bar(&mut c, width);
        let next = NOON + width;
        c.submit(&order("long", 1, OrderType::Market, None, next));
        quote(&mut c, next + 300, 101.20);
        c.submit(&order("stop", -1, OrderType::Stop, Some(99.50), next + 400));
        c.on_quote(SYMBOL, 99.80, 99.90, next + 10_000);
        assert_eq!(fills(&c).len(), 1, "{width}: the bid above the stop");
        c.on_quote(SYMBOL, 99.50, 99.60, next + 20_000);
        assert_eq!(fills(&c)[1], (99.50, next + 20_000), "{width}: at the bid that reached it");
        // a bid that gaps through the stop sells at that bid: the side that touched it
        let mut gap = book();
        the_noon_bar(&mut gap, width);
        gap.submit(&order("stop", -1, OrderType::Stop, Some(99.50), next));
        gap.on_quote(SYMBOL, 99.30, 99.40, next + 20_000);
        assert_eq!(fills(&gap), [(99.30, next + 20_000)], "{width}: a gap");
    }
}

/// Example 3: a manual market buy at 12:00:30, the ask 100.60 then, fills at 100.60 at 12:00:30, not
/// at the 12:00 open, a price from before the order.
#[test]
fn a_manual_market_order_fills_at_the_standing_quote_at_once() {
    for width in [MINUTE, HOUR] {
        let mut c = book();
        quote(&mut c, NOON + 100, 100.00);
        let at = NOON + width / 2;
        quote(&mut c, at - 100, 100.60);
        c.submit(&order("m", 1, OrderType::Market, None, at));
        assert_eq!(fills(&c), [(100.60, at)], "{width}");
        close_bar(&mut c, NOON, &[100.00, 100.60, 101.00]);
        assert_eq!(fills(&c).len(), 1, "{width}: the bar fills nothing again");
    }
}

/// Example 4: a manual buy limit at 100.20, placed at 12:00:30 with the ask at 100.60, fills at 100.20
/// when the ask touches it at 12:00:45. An ask under it BEFORE the order (12:00:10) is no fill, and
/// neither is the bar whose path holds that ask.
#[test]
fn a_manual_limit_fills_when_the_ask_touches_it_and_never_on_a_price_from_before_it() {
    for width in [MINUTE, HOUR] {
        let mut c = book();
        quote(&mut c, NOON + 100, 100.00);
        quote(&mut c, NOON + width / 6, 100.10);
        let at = NOON + width / 2;
        quote(&mut c, at - 100, 100.60);
        c.submit(&order("l", 1, OrderType::Limit, Some(100.20), at));
        assert!(fills(&c).is_empty(), "{width}: the ask is above it");
        close_bar(&mut c, NOON, &[100.00, 100.10, 100.60]);
        assert!(fills(&c).is_empty(), "{width}: the bar's 100.10 came before the order");
        let mut d = book();
        quote(&mut d, at - 100, 100.60);
        d.submit(&order("l", 1, OrderType::Limit, Some(100.20), at));
        let touch = NOON + width * 3 / 4;
        quote(&mut d, touch, 100.20);
        assert_eq!(fills(&d), [(100.20, touch)], "{width}: at its level, when touched");
    }
}

/// No quote reaches the book (every stored bar, a feed with no quotes): the bars are the clock exactly
/// as before, a market order filling at the next bar's open.
#[test]
fn a_book_no_quote_reaches_keeps_the_bar_clock() {
    let mut c = book();
    c.on_bar(&bar(NOON, 100.0, 101.0, 99.0, 100.5));
    c.submit(&order("s", 1, OrderType::Market, None, NOON + MINUTE));
    assert!(fills(&c).is_empty());
    c.on_bar(&bar(NOON + MINUTE, 102.0, 103.0, 101.0, 102.5));
    assert_eq!(fills(&c), [(102.0, NOON + MINUTE)]);
}

/// The reason of the `OrderCanceled` that ended `id`, if one did.
fn cancel_reason(events: &[Event], id: &str) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderCanceled(c) if c.order_client_id == id => Some(c.reason.to_string()),
        _ => None,
    })
}

/// **A paper market order fills at the CURRENT quote or not at all** (the owner, 2026-11-17). After a quote
/// that states a bid and no ask, a market buy is refused in words at once, never filled at the ask of the
/// older two-sided quote, and a market sell fills at that bid. A market buy a bar left resting meets the
/// next quote: one that states no ask refuses it, it is never left waiting.
#[test]
fn a_market_order_meets_the_current_quote_or_is_refused_in_words() {
    let mut c = book();
    c.on_quote(SYMBOL, 100.00, 100.10, NOON);
    c.on_quote(SYMBOL, 100.20, f64::NAN, NOON + 1_000);
    c.submit(&order("b", 1, OrderType::Market, None, NOON + 2_000));
    let why = vike_execution::testing::rejection(&drain(&mut c)).expect("the buy is refused");
    assert!(why.starts_with("no ask") && why.contains(SYMBOL), "{why}");
    assert!(fills(&c).is_empty(), "never at the older quote's ask");
    c.submit(&order("s", -1, OrderType::Market, None, NOON + 3_000));
    assert_eq!(fills(&c), [(100.20, NOON + 3_000)], "the sell meets the current bid");

    c.on_bar(&bar(NOON, 100.0, 100.3, 99.9, 100.2));
    c.submit(&order("rests", 1, OrderType::Market, None, NOON + MINUTE));
    assert_eq!(c.pending.len(), 1, "a buy placed on the bar waits for the next quote");
    drain(&mut c);
    c.on_quote(SYMBOL, 100.30, 0.0, NOON + MINUTE + 300);
    let why = cancel_reason(&drain(&mut c), "rests").expect("the resting buy is ended");
    assert!(why.starts_with("no ask"), "{why}");
    assert!(c.pending.is_empty(), "nothing is left waiting");
    assert_eq!(fills(&c).len(), 1, "only the sell filled");
}
