//! Public options + futures/perp instrument catalog — the Deribit twin of okx's `catalog`
//! module, exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue
//! symbol search. Reads the KEYLESS public `/api/v2/public/get_instruments` (one UNSIGNED GET
//! per currency — no credentials, no signer), mapped to universal [`Instrument`]s.
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the blocking `fetch_json` so
//! the mapping stays testable without network — mirrors binance/bybit/okx's catalog modules
//! exactly. Deribit has no single "all currencies" endpoint, so `list_instruments` fetches a
//! fixed currency set (BTC, ETH) and concatenates.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, Instrument};
use vike_model::AssetClass;

const BASE: &str = "https://www.deribit.com/api/v2/public/get_instruments";
/// Currencies fetched to cover the tradable universe (crate-reorg twin of `chain.rs`'s
/// `list_underlyings`, minus SOL — SOL has no options; it only appears under the shared USDC
/// book, out of scope for this venue-native currency-keyed enumeration).
const CURRENCIES: [&str; 2] = ["BTC", "ETH"];

/// Classify ONE Deribit instrument row from the VENUE'S OWN words: `kind`, refined for futures by
/// `settlement_period`. `kind == "option"` → [`AssetClass::Option`]; `kind == "future"` →
/// [`AssetClass::CryptoPerp`] when `settlement_period == "perpetual"`, else
/// [`AssetClass::CryptoFuture`].
///
/// `None` for every other `kind` (`future_combo`, `option_combo`, `spot`) and for a row carrying no
/// `kind` at all — an honestly ABSENT class, never a guess. The catalog SKIPS such a row (it
/// advertises exactly the three classes above); the grid parser leaves
/// `SymbolProperties::asset_class` unset.
///
/// **This is the venue's ONE classifier.** Both of Deribit's instrument payloads reach it —
/// [`parse_instruments`] below (the plural `public/get_instruments` list) and
/// `crates/bridges/deribit/src/exec.rs`'s `parse_get_instrument` (the singular
/// `public/get_instrument` grid, the row the PIT properties store actually records) — so the two
/// cannot answer differently about the same instrument.
///
/// ⚠ **Never derived from `instrument_name`.** `BTC-PERPETUAL` looks decisive and is exactly the
/// implicit encoding `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove; the
/// venue publishes both words this reads.
pub fn asset_class_of(row: &Value) -> Option<AssetClass> {
    match row.get("kind").and_then(|v| v.as_str())? {
        "option" => Some(AssetClass::Option),
        "future" => {
            Some(if row.get("settlement_period").and_then(|v| v.as_str()) == Some("perpetual") {
                AssetClass::CryptoPerp
            } else {
                AssetClass::CryptoFuture
            })
        }
        _ => None,
    }
}

/// Parse a Deribit `{result:[…]}` payload (one currency's instruments) into universal
/// [`Instrument`]s. `is_active == false` rows are skipped, and so is every row
/// [`asset_class_of`] cannot classify (`future_combo`/`option_combo`/`spot`, or a `kind`-less row).
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("result").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("is_active").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let Some(class) = asset_class_of(e) else { continue };
        let name = e.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if name.is_empty() {
            continue;
        }
        let base = e.get("base_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let quote = e.get("quote_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        out.push(Instrument {
            venue: "deribit".into(),
            raw_symbol: name,
            asset_class: class,
            base,
            quote,
            description: String::new(),
            // ⚠ **The venue where one `AssetClass::CryptoPerp` covers BOTH settlement modes**, which
            // `docs/decisions/0061` names as the two-venues-one-word problem: `BTC-PERPETUAL` is
            // coin-settled and a USDC-quoted perpetual is linear, and until now the ONLY signal was
            // the shape of the instrument name. Deribit publishes its own word for it
            // (`instrument_type`: `"reversed"` / `"linear"`) plus the settling currency, and both are
            // carried VERBATIM and UNINTERPRETED. Nothing routes on either — see
            // `vike_catalog::Instrument::contract_type`'s doc for why that is a decision rather
            // than a refactor.
            properties: Default::default(),
            contract_type: e.get("instrument_type").and_then(|v| v.as_str()).map(str::to_string),
            settle_asset: e
                .get("settlement_currency")
                .and_then(|v| v.as_str())
                .map(|s| s.to_uppercase()),
        });
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — `public/get_instruments` is a keyless public read); called once per
/// currency. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of `list_instruments` over an injectable fetch — per-currency TOLERANT: one currency
/// failing must NOT drop the others (okx's catalog documents the bug shape this prevents: a `?` on
/// one endpoint wiped the whole venue from the Symbol picker). Accumulate what succeeds; warn (not
/// fail) on each miss. Split from the provider so the tolerance is testable without network.
fn list_tolerant(fetch: impl Fn(&str) -> Result<Value, CatalogError>) -> Vec<Instrument> {
    let mut out = Vec::new();
    for ccy in CURRENCIES {
        match fetch(&format!("{BASE}?currency={ccy}")) {
            Ok(v) => out.extend(parse_instruments(&v)),
            Err(e) => {
                tracing::warn!(target: "vike_deribit::catalog", "{ccy} fetch failed (skipped): {e}")
            }
        }
    }
    out
}

/// The deribit venue's `CatalogProvider` contribution: options + futures + perps, fetched one
/// currency at a time (BTC, ETH) and tagged via [`parse_instruments`]. Keyless. Per-currency
/// tolerant (see [`list_tolerant`]) — mirrors okx's per-instType accumulate.
pub struct DeribitCatalog;

impl CatalogProvider for DeribitCatalog {
    fn venue(&self) -> &str {
        "deribit"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Option, AssetClass::CryptoFuture, AssetClass::CryptoPerp]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(list_tolerant(fetch_json))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    fn row(
        name: &str,
        kind: &str,
        settlement_period: &str,
        is_active: bool,
        base: &str,
        quote: &str,
    ) -> Value {
        serde_json::json!({
            "instrument_name": name,
            "kind": kind,
            "settlement_period": settlement_period,
            "is_active": is_active,
            "base_currency": base,
            "quote_currency": quote,
        })
    }

    #[test]
    fn maps_option_perp_future_and_skips_inactive() {
        let payload = serde_json::json!({ "result": [
            row("BTC-27JUN25-60000-C", "option", "month", true, "BTC", "BTC"),
            row("BTC-PERPETUAL", "future", "perpetual", true, "BTC", "USD"),
            row("BTC-27JUN25", "future", "month", true, "BTC", "USD"),
            row("ETH-27JUN25-3000-P", "option", "month", false, "ETH", "ETH"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out.len(), 3);

        assert_eq!(out[0].raw_symbol, "BTC-27JUN25-60000-C");
        assert_eq!(out[0].asset_class, AssetClass::Option);
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].quote, "BTC");

        assert_eq!(out[1].raw_symbol, "BTC-PERPETUAL");
        assert_eq!(out[1].asset_class, AssetClass::CryptoPerp);

        assert_eq!(out[2].raw_symbol, "BTC-27JUN25");
        assert_eq!(out[2].asset_class, AssetClass::CryptoFuture);
    }

    /// The shared classifier, tested on ITS OWN terms rather than only through the catalog's row
    /// loop — `crates/bridges/deribit/src/exec.rs`'s `parse_get_instrument` reaches it directly, so
    /// its `None` arms are a contract two callers now depend on rather than a `continue` detail.
    #[test]
    fn the_classifier_reads_kind_and_settlement_period_and_nothing_else() {
        let class = |kind: &str, period: &str| {
            asset_class_of(&serde_json::json!({
                "instrument_name": "BTC-PERPETUAL", "kind": kind, "settlement_period": period
            }))
        };
        assert_eq!(class("option", "month"), Some(AssetClass::Option));
        assert_eq!(class("option", "perpetual"), Some(AssetClass::Option), "kind wins for options");
        assert_eq!(class("future", "perpetual"), Some(AssetClass::CryptoPerp));
        assert_eq!(class("future", "month"), Some(AssetClass::CryptoFuture));
        assert_eq!(class("future", "week"), Some(AssetClass::CryptoFuture));
        // A future whose `settlement_period` the venue omitted is a DATED future, not a perp: only
        // the literal word `perpetual` buys `CryptoPerp`.
        assert_eq!(
            asset_class_of(&serde_json::json!({"kind": "future"})),
            Some(AssetClass::CryptoFuture)
        );
        // Every other kind, and a row with no `kind`, is UNCLASSIFIED — never guessed.
        for kind in ["future_combo", "option_combo", "spot", ""] {
            assert_eq!(class(kind, "perpetual"), None, "{kind}");
        }
        assert_eq!(asset_class_of(&serde_json::json!({})), None);
        assert_eq!(asset_class_of(&serde_json::json!({"kind": 7})), None, "non-string kind");
    }

    /// ⚠ The name is NOT evidence (0061): a row the venue calls `spot` stays unclassified however
    /// decisively `BTC-PERPETUAL` reads, and a `future` row named nothing at all is still a perp
    /// when the venue says `perpetual`.
    #[test]
    fn the_instrument_name_is_never_consulted() {
        assert_eq!(
            asset_class_of(&serde_json::json!({
                "instrument_name": "BTC-PERPETUAL", "kind": "spot", "settlement_period": "perpetual"
            })),
            None,
        );
        assert_eq!(
            asset_class_of(
                &serde_json::json!({"kind": "future", "settlement_period": "perpetual"})
            ),
            Some(AssetClass::CryptoPerp),
        );
    }

    /// ⚠ **The two-venues-one-word case, made answerable.** `BTC-PERPETUAL` and a USDC-settled
    /// perpetual are both `AssetClass::CryptoPerp` — 0061 keeps that variant WHOLE — so until the
    /// venue's own words were carried, the ONLY thing separating a coin-settled perp from a linear
    /// one anywhere in this tree was the shape of the instrument name.
    ///
    /// Nothing routes on either field; this asserts only that they arrive.
    #[test]
    fn the_venues_own_settlement_words_are_carried_verbatim() {
        let payload = serde_json::json!({ "result": [
            { "instrument_name": "BTC-PERPETUAL", "kind": "future", "settlement_period": "perpetual",
              "is_active": true, "base_currency": "BTC", "quote_currency": "USD",
              "instrument_type": "reversed", "settlement_currency": "BTC" },
            { "instrument_name": "BTC_USDC-PERPETUAL", "kind": "future",
              "settlement_period": "perpetual", "is_active": true, "base_currency": "BTC",
              "quote_currency": "USDC", "instrument_type": "linear",
              "settlement_currency": "usdc" },
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].asset_class, out[1].asset_class, "one variant covers both, by 0061");
        assert_eq!(out[0].contract_type.as_deref(), Some("reversed"));
        assert_eq!(out[0].settle_asset.as_deref(), Some("BTC"));
        assert_eq!(out[1].contract_type.as_deref(), Some("linear"));
        assert_eq!(out[1].settle_asset.as_deref(), Some("USDC"));
    }

    /// A payload without the two fields leaves both ABSENT — byte-identical to a world without them.
    #[test]
    fn a_payload_without_the_fields_leaves_them_absent() {
        let payload = serde_json::json!({ "result": [
            row("BTC-PERPETUAL", "future", "perpetual", true, "BTC", "USD"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].contract_type, None);
        assert_eq!(out[0].settle_asset, None);
    }

    #[test]
    fn venue_and_uppercases_raw_symbol() {
        let payload = serde_json::json!({ "result": [
            row("btc-perpetual", "future", "perpetual", true, "BTC", "USD"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].venue, "deribit");
        assert_eq!(out[0].raw_symbol, "BTC-PERPETUAL");
    }

    #[test]
    fn empty_or_missing_result_yields_empty() {
        assert!(parse_instruments(&serde_json::json!({})).is_empty());
        assert!(parse_instruments(&serde_json::json!({"result": []})).is_empty());
    }

    /// The (g) robustness contract, mirroring okx's per-instType accumulate: one currency's fetch
    /// failing must not wipe the venue — the OTHER currency's instruments still come back.
    #[test]
    fn one_failed_currency_still_yields_the_other_currencys_instruments() {
        let out = list_tolerant(|url| {
            if url.contains("currency=BTC") {
                Err(CatalogError("HTTP 400: simulated".into()))
            } else {
                Ok(serde_json::json!({ "result": [
                    row("ETH-PERPETUAL", "future", "perpetual", true, "ETH", "USD"),
                ]}))
            }
        });
        assert_eq!(out.len(), 1, "the ETH page must survive the BTC failure");
        assert_eq!(out[0].raw_symbol, "ETH-PERPETUAL");
        // all currencies failing yields an EMPTY catalog (warned), never an error
        assert!(list_tolerant(|_| Err(CatalogError("down".into()))).is_empty());
    }
}
