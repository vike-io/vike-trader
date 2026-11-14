//! **dukascopy's catalog: the account's instrument list, handed over at the sidecar's login.** There is no hand
//! table (owner 2026-11-13, "remove all hardcoded symbols"): the JForex sidecar states the account's list once per login
//! (`crate::instrument::grid`, the `instruments` line, built from `IClient.getAvailableInstruments()` and costing the
//! venue nothing), the exec reader PUSHES it here ([`hold`]) into the process's held-list slot
//! (`vike_catalog::held_list::hold`), and the gatewayhub's catalog entry (`vike_catalog::held_list::CatalogProviderHeldList`
//! over [`CATALOG_CLASSES`]) answers the datahub from that slot, which keeps it as `<project>/instruments/dukascopy.json`.
//! Until a login has pushed one the entry answers "no session held" and the datahub changes nothing
//! (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 3, amended 2026-11-13).
//!
//! **What each row's typed `Contract` carries** (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`),
//! decided by the row's CLASS, never its name:
//!
//! - a `FOREX` row is `Contract::Fx` whose `pip_size` is the login's pip; `lot_size` and the swaps are `None` (the list
//!   states none);
//! - every other row is a rolling `Contract::Cfd` whose underlying is the venue's group: `FX_METAL` (and a `METAL` row) a
//!   metal, `IDX_CASH` an index, `COM_SPOT` a commodity, `STK_CASH` and `ETF` a share, `BONDS` a bond, `CRYPTO_CURR` a coin
//!   (a coin the SDK types `FOREX` too), anything else `Other`;
//! - the price step is the row's tick (`properties.tick_size`), which is also what the datahub's archive import scales a
//!   `.bi5` record by (`vike_bi5::Bi5PriceScale::from_tick_size`); the venue's group is the row's venue word
//!   (`contract_type`). An instrument's sizes are its `terms` line, read after subscription, so the row carries none.
//!
//! The symbol is the wire spelling (`OrderMap.canonicalSymbol`: `EURUSD`, `COPPER.CMD/USD`), the base and quote are the
//! two halves of the SDK's name (`COPPER.CMD` and `USD`), and the description is the venue's.

use vike_model::{
    AssetClass, Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct,
    ContractCfdUnderlying, ContractFx, Instrument, SymbolProperties,
};

use crate::instrument::any_symbol::wire_symbol;
use crate::instrument::grid::{InstrumentType, JforexInstrument};

/// The venue key the list is held under.
const VENUE: &str = "dukascopy";

/// The classes the venue lists: currency pairs, and CFDs (metals, indices, commodities, shares, bonds, coins).
pub const CATALOG_CLASSES: &[AssetClass] = &[AssetClass::Fx, AssetClass::Cfd];

/// **Push the account's list, as a login stated it**, into the process's held-list slot (the last login's wins).
pub fn hold(rows: &[JforexInstrument]) {
    vike_catalog::held_list::hold(VENUE, rows.iter().map(instrument_of).collect());
}

/// One row of the list as the catalog's instrument (the module doc).
fn instrument_of(row: &JforexInstrument) -> Instrument {
    let (base, quote) = row.pair.rsplit_once('/').unwrap_or((row.pair.as_str(), ""));
    let underlying = match (row.kind, row.group.as_str()) {
        (_, "CRYPTO_CURR") => Some(ContractCfdUnderlying::Crypto),
        (InstrumentType::Forex, _) => None,
        (InstrumentType::Metal, _) | (_, "FX_METAL") => Some(ContractCfdUnderlying::Metal),
        (_, "IDX_CASH") => Some(ContractCfdUnderlying::Index),
        (_, "COM_SPOT") => Some(ContractCfdUnderlying::Commodity),
        (_, "STK_CASH" | "ETF") => Some(ContractCfdUnderlying::Share),
        (_, "BONDS") => Some(ContractCfdUnderlying::Bond),
        _ => Some(ContractCfdUnderlying::Other),
    };
    let contract = match underlying {
        None => Contract::Fx(ContractFx {
            pip_size: Some(row.pip_value),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        }),
        Some(underlying_kind) => Contract::Cfd(ContractCfd {
            underlying_kind,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
    };
    let instrument = Instrument::new(VENUE, wire_symbol(row), base, quote, contract)
        .with_description(row.description.clone().unwrap_or_else(|| row.pair.clone()))
        .with_properties(SymbolProperties {
            tick_size: row.tick_size(),
            ..SymbolProperties::default()
        });
    if row.group.is_empty() { instrument } else { instrument.with_venue_word(row.group.clone()) }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use crate::test_support::listed;
    use vike_catalog::CatalogProvider;
    use vike_catalog::held_list::CatalogProviderHeldList;

    fn find(rows: &[Instrument], symbol: &str) -> Instrument {
        rows.iter().find(|i| i.raw_symbol == symbol).cloned().unwrap_or_else(|| panic!("{symbol}"))
    }

    fn fx(pip: f64) -> Contract {
        Contract::Fx(ContractFx {
            pip_size: Some(pip),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        })
    }

    /// Every row of the list is an instrument of the catalog, typed by its class, on its own tick.
    #[test]
    fn every_listed_row_is_a_catalog_instrument_typed_by_its_class() {
        let rows: Vec<Instrument> = listed().iter().map(instrument_of).collect();
        assert_eq!(rows.len(), 180);
        let classes = |c: AssetClass| rows.iter().filter(|i| i.asset_class() == c).count();
        // 72 FOREX rows less the two coins are pairs; the coins, the two metals and the 106 CFDs are CFDs
        assert_eq!((classes(AssetClass::Fx), classes(AssetClass::Cfd)), (70, 110));
        let eur = find(&rows, "EURUSD");
        assert_eq!(
            (eur.venue.as_str(), eur.base.as_str(), eur.quote.as_str(), eur.contract.clone()),
            ("dukascopy", "EUR", "USD", fx(0.0001))
        );
        assert_eq!(eur.properties.tick_size, 0.00001);
        assert_eq!(find(&rows, "USDJPY").contract, fx(0.01));
        // the HUF and CZK crosses carry the pip the list states (the hand table had none: its scale was doubtful)
        assert_eq!(find(&rows, "EURHUF").contract, fx(0.01));
        assert_eq!(find(&rows, "EURCZK").contract, fx(0.001));
        let kind = |s: &str| match find(&rows, s).contract {
            Contract::Cfd(cfd) => (cfd.underlying_kind, cfd.expiry),
            other => panic!("{s}: {other:?}"),
        };
        let rolling = ContractCfdExpiry::Rolling;
        assert_eq!(kind("XAUUSD"), (ContractCfdUnderlying::Metal, rolling));
        assert_eq!(kind("BTCUSD"), (ContractCfdUnderlying::Crypto, rolling));
        assert_eq!(kind("DEU.IDX/EUR"), (ContractCfdUnderlying::Index, rolling));
        assert_eq!(kind("COPPER.CMD/USD"), (ContractCfdUnderlying::Commodity, rolling));
        assert_eq!(kind("BMW.DE/EUR"), (ContractCfdUnderlying::Share, rolling));
        let copper = find(&rows, "COPPER.CMD/USD");
        assert_eq!(
            (copper.base.as_str(), copper.quote.as_str(), copper.properties.tick_size),
            ("COPPER.CMD", "USD", 0.0001)
        );
        assert_eq!(
            copper.contract_type.as_deref(),
            Some("COM_SPOT"),
            "the venue's group, verbatim"
        );
    }

    /// The list a login pushed is what the gatewayhub's entry answers; before any login it answers "no session".
    #[test]
    fn the_pushed_list_is_what_the_held_list_entry_answers() {
        let entry = CatalogProviderHeldList { venue: VENUE, classes: CATALOG_CLASSES };
        hold(&listed());
        assert_eq!(entry.no_session(), None);
        let answered = entry.list_instruments().expect("held");
        assert_eq!(answered.len(), 180);
        assert_eq!(find(&answered, "EURUSD").contract, fx(0.0001));
    }
}
