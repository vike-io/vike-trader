//! Deribit on the instrument CLASSES the door used to refuse by name (decision 0233, A64), over the local
//! WebSocket stand-in in `crate::fake_deribit_ws`, on rows CAPTURED from the testnet
//! (`tests/fixtures/deribit_classes_testnet_*.json`, 2026-10-30). No venue, no credentials, no `#[ignore]`.
//!
//! What these pin, each a way an order or a reconcile could silently land wrong:
//!
//! * a DATED future is signed on its own instrument and grid by an account of its money model (an inverse
//!   one counts US dollars, a linear one the base coin) and refused for the other model;
//! * a USDC option is signed on its TIERED price grid, and its SALE goes out reduce-only: an added option
//!   is held long only, because a long's premium is its whole risk (the ledger books it exactly) and a
//!   short's margin is not the premium (measured 122.26 against 8.6 USDC for the same call);
//! * a coin-settled option and a spot pair are refused BY NAME, with the measured reason;
//! * a LINEAR future's position is read in BASE-coin units (`size_currency`): the venue's `size` is the
//!   quote notional (0.0001 BTC read 7.678599), which a reconcile would take for an amount of coin.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_deribit::client::{DeribitRest, InstrumentRows, MoneyModel};
use vike_deribit::recon_client::parse_positions;
use vike_model::events::Event;
use vike_model::{NotionalKind, OrderRequest, OrderType, SymbolProperties};

use crate::fake_deribit_ws::{FakeDeribit, Session, transport_against};

const INSTRUMENTS: &str = include_str!("../fixtures/deribit_classes_testnet_instruments.json");
const POSITIONS: &str = include_str!("../fixtures/deribit_classes_testnet_positions.json");

fn row(name: &str) -> Value {
    let all: Value = serde_json::from_str(INSTRUMENTS).expect("the captured rows parse");
    all.get(name).cloned().unwrap_or_else(|| panic!("no captured row for {name}"))
}

fn position(label: &str) -> Value {
    let all: Value = serde_json::from_str(POSITIONS).expect("the captured positions parse");
    all.get(label).cloned().unwrap_or_else(|| panic!("no captured position {label}"))
}

fn inverse_model() -> MoneyModel {
    MoneyModel { kind: NotionalKind::Inverse, settle: "BTC".into() }
}

fn linear_model() -> MoneyModel {
    MoneyModel { kind: NotionalKind::Linear, settle: "USDC".into() }
}

/// An account mounted on `mounted` (grid `props`, settlement `currency`), able to sign what the captured
/// rows hold.
fn client(
    fake: &FakeDeribit,
    mounted: &str,
    props: SymbolProperties,
    currency: &str,
    model: MoneyModel,
) -> DeribitRest {
    let source: InstrumentRows = Box::new(|name| {
        let all: Value = serde_json::from_str(INSTRUMENTS).expect("captured rows");
        all.get(name.to_ascii_uppercase().as_str())
            .or_else(|| all.get(name))
            .cloned()
            .ok_or_else(|| format!("{name} is not listed"))
    });
    DeribitRest::new(transport_against(fake), mounted, props, currency)
        .with_foreign_instruments(model, source)
}

fn inverse_mount(fake: &FakeDeribit) -> DeribitRest {
    let props =
        SymbolProperties { tick_size: 0.5, step_size: 10.0, min_qty: 10.0, ..Default::default() };
    client(fake, "BTC-PERPETUAL", props, "BTC", inverse_model())
}

fn linear_mount(fake: &FakeDeribit) -> DeribitRest {
    let props = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.0001,
        min_qty: 0.0001,
        ..Default::default()
    };
    client(fake, "BTC_USDC-PERPETUAL", props, "USDC", linear_model())
}

fn order(coid: &str, symbol: &str, side: i32, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "deribit".to_string(),
        symbol: symbol.to_string(),
        side,
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

/// **A dated INVERSE future is signed on its own instrument and grid** by an inverse account: the 2.5 tick
/// and the 10-dollar lot of the captured `BTC-3NOV26` row, not the mounted perpetual's 0.5.
#[test]
fn a_dated_inverse_future_is_signed_on_its_own_grid() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = inverse_mount(&fake);
    let event = outcome(rest.submit_order(&order("c-1", "BTC-3NOV26", 1, 20.0, 76_801.0)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let frame = fake.params_of("private/buy")[0].clone();
    assert_eq!(frame["instrument_name"], "BTC-3NOV26");
    assert_eq!(
        frame["price"], 76_800.0,
        "snapped on the 2.5 tick (76801 is between 76800 and 76802.5)"
    );
    assert_eq!(frame["amount"], 20.0, "US dollars, two lots of 10");
}

/// A dated LINEAR USDC future is signed by a linear account on its own row, in USDC, and refused to an
/// inverse one with the other model named.
#[test]
fn a_dated_linear_future_goes_to_the_linear_account_only() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = linear_mount(&fake);
    let event = outcome(rest.submit_order(&order("c-2", "BTC_USDC-3NOV26", 1, 0.0002, 76_800.0)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let frame = fake.params_of("private/buy")[0].clone();
    assert_eq!(frame["instrument_name"], "BTC_USDC-3NOV26");
    assert_eq!(frame["amount"], 0.0002, "the base coin, two lots of 0.0001");

    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = inverse_mount(&fake);
    let reason = rejection(outcome(rest.submit_order(&order(
        "c-3",
        "BTC_USDC-3NOV26",
        1,
        0.0001,
        76_800.0,
    ))));
    assert!(reason.contains("LINEAR") && reason.contains("INVERSE"), "{reason}");
    assert_eq!(fake.count("private/buy"), 0, "nothing reached the wire");
}

/// **A USDC option is signed on its tiered price grid, and its SALE is reduce-only.** The captured row
/// ticks 5 up to 1000 and 20 above it: 1722 snaps to 1720, 860.5 to 860. A buy carries no reduce-only
/// flag (it opens a long); the sale carries it (it can only close one).
#[test]
fn a_usdc_option_is_signed_on_its_tier_and_is_sold_reduce_only() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = linear_mount(&fake);
    let name = "BTC_USDC-6NOV26-77000-C";
    for (coid, price) in [("o-1", 1_722.0), ("o-2", 860.5)] {
        let event = outcome(rest.submit_order(&order(coid, name, 1, 0.01, price)));
        assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    }
    let bought = fake.params_of("private/buy");
    assert_eq!(bought[0]["price"], 1_720.0, "the 20 tier above 1000");
    assert_eq!(bought[1]["price"], 860.0, "the 5 tier below it");
    assert_eq!(bought[0]["amount"], 0.01, "the option's 0.01 minimum, in the underlying");
    assert!(
        bought.iter().all(|f| f.get("reduce_only").is_none()),
        "a buy opens a long: {bought:?}"
    );

    let event = outcome(rest.submit_order(&order("o-3", name, -1, 0.01, 1_740.0)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    let sold = fake.params_of("private/sell")[0].clone();
    assert_eq!(sold["instrument_name"], name);
    assert_eq!(sold["reduce_only"], json!(true), "an added option is held long only: {sold}");
}

/// **A decimal strike is asked in the venue's spelling** (`2d5`, lower-case `d`: 2,908 of the testnet's
/// USDC options, XRP and TRX and HYPE among them): an upper-cased name is another string to the venue.
#[test]
fn an_option_with_a_decimal_strike_is_asked_in_the_venues_spelling() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let source: InstrumentRows = Box::new(move |name| {
        seen.lock().unwrap().push(name.to_string());
        let mut r = row("BTC_USDC-6NOV26-77000-C");
        r["instrument_name"] = json!(name);
        Ok(r)
    });
    let props = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.0001,
        min_qty: 0.0001,
        ..Default::default()
    };
    let rest = DeribitRest::new(transport_against(&fake), "BTC_USDC-PERPETUAL", props, "USDC")
        .with_foreign_instruments(linear_model(), source);
    let event = outcome(rest.submit_order(&order("d-1", "xrp_usdc-6nov26-2D5-c", 1, 0.01, 100.0)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    assert_eq!(*asked.lock().unwrap(), ["XRP_USDC-6NOV26-2d5-C"]);
    assert_eq!(fake.params_of("private/buy")[0]["instrument_name"], "XRP_USDC-6NOV26-2d5-C");
}

/// A sale of a FUTURE is never touched by the long-only rule.
#[test]
fn a_future_sale_is_not_reduce_only() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = linear_mount(&fake);
    let event = outcome(rest.submit_order(&order("f-1", "BTC_USDC-3NOV26", -1, 0.0001, 76_800.0)));
    assert!(matches!(event, Event::OrderAccepted(_)), "{event:?}");
    assert!(fake.params_of("private/sell")[0].get("reduce_only").is_none());
}

/// **A coin-settled option and a spot pair are refused BY NAME** with the measured reason, and nothing is
/// written. The option's premium is coin (a ledger of one currency would book 0.0225 BTC as 0.0225
/// dollars); a spot pair has no position at the venue at all.
#[test]
fn a_coin_option_and_a_spot_pair_are_refused_by_name() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = inverse_mount(&fake);
    let reason =
        rejection(outcome(rest.submit_order(&order("r-1", "BTC-6NOV26-77000-C", 1, 0.1, 0.0225))));
    assert!(reason.contains("coin-settled option") && reason.contains("BTC"), "{reason}");
    let reason =
        rejection(outcome(rest.submit_order(&order("r-2", "BTC_USDC", 1, 0.0001, 70_000.0))));
    assert!(reason.contains("spot pair") && reason.contains("no position"), "{reason}");
    assert_eq!(fake.count("private/buy"), 0, "nothing reached the wire");
}

/// **A LINEAR future's position is read in base-coin units**: the captured rows of a 0.0001 BTC long
/// (`size` 7.678599 USDC, `size_currency` 0.0001) and short (`size` -7.68406, `size_currency` -0.0001), on
/// the dated future and on the perpetual. An INVERSE future's `size` is already the order's dollars and an
/// option's the base coin, so those stay as they were.
#[test]
fn a_linear_futures_position_is_read_in_base_coin_units() {
    let rows = |label: &str| format!("[{}]", position(label));
    let qty = |label: &str, symbol: &str| {
        parse_positions(&rows(label), symbol, 1_793_390_000_000).expect("parses")[0].qty
    };
    assert_eq!(qty("position BTC_USDC-3NOV26", "BTC_USDC-3NOV26"), 0.0001, "linear dated, long");
    assert_eq!(
        qty("short position BTC_USDC-3NOV26", "BTC_USDC-3NOV26"),
        -0.0001,
        "linear dated, short"
    );
    assert_eq!(
        qty("position BTC_USDC-PERPETUAL", "BTC_USDC-PERPETUAL"),
        0.0001,
        "linear perpetual, long"
    );
    assert_eq!(
        qty("short position BTC_USDC-PERPETUAL", "BTC_USDC-PERPETUAL"),
        -0.0001,
        "linear perpetual, short"
    );
    assert_eq!(qty("position BTC-3NOV26", "BTC-3NOV26"), 10.0, "inverse: US dollars, untouched");
    assert_eq!(qty("short position BTC-PERPETUAL", "BTC-PERPETUAL"), -10.0, "inverse short");
    assert_eq!(
        qty("position BTC_USDC-6NOV26-77000-C", "BTC_USDC-6NOV26-77000-C"),
        0.01,
        "an option's size is the base coin"
    );
}
