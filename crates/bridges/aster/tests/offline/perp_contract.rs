//! `vike_aster::perp::parse_aster_perp_instruments` over a REAL Aster fapi `exchangeInfo` body: each
//! entry's typed `Contract` is read off its own fields (`contractType`, `marginAsset` against
//! `quoteAsset`/`baseAsset`, `deliveryDate`), and `SymbolProperties::asset_class` is that contract's
//! projection, `None` when the entry does not say.
//!
//! `tests/fixtures/exchange_info/aster_fapi.json` is four entries, verbatim, of mainnet `GET
//! https://fapi.asterdex.com/fapi/v3/exchangeInfo` read 2026-10-11 (621 symbols): `BTCUSDT` (a
//! `TRADING` USDT perp), `BTCUSD1` (a `USD1`-margined perp), `TONUSDT` (a `SETTLING` perp with a real
//! `deliveryDate`) and `MBLUSDT` (a `PENDING_TRADING` listing whose `contractType` is EMPTY). The
//! shapes Aster lists none of today (a dated future, an inverse or quanto margin asset, an unmeasured
//! `contractType` word) are pinned below on minimal synthetic entries.
#![cfg(feature = "exec")]

use serde_json::{Value, json};
use vike_aster::perp::{NO_DELIVERY_SENTINEL_MS, parse_aster_perp_instruments, perp_contract};
use vike_model::{
    AssetClass, Contract, Delivery, FutureFamily, FutureSpec, Margining, PerpSpec,
    SymbolProperties, Tenor,
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/exchange_info/aster_fapi.json"))
        .expect("aster_fapi.json is valid JSON")
}

fn linear_perp(settle: &str, settling_ms: Option<i64>) -> Contract {
    Contract::Perp(PerpSpec {
        margining: Margining::Linear,
        settle: settle.to_string(),
        value_ccy: None,
        funding_interval_ms: None,
        settling_ms,
    })
}

#[test]
fn real_exchange_info_entries_map_to_their_typed_contract() {
    let out = parse_aster_perp_instruments(&fixture());
    assert_eq!(out.len(), 4);

    let btc = &out["BTCUSDT"];
    assert_eq!(btc.contract, Some(linear_perp("USDT", None)));
    assert_eq!(btc.base_asset, "BTC");
    assert_eq!(
        btc.properties,
        SymbolProperties {
            tick_size: 0.1,
            step_size: 0.001,
            min_qty: 0.001,
            max_qty: 120.0,
            min_notional: 5.0,
            asset_class: Some(AssetClass::CryptoPerp),
            ..Default::default()
        }
    );

    assert_eq!(out["BTCUSD1"].contract, Some(linear_perp("USD1", None)));
    assert_eq!(out["BTCUSD1"].properties.asset_class, Some(AssetClass::CryptoPerp));

    // A SETTLING perp is still a perp (no expiry); its real `deliveryDate` is the settlement time.
    assert_eq!(out["TONUSDT"].contract, Some(linear_perp("USDT", Some(1_781_859_600_000))));

    // An EMPTY `contractType` states nothing: no contract, no class, the grid still parses.
    let pending = &out["MBLUSDT"];
    assert_eq!(pending.contract, None);
    assert_eq!(pending.properties.asset_class, None);
    assert!(pending.properties.tick_size > 0.0);
}

#[test]
fn margining_is_read_from_margin_asset_against_the_entry_own_assets() {
    let entry = |margin: &str| {
        json!({ "contractType": "PERPETUAL", "baseAsset": "BTC", "quoteAsset": "USD",
                "marginAsset": margin, "deliveryDate": NO_DELIVERY_SENTINEL_MS })
    };
    let margining = |margin: &str| match perp_contract(&entry(margin)) {
        Some(Contract::Perp(spec)) => (spec.margining, spec.settle),
        other => panic!("{margin}: expected a perp, got {other:?}"),
    };
    assert_eq!(margining("USD"), (Margining::Linear, "USD".to_string()));
    assert_eq!(margining("BTC"), (Margining::Inverse, "BTC".to_string()));
    assert_eq!(margining("ETH"), (Margining::Quanto, "ETH".to_string()));
}

#[test]
fn a_dated_contract_type_is_a_future_expiring_at_delivery_date() {
    let entry = json!({ "contractType": "CURRENT_QUARTER", "baseAsset": "BTC", "quoteAsset": "USDT",
                        "marginAsset": "USDT", "deliveryDate": 1_790_931_600_000i64 });
    assert_eq!(
        perp_contract(&entry),
        Some(Contract::Future(FutureSpec {
            margining: Margining::Linear,
            settle: "USDT".to_string(),
            value_ccy: None,
            expiry_ms: 1_790_931_600_000,
            tenor: Some(Tenor::Quarter),
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        }))
    );
    assert_eq!(perp_contract(&entry).map(|c| c.class()), Some(AssetClass::CryptoFuture));
}

#[test]
fn an_entry_that_does_not_say_has_no_contract() {
    // An EMPTY `contractType` (Aster's `PENDING_TRADING` listings) names nothing.
    assert_eq!(
        perp_contract(&json!({ "contractType": "", "baseAsset": "MBL", "quoteAsset": "USDT",
                               "marginAsset": "USDT", "deliveryDate": NO_DELIVERY_SENTINEL_MS })),
        None
    );
    // A dated contract without `deliveryDate` has no expiry to state.
    assert_eq!(
        perp_contract(&json!({ "contractType": "CURRENT_QUARTER", "baseAsset": "BTC",
                               "quoteAsset": "USDT", "marginAsset": "USDT" })),
        None
    );
    // A word nobody measured names nothing either (the shared reading's open list).
    assert_eq!(
        perp_contract(&json!({ "contractType": "NEXT_MONTH", "baseAsset": "BTC",
                               "quoteAsset": "USDT", "marginAsset": "USDT",
                               "deliveryDate": 1_790_931_600_000i64 })),
        None
    );
}

#[test]
fn the_sentinel_delivery_date_is_not_a_settlement() {
    // Only a REAL `deliveryDate` on a `PERPETUAL` entry is a settlement (Aster's `SETTLING` rows);
    // the 2100-12-25 sentinel every trading perp carries is not.
    let entry = |delivery: i64| {
        json!({ "contractType": "PERPETUAL", "baseAsset": "BTC", "quoteAsset": "USDT",
                "marginAsset": "USDT", "deliveryDate": delivery })
    };
    assert_eq!(perp_contract(&entry(NO_DELIVERY_SENTINEL_MS)), Some(linear_perp("USDT", None)));
    assert_eq!(
        perp_contract(&entry(1_781_859_600_000)),
        Some(linear_perp("USDT", Some(1_781_859_600_000)))
    );
}
