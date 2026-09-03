//! MultiPaperExecutionClient gate: per-symbol book routing behind one ExecutionClient —
//! submits by request.symbol, bars by bar.symbol, cancels broadcast, route misses
//! synthesize the terminal OrderRejected (dead-path rule).

use vike_backtest::paper::{MultiPaperExecutionClient, PaperExecutionClient};
use vike_exec::ExecutionClient;
use vike_model::events::Event;
use vike_model::{Bar, OrderRequest};

fn bar(symbol: Option<&str>, ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: symbol.map(str::to_string),
    }
}

fn market(coid: &str, symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        order_type: "market".into(),
        side,
        qty,
        ts: 0,
        ..Default::default()
    }
}

fn drain(c: &mut MultiPaperExecutionClient) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(ev) = c.poll_events() {
        out.push(ev);
    }
    out
}

#[test]
fn bars_route_by_symbol_and_fill_only_their_book() {
    let mut c = MultiPaperExecutionClient::new();
    c.add_book(PaperExecutionClient::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.add_book(PaperExecutionClient::new("binance", "ETHUSDT", 0.0, 0.0, 0.0));
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
    assert_eq!(fills[0].last_px, 50.0); // next-open discipline

    // the BTC order is still resting; its own bar fills it
    c.on_bar(&bar(Some("BTCUSDT"), 60_000, 100.0, 101.0, 99.0, 100.5));
    let evs = drain(&mut c);
    assert!(evs
        .iter()
        .any(|e| matches!(e, Event::Fill(f) if f.symbol == "BTCUSDT" && f.last_px == 100.0)));
}

#[test]
fn symbolless_bar_reaches_every_book() {
    let mut c = MultiPaperExecutionClient::new();
    c.add_book(PaperExecutionClient::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.submit(&market("b1", "BTCUSDT", 1, 1.0));
    let _ = drain(&mut c);
    c.on_bar(&bar(None, 60_000, 100.0, 101.0, 99.0, 100.5)); // single-book compat case
    assert!(drain(&mut c).iter().any(|e| matches!(e, Event::Fill(f) if f.symbol == "BTCUSDT")));
}

#[test]
fn route_miss_synthesizes_terminal_rejected() {
    let mut c = MultiPaperExecutionClient::new();
    c.add_book(PaperExecutionClient::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.submit(&market("x1", "SOLUSDT", 1, 1.0)); // no SOL book
    let evs = drain(&mut c);
    assert!(matches!(&evs[0], Event::OrderSubmitted(s) if s.client_order_id == "x1"), "{evs:?}");
    assert!(
        matches!(&evs[1], Event::OrderRejected(r) if r.client_order_id == "x1"
            && r.reason.contains("no paper book")),
        "no order silently vanishes: {evs:?}"
    );
}

#[test]
fn cancel_broadcast_is_silent_on_nonholders() {
    let mut c = MultiPaperExecutionClient::new();
    c.add_book(PaperExecutionClient::new("binance", "BTCUSDT", 0.0, 0.0, 0.0));
    c.add_book(PaperExecutionClient::new("binance", "ETHUSDT", 0.0, 0.0, 0.0));
    let mut limit = market("b1", "BTCUSDT", 1, 1.0);
    limit.order_type = "limit".into();
    limit.price = Some(90.0);
    c.submit(&limit);
    let _ = drain(&mut c);
    c.cancel("b1");
    let evs = drain(&mut c);
    // exactly ONE OrderCanceled — the ETH book stayed silent
    assert_eq!(evs.iter().filter(|e| matches!(e, Event::OrderCanceled(_))).count(), 1, "{evs:?}");
}
