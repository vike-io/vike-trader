//! ExecutionClientPaperMulti gate: per-symbol book routing behind one ExecutionClient —
//! submits by request.symbol, bars by bar.symbol, cancels broadcast, route misses
//! synthesize the terminal OrderRejected (dead-path rule).

use std::assert_matches;
use vike_execution::ExecutionClient;
use vike_marketdata::Bar;
use vike_model::OrderRequest;
use vike_model::events::Event;
use vike_paper::{ExecutionClientPaper, ExecutionClientPaperMulti};

fn bar(symbol: Option<&str>, ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: symbol.map(Into::into),
    }
}

fn market(order_client_id: &str, symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        order_type: vike_model::OrderType::Market,
        side,
        qty,
        timestamp: 0,
        ..Default::default()
    }
}

fn drain(c: &mut ExecutionClientPaperMulti) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(event) = c.poll_events() {
        out.push(event);
    }
    out
}

#[test]
fn bars_route_by_symbol_and_fill_only_their_book() {
    let mut c = ExecutionClientPaperMulti::new();
    c.add_book(ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.add_book(ExecutionClientPaper::new("binance", "ETHUSDT", 0.0, 0.0, 0.0));
    c.submit(&market("b1", "BTCUSDT", 1, 1.0));
    c.submit(&market("e1", "ETHUSDT", 1, 3.0));
    let _ = drain(&mut c); // Submitted+Accepted pairs

    // an ETH bar fills ONLY the ETH book (market fills at next bar open)
    c.on_bar(&bar(Some("ETHUSDT"), 60_000, 50.0, 51.0, 49.0, 50.5));
    let evs = drain(&mut c);
    let fills: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fills.len(), 1, "only the ETH book fills: {evs:?}");
    assert_eq!(fills[0].symbol, "ETHUSDT");
    assert_eq!(fills[0].last_price, 50.0); // next-open discipline

    // the BTC order is still resting; its own bar fills it
    c.on_bar(&bar(Some("BTCUSDT"), 60_000, 100.0, 101.0, 99.0, 100.5));
    let evs = drain(&mut c);
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::Fill(f) if f.symbol == "BTCUSDT" && f.last_price == 100.0))
    );
}

/// **Two books' fills never share a trade id.** Each book counts its own fills from one, and the
/// books sit on ONE engine's account, which refuses a fill under a trade id it already folded: the
/// second book's first fill vanished from the account while its order stayed `Accepted` (measured by
/// the two-leg paper rehearsal, one broker plan step 12). Each id names its order.
#[test]
fn two_books_fills_never_share_a_trade_id() {
    let mut c = ExecutionClientPaperMulti::new();
    c.add_book(ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.add_book(ExecutionClientPaper::new("binance", "ETHUSDT", 0.0, 0.0, 0.0));
    c.submit(&market("b1", "BTCUSDT", 1, 1.0));
    c.submit(&market("e1", "ETHUSDT", 1, 3.0));
    c.on_bar(&bar(Some("BTCUSDT"), 60_000, 100.0, 101.0, 99.0, 100.5));
    c.on_bar(&bar(Some("ETHUSDT"), 60_000, 50.0, 51.0, 49.0, 50.5));
    let ids: Vec<String> = drain(&mut c)
        .into_iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["paper-b1-1", "paper-e1-1"], "one id per book's fill");
}

#[test]
fn symbolless_bar_reaches_every_book() {
    let mut c = ExecutionClientPaperMulti::new();
    c.add_book(ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.submit(&market("b1", "BTCUSDT", 1, 1.0));
    let _ = drain(&mut c);
    c.on_bar(&bar(None, 60_000, 100.0, 101.0, 99.0, 100.5)); // single-book compat case
    assert!(drain(&mut c).iter().any(|e| matches!(e, Event::Fill(f) if f.symbol == "BTCUSDT")));
}

#[test]
fn route_miss_synthesizes_terminal_rejected() {
    let mut c = ExecutionClientPaperMulti::new();
    c.add_book(ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.submit(&market("x1", "SOLUSDT", 1, 1.0)); // no SOL book
    let evs = drain(&mut c);
    assert_matches!(&evs[0], Event::OrderSubmitted(s) if s.order_client_id == "x1", "{evs:?}");
    assert_matches!(
        &evs[1], Event::OrderRejected(r) if r.order_client_id == "x1"
        && r.reason.contains("no paper book"),
        "no order silently vanishes: {evs:?}"
    );
}

#[test]
fn cancel_broadcast_is_silent_on_nonholders() {
    let mut c = ExecutionClientPaperMulti::new();
    c.add_book(ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.add_book(ExecutionClientPaper::new("binance", "ETHUSDT", 0.0, 0.0, 0.0));
    let mut limit = market("b1", "BTCUSDT", 1, 1.0);
    limit.order_type = vike_model::OrderType::Limit;
    limit.price = Some(90.0);
    c.submit(&limit);
    let _ = drain(&mut c);
    c.cancel("b1");
    let evs = drain(&mut c);
    // exactly ONE OrderCanceled — the ETH book stayed silent
    assert_eq!(evs.iter().filter(|e| matches!(e, Event::OrderCanceled(_))).count(), 1, "{evs:?}");
}

/// **An account-wide paper account makes a token's book as the token comes** (`with_book_maker`, decision 0285's
/// amendment of 2026-11-16): a market buy of a token no book holds rests on a new book of that token, its quote fills
/// it at the ask, and the empty symbol (a combo's) still gets no book: it is terminally rejected, as ever.
#[test]
fn a_book_maker_opens_a_tokens_book_and_its_quote_fills_at_the_ask() {
    let mut c = ExecutionClientPaperMulti::new()
        .with_book_maker(|token| ExecutionClientPaper::new("polymarket", token, 0.0, 0.0, 0.0));
    let mut buy = market("t1", "TOKEN_A", 1, 100.0);
    buy.venue = "polymarket".into();
    c.submit(&buy);
    let _ = drain(&mut c);
    c.on_quote("TOKEN_A", 0.48, 0.52, 1_000);
    let fills: Vec<(String, f64)> = drain(&mut c)
        .into_iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some((f.symbol.to_string(), f.last_price)),
            _ => None,
        })
        .collect();
    assert_eq!(fills, [("TOKEN_A".to_string(), 0.52)], "the ask, never the mid");

    c.submit(&market("x1", "", 1, 1.0));
    assert!(
        drain(&mut c)
            .iter()
            .any(|e| matches!(e, Event::OrderRejected(r) if r.order_client_id == "x1")),
        "no book for the empty symbol"
    );
}
