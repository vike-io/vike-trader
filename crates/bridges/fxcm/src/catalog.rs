//! Bundled static instrument catalog — FXCM's twin of dukascopy's `catalog` module. FXCM has no
//! bulk instrument-list endpoint reachable without the native ForexConnect SDK session, so this is
//! the bundled **confident FX + metals instrument set** (majors, crosses, common exotics, plus
//! XAU/XAG spot metals) — hand-curated. FXCM also offers indices/commodities/crypto CFDs; those are
//! deliberately omitted here since their FXCM symbol naming isn't confidently known (this table is
//! extendable — append rows to [`FX_TABLE`] once verified).
//!
//! Declared **unconditionally** — NOT behind the crate-local `fxcm` Cargo feature — because this is
//! pure data, no FFI, no network (mirrors `pub const CAPS` in `lib.rs`). [`bundled_instruments`] is a
//! pure function over a `const` table (`(raw_symbol, RowKind)` pairs), fully unit-testable with
//! no SDK and no credentials.
//!
//! **What each row's typed `Contract` carries** (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`):
//!
//! - an FX row is `Contract::Fx` whose `pip_size` is the conventional FX pip ([`fx_pip_size`]:
//!   0.01 for a JPY-quoted pair, 0.0001 otherwise — the same rule as dukascopy's
//!   `point_divisor`, a pip being ten of that venue's points). `lot_size`, `swap_long` and
//!   `swap_short` are `None`: no table here carries them, and FXCM's base unit size is read only
//!   through the C++ shim's `fc_base_unit_size` at session time, never at catalog time.
//! - a metals row is `Contract::Cfd` with `CfdUnderlying::Metal`, `CfdProduct::Cfd` and
//!   `CfdExpiry::Rolling` (a spot metal CFD rolls; it has no expiry date). Margin rate and swaps are
//!   `None` for the same reason.
//!
//! FXCM naming note: symbols keep their slash, e.g. `"EUR/USD"` (unlike dukascopy's unslashed
//! `EURUSD`) — `raw_symbol` is stored verbatim; base/quote are the two halves split on `'/'`.

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec, Instrument,
};

/// What a [`FX_TABLE`] row is — the hand-curated fact the table asserts, from which
/// [`bundled_instruments`] builds the typed `Contract`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    /// A spot FX pair.
    Fx,
    /// A spot precious-metal CFD (XAU, XAG).
    Metal,
}

/// `(raw_symbol, kind)` — the bundled confident FX majors/crosses/exotics + metals set.
/// FX pairs split into base/quote on `'/'` in [`bundled_instruments`]; the two metals rows use the
/// same split (their raw symbols are already slashed: `XAU/USD`, `XAG/USD`).
const FX_TABLE: &[(&str, RowKind)] = &[
    // FX majors
    ("EUR/USD", RowKind::Fx),
    ("USD/JPY", RowKind::Fx),
    ("GBP/USD", RowKind::Fx),
    ("USD/CHF", RowKind::Fx),
    ("USD/CAD", RowKind::Fx),
    ("AUD/USD", RowKind::Fx),
    ("NZD/USD", RowKind::Fx),
    // FX crosses
    ("EUR/JPY", RowKind::Fx),
    ("EUR/GBP", RowKind::Fx),
    ("EUR/CHF", RowKind::Fx),
    ("EUR/AUD", RowKind::Fx),
    ("EUR/CAD", RowKind::Fx),
    ("EUR/NZD", RowKind::Fx),
    ("GBP/JPY", RowKind::Fx),
    ("GBP/CHF", RowKind::Fx),
    ("GBP/AUD", RowKind::Fx),
    ("GBP/CAD", RowKind::Fx),
    ("GBP/NZD", RowKind::Fx),
    ("AUD/JPY", RowKind::Fx),
    ("AUD/CHF", RowKind::Fx),
    ("AUD/CAD", RowKind::Fx),
    ("AUD/NZD", RowKind::Fx),
    ("CAD/JPY", RowKind::Fx),
    ("CAD/CHF", RowKind::Fx),
    ("CHF/JPY", RowKind::Fx),
    ("NZD/JPY", RowKind::Fx),
    // FX exotics
    ("USD/TRY", RowKind::Fx),
    ("USD/MXN", RowKind::Fx),
    ("USD/ZAR", RowKind::Fx),
    ("USD/SEK", RowKind::Fx),
    ("USD/NOK", RowKind::Fx),
    ("USD/HKD", RowKind::Fx),
    ("USD/CNH", RowKind::Fx),
    ("EUR/TRY", RowKind::Fx),
    ("EUR/PLN", RowKind::Fx),
    ("EUR/NOK", RowKind::Fx),
    ("EUR/SEK", RowKind::Fx),
    ("TRY/JPY", RowKind::Fx),
    ("ZAR/JPY", RowKind::Fx),
    // Metals (Cfd)
    ("XAU/USD", RowKind::Metal),
    ("XAG/USD", RowKind::Metal),
];

/// The conventional FX pip for a pair quoted in `quote`: 0.01 when the quote currency is JPY,
/// 0.0001 otherwise. The rule is the venue-independent convention dukascopy's `point_divisor`
/// also encodes (JPY quotes at 1,000 points per unit, everything else at 100,000; a pip is ten
/// points), and it is applied only to the [`FX_TABLE`] pairs, none of which is quoted in a
/// currency (HUF, CZK) where the convention is known not to hold.
fn fx_pip_size(quote: &str) -> f64 {
    if quote == "JPY" { 0.01 } else { 0.0001 }
}

/// The typed `Contract` a [`FX_TABLE`] row describes. Every field the table does not carry is
/// `None` (see the module doc for which and why).
fn contract_for(kind: RowKind, quote: &str) -> Contract {
    match kind {
        RowKind::Fx => Contract::Fx(FxSpec {
            pip_size: Some(fx_pip_size(quote)),
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

/// Build the bundled instrument list from [`FX_TABLE`]. Pure — no I/O, no FFI. Every row (FX and
/// metals alike) splits base/quote on the raw symbol's `'/'`.
pub fn bundled_instruments() -> Vec<Instrument> {
    FX_TABLE
        .iter()
        .map(|&(symbol, kind)| {
            let (base, quote) = symbol.split_once('/').expect("FX_TABLE rows are slashed pairs");
            Instrument::new("fxcm", symbol, base, quote, contract_for(kind, quote))
                .with_description(symbol)
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

    fn fx(pip: f64) -> Contract {
        Contract::Fx(FxSpec {
            pip_size: Some(pip),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        })
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
        assert_eq!(out.len(), 41, "39 FX pairs + 2 metals");
        assert_eq!(out.iter().filter(|i| i.asset_class() == AssetClass::Fx).count(), 39);
        assert_eq!(out.iter().filter(|i| i.asset_class() == AssetClass::Cfd).count(), 2);
    }

    #[test]
    fn eurusd_maps_to_fx_with_eur_base_usd_quote() {
        let eurusd = find("EUR/USD");
        assert_eq!(eurusd.asset_class(), AssetClass::Fx);
        assert_eq!(eurusd.contract, fx(0.0001));
        assert_eq!(eurusd.base, "EUR");
        assert_eq!(eurusd.quote, "USD");
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(eurusd.venue, "fxcm");
    }

    /// Every JPY-quoted pair (majors, crosses and the TRY/ZAR exotics alike) carries the 0.01 pip;
    /// a JPY BASE (none in the table) would not.
    #[test]
    fn jpy_quoted_pairs_carry_the_hundredth_pip() {
        for symbol in ["USD/JPY", "EUR/JPY", "CHF/JPY", "TRY/JPY", "ZAR/JPY"] {
            assert_eq!(find(symbol).contract, fx(0.01), "{symbol}");
        }
        for symbol in ["USD/TRY", "EUR/PLN", "USD/CNH", "GBP/NZD"] {
            assert_eq!(find(symbol).contract, fx(0.0001), "{symbol}");
        }
    }

    #[test]
    fn xauusd_maps_to_a_rolling_metal_cfd_with_xau_base_usd_quote() {
        for symbol in ["XAU/USD", "XAG/USD"] {
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
        let xauusd = find("XAU/USD");
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
