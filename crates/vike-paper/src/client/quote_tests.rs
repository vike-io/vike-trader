//! A paper book is ONE dollar ledger with no rate source (decision 0183 item 7/13): an order on a
//! symbol whose NAME says it is quoted in another fiat currency is refused in words, never booked at
//! par (USDJPY at 150 books 150x too much, EURGBP 1.35x too little).

use super::test_support::drain;
use super::*;
use vike_exec::ExecutionClient;
use vike_model::OrderRequest;

fn market(symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: "o1".into(),
        venue: "oanda".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 100_000.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// The events one market order on `symbol` produces on a book mounted on that symbol.
fn events_for(symbol: &str) -> Vec<Event> {
    let mut c = PaperExecutionClient::new("oanda", symbol, 0.0, 0.0, 0.0);
    c.submit(&market(symbol));
    drain(&mut c)
}

fn refusal(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderRejected(r) => Some(r.reason.to_string()),
        _ => None,
    })
}

#[test]
fn a_foreign_quoted_symbol_is_refused_in_words_not_booked_at_par() {
    for (symbol, ccy) in [
        ("USDJPY", "JPY"),
        ("EURGBP", "GBP"),
        ("EUR_GBP", "GBP"),
        ("eurgbp", "GBP"),
        ("DE30_EUR", "EUR"),
        ("CS.D.GBPJPY.CFD.IP", "JPY"),
    ] {
        let events = events_for(symbol);
        let why = refusal(&events).unwrap_or_else(|| panic!("{symbol} was accepted: {events:?}"));
        assert!(why.contains(ccy), "{symbol}: the words name the currency {ccy}: {why}");
        assert!(why.contains("USD"), "{symbol}: and the ledger it would be booked in: {why}");
        assert!(
            !events.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "{symbol}: a refused order is never accepted: {events:?}"
        );
    }
}

/// Every symbol the old book took and that the new one must keep taking, byte for byte: dollar-quoted
/// pairs and CFDs, crypto, equities, and a name that says nothing about its quote.
#[test]
fn a_dollar_quoted_or_unnamed_quote_symbol_is_accepted_as_before() {
    for symbol in [
        "EURUSD",
        "EUR_USD",
        "SPX500_USD",
        "XAU_USD",
        "BTCUSDT",
        "BTCUSDC",
        "BTC-USD",
        "AAPL",
        "BTC",
    ] {
        let events = events_for(symbol);
        assert_eq!(refusal(&events), None, "{symbol}: {events:?}");
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "{symbol}: {events:?}"
        );
    }
}

/// The order's own symbol decides when it names one (a multi-symbol router hands each book its own),
/// and an order with no symbol is the book's.
#[test]
fn the_orders_symbol_decides_and_an_empty_one_is_the_books() {
    let mut c = PaperExecutionClient::new("oanda", "EURUSD", 0.0, 0.0, 0.0);
    c.submit(&market("USDJPY"));
    assert!(refusal(&drain(&mut c)).is_some(), "the order names a yen-quoted symbol");

    let mut c = PaperExecutionClient::new("oanda", "USDJPY", 0.0, 0.0, 0.0);
    c.submit(&OrderRequest { symbol: String::new(), ..market("USDJPY") });
    assert!(refusal(&drain(&mut c)).is_some(), "an unnamed symbol is the book's own");
}
