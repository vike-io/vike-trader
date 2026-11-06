//! The Binance classes beyond a plain perpetual, driven over slices of the REAL demo-host lists captured on
//! 2026-10-30 (`tests/fixtures/exchange_info/fapi_demo_classes_2026-10-30.json` and
//! `dapi_demo_classes_2026-10-30.json`, rows whole): TradFi perpetuals in the picker, the dated futures in the
//! symbol-facts listing only, the COIN-M book typed inverse, and the shared parser's behaviour for aster.
//!
//! | test | what it pins |
//! |---|---|
//! | [`a_tradfi_perpetual_is_a_picker_row_with_the_grid_the_venue_states`] | `TSLAUSDT.P`/`XAUUSDT.P` listed, their filters become the grid; pending and settling rows are not |
//! | [`the_facts_listing_adds_the_dated_futures_typed_with_their_expiry`] | quarterly and weekly rows typed `Future`, the date from `deliveryDate`; delivering/pending ones absent |
//! | [`the_picker_does_not_list_a_dated_future`] | the dated rows are facts-only |
//! | [`a_delivered_quarterlys_word_types_nothing`] | `CURRENT_QUARTER DELIVERING` is an unmeasured word, so `None` |
//! | [`the_coin_m_book_is_typed_inverse_with_its_contract_size`] | `parse_dapi`: inverse, `contractSize`, whole-contract grid |
//! | [`the_shared_picker_words_leave_an_asters_list_as_it_was`] | the aster shape (PERPETUAL + an empty word) lists the same rows |
//! | [`the_demo_list_url_is_the_demo_hosts`] | the const the facts read is the host the order path signs on |

use vike_model::{
    AssetClass, Contract, Delivery, FutureFamily, FutureSpec, Margining, SymbolProperties, Tenor,
};

use super::*;
use crate::family::catalog::fapi_contract;

fn fapi() -> Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/exchange_info/fapi_demo_classes_2026-10-30.json"
    ))
    .expect("fixture parses")
}

fn dapi() -> Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/exchange_info/dapi_demo_classes_2026-10-30.json"
    ))
    .expect("fixture parses")
}

fn symbols(rows: &[Instrument]) -> Vec<&str> {
    rows.iter().map(|i| i.raw_symbol.as_str()).collect()
}

fn find<'a>(rows: &'a [Instrument], symbol: &str) -> &'a Instrument {
    rows.iter().find(|i| i.raw_symbol == symbol).unwrap_or_else(|| panic!("{symbol} is not listed"))
}

#[test]
fn a_tradfi_perpetual_is_a_picker_row_with_the_grid_the_venue_states() {
    let rows = parse_perp(&fapi());
    // GOOGLEUSDT is PENDING_TRADING and FAKEKR000660USDT is SETTLING: not rows.
    assert_eq!(symbols(&rows), ["BTCUSDT.P", "XAUUSDT.P", "TSLAUSDT.P"]);
    let tsla = find(&rows, "TSLAUSDT.P");
    assert_eq!(
        tsla.contract_type.as_deref(),
        Some("TRADIFI_PERPETUAL"),
        "the venue's word, verbatim"
    );
    assert_eq!(tsla.asset_class(), AssetClass::CryptoPerp);
    assert_eq!(tsla.settle_asset(), Some("USDT"));
    assert!(matches!(&tsla.contract, Contract::Perp(p) if p.margining == Margining::Linear));
    assert_eq!(
        tsla.properties,
        SymbolProperties {
            tick_size: 0.01,
            step_size: 0.01,
            min_qty: 0.01,
            max_qty: 300.0,
            min_notional: 5.0,
            asset_class: Some(AssetClass::CryptoPerp),
            ..Default::default()
        }
    );
    let xau = find(&rows, "XAUUSDT.P");
    assert_eq!(
        (xau.properties.tick_size, xau.properties.step_size, xau.properties.max_qty),
        (0.01, 0.001, 60.0)
    );
}

#[test]
fn the_facts_listing_adds_the_dated_futures_typed_with_their_expiry() {
    let rows = parse_futures(&fapi());
    assert_eq!(
        symbols(&rows),
        [
            "BTCUSDT.P",
            "XAUUSDT.P",
            "TSLAUSDT.P",
            "BSBUSDT_260618.P",
            "BSBUSDT_260625.P",
            "BTCUSDT_261225.P",
            "ETHUSDT_261225.P",
        ],
        "PENDING_TRADING, SETTLING and DELIVERING rows are not listed"
    );
    let quarter = find(&rows, "BTCUSDT_261225.P");
    assert_eq!(
        quarter.contract,
        Contract::Future(FutureSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: None,
            expiry_ms: 1_798_185_600_000,
            tenor: Some(Tenor::Quarter),
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        })
    );
    assert_eq!(quarter.asset_class(), AssetClass::CryptoFuture);
    assert_eq!(quarter.contract_type.as_deref(), Some("CURRENT_QUARTER"));
    // Quantity is in BASE units, like a perpetual (MEASURED: 0.001 BTC filled at the touch).
    assert_eq!(
        (
            quarter.properties.tick_size,
            quarter.properties.step_size,
            quarter.properties.min_qty,
            quarter.properties.min_notional
        ),
        (0.1, 0.001, 0.001, 5.0)
    );
    // The weekly pair the demo lists: typed Week; its date is the (long past) deliveryDate, which the
    // gatewayhub's expiry guard refuses.
    for weekly in ["BSBUSDT_260618.P", "BSBUSDT_260625.P"] {
        assert!(
            matches!(&find(&rows, weekly).contract, Contract::Future(f) if f.tenor == Some(Tenor::Week)),
            "{weekly}"
        );
    }
    assert!(
        matches!(&find(&rows, "BSBUSDT_260618.P").contract, Contract::Future(f) if f.expiry_ms == 1_781_778_600_000)
    );
}

#[test]
fn the_picker_does_not_list_a_dated_future() {
    let picker = parse_perp(&fapi());
    assert!(
        picker.iter().all(|i| !matches!(i.contract, Contract::Future(_))),
        "{:?}",
        symbols(&picker)
    );
}

#[test]
fn a_delivered_quarterlys_word_types_nothing() {
    let payload = fapi();
    let row = payload["symbols"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["symbol"] == "ETHUSDT_250627"))
        .expect("the delivered quarterly");
    assert_eq!(row["contractType"], "CURRENT_QUARTER DELIVERING");
    assert_eq!(fapi_contract(row), None, "a contract being delivered is not typed");
}

#[test]
fn the_coin_m_book_is_typed_inverse_with_its_contract_size() {
    let rows = parse_dapi(&dapi());
    // EGLDUSD_PERP and ETCUSD_210924 are PENDING_TRADING, BTCUSD_260626 is DELIVERING.
    assert_eq!(
        symbols(&rows),
        ["BTCUSD_PERP.P", "ETHUSD_PERP.P", "BTCUSD_261225.P", "BTCUSD_270326.P"]
    );
    let perp = find(&rows, "BTCUSD_PERP.P");
    assert!(
        matches!(&perp.contract, Contract::Perp(p) if p.margining == Margining::Inverse && p.settle == "BTC"),
        "{:?}",
        perp.contract
    );
    assert_eq!((perp.base.as_str(), perp.quote.as_str()), ("BTC", "USD"));
    assert_eq!(perp.properties.contract_size, 100.0, "USD per contract, BTC");
    assert_eq!(find(&rows, "ETHUSD_PERP.P").properties.contract_size, 10.0);
    // Whole contracts: step 1, minimum 1; the market cap is MARKET_LOT_SIZE's; no MIN_NOTIONAL on this host.
    assert_eq!(
        (
            perp.properties.tick_size,
            perp.properties.step_size,
            perp.properties.min_qty,
            perp.properties.max_qty
        ),
        (0.1, 1.0, 1.0, 30_000.0)
    );
    assert_eq!(perp.properties.min_notional, 0.0);
    let quarter = find(&rows, "BTCUSD_261225.P");
    assert!(
        matches!(&quarter.contract, Contract::Future(f)
            if f.margining == Margining::Inverse && f.expiry_ms == 1_798_185_600_000
                && f.tenor == Some(Tenor::Quarter)),
        "{:?}",
        quarter.contract
    );
    assert_eq!(quarter.asset_class(), AssetClass::CryptoFuture);
    assert_eq!(quarter.contract_type.as_deref(), Some("CURRENT_QUARTER"));
    // A spot-shaped or USDⓈ-M payload has no `contractStatus`, so none of its rows is a COIN-M row.
    assert!(parse_dapi(&fapi()).is_empty());
}

#[test]
fn the_shared_picker_words_leave_an_asters_list_as_it_was() {
    // MEASURED 2026-10-30 on aster's live list: 638 `PERPETUAL` rows and 6 with an EMPTY `contractType`.
    let payload = serde_json::json!({ "symbols": [
        { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "marginAsset": "USDT",
          "status": "TRADING", "contractType": "PERPETUAL" },
        { "symbol": "NOWORDUSDT", "baseAsset": "NO", "quoteAsset": "USDT", "marginAsset": "USDT",
          "status": "TRADING", "contractType": "" },
    ]});
    let rows = crate::family::catalog::parse_perp(&payload, "aster");
    assert_eq!(symbols(&rows), ["BTCUSDT.P"]);
    assert_eq!(rows[0].venue, "aster");
}

#[cfg(feature = "exec")]
#[test]
fn the_demo_list_url_is_the_demo_hosts() {
    assert_eq!(
        DEMO_PERP_URL,
        format!("{}{}", crate::perp::DEMO_FAPI_REST, crate::perp::PATH_EXCHANGE_INFO)
    );
    assert_eq!(
        crate::instruments::COIN_M_EXCHANGE_INFO_URL,
        "https://dapi.binance.com/dapi/v1/exchangeInfo"
    );
}
