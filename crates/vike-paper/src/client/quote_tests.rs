//! A paper book is ONE dollar ledger with no rate source (decision 0183 item 7/13): an order on a
//! symbol whose NAME says it is quoted in another fiat currency is refused in words, never booked at
//! par (USDJPY at 150 books 150x too much, EURGBP 1.35x too little).

use super::test_support::drain;
use super::*;
use vike_execution::ExecutionClient;
use vike_execution::testing::rejection;
use vike_model::OrderRequest;

fn market(symbol: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: "o1".into(),
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
    events_on("oanda", symbol)
}

/// [`events_for`] on a book of `venue`.
fn events_on(venue: &str, symbol: &str) -> Vec<Event> {
    let mut c = ExecutionClientPaper::new(venue, symbol, 0.0, 0.0, 0.0);
    c.submit(&OrderRequest { venue: venue.into(), ..market(symbol) });
    drain(&mut c)
}

#[test]
fn a_foreign_quoted_symbol_is_refused_in_words_not_booked_at_par() {
    for (symbol, currency) in [
        ("USDJPY", "JPY"),
        ("EURGBP", "GBP"),
        ("EUR_GBP", "GBP"),
        ("eurgbp", "GBP"),
        ("DE30_EUR", "EUR"),
        ("CS.D.GBPJPY.CFD.IP", "JPY"),
    ] {
        let events = events_for(symbol);
        let why = rejection(&events).unwrap_or_else(|| panic!("{symbol} was accepted: {events:?}"));
        assert!(why.contains(currency), "{symbol}: the words name the currency {currency}: {why}");
        assert!(why.contains("USD"), "{symbol}: and the ledger it would be booked in: {why}");
        assert!(
            !events.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "{symbol}: a refused order is never accepted: {events:?}"
        );
    }
}

/// **IBKR-1: an ibkr instrument quoted in yen or euros is never booked in dollars** (decision 0290): its quote is its
/// KEPT row's (`vike_catalog::kept_fiat_quote` over the operator's row), so the euro stock `SAP`, whose user name says
/// nothing of its money, is refused like the pair `USDJPY`; the dollar stock `AAPL` is booked as before.
#[test]
fn an_ibkr_instrument_is_quoted_in_the_money_its_kept_row_states() {
    super::test_support::hold_ibkr_rows();
    for (symbol, currency) in [("USDJPY", "JPY"), ("SAP", "EUR")] {
        let reason = foreign_quote_refusal("ibkr", symbol).expect("refused");
        assert!(reason.contains(&format!("quoted in {currency}")), "{symbol}: {reason}");
    }
    assert_eq!(foreign_quote_refusal("ibkr", "AAPL"), None);
}

/// Every symbol the old book took and that the new one must keep taking, byte for byte: dollar-quoted
/// pairs and CFDs, crypto, equities, and a name that says nothing about its quote.
#[test]
fn a_dollar_quoted_or_unnamed_quote_symbol_is_accepted_as_before() {
    for (venue, symbol) in [
        ("oanda", "EURUSD"),
        ("oanda", "EUR_USD"),
        ("oanda", "SPX500_USD"),
        ("oanda", "XAU_USD"),
        ("binance", "BTCUSDT"),
        ("binance", "BTCUSDC"),
        ("okx", "BTC-USD"),
        ("alpaca", "AAPL"),
        ("hyperliquid", "BTC"),
    ] {
        let events = events_on(venue, symbol);
        assert_eq!(rejection(&events), None, "{symbol}: {events:?}");
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "{symbol}: {events:?}"
        );
    }
}

/// **IG-1: on a broker's book a symbol whose currency is not known is refused, never booked at par**: an IG index
/// epic and an FX broker's name that names no currency (no kept row states one on a test's box). ibkr's name is one
/// the operator's held rows lack: this binary holds those rows, so `AAPL` there is a dollar stock.
#[test]
fn a_brokers_symbol_of_no_known_currency_is_refused() {
    super::test_support::hold_ibkr_rows();
    for (venue, symbol) in [("ig", "IX.D.DAX.IFD.IP"), ("oanda", "AAPL"), ("ibkr", "MSFT")] {
        let events = events_on(venue, symbol);
        let why = rejection(&events).unwrap_or_else(|| panic!("{venue} {symbol}: {events:?}"));
        assert!(why.starts_with("paper-quote-currency") && why.contains("not known"), "{why}");
        assert!(!events.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{events:?}");
    }
}

/// **An IG paper `EURUSD` market order fills at the ask, quoted in the currency its KEPT row states** (the CI box's
/// 2026-11-17 regression: IG's list rows stated no quote, so a broker's row of no currency refused the pair the deploy
/// before had filled). The rows are IG's list rows as `crates/bridges/ig/src/catalog.rs`'s `row_of` writes them now
/// (`CS.D.EURUSD.CFD.IP` states base `EUR`, quote `USD`) and as it wrote them before (`GBPUSD` stating none): a row of
/// no quote on a broker is refused in words, never booked at par, whatever its name says.
#[test]
fn an_ig_paper_eurusd_market_order_fills_at_the_ask_quoted_as_its_kept_row_states() {
    use vike_model::{Contract, ContractFx, Instrument, InstrumentNaming};
    let row = |epic: &str, user: &str, base: &str, quote: &str| {
        let mut row = Instrument::new("ig", epic, base, quote, Contract::Fx(ContractFx::default()));
        row.naming = Some(InstrumentNaming::User(user.into()));
        row
    };
    // Every test of this binary that holds ig's list holds THESE rows (one list per process).
    vike_catalog::hold_kept_list(
        "ig",
        vec![
            row("CS.D.EURUSD.CFD.IP", "EURUSD", "EUR", "USD"),
            row("CS.D.GBPUSD.CFD.IP", "GBPUSD", "", ""),
        ],
    );
    assert_eq!(foreign_quote_refusal("ig", "EURUSD"), None, "the row's USD is the ledger's");
    let mut c = ExecutionClientPaper::new("ig", "EURUSD", 0.0, 0.0, 0.0);
    c.on_contract_value("EURUSD", vike_model::ContractValue::linear(100_000.0));
    c.on_quote("EURUSD", 1.08495, 1.08505, 60_000);
    c.submit(&OrderRequest { venue: "ig".into(), qty: 1.0, ..market("EURUSD") });
    assert_eq!(rejection(&drain(&mut c)), None, "taken");
    let fills: Vec<(i32, f64)> =
        c.fills.lock().unwrap().iter().map(|f| (f.side, f.price)).collect();
    assert_eq!(fills, [(1, 1.08505)], "a market buy fills at the ask at once");

    let why = foreign_quote_refusal("ig", "GBPUSD").expect("a broker's row of no quote");
    assert!(why.starts_with("paper-quote-currency") && why.contains("not known"), "{why}");
    let refused = events_on("ig", "GBPUSD");
    assert!(rejection(&refused).is_some_and(|r| r == why), "{refused:?}");
}

/// The order's own symbol decides when it names one (a multi-symbol router hands each book its own),
/// and an order with no symbol is the book's.
#[test]
fn the_orders_symbol_decides_and_an_empty_one_is_the_books() {
    let mut c = ExecutionClientPaper::new("oanda", "EURUSD", 0.0, 0.0, 0.0);
    c.submit(&market("USDJPY"));
    assert!(rejection(&drain(&mut c)).is_some(), "the order names a yen-quoted symbol");

    let mut c = ExecutionClientPaper::new("oanda", "USDJPY", 0.0, 0.0, 0.0);
    c.submit(&OrderRequest { symbol: String::new(), ..market("USDJPY") });
    assert!(rejection(&drain(&mut c)).is_some(), "an unnamed symbol is the book's own");
}
