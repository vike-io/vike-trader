//! Bundled static instrument catalog — dukascopy's twin of okx/binance's live `catalog` module,
//! except there is no instrument-list endpoint to fetch from: Dukascopy takes a symbol string and
//! builds a `.bi5` history URL directly (see [`crate::data`]), so there is nothing to query. This
//! is the bundled **canonical FX + metals instrument set** (majors, crosses, common exotics, plus
//! XAU/XAG spot metals) — confidently curated by hand. Dukascopy actually publishes on the order
//! of ~700 instruments (also indices, commodities, stocks); this core set is deliberately
//! extendable (append rows to [`FX_TABLE`]) rather than exhaustive.
//!
//! [`bundled_instruments`] is a pure function over a `const` table (`(symbol, asset_class)`
//! pairs), so it is fully unit-testable with no network and no credentials — mirrors the
//! pure-parse half of the fetch-backed venues' catalog modules (okx/binance/bybit), just without
//! the fetch half since Dukascopy has none.

use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

/// `(raw_symbol, asset_class)` — the bundled canonical FX majors/crosses/exotics + metals set.
/// FX pairs split into base/quote by the first-3/last-3-chars rule in [`bundled_instruments`];
/// the two metals rows get their base/quote spelled out explicitly there instead.
const FX_TABLE: &[(&str, AssetClass)] = &[
    // FX majors
    ("EURUSD", AssetClass::Fx),
    ("USDJPY", AssetClass::Fx),
    ("GBPUSD", AssetClass::Fx),
    ("USDCHF", AssetClass::Fx),
    ("USDCAD", AssetClass::Fx),
    ("AUDUSD", AssetClass::Fx),
    ("NZDUSD", AssetClass::Fx),
    // FX crosses
    ("EURJPY", AssetClass::Fx),
    ("EURGBP", AssetClass::Fx),
    ("EURCHF", AssetClass::Fx),
    ("EURAUD", AssetClass::Fx),
    ("EURCAD", AssetClass::Fx),
    ("EURNZD", AssetClass::Fx),
    ("GBPJPY", AssetClass::Fx),
    ("GBPCHF", AssetClass::Fx),
    ("GBPAUD", AssetClass::Fx),
    ("GBPCAD", AssetClass::Fx),
    ("GBPNZD", AssetClass::Fx),
    ("AUDJPY", AssetClass::Fx),
    ("AUDCHF", AssetClass::Fx),
    ("AUDCAD", AssetClass::Fx),
    ("AUDNZD", AssetClass::Fx),
    ("CADJPY", AssetClass::Fx),
    ("CADCHF", AssetClass::Fx),
    ("CHFJPY", AssetClass::Fx),
    ("NZDJPY", AssetClass::Fx),
    ("NZDCHF", AssetClass::Fx),
    ("NZDCAD", AssetClass::Fx),
    // FX exotics
    ("USDTRY", AssetClass::Fx),
    ("USDMXN", AssetClass::Fx),
    ("USDZAR", AssetClass::Fx),
    ("USDSGD", AssetClass::Fx),
    ("USDHKD", AssetClass::Fx),
    ("USDNOK", AssetClass::Fx),
    ("USDSEK", AssetClass::Fx),
    ("USDPLN", AssetClass::Fx),
    ("USDDKK", AssetClass::Fx),
    ("USDCNH", AssetClass::Fx),
    ("EURTRY", AssetClass::Fx),
    ("EURPLN", AssetClass::Fx),
    ("EURNOK", AssetClass::Fx),
    ("EURSEK", AssetClass::Fx),
    ("EURHUF", AssetClass::Fx),
    ("EURDKK", AssetClass::Fx),
    ("EURCZK", AssetClass::Fx),
    ("GBPNOK", AssetClass::Fx),
    ("GBPSEK", AssetClass::Fx),
    // Metals (Cfd)
    ("XAUUSD", AssetClass::Cfd),
    ("XAGUSD", AssetClass::Cfd),
];

/// Build the bundled instrument list from [`FX_TABLE`]. Pure — no I/O. FX rows split
/// base/quote by the first-3/last-3-char rule; the two metals rows spell out base/quote
/// explicitly (`XAUUSD` -> XAU/USD, `XAGUSD` -> XAG/USD).
pub fn bundled_instruments() -> Vec<Instrument> {
    FX_TABLE
        .iter()
        .map(|&(symbol, asset_class)| {
            let (base, quote) = match symbol {
                "XAUUSD" => ("XAU", "USD"),
                "XAGUSD" => ("XAG", "USD"),
                _ => (&symbol[0..3], &symbol[3..6]),
            };
            Instrument {
                venue: "dukascopy".into(),
                raw_symbol: symbol.to_string(),
                asset_class,
                base: base.to_string(),
                quote: quote.to_string(),
                description: format!("{base}/{quote}"),
                properties: Default::default(),
            }
        })
        .collect()
}

/// The dukascopy venue's `CatalogProvider` contribution: a bundled static FX + metals instrument
/// list (Enumerable — no network, no credentials; Dukascopy has no instrument-list endpoint).
pub struct DukascopyCatalog;

impl CatalogProvider for DukascopyCatalog {
    fn venue(&self) -> &str {
        "dukascopy"
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
        let eurusd = out.iter().find(|i| i.raw_symbol == "EURUSD").expect("EURUSD present");
        assert_eq!(eurusd.asset_class, AssetClass::Fx);
        assert_eq!(eurusd.base, "EUR");
        assert_eq!(eurusd.quote, "USD");
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(eurusd.venue, "dukascopy");
    }

    #[test]
    fn xauusd_maps_to_cfd_with_xau_base_usd_quote() {
        let out = bundled_instruments();
        let xauusd = out.iter().find(|i| i.raw_symbol == "XAUUSD").expect("XAUUSD present");
        assert_eq!(xauusd.asset_class, AssetClass::Cfd);
        assert_eq!(xauusd.base, "XAU");
        assert_eq!(xauusd.quote, "USD");
    }

    #[test]
    fn every_raw_symbol_is_upper_case_and_non_empty() {
        let out = bundled_instruments();
        assert!(!out.is_empty());
        for inst in &out {
            assert!(!inst.raw_symbol.is_empty());
            assert_eq!(inst.raw_symbol, inst.raw_symbol.to_uppercase());
        }
    }

    #[test]
    fn provider_reports_venue_classes_and_mode() {
        let provider = DukascopyCatalog;
        assert_eq!(provider.venue(), "dukascopy");
        assert_eq!(provider.asset_classes(), &[AssetClass::Fx, AssetClass::Cfd]);
        assert_eq!(provider.mode(), CatalogMode::Enumerable);
        assert_eq!(provider.list_instruments().unwrap().len(), FX_TABLE.len());
    }
}
