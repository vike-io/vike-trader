//! Deribit on ANY perpetual of the account's own money model (`docs/ops/deribit-any-symbol-plan.md`),
//! over the local WebSocket stand-in in `crate::fake_deribit_ws`. No venue, no credentials, no
//! `#[ignore]`: the instrument rows come from an injected closure, the order socket from the fake.
//!
//! What these pin, each a way an order or a lookup could silently land wrong:
//!
//! * an order on another instrument is signed ON THAT INSTRUMENT, on ITS grid (the mounted tick would
//!   snap the price somewhere else);
//! * the audit-T1 re-query is asked in the currency the ORDER settles in: the endpoint answers `[]`
//!   with no error for a wrong one, which would be read as "the venue never took it";
//! * the other money model, a dated future, an option, an unlisted symbol and another linear settlement
//!   currency are each refused with the reason and NOTHING reaches the wire;
//! * a cancel or an amend of an order this client never submitted still finds it, whatever currency it
//!   sits in, and a client without the capability asks exactly what it always asked;
//! * the reconcile client reads every admitted instrument with the mounted instrument's request.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_deribit::DeribitReconClient;
use vike_deribit::client::{DeribitRest, InstrumentRows, MoneyModel};
use vike_exec::recon::ReconClient;
use vike_model::events::Event;
use vike_model::{NotionalKind, OrderRequest, OrderType, SymbolProperties};

use crate::fake_deribit_ws::{
    FakeDeribit, ORDER_ID, SYMBOL, Session, rest_against, transport_against,
};

const COID: &str = "vike-1";

fn inverse_model() -> MoneyModel {
    MoneyModel { kind: NotionalKind::Inverse, settle: "BTC".into() }
}

fn linear_model(settle: &str) -> MoneyModel {
    MoneyModel { kind: NotionalKind::Linear, settle: settle.into() }
}

/// A real-shaped `public/get_instrument` row.
fn row(
    name: &str,
    kind: &str,
    period: &str,
    margining: &str,
    settle: &str,
    tick: f64,
    step: f64,
) -> Value {
    json!({
        "instrument_name": name, "kind": kind, "settlement_period": period,
        "instrument_type": margining, "settlement_currency": settle,
        "base_currency": "ETH", "quote_currency": "USD", "counter_currency": "USD",
        "tick_size": tick, "min_trade_amount": step, "contract_size": step,
        "expiration_timestamp": 32_503_708_800_000i64,
        // Only an option reads these two; the rest ignore them.
        "option_type": "call", "strike": 100_000.0,
    })
}

fn rows() -> Vec<Value> {
    vec![
        row("ETH-PERPETUAL", "future", "perpetual", "reversed", "eth", 0.05, 1.0),
        row("SOL-PERPETUAL", "future", "perpetual", "reversed", "sol", 0.01, 1.0),
        row("BTC_USDC-PERPETUAL", "future", "perpetual", "linear", "usdc", 0.1, 0.0001),
        row("ETH_USDC-PERPETUAL", "future", "perpetual", "linear", "usdc", 0.01, 0.0001),
        row("ETH_USDT-PERPETUAL", "future", "perpetual", "linear", "usdt", 0.01, 0.0001),
        row("BTC-27MAR27", "future", "month", "reversed", "btc", 0.5, 10.0),
        row("BTC-1JAN27-100000-C", "option", "month", "reversed", "btc", 0.0005, 0.1),
        row("BTC_USDC", "spot", "", "linear", "usdc", 0.01, 0.0001),
    ]
}

fn btc_grid() -> SymbolProperties {
    SymbolProperties { tick_size: 0.5, step_size: 10.0, min_qty: 10.0, ..Default::default() }
}

fn usdc_grid() -> SymbolProperties {
    SymbolProperties { tick_size: 0.1, step_size: 0.0001, min_qty: 0.0001, ..Default::default() }
}

/// A client mounted on `mounted`/`currency`, able to sign foreign instruments of `model`, whose rows
/// are `rows()`; `reads` counts how often a row was read.
fn client(
    fake: &FakeDeribit,
    mounted: &str,
    props: SymbolProperties,
    currency: &str,
    model: MoneyModel,
    reads: &Arc<AtomicUsize>,
) -> DeribitRest {
    let counted = Arc::clone(reads);
    let source: InstrumentRows = Box::new(move |name| {
        counted.fetch_add(1, Ordering::SeqCst);
        rows()
            .into_iter()
            .find(|r| r["instrument_name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .ok_or_else(|| format!("{name} is not listed"))
    });
    DeribitRest::new(transport_against(fake), mounted, props, currency)
        .with_foreign_instruments(model, source)
}

fn limit(coid: &str, symbol: &str, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "deribit".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        ts: 1_700_000_000_000,
        ..Default::default()
    }
}

/// `[OrderSubmitted, X]`; returns X.
fn outcome(events: Vec<Event>) -> Event {
    assert_eq!(events.len(), 2, "the emitter split is [OrderSubmitted, terminal-or-accepted]");
    events.into_iter().nth(1).expect("second event")
}

fn rejection(event: Event) -> String {
    match event {
        Event::OrderRejected(r) => r.reason.to_string(),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// **The headline.** An order on `ETH-PERPETUAL` from an account mounted on `BTC-PERPETUAL` goes out
/// on `ETH-PERPETUAL`, its price on THAT grid (a 0.05 tick: 2500.05 survives; the mounted 0.5 tick
/// would have snapped it away) and its amount on THAT lot. The row is read once however many orders
/// follow.
#[test]
fn a_foreign_inverse_perpetual_is_signed_on_its_own_instrument_and_grid() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);

    let event = outcome(rest.submit_order(&limit(COID, "ETH-PERPETUAL", 3.0, 2500.05)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let event = outcome(rest.submit_order(&limit("vike-2", "eth-perpetual", 4.0, 2500.10)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");

    let sent = fake.params_of("private/buy");
    assert_eq!(sent.len(), 2);
    for (frame, price, amount) in [(&sent[0], 2500.05, 3.0), (&sent[1], 2500.1, 4.0)] {
        assert_eq!(frame["instrument_name"], "ETH-PERPETUAL");
        assert_eq!(frame["price"], price, "snapped on the ETH grid, not the mounted one");
        assert_eq!(frame["amount"], amount);
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1, "one instrument row, however many orders");

    // The mounted symbol (any case, or empty) is the old path: its own instrument and grid, no read.
    let event = outcome(rest.submit_order(&limit("vike-3", "", 10.0, 50_000.2)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let frame = fake.params_of("private/buy")[2].clone();
    assert_eq!(frame["instrument_name"], SYMBOL);
    assert_eq!(frame["price"], 50_000.0, "the mounted 0.5 tick");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

/// A linear account takes another linear perpetual of ITS settlement currency, on that row's grid.
#[test]
fn a_linear_account_signs_another_linear_perpetual_of_the_same_settlement() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest =
        client(&fake, "BTC_USDC-PERPETUAL", usdc_grid(), "USDC", linear_model("USDC"), &reads);
    let event = outcome(rest.submit_order(&limit(COID, "ETH_USDC-PERPETUAL", 0.0005, 2500.05)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let frame = fake.params_of("private/buy")[0].clone();
    assert_eq!(frame["instrument_name"], "ETH_USDC-PERPETUAL");
    assert_eq!(frame["price"], 2500.05, "the 0.01 tick of ETH_USDC");
    assert_eq!(frame["amount"], 0.0005);
}

/// **THE LIVE-MONEY CASE.** The connection dies after `private/buy` left; the order IS live under its
/// label in ETH only. The re-query must ask ETH: a wrong currency answers `[]` with no error, and `[]`
/// is read as "the venue never took it" (a terminal reject of a live order).
#[test]
fn the_audit_t1_re_query_of_a_foreign_order_asks_in_its_own_currency() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::ForeignBook("ETH")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);

    let event = outcome(rest.submit_order(&limit(COID, "ETH-PERPETUAL", 3.0, 2500.05)));
    match event {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("a live order must never be reported rejected: {other:?}"),
    }
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked.len(), 1, "one re-query");
    assert_eq!(asked[0]["currency"], "ETH", "the ORDER's settlement currency, not the mounted BTC");
    assert_eq!(asked[0]["label"], COID);
    assert_eq!(fake.count("private/buy"), 1, "the order frame is never re-sent");
}

/// The same, on a USDC instrument: `USDC`, never the instrument-name token `ETH_USDC` (error -32602).
#[test]
fn a_usdc_instrument_is_looked_up_in_usdc_not_in_its_name_token() {
    let fake =
        FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::ForeignBook("USDC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest =
        client(&fake, "BTC_USDC-PERPETUAL", usdc_grid(), "USDC", linear_model("USDC"), &reads);
    let event = outcome(rest.submit_order(&limit(COID, "ETH_USDC-PERPETUAL", 0.0005, 2500.05)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked[0]["currency"], "USDC");
}

/// **Every refusal says why and writes nothing**: the other money model, another linear settlement
/// currency, a dated future, an option, a spot pair, an unlisted symbol. A refused symbol is read once
/// (the refusal is remembered for a while).
#[test]
fn what_this_account_cannot_hold_is_refused_with_the_reason_and_nothing_is_sent() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    for (symbol, why) in [
        ("BTC_USDC-PERPETUAL", "LINEAR"),
        ("BTC-27MAR27", "not a perpetual"),
        ("BTC-1JAN27-100000-C", "not a perpetual"),
        ("BTC_USDC", "not a perpetual"),
        ("XYZ-PERPETUAL", "is not listed"),
    ] {
        let reason = rejection(outcome(rest.submit_order(&limit(COID, symbol, 1.0, 100.0))));
        assert!(reason.contains(why) && reason.contains(symbol), "{symbol}: {reason}");
    }
    assert_eq!(fake.count("private/buy"), 0, "nothing reached the wire");
    let before = reads.load(Ordering::SeqCst);
    let again = rejection(outcome(rest.submit_order(&limit(COID, "BTC_USDC-PERPETUAL", 1.0, 1.0))));
    assert!(again.contains("LINEAR"), "{again}");
    assert_eq!(reads.load(Ordering::SeqCst), before, "a refusal is remembered, not re-read");

    // A linear account is refused the inverse model, and a linear instrument of another settlement.
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let rest =
        client(&fake, "BTC_USDC-PERPETUAL", usdc_grid(), "USDC", linear_model("USDC"), &reads);
    let reason = rejection(outcome(rest.submit_order(&limit(COID, "ETH-PERPETUAL", 1.0, 100.0))));
    assert!(reason.contains("INVERSE"), "{reason}");
    let reason =
        rejection(outcome(rest.submit_order(&limit(COID, "ETH_USDT-PERPETUAL", 1.0, 100.0))));
    assert!(reason.contains("settles in USDT") && reason.contains("USDC"), "{reason}");
    assert_eq!(fake.count("private/buy"), 0);
}

/// A client built the old way (no capability) refuses another instrument too, instead of signing it
/// on the mounted one, which is what it used to do.
#[test]
fn a_client_without_the_capability_never_signs_another_instrument() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);
    let reason = rejection(outcome(rest.submit_order(&limit(COID, "ETH-PERPETUAL", 1.0, 100.0))));
    assert!(reason.contains("mounted instrument"), "{reason}");
    assert_eq!(fake.count("private/buy"), 0);
}

/// **A cancel of an order this client never submitted** (a restart's adopted order) asks the mounted
/// currency, finds nothing there, and then reads the account's open orders for the label, which span
/// every currency, before concluding anything; it cancels by the id it found.
#[test]
fn a_cancel_miss_falls_back_to_the_open_orders_of_every_currency() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    rest.cancel_order(COID).expect("found by the open-order list and cancelled");
    assert_eq!(
        fake.methods(),
        ["private/get_order_state_by_label", "private/get_open_orders", "private/cancel"]
    );
    assert_eq!(fake.params_of("private/cancel")[0]["order_id"], ORDER_ID);
}

/// A cancel miss on an instrument whose currency the client already knows asks THAT currency (the
/// row's `USDC`) and needs nothing more.
#[test]
fn a_cancel_miss_asks_every_currency_the_client_signed_in() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest =
        client(&fake, "BTC_USDC-PERPETUAL", usdc_grid(), "USDC", linear_model("USDC"), &reads);
    rest.cancel_order("vike-9").expect("found in USDC and cancelled");
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["currency"], "USDC", "the mounted row's settlement, not `BTC_USDC`");
    assert_eq!(fake.count("private/get_open_orders"), 0);
}

/// A client without the capability asks exactly what it always asked: the mounted currency, once; an
/// order the venue holds under no label of that currency is the old `Ok(())`, and no open-order list
/// is read.
#[test]
fn a_client_without_the_capability_cancels_exactly_as_before() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let rest = rest_against(&fake);
    rest.cancel_order(COID).expect("the venue holds nothing under the label: already gone");
    assert_eq!(fake.methods(), ["private/get_order_state_by_label"]);
}

/// An amend of a foreign order is formatted on the grid of the ORDER's instrument.
#[test]
fn an_amend_of_a_foreign_order_is_formatted_on_its_own_grid() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    let resting = limit(COID, "ETH-PERPETUAL", 3.0, 2500.0);
    let events = rest.modify_order(&resting, None, Some(2500.05));
    assert!(matches!(events.as_slice(), [Event::OrderModified(_)]), "{events:?}");
    let edit = fake.params_of("private/edit")[0].clone();
    assert_eq!(edit["order_id"], ORDER_ID);
    assert_eq!(edit["price"], 2500.05, "the ETH 0.05 tick");
    assert_eq!(edit["amount"], 3.0);
    // It found the order in ETH after the mounted BTC lookup came back empty.
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked.last().expect("a lookup")["currency"], "ETH");
}

/// **The reconcile client reads every admitted instrument** with the mounted instrument's request, and
/// positions from one `currency=any` read (an admitted instrument may settle in another currency).
#[test]
fn the_recon_client_reads_every_admitted_instrument() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let client = DeribitReconClient::new(rest_against(&fake));
    assert_eq!(client.admitted_symbols(), Some(vec![]), "a claim: told nothing yet");
    client.admit_symbol("ETH-PERPETUAL");
    client.admit_symbol("eth_usdc-perpetual");
    client.admit_symbol("BTC-PERPETUAL");
    client.admit_symbol("ETH-PERPETUAL");
    assert_eq!(
        client.admitted_symbols(),
        Some(vec!["ETH-PERPETUAL".to_string(), "ETH_USDC-PERPETUAL".to_string()]),
        "upper case, the mounted symbol and a repeat ignored"
    );

    let orders = client.fetch_order_status_reports(0).expect("orders");
    let symbols: Vec<&str> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, [SYMBOL, "ETH-PERPETUAL", "ETH_USDC-PERPETUAL"]);

    let fills = client.fetch_fill_reports(0).expect("fills");
    let ids: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
    assert_eq!(ids, ["t-BTC-PERPETUAL", "t-ETH-PERPETUAL", "t-ETH_USDC-PERPETUAL"]);

    let positions = client.fetch_position_status_reports().expect("positions");
    let held: Vec<(&str, f64)> = positions.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(held, [(SYMBOL, 10.0), ("ETH-PERPETUAL", -5.0), ("ETH_USDC-PERPETUAL", 0.5)]);
    let currencies: Vec<Value> =
        fake.params_of("private/get_positions").iter().map(|p| p["currency"].clone()).collect();
    assert_eq!(currencies, [json!("BTC"), json!("any")]);
}

/// With nothing admitted the reads are the old ones, byte for byte: one request per kind, the mounted
/// currency's positions, no `currency=any`.
#[test]
fn without_an_admitted_instrument_the_recon_reads_are_unchanged() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let client = DeribitReconClient::new(rest_against(&fake));
    client.fetch_order_status_reports(0).expect("orders");
    client.fetch_fill_reports(0).expect("fills");
    client.fetch_position_status_reports().expect("positions");
    assert_eq!(
        fake.methods(),
        [
            "private/get_open_orders_by_instrument",
            "private/get_user_trades_by_instrument",
            "private/get_positions"
        ]
    );
    assert_eq!(fake.params_of("private/get_positions")[0]["currency"], "BTC");
}

/// One failed read fails the whole fetch: the restore planner reads an order a SUCCESSFUL report lacks
/// as gone, so a partial report must never look complete.
#[test]
fn a_failed_read_of_an_admitted_instrument_fails_the_whole_fetch() {
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever]);
    let client = DeribitReconClient::new(rest_against(&fake));
    client.admit_symbol("ETH-PERPETUAL");
    assert!(client.fetch_order_status_reports(0).is_err());
}
