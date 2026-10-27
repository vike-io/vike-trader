//! The instrument grid: what each row's own precision becomes, what a garbage row becomes (nothing, with a
//! sentence), how a price and a unit count are written on it, and when the book reads the list.
//!
//! ⚠ **Docs-shaped, not captured.** The payload below follows OANDA's documentation of
//! `GET /v3/accounts/{id}/instruments` (<https://developer.oanda.com/rest-live-v20/account-ep/>) and of the
//! `Instrument` object (<https://developer.oanda.com/rest-live-v20/primitives-df/>): the precisions are the
//! documented examples (5 for `EUR_USD`, 3 for `USD_JPY` and the other JPY pairs); no practice-account
//! response has been recorded, and the invented instruments (`NEGPR_USD` ...) exist only to be garbage.

use std::cell::Cell;

use serde_json::json;

use super::*;

/// One docs-shaped `Instrument`; `display` / `units` are the raw JSON values so a row can be garbage.
fn row(name: &str, kind: &str, display: Option<Value>, units: Value) -> Value {
    let mut row = json!({
        "name": name,
        "type": kind,
        "displayName": name.replace('_', "/"),
        "pipLocation": -4,
        "tradeUnitsPrecision": units,
        "minimumTradeSize": "1",
        "maximumOrderUnits": "100000000",
        "marginRate": "0.03",
    });
    if let Some(display) = display {
        row["displayPrecision"] = display;
    }
    row
}

fn currency(name: &str, display: i64) -> Value {
    row(name, "CURRENCY", Some(json!(display)), json!(0))
}

fn payload() -> Value {
    json!({ "instruments": [
        currency("EUR_USD", 5),
        currency("AUD_USD", 5),
        currency("GBP_USD", 5),
        currency("NZD_USD", 5),
        currency("USD_JPY", 3),
        currency("EUR_JPY", 3),
        currency("GBP_JPY", 3),
        row("XAU_USD", "METAL", Some(json!(3)), json!(2)),
        row("XAG_USD", "METAL", Some(json!(5)), json!(0)),
        row("SPX500_USD", "CFD", Some(json!(1)), json!(1)),
        row("NEGPR_USD", "CURRENCY", Some(json!(-1)), json!(0)),
        row("STRPR_USD", "CURRENCY", Some(json!("5")), json!(0)),
        row("BIGPR_USD", "CURRENCY", Some(json!(12)), json!(0)),
        row("NOPR_USD", "CURRENCY", None, json!(0)),
        row("NEGUN_USD", "CURRENCY", Some(json!(5)), json!(-1)),
        row("STRUN_USD", "CURRENCY", Some(json!(5)), json!("0")),
    ]})
}

fn grid(name: &str) -> Result<InstrumentGrid, String> {
    parse_grids(&payload()).remove(name).unwrap_or_else(|| panic!("{name} is in the payload"))
}

fn dp(name: &str) -> (u32, u32) {
    let g = grid(name).unwrap_or_else(|e| panic!("{name}: {e}"));
    (g.price_decimals(), g.unit_decimals())
}

/// A grid built the way the exec loop builds it: from the properties a catalog row states.
fn on(tick: f64, step: f64) -> InstrumentGrid {
    InstrumentGrid::from_properties(&SymbolProperties {
        tick_size: tick,
        step_size: step,
        ..Default::default()
    })
    .expect("a power-of-ten grid")
}

#[test]
fn each_instrument_gets_the_precision_its_own_row_states() {
    for name in ["EUR_USD", "AUD_USD", "GBP_USD", "NZD_USD"] {
        assert_eq!(dp(name), (5, 0), "{name}");
    }
    for name in ["USD_JPY", "EUR_JPY", "GBP_JPY"] {
        assert_eq!(dp(name), (3, 0), "{name}: a JPY pair is three decimals");
    }
    // The units precision is read too, though only a currency pair is admitted at the door today.
    assert_eq!(dp("XAU_USD"), (3, 2));
    assert_eq!(dp("SPX500_USD"), (1, 1));
}

/// **Fail closed.** A row whose precision is a string, negative, beyond the believed range or absent has
/// NO grid, and says which instrument and which field; nothing falls back to five decimals.
#[test]
fn a_garbage_precision_has_no_grid_and_the_sentence_names_it() {
    for (name, field) in [
        ("NEGPR_USD", "displayPrecision"),
        ("STRPR_USD", "displayPrecision"),
        ("BIGPR_USD", "displayPrecision"),
        ("NOPR_USD", "displayPrecision"),
        ("NEGUN_USD", "tradeUnitsPrecision"),
        ("STRUN_USD", "tradeUnitsPrecision"),
    ] {
        let why = grid(name).expect_err(name);
        assert!(
            why.starts_with(&format!("oanda: {name}: ")) && why.contains(field),
            "{name}: {why}"
        );
    }
}

/// OANDA refuses a price with more decimals than `displayPrecision`, so the text has EXACTLY that many.
#[test]
fn a_price_is_written_with_exactly_display_precision_decimals() {
    let eur = grid("EUR_USD").expect("EUR_USD");
    let jpy = grid("USD_JPY").expect("USD_JPY");
    for side in [1, -1] {
        assert_eq!(eur.price(side, 1.09), "1.09000", "side {side}");
        assert_eq!(eur.price(side, 1.0), "1.00000", "side {side}");
        assert_eq!(jpy.price(side, 150.123), "150.123", "side {side}");
        assert_eq!(jpy.price(side, 150.0), "150.000", "side {side}");
        assert_eq!(jpy.price(side, 99.5), "99.500", "side {side}");
    }
}

/// An off-grid price is rounded to the PASSIVE side: a BUY down, a SELL up. A price on the grid is the
/// grid point whichever side it is on.
#[test]
fn an_off_grid_limit_price_rounds_to_the_passive_side() {
    let jpy = on(0.001, 1.0);
    assert_eq!(jpy.price(1, 150.1234), "150.123");
    assert_eq!(jpy.price(-1, 150.1234), "150.124");
    assert_eq!(jpy.price(1, 150.1239), "150.123");
    assert_eq!(jpy.price(-1, 150.1239), "150.124");
    let eur = on(0.00001, 1.0);
    assert_eq!(eur.price(1, 1.090004), "1.09000");
    assert_eq!(eur.price(-1, 1.090004), "1.09001");
    assert_eq!(eur.price(-1, 0.0000004), "0.00001", "less than a tick up is a tick");
    let whole = on(1.0, 1.0);
    assert_eq!(whole.price(1, 150.4), "150");
    assert_eq!(whole.price(-1, 150.4), "151");
    // What the engine's `(x / step).round() * step` hands over is the grid point it names, a SELL included,
    // whether the product lands on the decimal's own f64 or the ULP above it: never the next tick up.
    let product = 150_123_f64 * 0.001_f64;
    for price in [product, f64::from_bits(product.to_bits() + 1)] {
        assert_eq!(jpy.price(-1, price), "150.123", "{price:?}");
        assert_eq!(jpy.price(1, price), "150.123", "{price:?}");
    }
}

#[test]
fn units_are_whole_for_a_currency_pair_and_fractional_where_the_row_says_so() {
    let whole = grid("EUR_USD").expect("EUR_USD");
    assert_eq!(whole.units(1, 1000.5).as_deref(), Some("1001"), "ties away from zero, as always");
    assert_eq!(whole.units(-1, 1000.4).as_deref(), Some("-1000"));
    assert_eq!(whole.units(1, 0.4), None, "rounds to nothing");
    assert_eq!(whole.units(-1, 0.4), None);
    assert_eq!(whole.units(1, f64::NAN), None);

    let tenths = on(0.001, 0.1);
    assert_eq!(tenths.units(1, 1.26).as_deref(), Some("1.3"));
    assert_eq!(tenths.units(-1, 1.26).as_deref(), Some("-1.3"));
    assert_eq!(tenths.units(1, 2.0).as_deref(), Some("2.0"));
    assert_eq!(tenths.units(1, 0.04), None);
    assert_eq!(on(0.01, 0.001).units(-1, 0.0004), None);
    assert_eq!(on(0.01, 0.001).units(-1, 0.0125).as_deref(), Some("-0.013"));
}

/// The door's fence over the SAME rows: a currency pair is admitted on its own price grid, whole units
/// only; a metal or CFD keeps the old five-decimal rule; garbage has no grid at the door either.
#[test]
fn the_door_admits_a_currency_pair_on_its_own_grid_and_keeps_the_old_rule_for_the_rest() {
    let rows = parse_instruments(&payload());
    let refusal = |name: &str| {
        let r = rows.iter().find(|r| r.raw_symbol == name).expect(name);
        grid_refusal(name, r.contract_type.as_deref(), &r.properties)
    };
    for name in ["EUR_USD", "AUD_USD", "USD_JPY", "EUR_JPY", "GBP_JPY"] {
        assert_eq!(refusal(name), None, "{name}");
    }
    // metals and CFDs: the old rule, the old words
    assert!(refusal("XAU_USD").expect("three decimals").contains("five decimals"));
    assert!(refusal("SPX500_USD").expect("one decimal").contains("five decimals"));
    assert_eq!(
        refusal("XAG_USD"),
        None,
        "five decimals, whole units: admitted before, admitted now"
    );
    // garbage: a negative or out-of-range displayPrecision is named; an absent or string one has a zero tick
    for name in ["NEGPR_USD", "BIGPR_USD"] {
        let why = refusal(name).expect(name);
        assert!(why.contains("not a whole number of decimal places"), "{name}: {why}");
    }
    for name in ["STRPR_USD", "NOPR_USD"] {
        assert!(refusal(name).is_some(), "{name}: no tick is never an admission");
    }
    assert!(refusal("NEGUN_USD").is_some() && refusal("STRUN_USD").is_some());
}

/// The door and the exec client read ONE rule: a currency pair the door admits always has an exec grid with
/// whole units and the very decimals the door's tick states.
#[test]
fn what_the_door_admits_the_exec_client_can_write() {
    for r in parse_instruments(&payload()) {
        if r.contract_type.as_deref() == Some("CURRENCY")
            && grid_refusal(&r.raw_symbol, r.contract_type.as_deref(), &r.properties).is_none()
        {
            let g = InstrumentGrid::from_properties(&r.properties)
                .unwrap_or_else(|e| panic!("{}: {e}", r.raw_symbol));
            assert_eq!(g.unit_decimals(), 0, "{}", r.raw_symbol);
            assert_eq!(
                TICKS[g.price_decimals() as usize],
                r.properties.tick_size,
                "{}",
                r.raw_symbol
            );
        }
    }
}

// --- the book ------------------------------------------------------------------------------------

/// A reader that counts its calls and answers `reply`.
fn reader<'a>(
    calls: &'a Cell<u32>,
    reply: Result<Value, String>,
) -> impl FnOnce() -> Result<Value, String> + 'a {
    move || {
        calls.set(calls.get() + 1);
        reply
    }
}

#[test]
fn the_book_reads_the_list_once_for_every_instrument() {
    let (mut book, calls, t0) = (GridBook::default(), Cell::new(0), Instant::now());
    for (name, want) in [("EUR_USD", (5, 0)), ("USD_JPY", (3, 0)), ("EUR_JPY", (3, 0))] {
        let g = book.of(name, t0, reader(&calls, Ok(payload()))).expect(name);
        assert_eq!((g.price_decimals(), g.unit_decimals()), want, "{name}");
    }
    assert_eq!(calls.get(), 1);
}

#[test]
fn an_unlisted_instrument_is_refused_and_the_list_is_not_read_again_inside_the_pause() {
    let (mut book, calls, t0) = (GridBook::default(), Cell::new(0), Instant::now());
    let why = book.of("NOPE_USD", t0, reader(&calls, Ok(payload()))).expect_err("unlisted");
    assert!(
        why.contains("not in the account's instrument list") && why.contains("guessed grid"),
        "{why}"
    );
    book.of("NOPE_USD", t0 + Duration::from_secs(5), reader(&calls, Ok(payload())))
        .expect_err("still");
    assert_eq!(calls.get(), 1, "a flood of orders costs one read");
    book.of("NOPE_USD", t0 + READ_PAUSE, reader(&calls, Ok(payload())))
        .expect_err("after the pause");
    assert_eq!(calls.get(), 2, "read again once the pause is over");
}

/// An unreadable list is a refusal that says so: never the five decimals this adapter used to write.
#[test]
fn an_unreadable_list_refuses_the_order_with_the_reason_and_is_retried_after_the_pause() {
    let (mut book, calls, t0) = (GridBook::default(), Cell::new(0), Instant::now());
    let why = book
        .of("EUR_USD", t0, reader(&calls, Err("OANDA error 503: down".into())))
        .expect_err("no list, no grid");
    assert!(
        why.contains("EUR_USD") && why.contains("503") && why.contains("guessed grid"),
        "{why}"
    );
    book.of("USD_JPY", t0 + Duration::from_secs(1), reader(&calls, Ok(payload())))
        .expect_err("inside the pause nothing is read");
    assert_eq!(calls.get(), 1);
    let g = book.of("USD_JPY", t0 + READ_PAUSE, reader(&calls, Ok(payload()))).expect("recovered");
    assert_eq!(g.price_decimals(), 3);
    assert_eq!(calls.get(), 2);
}

#[test]
fn a_failed_reread_keeps_what_the_book_already_holds() {
    let (mut book, calls, t0) = (GridBook::default(), Cell::new(0), Instant::now());
    book.of("EUR_USD", t0, reader(&calls, Ok(payload()))).expect("first read");
    let later = t0 + READ_PAUSE;
    let why =
        book.of("NOPE_USD", later, reader(&calls, Err("timeout".into()))).expect_err("unlisted");
    assert!(why.contains("timeout"), "{why}");
    assert_eq!(
        book.of("USD_JPY", later, reader(&calls, Ok(payload()))).expect("held").price_decimals(),
        3
    );
    assert_eq!(calls.get(), 2, "the held list answered without a third read");
}

/// A garbage row is cached as a refusal: asking again does not re-read, and never yields a grid.
#[test]
fn a_garbage_instrument_stays_refused_without_another_read() {
    let (mut book, calls, t0) = (GridBook::default(), Cell::new(0), Instant::now());
    for _ in 0..3 {
        let why = book.of("NEGPR_USD", t0, reader(&calls, Ok(payload()))).expect_err("garbage");
        assert!(why.contains("displayPrecision"), "{why}");
    }
    assert_eq!(calls.get(), 1);
}

#[test]
fn an_empty_list_is_a_failed_read_not_a_book_of_nothing() {
    let (mut book, calls, t0) = (GridBook::default(), Cell::new(0), Instant::now());
    let why = book
        .of("EUR_USD", t0, reader(&calls, Ok(json!({ "instruments": [] }))))
        .expect_err("empty");
    assert!(why.contains("came back empty"), "{why}");
}
