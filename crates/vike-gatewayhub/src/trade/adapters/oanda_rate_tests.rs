//! [`quoted_rate`] over the pricing body OANDA's pages DOCUMENT (a top-level `homeConversions` array of
//! `{currency, accountGain, accountLoss, positionValue}` decimal strings beside `prices`; DOCS-SHAPED: no body
//! was captured from a practice account). The goldens are the decision's own, in the orientation the engine
//! takes: account-currency units per ONE unit of the quote currency.

use serde_json::{Value, json};
use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_oanda::{HomeConversions, RateError};

use super::*;

const ASKED_AT_MS: i64 = 1_700_000_000_123;

fn body(entries: Value) -> Value {
    json!({ "time": "1666000000.000000000", "prices": [], "homeConversions": entries })
}

fn entry(currency: &str, gain: &str, loss: &str, position: &str) -> Value {
    json!({
        "currency": currency, "accountGain": gain, "accountLoss": loss, "positionValue": position
    })
}

/// A USD account asking after USD_JPY at 150.10 and EUR_GBP: JPY is a FRACTION of a dollar per yen, GBP is MORE
/// than a dollar per pound, USD converts at exactly 1.
fn usd_account(_instrument: &str) -> Result<HomeConversions, RateError> {
    let entries = json!([
        entry("USD", "1", "1", "1"),
        entry("JPY", "0.0066600", "0.0066650", "0.0066622"),
        entry("GBP", "1.3495", "1.3505", "1.35"),
    ]);
    HomeConversions::from_pricing_body("USD", &body(entries), ASKED_AT_MS)
}

/// **The orientation, by value**: USDJPY is a yen quote and the rate is a fraction of a dollar, EURGBP a pound
/// quote and the rate is above one, in both spellings of the instrument; the moment is the one the read stamped.
#[test]
fn a_yen_is_a_fraction_of_a_dollar_and_a_pound_is_more_than_one() {
    for jpy in ["USD_JPY", "USDJPY"] {
        let rate = quoted_rate(jpy, "USD", usd_account).expect("priced").expect("foreign");
        assert_eq!((rate.quote_ccy.as_str(), rate.as_of_ms), ("JPY", ASKED_AT_MS), "{jpy}");
        assert!(rate.account_per_quote < 0.01 && rate.account_per_quote > 0.006, "{jpy}");
        // 100,000 units at 150.10 are 15,010,000 yen = 100,000 dollars; a 0.10 move is 66.62 dollars.
        assert!((rate.account_per_quote * 100_000.0 * 150.10 - 100_000.0).abs() < 1.0, "{jpy}");
        assert!((rate.account_per_quote * 100_000.0 * 0.10 - 66.62).abs() < 0.01, "{jpy}");
    }
    for gbp in ["EUR_GBP", "EURGBP"] {
        let rate = quoted_rate(gbp, "USD", usd_account).expect("priced").expect("foreign");
        assert_eq!((rate.quote_ccy.as_str(), rate.account_per_quote), ("GBP", 1.35), "{gbp}");
        assert!((rate.account_per_quote * 85_000.0 - 114_750.0).abs() < 1e-6, "{gbp}");
    }
}

/// A pair quoted in the account's own currency needs no conversion, and the pricing is never read for it.
#[test]
fn a_usd_quoted_pair_on_a_usd_account_needs_no_rate_and_asks_nothing() {
    for pair in ["EUR_USD", "EURUSD", "SPX500_USD", "GBPUSD"] {
        let mut asked = false;
        let out = quoted_rate(pair, "USD", |i| {
            asked = true;
            usd_account(i)
        });
        assert_eq!(out, Ok(None), "{pair}");
        assert!(!asked, "{pair}: the venue is not asked about a pair that needs no conversion");
    }
    // The same pair on a GBP account IS foreign, and the same read cannot price it for that account.
    let wrong = quoted_rate("EUR_USD", "GBP", usd_account).expect_err("the read converts into USD");
    assert!(wrong.contains("converts into USD, not the account's GBP"), "{wrong}");
}

/// **No rate, no admission**: a name with no quote currency, a read that failed, a currency OANDA sent no factor
/// for and an unusable factor are each a refusal worded as a failure of the rate, which the node does not
/// remember as a verdict on the symbol; none answers 1.0.
#[test]
fn what_cannot_be_priced_is_refused_as_a_failed_rate_never_as_par() {
    let unnamed = quoted_rate("EURGBPX", "USD", usd_account).expect_err("no underscore");
    assert!(unnamed.contains("names no quote currency"), "{unnamed}");

    let down = quoted_rate("EUR_GBP", "USD", |_| Err(RateError::Unavailable("HTTP 503".into())))
        .expect_err("the venue is down");
    assert!(down.contains("no rate for GBP in USD") && down.contains("HTTP 503"), "{down}");
    assert!(facts_refusal_is_transient(&down), "{down}");

    let missing = quoted_rate("USD_CHF", "USD", usd_account).expect_err("CHF was not sent");
    assert!(missing.contains("no rate for CHF in USD"), "{missing}");
    assert!(facts_refusal_is_transient(&missing), "{missing}");

    let zero = |_: &str| {
        let entries = json!([entry("GBP", "1.35", "1.35", "0")]);
        HomeConversions::from_pricing_body("USD", &body(entries), ASKED_AT_MS)
    };
    let bad = quoted_rate("EUR_GBP", "USD", zero).expect_err("a zero factor is not a rate");
    assert!(bad.contains("no rate for GBP in USD"), "{bad}");
}

/// **The orientation through the PRODUCTION path, ending in the engine's books.** The engine cannot see the first
/// rate of a currency (nothing to compare it with), so the direction is the rate builder's to get right, and the
/// property tests that claim it (`vike-exec`'s `quote_rate_props.rs`) run over `RateBook`, which no production
/// code calls. This is the chain that does run: the docs-shaped pricing body -> `HomeConversions::from_pricing_body`
/// -> `quoted_rate` -> the admission the node's door builds from the statement (its three fields, copied) -> the
/// REAL engine -> a fill. A yen rate stated as 150 instead of 1/150 books a yen trade 22,500x too large, a pound
/// rate stated as 1/1.35 books 1.8x too small; each assertion below is off by that factor.
#[test]
fn the_venues_statement_books_a_yen_trade_in_dollars_and_a_pound_one_in_more_dollars() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{
        Account, AdmitOutcome, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate,
        SymbolAdmission,
    };
    use vike_model::events::{Event, FillEvent, PositionSide};

    const UNITS: f64 = 100_000.0;
    // The admission the node builds from a facts statement: the quote currency, the rate and the moment, as stated.
    let admission = |symbol: &str, instrument: &str| {
        let rate = quoted_rate(instrument, "USD", usd_account).expect("priced").expect("foreign");
        SymbolAdmission {
            route_key: "oanda".to_string(),
            symbol: symbol.to_string(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.00001),
                lot_size: Some(1.0),
                ..vike_model::SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: Some(rate.quote_ccy),
            quote_rate: Some(rate.account_per_quote),
            rate_as_of_ms: Some(rate.as_of_ms),
            im_requirement: None,
        }
    };
    let fill = |id: &'static str, symbol: &str, side: i32, px: f64| FillEvent {
        trade_id: id.into(),
        client_order_id: String::new(),
        venue: "oanda".into(),
        symbol: symbol.into(),
        side,
        last_qty: UNITS,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: ASKED_AT_MS,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "oanda", None, BalanceMode::Delta),
        RiskGate::new(vike_model::RiskLimits::new()),
        RecordingClient::default(),
        "oanda",
        "EURUSD",
    );
    engine.set_account_ccy("USD").expect("a currency code");
    engine.now_ms = ASKED_AT_MS;

    // USDJPY: 100,000 units at 150.00 are 15,000,000 yen = about 100,000 dollars (never 15,000,000 "dollars").
    assert_eq!(engine.admit_symbol(&admission("USDJPY", "USD_JPY")), AdmitOutcome::Admitted);
    let _ = engine.on_event(&Event::Fill(fill("o1", "USDJPY", 1, 150.0)), &mut Outbox::default());
    let notional = engine.account.contract_value_of("USDJPY").gross_notional(UNITS, 150.0);
    assert!((notional / 100_000.0 - 1.0).abs() < 0.001, "100,000 dollars, not yen: {notional}");
    // ...and closed 0.10 higher (the rate re-stated first, as the node does) it realizes 10,000 yen = 66.62 dollars.
    assert_eq!(engine.admit_symbol(&admission("USDJPY", "USD_JPY")), AdmitOutcome::Refreshed);
    let _ = engine.on_event(&Event::Fill(fill("c1", "USDJPY", -1, 150.10)), &mut Outbox::default());
    let realized = engine.account.realized_pnl;
    assert!(
        (realized - 10_000.0 * 0.0066622).abs() < 1e-6,
        "66.62 dollars, not 10,000: {realized}"
    );

    // EURGBP: 100,000 euros at 0.85 are 85,000 pounds = 114,750 dollars (never 85,000, never 62,963).
    assert_eq!(engine.admit_symbol(&admission("EURGBP", "EUR_GBP")), AdmitOutcome::Admitted);
    let value = engine.account.contract_value_of("EURGBP").gross_notional(UNITS, 0.85);
    assert!((value - 114_750.0).abs() < 1e-6, "114,750 dollars: {value}");
}
