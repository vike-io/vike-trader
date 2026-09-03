//! Bundled static instrument catalog — FXCM's twin of dukascopy's `catalog` module. FXCM has no
//! bulk instrument-list endpoint reachable without the native ForexConnect SDK session, so this is
//! the bundled **confident FX + metals instrument set** (majors, crosses, common exotics, plus
//! XAU/XAG spot metals) — hand-curated. FXCM also offers indices/commodities/crypto CFDs; those are
//! deliberately omitted here since their FXCM symbol naming isn't confidently known (this table is
//! extendable — append rows to [`FX_TABLE`] once verified).
//!
//! Declared **unconditionally** — NOT behind the crate-local `fxcm` Cargo feature — because this is
//! pure data, no FFI, no network (mirrors `pub const CAPS` in `lib.rs`). [`bundled_instruments`] is a
//! pure function over a `const` table (`(raw_symbol, asset_class)` pairs), fully unit-testable with
//! no SDK and no credentials.
//!
//! FXCM naming note: symbols keep their slash, e.g. `"EUR/USD"` (unlike dukascopy's unslashed
//! `EURUSD`) — `raw_symbol` is stored verbatim; base/quote are the two halves split on `'/'`.

use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

/// `(raw_symbol, asset_class)` — the bundled confident FX majors/crosses/exotics + metals set.
/// FX pairs split into base/quote on `'/'` in [`bundled_instruments`]; the two metals rows use the
/// same split (their raw symbols are already slashed: `XAU/USD`, `XAG/USD`).
const FX_TABLE: &[(&str, AssetClass)] = &[
    // FX majors
    ("EUR/USD", AssetClass::Fx),
    ("USD/JPY", AssetClass::Fx),
    ("GBP/USD", AssetClass::Fx),
    ("USD/CHF", AssetClass::Fx),
    ("USD/CAD", AssetClass::Fx),
    ("AUD/USD", AssetClass::Fx),
    ("NZD/USD", AssetClass::Fx),
    // FX crosses
    ("EUR/JPY", AssetClass::Fx),
    ("EUR/GBP", AssetClass::Fx),
    ("EUR/CHF", AssetClass::Fx),
    ("EUR/AUD", AssetClass::Fx),
    ("EUR/CAD", AssetClass::Fx),
    ("EUR/NZD", AssetClass::Fx),
    ("GBP/JPY", AssetClass::Fx),
    ("GBP/CHF", AssetClass::Fx),
    ("GBP/AUD", AssetClass::Fx),
    ("GBP/CAD", AssetClass::Fx),
    ("GBP/NZD", AssetClass::Fx),
    ("AUD/JPY", AssetClass::Fx),
    ("AUD/CHF", AssetClass::Fx),
    ("AUD/CAD", AssetClass::Fx),
    ("AUD/NZD", AssetClass::Fx),
    ("CAD/JPY", AssetClass::Fx),
    ("CAD/CHF", AssetClass::Fx),
    ("CHF/JPY", AssetClass::Fx),
    ("NZD/JPY", AssetClass::Fx),
    // FX exotics
    ("USD/TRY", AssetClass::Fx),
    ("USD/MXN", AssetClass::Fx),
    ("USD/ZAR", AssetClass::Fx),
    ("USD/SEK", AssetClass::Fx),
    ("USD/NOK", AssetClass::Fx),
    ("USD/HKD", AssetClass::Fx),
    ("USD/CNH", AssetClass::Fx),
    ("EUR/TRY", AssetClass::Fx),
    ("EUR/PLN", AssetClass::Fx),
    ("EUR/NOK", AssetClass::Fx),
    ("EUR/SEK", AssetClass::Fx),
    ("TRY/JPY", AssetClass::Fx),
    ("ZAR/JPY", AssetClass::Fx),
    // Metals (Cfd)
    ("XAU/USD", AssetClass::Cfd),
    ("XAG/USD", AssetClass::Cfd),
];

/// Build the bundled instrument list from [`FX_TABLE`]. Pure — no I/O, no FFI. Every row (FX and
/// metals alike) splits base/quote on the raw symbol's `'/'`.
pub fn bundled_instruments() -> Vec<Instrument> {
    FX_TABLE
        .iter()
        .map(|&(symbol, asset_class)| {
            let (base, quote) = symbol.split_once('/').expect("FX_TABLE rows are slashed pairs");
            Instrument {
                venue: "fxcm".into(),
                raw_symbol: symbol.to_string(),
                asset_class,
                base: base.to_string(),
                quote: quote.to_string(),
                description: symbol.to_string(),
                properties: Default::default(),
            }
        })
        .collect()
}

/// The fxcm venue's `CatalogProvider` contribution: a bundled static FX + metals instrument list
/// (Enumerable — no network, no FFI, no credentials; pure data, unconditionally compiled).
pub struct FxcmCatalog;

impl CatalogProvider for FxcmCatalog {
    fn venue(&self) -> &str {
        "fxcm"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(bundled_instruments())
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn count_matches_table_length() {
        let out = bundled_instruments();
        assert_eq!(out.len(), FX_TABLE.len());
    }

    #[test]
    fn eurusd_maps_to_fx_with_eur_base_usd_quote() {
        let out = bundled_instruments();
        let eurusd = out.iter().find(|i| i.raw_symbol == "EUR/USD").expect("EUR/USD present");
        assert_eq!(eurusd.asset_class, AssetClass::Fx);
        assert_eq!(eurusd.base, "EUR");
        assert_eq!(eurusd.quote, "USD");
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(eurusd.venue, "fxcm");
    }

    #[test]
    fn xauusd_maps_to_cfd_with_xau_base_usd_quote() {
        let out = bundled_instruments();
        let xauusd = out.iter().find(|i| i.raw_symbol == "XAU/USD").expect("XAU/USD present");
        assert_eq!(xauusd.asset_class, AssetClass::Cfd);
        assert_eq!(xauusd.base, "XAU");
        assert_eq!(xauusd.quote, "USD");
    }

    #[test]
    fn every_raw_symbol_is_non_empty() {
        let out = bundled_instruments();
        assert!(!out.is_empty());
        for inst in &out {
            assert!(!inst.raw_symbol.is_empty());
            assert!(inst.raw_symbol.contains('/'));
        }
    }

    #[test]
    fn provider_reports_venue_classes_and_mode() {
        let provider = FxcmCatalog;
        assert_eq!(provider.venue(), "fxcm");
        assert_eq!(provider.asset_classes(), &[AssetClass::Fx, AssetClass::Cfd]);
        assert_eq!(provider.mode(), CatalogMode::Enumerable);
        assert_eq!(provider.list_instruments().unwrap().len(), FX_TABLE.len());
    }
}
