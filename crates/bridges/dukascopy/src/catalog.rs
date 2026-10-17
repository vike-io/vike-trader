//! Bundled static instrument catalog — dukascopy's twin of okx/binance's live `catalog` module,
//! except there is no instrument-list endpoint to fetch from: Dukascopy takes a symbol string and
//! builds a `.bi5` history URL directly (see [`crate::data`]), so there is nothing to query. This
//! is the bundled **canonical FX + metals instrument set** (majors, crosses, common exotics, plus
//! XAU/XAG spot metals) — confidently curated by hand. Dukascopy actually publishes on the order
//! of ~700 instruments (also indices, commodities, stocks); this core set is deliberately
//! extendable (append rows to [`FX_TABLE`]) rather than exhaustive.
//!
//! [`bundled_instruments`] is a pure function over a `const` table (`(symbol, RowKind)`
//! pairs), so it is fully unit-testable with no network and no credentials — mirrors the
//! pure-parse half of the fetch-backed venues' catalog modules (okx/binance/bybit), just without
//! the fetch half since Dukascopy has none.
//!
//! **What each row's typed `Contract` carries** (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`):
//!
//! - an FX row is `Contract::Fx` whose `pip_size` is [`fx_pip_size`]: ten of the points the HTTP
//!   lane decodes by (`crate::data::point_divisor`, the crate's one per-instrument decimals rule:
//!   0.01 for a JPY-quoted pair, 0.0001 otherwise) — EXCEPT a HUF- or CZK-quoted cross, whose
//!   scale `crates/vike-bi5/src/price_scale.rs`'s module doc names as doubtful, which carries `None` rather than a
//!   pip nobody measured. `lot_size`, `swap_long` and `swap_short` are `None`: no table here
//!   carries them.
//! - a metals row is `Contract::Cfd` with `CfdUnderlying::Metal`, `CfdProduct::Cfd` and
//!   `CfdExpiry::Rolling` (a spot metal CFD rolls; it has no expiry date). Margin rate and swaps
//!   are `None` for the same reason.
//!
//! Every row is listed whether it is merely downloadable (the keyless `.bi5` history) or also
//! tradeable through the JForex sidecar; the catalog does not draw that line and this module does
//! not start to.

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec, Instrument,
};

use crate::data::point_divisor;

/// What a [`FX_TABLE`] row is — the hand-curated fact the table asserts, from which
/// [`bundled_instruments`] builds the typed `Contract`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    /// A spot FX pair.
    Fx,
    /// A spot precious-metal CFD (XAU, XAG).
    Metal,
}

/// Quote currencies whose point scale `crates/vike-bi5/src/price_scale.rs`'s module doc calls doubtful (the
/// vendor's FX rule says "most" pairs, and a HUF- or CZK-quoted cross is where it is doubtful), so
/// [`fx_pip_size`] states no pip for them.
const DOUBTFUL_SCALE_QUOTES: &[&str] = &["HUF", "CZK"];

/// `(raw_symbol, kind)` — the bundled canonical FX majors/crosses/exotics + metals set.
/// FX pairs split into base/quote by the first-3/last-3-chars rule in [`bundled_instruments`];
/// the two metals rows get their base/quote spelled out explicitly there instead.
const FX_TABLE: &[(&str, RowKind)] = &[
    // FX majors
    ("EURUSD", RowKind::Fx),
    ("USDJPY", RowKind::Fx),
    ("GBPUSD", RowKind::Fx),
    ("USDCHF", RowKind::Fx),
    ("USDCAD", RowKind::Fx),
    ("AUDUSD", RowKind::Fx),
    ("NZDUSD", RowKind::Fx),
    // FX crosses
    ("EURJPY", RowKind::Fx),
    ("EURGBP", RowKind::Fx),
    ("EURCHF", RowKind::Fx),
    ("EURAUD", RowKind::Fx),
    ("EURCAD", RowKind::Fx),
    ("EURNZD", RowKind::Fx),
    ("GBPJPY", RowKind::Fx),
    ("GBPCHF", RowKind::Fx),
    ("GBPAUD", RowKind::Fx),
    ("GBPCAD", RowKind::Fx),
    ("GBPNZD", RowKind::Fx),
    ("AUDJPY", RowKind::Fx),
    ("AUDCHF", RowKind::Fx),
    ("AUDCAD", RowKind::Fx),
    ("AUDNZD", RowKind::Fx),
    ("CADJPY", RowKind::Fx),
    ("CADCHF", RowKind::Fx),
    ("CHFJPY", RowKind::Fx),
    ("NZDJPY", RowKind::Fx),
    ("NZDCHF", RowKind::Fx),
    ("NZDCAD", RowKind::Fx),
    // FX exotics
    ("USDTRY", RowKind::Fx),
    ("USDMXN", RowKind::Fx),
    ("USDZAR", RowKind::Fx),
    ("USDSGD", RowKind::Fx),
    ("USDHKD", RowKind::Fx),
    ("USDNOK", RowKind::Fx),
    ("USDSEK", RowKind::Fx),
    ("USDPLN", RowKind::Fx),
    ("USDDKK", RowKind::Fx),
    ("USDCNH", RowKind::Fx),
    ("EURTRY", RowKind::Fx),
    ("EURPLN", RowKind::Fx),
    ("EURNOK", RowKind::Fx),
    ("EURSEK", RowKind::Fx),
    ("EURHUF", RowKind::Fx),
    ("EURDKK", RowKind::Fx),
    ("EURCZK", RowKind::Fx),
    ("GBPNOK", RowKind::Fx),
    ("GBPSEK", RowKind::Fx),
    // Metals (Cfd)
    ("XAUUSD", RowKind::Metal),
    ("XAGUSD", RowKind::Metal),
];

/// Build the bundled instrument list from [`FX_TABLE`]. Pure — no I/O. FX rows split
/// base/quote by the first-3/last-3-char rule; the two metals rows spell out base/quote
/// explicitly (`XAUUSD` -> XAU/USD, `XAGUSD` -> XAG/USD).
pub fn bundled_instruments() -> Vec<Instrument> {
    FX_TABLE
        .iter()
        .map(|&(symbol, kind)| {
            let (base, quote) = match symbol {
                "XAUUSD" => ("XAU", "USD"),
                "XAGUSD" => ("XAG", "USD"),
                _ => (&symbol[0..3], &symbol[3..6]),
            };
            Instrument::new("dukascopy", symbol, base, quote, contract_for(kind, symbol, quote))
                .with_description(format!("{base}/{quote}"))
        })
        .collect()
}

/// The pip of the FX pair `symbol` (quoted in `quote`): ten of `point_divisor`'s points
/// (`10 / 1e3` = 0.01 for a JPY-quoted pair, `10 / 1e5` = 0.0001 otherwise — both quotients are
/// exact-to-nearest, so they are the same `f64` as the literals), or `None` for a quote currency
/// in [`DOUBTFUL_SCALE_QUOTES`].
fn fx_pip_size(symbol: &str, quote: &str) -> Option<f64> {
    if DOUBTFUL_SCALE_QUOTES.contains(&quote) {
        return None;
    }
    Some(10.0 / point_divisor(symbol))
}

/// The typed `Contract` a [`FX_TABLE`] row describes. Every field the table does not carry is
/// `None` (see the module doc for which and why).
fn contract_for(kind: RowKind, symbol: &str, quote: &str) -> Contract {
    match kind {
        RowKind::Fx => Contract::Fx(FxSpec {
            pip_size: fx_pip_size(symbol, quote),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        }),
        RowKind::Metal => Contract::Cfd(CfdSpec {
            underlying_kind: CfdUnderlying::Metal,
            underlying: None,
            product: CfdProduct::Cfd,
            expiry: CfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
    }
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

    fn fx(pip: Option<f64>) -> Contract {
        Contract::Fx(FxSpec { pip_size: pip, lot_size: None, swap_long: None, swap_short: None })
    }

    fn find(symbol: &str) -> Instrument {
        bundled_instruments()
            .into_iter()
            .find(|i| i.raw_symbol == symbol)
            .unwrap_or_else(|| panic!("{symbol} present"))
    }

    #[test]
    fn count_matches_table_length() {
        let out = bundled_instruments();
        assert_eq!(out.len(), FX_TABLE.len());
        assert_eq!(out.len(), 49, "47 FX pairs + 2 metals");
        assert_eq!(out.iter().filter(|i| i.asset_class() == AssetClass::Fx).count(), 47);
        assert_eq!(out.iter().filter(|i| i.asset_class() == AssetClass::Cfd).count(), 2);
    }

    #[test]
    fn eurusd_maps_to_fx_with_eur_base_usd_quote() {
        let eurusd = find("EURUSD");
        assert_eq!(eurusd.asset_class(), AssetClass::Fx);
        assert_eq!(eurusd.contract, fx(Some(0.0001)));
        assert_eq!(eurusd.base, "EUR");
        assert_eq!(eurusd.quote, "USD");
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(eurusd.venue, "dukascopy");
    }

    /// The pip is ten of `point_divisor`'s points, bit for bit the conventional literal: 0.01 for
    /// every JPY-quoted pair, 0.0001 for the rest — and no pip at all where the scale is doubtful.
    #[test]
    fn the_pip_follows_the_point_divisor_and_refuses_the_doubtful_quotes() {
        for symbol in ["USDJPY", "EURJPY", "CHFJPY", "NZDJPY"] {
            assert_eq!(find(symbol).contract, fx(Some(0.01)), "{symbol}");
        }
        for symbol in ["GBPUSD", "USDTRY", "USDCNH", "EURDKK", "GBPSEK"] {
            assert_eq!(find(symbol).contract, fx(Some(0.0001)), "{symbol}");
        }
        for symbol in ["EURHUF", "EURCZK"] {
            assert_eq!(find(symbol).contract, fx(None), "{symbol}");
        }
        let no_pip: Vec<String> = bundled_instruments()
            .into_iter()
            .filter(|i| i.contract == fx(None))
            .map(|i| i.raw_symbol)
            .collect();
        assert_eq!(no_pip, ["EURHUF", "EURCZK"], "only the doubtful-scale quotes lack a pip");
    }

    #[test]
    fn xauusd_maps_to_a_rolling_metal_cfd_with_xau_base_usd_quote() {
        for symbol in ["XAUUSD", "XAGUSD"] {
            let metal = find(symbol);
            assert_eq!(metal.asset_class(), AssetClass::Cfd);
            assert_eq!(
                metal.contract,
                Contract::Cfd(CfdSpec {
                    underlying_kind: CfdUnderlying::Metal,
                    underlying: None,
                    product: CfdProduct::Cfd,
                    expiry: CfdExpiry::Rolling,
                    margin_rate: None,
                    swap_long: None,
                    swap_short: None,
                }),
                "{symbol}"
            );
        }
        let xauusd = find("XAUUSD");
        assert_eq!(xauusd.base, "XAU");
        assert_eq!(xauusd.quote, "USD");
        assert_eq!(xauusd.description, "XAU/USD");
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
