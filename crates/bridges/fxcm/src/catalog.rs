//! **FXCM's instrument list: the Offers table of the session a RUNNING account already holds** —
//! [`CatalogProviderFxcm`], the gatewayhub's fxcm catalog entry (`crates/vike-gatewayhub/src/rest/catalog.rs`'s
//! `CatalogProviders::real`), whose answer the datahub keeps as `instruments/fxcm.json` when the operator refreshes it.
//!
//! FXCM has ONE source of its list: a logged-in session's Offers table (`forex_connect_offers`, 497 rows on the EUR demo).
//! There is no keyless endpoint. So the list is read from an account's RECONCILE session through
//! `crate::account::reads` (the session the facts lane reads, never the exec one) and never through a login of its
//! own (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 3, amended
//! 2026-11-13). One listing is one table refresh, bounded by `crate::account::reads::OFFERS_WAIT`; nothing is
//! subscribed. With no account's reconcile session up, [`CatalogProvider::no_session`] answers and the datahub
//! keeps the list it had: no change, no error.
//!
//! **Each row** ([`instruments_of`]) is typed by its CLASS, the row's `InstrumentType` (never its name): a forex
//! pair is `Contract::Fx` with the row's pip (`point_size`, absent from a shim older than it) and its lot (the base
//! unit size); every other class is a rolling `Contract::Cfd` of its kind. The grid is the facts' own
//! (`crate::account::quote_rate::grid_of`), the quote currency [`quote_currency_of`]'s, and `raw_symbol` and the
//! description are the venue's name verbatim (`EUR/USD`, `US30`, `XAU/USD`). A row of no class the SDK names, or
//! whose quote currency cannot be determined, is left out: the bridge refuses to trade it.

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AssetClass, Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct,
    ContractCfdUnderlying, ContractFx, Instrument, SymbolProperties,
};
use vike_utility::errors::CarriedRefusal;

use crate::account::quote_rate::{InstrumentType, Offer, grid_of, quote_currency_of};
use crate::account::reads::any_account_reads;
use crate::reconcile::VENUE;
use crate::refusals::FxcmCatalogRefusal;

/// FXCM's `CatalogProvider`: the Offers table of a running account's reconcile session (module doc).
#[derive(Clone, Copy, Debug, Default)]
pub struct CatalogProviderFxcm;

/// Why there is no list to read now.
fn not_held() -> CarriedRefusal {
    FxcmCatalogRefusal::NoSession.carried()
}

impl CatalogProvider for CatalogProviderFxcm {
    fn venue(&self) -> &str {
        VENUE
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn no_session(&self) -> Option<CarriedRefusal> {
        any_account_reads().is_none().then(not_held)
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let reads = any_account_reads().ok_or_else(|| CatalogError(not_held()))?;
        let offers = reads.offers().map_err(|why| CatalogError(why.carried()))?;
        Ok(instruments_of(&offers))
    }
}

/// **The instrument list of one Offers table**, in table order (module doc).
pub(crate) fn instruments_of(offers: &[Offer]) -> Vec<Instrument> {
    offers.iter().filter_map(instrument_of).collect()
}

/// The kept row of one Offers row, `None` for a row the bridge refuses to trade (module doc): what the row's user
/// name is read off (`crate::instrument::names::user_name_of_offer`).
pub(crate) fn instrument_of(row: &Offer) -> Option<Instrument> {
    let class = InstrumentType::of(row.instrument_type)?;
    let quote = quote_currency_of(row).ok()?;
    let name = row.instrument.as_str();
    let base = name.split_once('/').map_or(name, |(base, _)| base);
    let contract = match class {
        InstrumentType::Forex => Contract::Fx(ContractFx {
            pip_size: row.point_size,
            lot_size: (row.base_unit > 0).then_some(row.base_unit as f64),
            swap_long: None,
            swap_short: None,
        }),
        other => Contract::Cfd(ContractCfd {
            underlying_kind: underlying_of(other),
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
    };
    // A row the facts would refuse for its grid keeps no grid here either.
    let properties = if (0..=8).contains(&row.digits) && row.base_unit > 0 {
        grid_of(row)
    } else {
        SymbolProperties::default()
    };
    Some(
        Instrument::new(VENUE, name, base, quote, contract)
            .with_description(name)
            .with_properties(properties),
    )
}

/// The kind of thing a CFD-class row tracks, from its class.
fn underlying_of(class: InstrumentType) -> ContractCfdUnderlying {
    match class {
        InstrumentType::Index | InstrumentType::FxIndex => ContractCfdUnderlying::Index,
        InstrumentType::Commodity => ContractCfdUnderlying::Commodity,
        InstrumentType::Treasury => ContractCfdUnderlying::Bond,
        InstrumentType::Bullion => ContractCfdUnderlying::Metal,
        InstrumentType::Share | InstrumentType::CfdShare => ContractCfdUnderlying::Share,
        InstrumentType::Crypto => ContractCfdUnderlying::Crypto,
        InstrumentType::Forex => ContractCfdUnderlying::Fx,
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use crate::account::quote_rate::parse_offers;
    use crate::test_support::offer;

    /// The shim's `forex_connect_offers` rows of a committed, MEASURED fixture.
    fn fixture(json: &str) -> Vec<Offer> {
        let fixture: serde_json::Value = serde_json::from_str(json).expect("the fixture is JSON");
        parse_offers(&fixture["offers"].to_string()).expect("the shim's shape")
    }

    fn find<'a>(list: &'a [Instrument], name: &str) -> &'a Instrument {
        list.iter().find(|i| i.raw_symbol == name).unwrap_or_else(|| panic!("{name} listed"))
    }

    fn cfd(kind: ContractCfdUnderlying) -> Contract {
        Contract::Cfd(ContractCfd {
            underlying_kind: kind,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        })
    }

    /// **The 48 forex rows of the EUR demo** (measured 2026-10-26, a shim with no `point_size` yet): every one is an
    /// FX pair split on its slash, with the row's own tick, its 1000 lot and NO pip (the row states none).
    #[test]
    fn the_measured_forex_rows_are_fx_pairs_with_their_grid() {
        let list = instruments_of(&fixture(include_str!("../tests/fixtures/eur_demo_offers.json")));
        assert_eq!(list.len(), 48);
        assert!(list.iter().all(|i| i.asset_class() == AssetClass::Fx && i.venue == "fxcm"));
        let eurusd = find(&list, "EUR/USD");
        assert_eq!((eurusd.base.as_str(), eurusd.quote.as_str()), ("EUR", "USD"));
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(
            eurusd.contract,
            Contract::Fx(ContractFx {
                pip_size: None,
                lot_size: Some(1000.0),
                swap_long: None,
                swap_short: None
            })
        );
        let grid = eurusd.properties;
        assert_eq!((grid.tick_size, grid.step_size, grid.min_qty), (1e-5, 1000.0, 1000.0));
        assert_eq!(find(&list, "USD/JPY").properties.tick_size, 1e-3);
    }

    /// **Every non-forex row of the EUR demo** (449, measured 2026-10-30, with the terms keys): each is a rolling CFD
    /// of its class, quoted in its native currency, with the venue's own minimum and maximum.
    #[test]
    fn the_measured_class_rows_are_cfds_of_their_kind() {
        let list = instruments_of(&fixture(include_str!("../tests/fixtures/offers_classes.json")));
        assert_eq!(list.len(), 449, "every measured class row has a class and a quote currency");
        assert!(list.iter().all(|i| i.asset_class() == AssetClass::Cfd));
        for (name, kind, base, quote) in [
            ("US30", ContractCfdUnderlying::Index, "US30", "USD"),
            ("USDOLLAR", ContractCfdUnderlying::Index, "USDOLLAR", "USD"),
            ("XAU/USD", ContractCfdUnderlying::Metal, "XAU", "USD"),
            ("USOil", ContractCfdUnderlying::Commodity, "USOil", "USD"),
            ("Bund", ContractCfdUnderlying::Bond, "Bund", "EUR"),
            ("NVDA.ext", ContractCfdUnderlying::Share, "NVDA.ext", "USD"),
            ("BTC/USD", ContractCfdUnderlying::Crypto, "BTC", "USD"),
        ] {
            let row = find(&list, name);
            assert_eq!(row.contract, cfd(kind), "{name}");
            assert_eq!((row.base.as_str(), row.quote.as_str()), (base, quote), "{name}");
        }
        let us30 = find(&list, "US30").properties;
        assert_eq!(
            (us30.tick_size, us30.step_size, us30.min_qty, us30.max_qty),
            (0.01, 1.0, 1.0, 4000.0)
        );
    }

    /// A forex row carries the pip the row states; a row of no class the SDK names, and a forex row whose
    /// ContractCurrency disagrees with its name, are left out (the bridge refuses to trade either).
    #[test]
    fn a_pip_is_the_rows_and_an_untradable_row_is_left_out() {
        let mut eurusd = offer("EUR/USD", 1.1, 1.1002);
        eurusd.point_size = Some(0.0001);
        let mut unknown = offer("ZZZ/USD", 1.0, 1.1);
        unknown.instrument_type = 42;
        let mut disagreeing = offer("GBP/USD", 1.3, 1.31);
        disagreeing.contract_currency = "USD".to_string();
        let list = instruments_of(&[eurusd, unknown, disagreeing]);
        assert_eq!(list.len(), 1);
        assert!(
            matches!(list[0].contract, Contract::Fx(ContractFx { pip_size: Some(p), .. }) if p == 0.0001)
        );
    }

    #[test]
    fn the_provider_describes_itself_with_no_session() {
        let provider = CatalogProviderFxcm;
        assert_eq!(provider.venue(), "fxcm");
        assert_eq!(provider.asset_classes(), &[AssetClass::Fx, AssetClass::Cfd]);
        assert_eq!(provider.mode(), CatalogMode::Enumerable);
    }
}
