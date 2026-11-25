//! Deribit on ANY perpetual of the account's own money model (`docs/ops/deribit-any-symbol-plan.md`),
//! over the local WebSocket stand-in in `crate::fake_deribit_websocket`. No venue, no credentials, no
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
use vike_deribit::ReconcileClientDeribit;
use vike_deribit::execution::rest::{InstrumentRows, VenueRestDeribit};
use vike_execution::recon::ReconcileClient;
use vike_model::events::{Event, PositionSide};
use vike_model::{
    MoneyModel, NotionalKind, OrderCombo, OrderComboLeg, OrderRequest, OrderType, SymbolProperties,
    build_combo,
};

use crate::fake_deribit_websocket::{
    FakeDeribit, ORDER_ID, SYMBOL, Session, outcome, rejection, rest_against, transport_against,
};

const ORDER_CLIENT_ID: &str = "vike-1";

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
        // A dated BTC future on a FINER grid than the mounted perpetual's: an instrument an inverse BTC
        // account may add (one settle coin per engine, decision 0258) whose own grid shows in a price.
        row("BTC-26JUN26", "future", "month", "reversed", "btc", 0.05, 1.0),
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
    properties: SymbolProperties,
    currency: &str,
    model: MoneyModel,
    reads: &Arc<AtomicUsize>,
) -> VenueRestDeribit {
    // held before the client is built: it reads the mounted instrument's USER name, and every order names one
    vike_deribit::names::hold_deribit_kept_rows();
    let counted = Arc::clone(reads);
    let source: InstrumentRows = Box::new(move |name| {
        counted.fetch_add(1, Ordering::SeqCst);
        rows()
            .into_iter()
            .find(|r| r["instrument_name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .ok_or_else(|| format!("{name} is not listed"))
    });
    VenueRestDeribit::new(transport_against(fake), mounted, properties, currency)
        .with_foreign_instruments(model, source)
        .with_clock(crate::before_the_captures)
}

fn limit(order_client_id: &str, symbol: &str, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "deribit".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        timestamp: 1_700_000_000_000,
        ..Default::default()
    }
}

/// **The headline.** An order on `BTC-26JUN26` from an account mounted on `BTC-PERPETUAL` goes out
/// on `BTC-26JUN26`, its price on THAT grid (a 0.05 tick: 2500.05 survives; the mounted 0.5 tick
/// would have snapped it away) and its amount on THAT lot. The row is read once however many orders
/// follow.
#[test]
fn a_foreign_inverse_future_is_signed_on_its_own_instrument_and_grid() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("BTC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);

    let event = outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("BTC-26JUN26"),
        3.0,
        2500.05,
    )));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let event = outcome(rest.submit_order(&limit(
        "vike-2",
        &crate::user("BTC-26JUN26").to_ascii_lowercase(),
        4.0,
        2500.10,
    )));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");

    let sent = fake.params_of("private/buy");
    assert_eq!(sent.len(), 2);
    for (frame, price, amount) in [(&sent[0], 2500.05, 3.0), (&sent[1], 2500.1, 4.0)] {
        assert_eq!(frame["instrument_name"], "BTC-26JUN26");
        assert_eq!(frame["price"], price, "snapped on the future's grid, not the mounted one");
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
    let event = outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("ETH_USDC-PERPETUAL"),
        0.0005,
        2500.05,
    )));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let frame = fake.params_of("private/buy")[0].clone();
    assert_eq!(frame["instrument_name"], "ETH_USDC-PERPETUAL");
    assert_eq!(frame["price"], 2500.05, "the 0.01 tick of ETH_USDC");
    assert_eq!(frame["amount"], 0.0005);
}

/// **THE LIVE-MONEY CASE.** The connection dies after `private/buy` left; the order IS live under its
/// label in its settlement currency only. The re-query must ask that currency: a wrong one answers `[]`
/// with no error, and `[]` is read as "the venue never took it" (a terminal reject of a live order). One
/// settle coin per engine (decision 0258) makes an added inverse instrument's currency the mounted one;
/// the USDC case below is the one where the instrument's name would mislead.
#[test]
fn the_audit_t1_re_query_of_a_foreign_order_asks_in_its_own_currency() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::ForeignBook("BTC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);

    let event = outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("BTC-26JUN26"),
        3.0,
        2500.05,
    )));
    match event {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("a live order must never be reported rejected: {other:?}"),
    }
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked.len(), 1, "one re-query");
    assert_eq!(asked[0]["currency"], "BTC", "the ORDER's settlement currency");
    assert_eq!(asked[0]["label"], ORDER_CLIENT_ID);
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
    let event = outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("ETH_USDC-PERPETUAL"),
        0.0005,
        2500.05,
    )));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked[0]["currency"], "USDC");
}

/// A combo whose first leg is `first`: its `symbol` is empty at submit.
fn combo(first: &str) -> OrderRequest {
    let spec = OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            OrderComboLeg { symbol: first.into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: crate::user("BTC-27MAR27"), ratio: -1, limit: None },
        ],
        net_limit: Some(5.0),
        time_in_force: Default::default(),
        account: None,
        order_client_id: None,
    };
    build_combo(&spec, ORDER_CLIENT_ID).expect("the spec is valid")
}

/// **An ambiguous combo is asked in ITS OWN currency.** Its `symbol` is empty, which used to mean the
/// mounted one; the first leg's instrument names the currency (one settle coin per engine, decision
/// 0258, makes it the mounted one for an inverse account). (A sale signed first is what makes the
/// client know BTC-26JUN26's row.)
#[test]
fn an_ambiguous_combo_is_looked_up_in_its_first_legs_currency() {
    let fake = FakeDeribit::spawn(vec![Session::CloseOnBuy, Session::ForeignBook("BTC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    let sale =
        OrderRequest { side: -1, ..limit("vike-sale", &crate::user("BTC-26JUN26"), 3.0, 2500.05) };
    assert!(matches!(outcome(rest.submit_order(&sale)), Event::OrderAccepted(_)));

    match outcome(rest.submit_order(&combo(&crate::user("BTC-26JUN26")))) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the combo landed in BTC: {other:?}"),
    }
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["currency"], "BTC", "the leg's settlement");
    assert_eq!(fake.count("private/buy"), 1, "the order frame is never re-sent");
}

/// A first leg this client knows no row for cannot be asked: inconclusive, never a reject (the mounted
/// currency's `[]` used to reject it).
#[test]
fn an_ambiguous_combo_on_an_unknown_currency_stays_unknown() {
    let fake = FakeDeribit::spawn(vec![Session::CloseOnBuy, Session::OkForeverOrderAbsent]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);

    crate::fake_deribit_websocket::stays_unknown(
        rest.submit_order(&combo(&crate::user("BTC-26JUN26"))),
    );
    assert_eq!(fake.count("private/get_order_state_by_label"), 0, "no currency to ask in");
}

/// A combo on the mounted instrument is asked in the mounted currency, and a venue listing nothing
/// there says nothing: the stuck-order ladder ends the order.
#[test]
fn an_ambiguous_combo_absent_in_its_own_currency_says_nothing() {
    let fake = FakeDeribit::spawn(vec![Session::CloseOnBuy, Session::OkForeverOrderAbsent]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);

    crate::fake_deribit_websocket::stays_unknown(rest.submit_order(&combo(&crate::user(SYMBOL))));
    let asked = fake.params_of("private/get_order_state_by_label");
    assert!(asked.iter().all(|a| a["currency"] == "BTC"), "{asked:?}");
}

/// **Every refusal says why and writes nothing**: the other money model, an inverse contract of another
/// coin (decision 0258), another linear settlement currency, a coin-settled option, a spot pair, an
/// unlisted symbol (a dated future of the account's own model is admitted: `class_admission`). A refused
/// symbol is read once (the refusal is remembered for a while).
#[test]
fn what_this_account_cannot_hold_is_refused_with_the_reason_and_nothing_is_sent() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    for (symbol, why) in [
        (crate::user("BTC_USDC-PERPETUAL"), "LINEAR"),
        (crate::user("ETH-PERPETUAL"), "settles in ETH"),
        ("BTC-1JAN27-100000-C".to_string(), "coin-settled option"),
        (crate::user("BTC_USDC"), "spot pair"),
        // no kept row, no instrument: refused before any read (decision 0290)
        ("XYZUSD.P".to_string(), "nothing was sent"),
    ] {
        let refused = outcome(rest.submit_order(&limit(ORDER_CLIENT_ID, &symbol, 1.0, 100.0)));
        let Event::OrderRejected(r) = &refused else { panic!("{refused:?}") };
        assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        let reason = rejection(refused);
        assert!(reason.contains(why) && reason.contains(&symbol), "{symbol}: {reason}");
    }
    assert_eq!(fake.count("private/buy"), 0, "nothing reached the wire");
    let before = reads.load(Ordering::SeqCst);
    let again = rejection(outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("BTC_USDC-PERPETUAL"),
        1.0,
        1.0,
    ))));
    assert!(again.contains("LINEAR"), "{again}");
    assert_eq!(reads.load(Ordering::SeqCst), before, "a refusal is remembered, not re-read");

    // A linear account is refused the inverse model, and a linear instrument of another settlement.
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let rest =
        client(&fake, "BTC_USDC-PERPETUAL", usdc_grid(), "USDC", linear_model("USDC"), &reads);
    let reason = rejection(outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("ETH-PERPETUAL"),
        1.0,
        100.0,
    ))));
    assert!(reason.contains("INVERSE"), "{reason}");
    let reason = rejection(outcome(rest.submit_order(&limit(
        ORDER_CLIENT_ID,
        &crate::user("ETH_USDT-PERPETUAL"),
        1.0,
        100.0,
    ))));
    assert!(reason.contains("settles in USDT") && reason.contains("USDC"), "{reason}");
    assert_eq!(fake.count("private/buy"), 0);
}

/// A client built the old way (no capability) refuses another instrument too, instead of signing it
/// on the mounted one, which is what it used to do.
#[test]
fn a_client_without_the_capability_never_signs_another_instrument() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);
    let reason =
        rejection(outcome(rest.submit_order(&limit(ORDER_CLIENT_ID, "ETH-PERPETUAL", 1.0, 100.0))));
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
    rest.cancel_order(ORDER_CLIENT_ID).expect("found by the open-order list and cancelled");
    assert_eq!(
        fake.methods(),
        ["private/get_order_state_by_label", "private/get_open_orders", "private/cancel"]
    );
    assert_eq!(fake.params_of("private/cancel")[0]["order_id"], ORDER_ID);
}

/// **An open-order answer that names no order is no answer, not "nothing there".** Something other than
/// a list, or a row under the label with no order id, used to let the cancel report success with the
/// order still resting; now the cancel says it could not find out. A real empty list is still an answer.
#[test]
fn an_open_order_answer_naming_no_order_is_no_answer_for_a_cancel() {
    let reads = Arc::new(AtomicUsize::new(0));
    for answer in [r#"{"orders":[]}"#, r#"[{"label":"vike-1","order_state":"open"}]"#] {
        let fake = FakeDeribit::spawn(vec![Session::OpenOrdersAnswer(answer)]);
        let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
        let err = rest.cancel_order(ORDER_CLIENT_ID).expect_err("the list was not answered");
        // No cancel left: NOT SENT, never a cancel of unknown outcome (decision 0303).
        assert_eq!(err.code, 0, "{answer}");
        assert_eq!(fake.count("private/cancel"), 0, "{answer}");
    }
    let fake = FakeDeribit::spawn(vec![Session::OpenOrdersAnswer("[]")]);
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    rest.cancel_order(ORDER_CLIENT_ID)
        .expect("the venue holds nothing under the label, in any currency");
    assert_eq!(fake.count("private/cancel"), 0);
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
    rest.cancel_order(ORDER_CLIENT_ID)
        .expect("the venue holds nothing under the label: already gone");
    assert_eq!(fake.methods(), ["private/get_order_state_by_label"]);
}

/// An amend of a foreign order is formatted on the grid of the ORDER's instrument.
#[test]
fn an_amend_of_a_foreign_order_is_formatted_on_its_own_grid() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("BTC")]);
    let reads = Arc::new(AtomicUsize::new(0));
    let rest = client(&fake, SYMBOL, btc_grid(), "BTC", inverse_model(), &reads);
    let resting = limit(ORDER_CLIENT_ID, &crate::user("BTC-26JUN26"), 3.0, 2500.0);
    let events = rest.modify_order(&resting, None, Some(2500.05));
    assert!(matches!(events.as_slice(), [Event::OrderModified(_)]), "{events:?}");
    let edit = fake.params_of("private/edit")[0].clone();
    assert_eq!(edit["order_id"], ORDER_ID);
    assert_eq!(edit["price"], 2500.05, "the future's 0.05 tick");
    assert_eq!(edit["amount"], 3.0);
    // It found the order in the order's settlement currency.
    let asked = fake.params_of("private/get_order_state_by_label");
    assert_eq!(asked.last().expect("a lookup")["currency"], "BTC");
}

/// **The reconcile client reads every admitted instrument** with the mounted instrument's request, and
/// positions from one `currency=any` read (an admitted instrument may settle in another currency).
#[test]
fn the_reconcile_client_reads_every_admitted_instrument() {
    // told USER names and reporting under them (decision 0290); the venue is asked by its own
    let (mounted, eth, eth_usdc) =
        (crate::user(SYMBOL), crate::user("ETH-PERPETUAL"), crate::user("ETH_USDC-PERPETUAL"));
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let client = ReconcileClientDeribit::new(rest_against(&fake));
    assert_eq!(client.admitted_symbols(), Some(vec![]), "a claim: told nothing yet");
    client.admit_symbol(&eth);
    client.admit_symbol(&eth_usdc.to_ascii_lowercase());
    client.admit_symbol(&mounted);
    client.admit_symbol(&eth);
    client.admit_symbol("ETH-PERPETUAL");
    assert_eq!(
        client.admitted_symbols(),
        Some(vec![eth.clone(), eth_usdc.clone()]),
        "any case, the mounted symbol, a repeat and a venue name typed as a user name ignored"
    );

    let orders = client.fetch_order_status_reports(0).expect("orders");
    let symbols: Vec<&str> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, [mounted.as_str(), eth.as_str(), eth_usdc.as_str()]);

    let fills = client.fetch_fill_reports(0).expect("fills");
    let ids: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
    assert_eq!(ids, ["t-BTC-PERPETUAL", "t-ETH-PERPETUAL", "t-ETH_USDC-PERPETUAL"]);

    let positions = client.fetch_position_status_reports().expect("positions");
    let held: Vec<(&str, f64)> = positions.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(held, [(mounted.as_str(), 10.0), (eth.as_str(), -5.0), (eth_usdc.as_str(), 0.5)]);
    let currencies: Vec<Value> =
        fake.params_of("private/get_positions").iter().map(|p| p["currency"].clone()).collect();
    assert_eq!(currencies, [json!("BTC"), json!("any")]);
}

/// **B-5: a rate limit on an ADMITTED read silences the pass** (`vike_bridge_core::reconcile_admitted`).
/// The admitted instrument's read answered 10028 fails the fetch in the words every fetch has always
/// used, and the next fetch refuses before it asks anything. Any other refusal fails the fetch alone: the
/// next one asks again.
#[test]
fn a_rate_limited_admitted_read_silences_the_pass_and_any_other_refusal_does_not() {
    for (code, message, pauses) in
        [(10028, "too_many_requests", true), (10009, "not_enough_funds", false)]
    {
        let fake = FakeDeribit::spawn(vec![Session::VenueErrorFor {
            instrument: "ETH-PERPETUAL",
            code,
            message,
        }]);
        let client = ReconcileClientDeribit::new(rest_against(&fake));
        client.admit_symbol(&crate::user("ETH-PERPETUAL"));
        let err = client.fetch_order_status_reports(0).expect_err("the admitted read failed");
        assert_eq!(err, format!("[{code}] {message}"));
        let next = client.fetch_fill_reports(0).expect_err("the admitted fill read fails too");
        let asked = fake.params_of("private/get_user_trades_by_instrument").len();
        if pauses {
            assert!(next.contains("backing off"), "{next}");
            assert_eq!(asked, 0, "nothing is asked while the pass is silent");
        } else {
            assert_eq!(next, format!("[{code}] {message}"));
            assert_eq!(asked, 2, "the mounted and the admitted fill reads were asked");
        }
    }
}

/// With nothing admitted the reads are the mounted instrument's: one request per kind, the mounted
/// currency's positions, no `currency=any`; the order fetch adds the account's open orders, for its
/// combos alone (the plain ETH row that answer carries is no combo and is not reported), and the fill
/// fetch the settle currency's fills, for its combo legs alone.
#[test]
fn without_an_admitted_instrument_the_reconcile_reads_the_mounted_one_and_the_combos() {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("ETH")]);
    let client = ReconcileClientDeribit::new(rest_against(&fake));
    let orders = client.fetch_order_status_reports(0).expect("orders");
    let mounted = crate::user(SYMBOL);
    assert_eq!(orders.iter().map(|o| o.symbol.as_str()).collect::<Vec<_>>(), [mounted.as_str()]);
    client.fetch_fill_reports(0).expect("fills");
    client.fetch_position_status_reports().expect("positions");
    assert_eq!(
        fake.methods(),
        [
            "private/get_open_orders_by_instrument",
            "private/get_open_orders",
            "private/get_user_trades_by_instrument",
            "private/get_user_trades_by_currency",
            "private/get_positions"
        ]
    );
    assert_eq!(fake.params_of("private/get_open_orders")[0], json!({}));
    assert_eq!(
        fake.params_of("private/get_user_trades_by_currency")[0],
        json!({"currency": "BTC", "count": 1000, "sorting": "asc"})
    );
    assert_eq!(fake.params_of("private/get_positions")[0]["currency"], "BTC");
}

/// The settle currency's fills: the two LEG rows and the combo book's net print of ONE combo execution,
/// VERBATIM from the testnet capture (`combo_fill_fixtures.rs`, 2026-07-19: buy 10 USD of
/// `BTC-FS-25DEC26_20JUL26`), and a leg row on the MOUNTED instrument, which its own read reports too.
const LEG_TRADES: &str = r#"{"trades":[
    {"label":"gate4-fill-1784494768234","timestamp":1784494768235,"state":"filled","price":64451.5,"user_id":84789,"trade_id":"258544119","direction":"sell","instrument_name":"BTC-20JUL26","amount":10.0,"order_id":"108338717926","index_price":64466.04,"trade_seq":7396,"api":false,"mark_price":64470.33,"matching_id":null,"tick_direction":2,"profit_loss":0.0,"combo_id":"BTC-FS-25DEC26_20JUL26","mmp":false,"post_only":false,"reduce_only":false,"self_trade":false,"contracts":1.0,"combo_trade_id":"258544118","order_type":"limit","fee":8.0e-8,"fee_currency":"BTC","liquidity":"T","risk_reducing":false},
    {"label":"gate4-fill-1784494768234","timestamp":1784494768235,"state":"filled","price":65650.5,"user_id":84789,"trade_id":"258544120","direction":"buy","instrument_name":"BTC-25DEC26","amount":10.0,"order_id":"108338717928","index_price":64466.04,"trade_seq":2627757,"api":false,"mark_price":65634.83,"matching_id":null,"tick_direction":0,"profit_loss":0.0,"combo_id":"BTC-FS-25DEC26_20JUL26","mmp":false,"post_only":false,"reduce_only":false,"self_trade":false,"contracts":1.0,"combo_trade_id":"258544118","order_type":"limit","fee":0.0,"fee_currency":"BTC","liquidity":"T","risk_reducing":false},
    {"label":"gate4-fill-1784494768234","timestamp":1784494768235,"state":"filled","price":1199.0,"user_id":84789,"trade_id":"258544118","direction":"buy","instrument_name":"BTC-FS-25DEC26_20JUL26","amount":10.0,"order_id":"108338717925","index_price":64466.04,"trade_seq":1,"api":true,"mark_price":1164.5,"matching_id":null,"tick_direction":1,"profit_loss":null,"mmp":false,"post_only":false,"reduce_only":false,"self_trade":false,"contracts":1.0,"order_type":"limit","fee":0.0,"fee_currency":"BTC","liquidity":"T","risk_reducing":false},
    {"label":"vike-perp-combo","timestamp":1784494769000,"state":"filled","price":64500.0,"trade_id":"t-perp-leg","direction":"buy","instrument_name":"BTC-PERPETUAL","amount":10.0,"order_id":"108338717930","combo_id":"BTC-FS-5OCT26_PERP","combo_trade_id":"t-perp-net","fee":0.0,"fee_currency":"BTC","liquidity":"T"}
],"has_more":false}"#;
/// The mounted instrument's own fill read: the PERP leg's row, under the same `trade_id`.
const MOUNTED_TRADES: &str = r#"{"trades":[
    {"label":"vike-perp-combo","timestamp":1784494769000,"state":"filled","price":64500.0,"trade_id":"t-perp-leg","direction":"buy","instrument_name":"BTC-PERPETUAL","amount":10.0,"order_id":"108338717930","combo_id":"BTC-FS-5OCT26_PERP","combo_trade_id":"t-perp-net","fee":0.0,"fee_currency":"BTC","liquidity":"T"}
],"has_more":false}"#;
/// The BTC positions after the fills: the mounted perpetual, the two legs, and a future no combo of ours
/// touched (the probe found the legs ONLY under the combo's execution).
const LEG_POSITIONS: &str = r#"[
    {"instrument_name":"BTC-PERPETUAL","kind":"future","direction":"buy","size":10.0,"average_price":64500.0},
    {"instrument_name":"BTC-20JUL26","kind":"future","direction":"sell","size":-10.0,"average_price":64451.5},
    {"instrument_name":"BTC-25DEC26","kind":"future","direction":"buy","size":10.0,"average_price":65650.5},
    {"instrument_name":"BTC-27MAR26","kind":"future","direction":"buy","size":30.0,"average_price":70000.0}
]"#;

/// **A combo's LEGS are reconciled.** The core writes a combo's legs into its engine directly and never
/// admits them to this client, so a leg's fill and position were never read: a leg fill the stream
/// missed was never repaired, and the diff raised every held leg as an `OrphanLocalPosition`. Now the
/// leg rows of the settle currency's fills are reported (the combo book's net print never is, nor a row
/// the mounted read reports already), and each leg's position from the position read. A leg fill folded
/// live dedups; the one the stream missed is one `MissingFill`; nothing is reported twice.
#[test]
fn a_combos_legs_fill_and_position_are_reconciled_once() {
    let fake = FakeDeribit::spawn(vec![Session::Scripted(&[
        ("private/get_user_trades_by_currency", LEG_TRADES),
        ("private/get_user_trades_by_instrument", MOUNTED_TRADES),
        ("private/get_positions", LEG_POSITIONS),
    ])]);
    let client = ReconcileClientDeribit::new(rest_against(&fake));
    let fills = client.fetch_fill_reports(0).expect("fills");
    let ids: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
    assert_eq!(ids, ["t-perp-leg", "258544119", "258544120"], "no net print, no leg twice");
    assert_eq!(fills[1].order_client_id.as_deref(), Some("gate4-fill-1784494768234"));
    let positions = client.fetch_position_status_reports().expect("positions");
    let held: Vec<(&str, f64)> = positions.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    // the reports come back under the USER names the engine holds (decision 0290)
    let (mounted, near, far) =
        (crate::user(SYMBOL), crate::user("BTC-20JUL26"), crate::user("BTC-25DEC26"));
    assert_eq!(held, [(mounted.as_str(), 10.0), (near.as_str(), -10.0), (far.as_str(), 10.0)]);

    // The engine folded the perp leg and the near leg live; the stream missed the far leg.
    let seen: std::collections::HashSet<String> =
        ["t-perp-leg", "258544119"].map(String::from).into();
    let local_positions = indexmap::IndexMap::from([
        ((mounted.clone(), PositionSide::Both), 10.0),
        ((near, PositionSide::Both), -10.0),
        ((far, PositionSide::Both), 10.0),
    ]);
    let orders = indexmap::IndexMap::new();
    let local = vike_execution::recon::ReconcileLocalView {
        venue: "deribit",
        orders: &orders,
        seen_trade_ids: &seen,
        positions: &local_positions,
        quantity_tolerance: 0.0,
        cash: vike_execution::recon::ReconcileLocalCash::default(),
    };
    let kinds = |positions: &[vike_model::PositionStatusReport]| -> Vec<String> {
        vike_execution::recon::diff(&[], &fills, positions, &local, None)
            .iter()
            .map(|d| match d {
                vike_execution::recon::ReconcileDivergence::MissingFill(f) => {
                    format!("missing {}", f.trade_id)
                }
                other => format!("{:?}", other.kind()),
            })
            .collect()
    };
    assert_eq!(kinds(&positions), ["missing 258544120"]);
    let mounted_only: Vec<_> = positions.iter().filter(|p| p.symbol == mounted).cloned().collect();
    assert_eq!(
        kinds(&mounted_only),
        ["missing 258544120", "OrphanLocalPosition", "OrphanLocalPosition"],
        "the mounted read alone leaves each held leg an orphan"
    );
}

/// **A held leg is no orphan after a gatewayhub restart once the node tells the client the legs.** A new client
/// remembers no leg: the fills that taught the old one are older than the pass's window, so the settle currency's
/// fill read names none, and its position read reports the mounted instrument alone, every held leg an
/// `OrphanLocalPosition`. The node tells the new client each leg as an admitted instrument (the tradehub commits a
/// combo's legs and re-tells a restarted gatewayhub), and the admitted read reports both legs' positions.
#[test]
fn a_held_leg_the_node_told_a_new_client_is_no_orphan() {
    const NO_TRADES: &str = r#"{"trades":[],"has_more":false}"#;
    let answers: &'static [(&str, &str)] = &[
        ("private/get_user_trades_by_currency", NO_TRADES),
        ("private/get_user_trades_by_instrument", NO_TRADES),
        ("private/get_positions", LEG_POSITIONS),
    ];
    // the engine holds and tells USER names; the reports come back under them (decision 0290)
    let (near, far) = (crate::user("BTC-20JUL26"), crate::user("BTC-25DEC26"));
    let local_positions = indexmap::IndexMap::from([
        ((crate::user(SYMBOL), PositionSide::Both), 10.0),
        ((near.clone(), PositionSide::Both), -10.0),
        ((far.clone(), PositionSide::Both), 10.0),
    ]);
    let (orders, seen) = (indexmap::IndexMap::new(), std::collections::HashSet::new());
    let local = vike_execution::recon::ReconcileLocalView {
        venue: "deribit",
        orders: &orders,
        seen_trade_ids: &seen,
        positions: &local_positions,
        quantity_tolerance: 0.0,
        cash: vike_execution::recon::ReconcileLocalCash::default(),
    };
    let divergences = |client: &ReconcileClientDeribit| -> Vec<String> {
        let fills = client.fetch_fill_reports(0).expect("fills");
        let positions = client.fetch_position_status_reports().expect("positions");
        vike_execution::recon::diff(&[], &fills, &positions, &local, None)
            .iter()
            .map(|d| format!("{:?}", d.kind()))
            .collect()
    };

    let untold_fake = FakeDeribit::spawn(vec![Session::Scripted(answers)]);
    let untold = ReconcileClientDeribit::new(rest_against(&untold_fake));
    assert_eq!(divergences(&untold), ["OrphanLocalPosition", "OrphanLocalPosition"]);

    let told_fake = FakeDeribit::spawn(vec![Session::Scripted(answers)]);
    let told = ReconcileClientDeribit::new(rest_against(&told_fake));
    told.admit_symbol(&far);
    told.admit_symbol(&near);
    assert!(divergences(&told).is_empty(), "both legs are read: {:?}", divergences(&told));
}

/// **A resting COMBO is listed.** It rests on its own book (`BTC-CSR12-…`, a captured `option_combo`
/// name), which no instrument read names, so before the account read the pass never saw it and the
/// diff raised the live local combo as an `OrphanLocalOrder`. The account's open orders (the
/// documented order object: the combo smoke's fields, a credit's NEGATIVE net) carry our combo, a
/// plain order of another instrument and a combo of another currency; only our currency's combo is
/// reported, at its signed price, and the diff of a live local combo (no symbol of its own) against
/// the pass is quiet.
#[test]
fn a_resting_combo_is_listed_and_reconciled_like_any_order() {
    const ANSWER: &str = r#"[
        {"instrument_name":"BTC-CSR12-6OCT26-86000_86500","order_id":"108338717925","label":"vike-combo",
         "direction":"sell","order_type":"limit","order_state":"open","amount":1.0,"filled_amount":0.0,
         "average_price":0.0,"price":-0.0015,"post_only":false,"reduce_only":false,
         "time_in_force":"good_til_cancelled","creation_timestamp":1791088400000,
         "last_update_timestamp":1791088400000},
        {"instrument_name":"ETH_USDC-PERPETUAL","order_id":"2","label":"vike-plain","direction":"buy",
         "order_type":"limit","order_state":"open","amount":0.5,"filled_amount":0.0,"price":2500.0,
         "last_update_timestamp":1791088400000},
        {"instrument_name":"ETH-FS-5OCT26_PERP","order_id":"3","label":"vike-eth-combo","direction":"buy",
         "order_type":"limit","order_state":"open","amount":10.0,"filled_amount":0.0,"price":-1.5,
         "last_update_timestamp":1791088400000}
    ]"#;
    let fake = FakeDeribit::spawn(vec![Session::OpenOrdersAnswer(ANSWER)]);
    let client = ReconcileClientDeribit::new(rest_against(&fake));
    let orders = client.fetch_order_status_reports(0).expect("orders");
    let mounted = crate::user(SYMBOL);
    let combo = orders.iter().find(|o| o.symbol != mounted).expect("the combo row");
    assert_eq!(
        orders.len(),
        2,
        "the mounted instrument's row and our currency's combo: {orders:?}"
    );
    assert_eq!(combo.symbol, "BTC-CSR12-6OCT26-86000_86500");
    assert_eq!(combo.order_client_id.as_deref(), Some("vike-combo"));
    assert_eq!((combo.side, combo.qty, combo.price), (-1, 1.0, Some(-0.0015)));
    assert_eq!(
        (combo.order_type.clone(), combo.status),
        (OrderType::Limit, vike_model::OrderStatus::Accepted)
    );

    let mut live = vike_model::OrderRecord::new(OrderRequest {
        order_client_id: "vike-combo".into(),
        venue: "deribit".into(),
        // a combo order names no symbol of its own (`vike_model::build_combo`): its legs do
        order_type: OrderType::Limit,
        ..Default::default()
    });
    live.status = vike_model::OrderStatus::Accepted;
    let registry = indexmap::IndexMap::from([("vike-combo".to_string(), live)]);
    let (seen, positions) = (std::collections::HashSet::new(), indexmap::IndexMap::new());
    let local = vike_execution::recon::ReconcileLocalView {
        venue: "deribit",
        orders: &registry,
        seen_trade_ids: &seen,
        positions: &positions,
        quantity_tolerance: 0.0,
        cash: vike_execution::recon::ReconcileLocalCash::default(),
    };
    let orphaned = |reports: &[vike_model::OrderStatusReport]| {
        vike_execution::recon::diff(reports, &[], &[], &local, None).iter().any(|d| {
            matches!(d, vike_execution::recon::ReconcileDivergence::OrphanLocalOrder { .. })
        })
    };
    assert!(!orphaned(&orders), "a listed combo is no orphan");
    let mounted_only: Vec<_> = orders.iter().filter(|o| o.symbol == mounted).cloned().collect();
    assert!(orphaned(&mounted_only), "the instrument read alone leaves the live combo an orphan");
}

/// One failed read fails the whole fetch: the restore planner reads an order a SUCCESSFUL report lacks
/// as gone, so a partial report must never look complete.
#[test]
fn a_failed_read_of_an_admitted_instrument_fails_the_whole_fetch() {
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever]);
    let client = ReconcileClientDeribit::new(rest_against(&fake));
    client.admit_symbol("ETH-PERPETUAL");
    assert!(client.fetch_order_status_reports(0).is_err());
}

/// F-2: the fill read follows a page that says `has_more` (from its newest trade), and a venue that keeps
/// saying it while handing the same trades fails the read instead of reporting one page as the window. Red
/// before the fix: `has_more` was never read and the first page was the whole report.
#[test]
fn a_fill_read_follows_has_more_and_fails_when_the_chain_never_ends() {
    const MORE: &str = r#"{"trades":[{"label":"c1","timestamp":5,"state":"filled","price":1.0,"trade_id":"t1","direction":"buy","instrument_name":"BTC-PERPETUAL","amount":10.0,"order_id":"o1","fee":0.0,"fee_currency":"BTC","liquidity":"T"}],"has_more":true}"#;
    let fake = FakeDeribit::spawn(vec![Session::Scripted(&[
        ("private/get_user_trades_by_instrument", MORE),
        ("private/get_user_trades_by_currency", r#"{"trades":[],"has_more":false}"#),
    ])]);
    let client = ReconcileClientDeribit::new(rest_against(&fake));
    let why = client.fetch_fill_reports(1).expect_err("a chain that never ends");
    assert!(why.contains("brought no new one"), "{why}");
    let asked = fake.params_of("private/get_user_trades_by_instrument");
    assert_eq!(asked.len(), 2, "the page that said has_more was followed");
    assert_eq!(asked[0]["instrument_name"], SYMBOL);
    assert_eq!(asked[1]["start_timestamp"], 5, "from the newest trade read");
}
