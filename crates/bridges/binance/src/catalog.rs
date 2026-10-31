//! Public spot + USDS-M perp instrument catalog — the searchable symbol universe behind the
//! chart's symbol picker, exposed as the venue's [`CatalogProvider`] contribution: the venue face
//! of the shared [`crate::family::catalog`].
//!
//! Reads the KEYLESS public `/api/v3/exchangeInfo` (spot) and `fapi /fapi/v1/exchangeInfo` (perp),
//! mapped to universal [`Instrument`]s carrying a typed `vike_model::Contract` and the row's
//! tick/lot grid (the shared module doc says which rows are listed and why). Aster serves the
//! same `exchangeInfo` grammar, so the parse itself — previously a near-verbatim copy in each crate — lives once in
//! `family` and both venues pass their own venue string (F14, dedup rung 3). What stays HERE is the
//! genuine per-venue delta: the two endpoint URLs (plain `const`s — Binance's hosts are NOT
//! env-resolved) and this crate's own provider struct. Pure parse (`parse_spot`/`parse_perp`,
//! fixture-tested) is split from the blocking fetch so the mapping stays testable without network —
//! mirrors data.rs's parse/fetch split. The tests below stay HERE, exercising the shared code
//! through Binance's own wrapper: Aster's twin does the same through its own.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{AssetClass, Instrument};

const SPOT_URL: &str = "https://api.binance.com/api/v3/exchangeInfo";
const PERP_URL: &str = "https://fapi.binance.com/fapi/v1/exchangeInfo";
/// The DEMO futures host's list (`crate::perp::DEMO_FAPI_REST` + `PATH_EXCHANGE_INFO`, held equal by
/// `catalog_class_tests`): a `demo` account trades the symbols THIS list carries, which is not mainnet's.
const DEMO_PERP_URL: &str = "https://demo-fapi.binance.com/fapi/v1/exchangeInfo";

/// Parse `/api/v3/exchangeInfo` into binance `Contract::CryptoSpot` [`Instrument`]s.
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    crate::family::catalog::parse_spot(payload, "binance")
}

/// Parse `fapi /fapi/v1/exchangeInfo` into binance `Contract::Perp` [`Instrument`]s: the PICKER's rows
/// (`PERPETUAL`, `PERPETUAL_DELIVERING` and `TRADIFI_PERPETUAL`; the dated futures share the endpoint and
/// are typed, but only the symbol facts list them: `crate::family::catalog::futures_listing`).
pub fn parse_perp(payload: &Value) -> Vec<Instrument> {
    crate::family::catalog::parse_perp(payload, "binance")
}

/// Parse `fapi /fapi/v1/exchangeInfo` into the rows the symbol FACTS answer from: [`parse_perp`]'s plus the
/// dated futures (`BTCUSDT_261225.P`, `CURRENT_QUARTER`, `NEXT_QUARTER`, `CURRENT_WEEK`, `NEXT_WEEK`), the
/// TRADING rows only.
pub fn parse_futures(payload: &Value) -> Vec<Instrument> {
    crate::family::catalog::parse_futures(
        payload,
        "binance",
        crate::family::catalog::FUTURES_CONTRACT_TYPES,
    )
}

/// Parse `dapi /dapi/v1/exchangeInfo` — the COIN-M (coin-margined, INVERSE) book — into typed [`Instrument`]s,
/// the TRADING rows only. NOT a picker list (the picker offers none of these) and NOT an admission: the symbol
/// facts read it to refuse a COIN-M symbol BY NAME, with what the venue states for it.
///
/// A dapi row spells its status `contractStatus` (every other binance host: `status`), carries `contractSize`
/// (USD per contract: 100 for BTC, 10 for the rest, MEASURED 2026-10-30 on the demo and mainnet hosts) and a
/// `marginAsset` equal to its base coin, so [`crate::family::catalog::fapi_contract`] types it
/// [`vike_model::Margining::Inverse`] from the row's own assets. Quantity is in WHOLE contracts (`stepSize` 1).
/// Spelled with the lane suffix (`BTCUSD_PERP.P`), as the history path already spells a COIN-M perpetual.
pub fn parse_dapi(payload: &Value) -> Vec<Instrument> {
    use crate::family::catalog::{fapi_contract, perp_grid};
    use vike_bridge_core::json::json_num;
    let Some(list) = payload.get("symbols").and_then(Value::as_array) else { return Vec::new() };
    let text = |e: &Value, key: &str| {
        e.get(key).and_then(Value::as_str).map(str::to_uppercase).filter(|s| !s.is_empty())
    };
    list.iter()
        .filter(|e| e.get("contractStatus").and_then(Value::as_str) == Some("TRADING"))
        .filter_map(|e| {
            let symbol = text(e, "symbol")?;
            let word = e.get("contractType").and_then(Value::as_str)?;
            let contract = fapi_contract(e)?;
            let properties = vike_model::SymbolProperties {
                asset_class: Some(contract.class()),
                contract_size: e.get("contractSize").and_then(json_num).unwrap_or(0.0),
                ..perp_grid(e)
            };
            Some(
                Instrument::new(
                    "binance",
                    crate::family::perp_series_symbol(&symbol),
                    text(e, "baseAsset").unwrap_or_default(),
                    text(e, "quoteAsset").unwrap_or_default(),
                    contract,
                )
                .with_properties(properties)
                .with_venue_word(word),
            )
        })
        .collect()
}

/// The binance venue's `CatalogProvider` contribution: spot (`CryptoSpot`) + USDS-M perp
/// (`CryptoPerp`), both bulk-enumerable off the keyless public `exchangeInfo` endpoints.
pub struct BinanceCatalog;

impl CatalogProvider for BinanceCatalog {
    fn venue(&self) -> &str {
        "binance"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        crate::family::catalog::ASSET_CLASSES
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        crate::family::catalog::list_instruments("binance", SPOT_URL, PERP_URL)
    }
}

impl BinanceCatalog {
    /// The rows of ONE spot symbol (`?symbol=`, not the ~17.6 MB list): the symbol-facts lookup.
    /// `Ok(vec![])` = the venue has no such TRADING symbol; see
    /// `crates/bridges/binance/src/family/catalog.rs`'s `spot_symbol`.
    pub fn spot_symbol(&self, symbol: &str) -> Result<Vec<Instrument>, CatalogError> {
        crate::family::catalog::spot_symbol("binance", SPOT_URL, symbol)
    }

    /// The USDS-M futures list alone, strict (a failure is an `Err`): the symbol-facts lookup of a
    /// `.P` symbol. Perpetuals (TradFi included) AND the dated futures (`BTCUSDT_261225.P`), the
    /// TRADING rows only: `crates/bridges/binance/src/family/catalog.rs`'s `futures_listing`.
    pub fn perp_listing(&self) -> Result<Vec<Instrument>, CatalogError> {
        crate::family::catalog::futures_listing("binance", PERP_URL)
    }

    /// [`Self::perp_listing`] of the DEMO futures host: what a `demo` account can actually sign (the demo list is
    /// shorter than mainnet's: 30 TradFi perpetuals TRADING against 235 on 2026-10-30).
    pub fn demo_perp_listing(&self) -> Result<Vec<Instrument>, CatalogError> {
        crate::family::catalog::futures_listing("binance", DEMO_PERP_URL)
    }

    /// The COIN-M book alone, strict ([`parse_dapi`]): what the symbol-facts lookup reads to refuse a COIN-M
    /// symbol by name. One `GET` of about 35 KB (`REQUEST_WEIGHT` 1).
    pub fn coin_m_listing(&self) -> Result<Vec<Instrument>, CatalogError> {
        let payload =
            vike_bridge_core::http::get_json(crate::instruments::COIN_M_EXCHANGE_INFO_URL)
                .map_err(CatalogError)?;
        Ok(parse_dapi(&payload))
    }
}

#[cfg(test)]
mod catalog_tests {
    //! Driven over slices of the REAL keyless lists, fetched 2026-10-11 and kept under
    //! `tests/fixtures/exchange_info/` (rows whole; spot rows without `permissionSets`).
    use super::*;
    use crate::family::catalog::fapi_contract;
    use vike_model::{
        AssetClass, Contract, Delivery, FutureFamily, FutureSpec, Margining, PerpSpec,
        SymbolProperties, Tenor,
    };

    fn fixture(name: &str) -> Value {
        let text = match name {
            "spot" => include_str!("../tests/fixtures/exchange_info/spot_2026-10-11.json"),
            _ => include_str!("../tests/fixtures/exchange_info/fapi_2026-10-11.json"),
        };
        serde_json::from_str(text).expect("fixture parses")
    }

    fn row<'a>(payload: &'a Value, symbol: &str) -> &'a Value {
        payload["symbols"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["symbol"] == symbol))
            .expect("fixture row")
    }

    fn usdt_perp(settling_ms: Option<i64>) -> Contract {
        Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms,
        })
    }

    #[test]
    fn spot_exchange_info_maps_to_cryptospot_instruments_with_their_grid() {
        let out = parse_spot(&fixture("spot"));
        let symbols: Vec<&str> = out.iter().map(|i| i.raw_symbol.as_str()).collect();
        assert_eq!(symbols, ["ETHBTC", "BTCUSDT"]);
        let btc = &out[1];
        assert_eq!(btc.contract, Contract::CryptoSpot);
        assert_eq!(btc.asset_class(), AssetClass::CryptoSpot);
        assert_eq!((btc.base.as_str(), btc.quote.as_str()), ("BTC", "USDT"));
        assert_eq!(btc.contract_type, None, "spot publishes no contract word");
        assert_eq!(
            btc.properties,
            SymbolProperties {
                tick_size: 0.01,
                step_size: 0.00001,
                min_qty: 0.00001,
                max_qty: 9000.0,
                min_notional: 5.0,
                asset_class: Some(AssetClass::CryptoSpot),
                ..Default::default()
            }
        );
        // A non-TRADING row is not offered.
        let halted = serde_json::json!({ "symbols": [
            { "symbol": "OLDUSDT", "baseAsset": "OLD", "quoteAsset": "USDT", "status": "BREAK" }
        ]});
        assert!(parse_spot(&halted).is_empty());
    }

    /// The picker lists the TRADING `PERPETUAL` row and the TRADING `TRADIFI_PERPETUAL` one (`XAUUSDT`, the same
    /// linear USDT contract on the same host): `OMGUSDT` is `SETTLING` and the two quarterlies are not picker rows.
    #[test]
    fn perp_exchange_info_maps_to_a_linear_usdt_perp_with_its_grid() {
        let out = parse_perp(&fixture("fapi"));
        let symbols: Vec<&str> = out.iter().map(|i| i.raw_symbol.as_str()).collect();
        assert_eq!(symbols, ["BTCUSDT.P", "XAUUSDT.P"], "{out:?}");
        let p = &out[0];
        assert_eq!(p.raw_symbol, "BTCUSDT.P", "perp gets the .P distinct-symbol suffix");
        assert_eq!((p.base.as_str(), p.quote.as_str()), ("BTC", "USDT"));
        assert_eq!(p.contract, usdt_perp(None), "the no-expiry sentinel deliveryDate is not read");
        assert_eq!(p.asset_class(), AssetClass::CryptoPerp);
        assert_eq!(p.settle_asset(), Some("USDT"));
        assert_eq!(p.contract_type.as_deref(), Some("PERPETUAL"), "the venue's word, verbatim");
        assert_eq!(
            p.properties,
            SymbolProperties {
                tick_size: 0.1,
                step_size: 0.001,
                min_qty: 0.001,
                max_qty: 120.0,
                min_notional: 50.0,
                asset_class: Some(AssetClass::CryptoPerp),
                ..Default::default()
            }
        );
    }

    /// A perpetual binance has scheduled for delivery stays a listed perp (same `.P` route) and
    /// carries the date as `settling_ms`. No such row is live today, so one is derived from the
    /// real BTCUSDT row.
    #[test]
    fn a_delivering_perpetual_is_a_perp_with_its_settling_date() {
        let mut payload = fixture("fapi");
        let rows = payload["symbols"].as_array_mut().expect("symbols[]");
        rows.retain(|r| r["symbol"] == "BTCUSDT");
        rows[0]["contractType"] = "PERPETUAL_DELIVERING".into();
        rows[0]["deliveryDate"] = 1_798_185_600_000_i64.into();
        let out = parse_perp(&payload);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].contract, usdt_perp(Some(1_798_185_600_000)));
        assert_eq!(out[0].contract_type.as_deref(), Some("PERPETUAL_DELIVERING"));
    }

    /// Every row of the real list is TYPED, including the ones the picker does not list: the
    /// dated quarterlies are linear USDT futures expiring at their `deliveryDate`, and the TradFi
    /// perpetual is a perp.
    #[test]
    fn every_fapi_row_is_typed_from_its_own_fields() {
        let payload = fixture("fapi");
        let future = |expiry_ms| {
            Contract::Future(FutureSpec {
                margining: Margining::Linear,
                settle: "USDT".into(),
                value_ccy: None,
                expiry_ms,
                tenor: Some(Tenor::Quarter),
                delivery: Delivery::Unknown,
                family: FutureFamily::Crypto,
            })
        };
        assert_eq!(fapi_contract(row(&payload, "BTCUSDT_261225")), Some(future(1_798_185_600_000)));
        assert_eq!(fapi_contract(row(&payload, "BTCUSDT_270326")), Some(future(1_806_048_000_000)));
        assert_eq!(fapi_contract(row(&payload, "XAUUSDT")), Some(usdt_perp(None)));
        assert_eq!(fapi_contract(row(&payload, "OMGUSDT")), Some(usdt_perp(None)));
        // A dated row without its date, or a word nobody measured, is not typed at all.
        let mut undated = row(&payload, "BTCUSDT_261225").clone();
        undated.as_object_mut().expect("row").remove("deliveryDate");
        assert_eq!(fapi_contract(&undated), None);
        let mut unmeasured = row(&payload, "BTCUSDT").clone();
        unmeasured["contractType"] = "NEXT_MONTH".into();
        assert_eq!(fapi_contract(&unmeasured), None);
        // Settle falls back to `quoteAsset` when `marginAsset` is absent; settling in the base
        // asset is inverse.
        let mut no_margin = row(&payload, "BTCUSDT").clone();
        no_margin.as_object_mut().expect("row").remove("marginAsset");
        assert_eq!(fapi_contract(&no_margin), Some(usdt_perp(None)));
        let mut coin_settled = row(&payload, "BTCUSDT").clone();
        coin_settled["marginAsset"] = "BTC".into();
        assert!(matches!(
            fapi_contract(&coin_settled),
            Some(Contract::Perp(PerpSpec { margining: Margining::Inverse, .. }))
        ));
    }
}

#[cfg(test)]
#[path = "catalog_class_tests.rs"]
mod catalog_class_tests;
