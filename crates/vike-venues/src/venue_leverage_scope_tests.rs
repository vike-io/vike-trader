//! The leverage-scope table's pins: the matrix verbatim, one row per roster venue, and the one relation to what
//! vike sends (`crate::LEVERAGE_VENUES`).

use super::*;
use vike_model::VENUES;

use LeverageGrain::{
    BrokerMarginRatePerInstrument, FullyCollateralized, PerAccount, PerAsset,
    PerInstrumentAndMarginMode, PerSymbol, PerSymbolAndSide, WholeAccountMarginModel,
};
use LeverageMaximumSource::{AccountField, AuthenticatedBracketRequest, InstrumentField, Nowhere};
use LeverageSetter::{BrokerOnly, NothingToSet, VenueApi};

/// The whole table, verbatim: `(venue, grain, where the maximum is published, who sets the figure)`.
///
/// `#[rustfmt::skip]` keeps one aligned row per venue, and keeps a scaffolded row (a long venue id) from being
/// re-wrapped by rustfmt.
#[rustfmt::skip]
const MATRIX: &[(&str, LeverageGrain, LeverageMaximumSource, LeverageSetter)] = &[
    ("binance",     PerSymbol,                     AuthenticatedBracketRequest,    VenueApi),
    ("bybit",       PerSymbolAndSide,              InstrumentField,                VenueApi),
    ("okx",         PerInstrumentAndMarginMode,    InstrumentField,                VenueApi),
    ("deribit",     WholeAccountMarginModel,       InstrumentField,                NothingToSet),
    ("oanda",       PerAccount,                    InstrumentField,                VenueApi),
    ("ig",          BrokerMarginRatePerInstrument, InstrumentField,                BrokerOnly),
    ("fxcm",        BrokerMarginRatePerInstrument, InstrumentField,                BrokerOnly),
    ("dukascopy",   PerAccount,                    AccountField,                   BrokerOnly),
    ("polymarket",  FullyCollateralized,           Nowhere,                        NothingToSet),
    ("ibkr",        WholeAccountMarginModel,       LeverageMaximumSource::Unknown, NothingToSet),
    ("ctrader",     PerAccount,                    AuthenticatedBracketRequest,    BrokerOnly),
    ("alpaca",      PerAccount,                    LeverageMaximumSource::Unknown, VenueApi),
    ("aster",       PerSymbol,                     AuthenticatedBracketRequest,    VenueApi),
    ("hyperliquid", PerAsset,                      InstrumentField,                VenueApi),
    // vike:new-venue:row // TODO(new-venue: {venue}): pin the row `leverage_scope_for` declares
    // vike:new-venue:row ("{venue}", LeverageGrain::Unknown, LeverageMaximumSource::Unknown, LeverageSetter::Unknown),
];

#[test]
fn the_leverage_scope_matrix_is_pinned_verbatim() {
    for &(venue, grain, maximum_source, setter) in MATRIX {
        assert_eq!(leverage_scope_for(venue), scope(grain, maximum_source, setter), "{venue}");
    }
    assert_eq!(leverage_scope_for("nasdaq"), LeverageScope::UNKNOWN);
}

/// Completeness over the roster: every `vike_model::VENUES` entry has exactly one pinned row, so a venue added to
/// the roster fails here until its row is declared.
#[test]
fn every_roster_venue_has_one_declared_row() {
    assert_eq!(MATRIX.len(), VENUES.len(), "one pinned row per roster venue");
    for venue in VENUES {
        let rows = MATRIX.iter().filter(|(v, ..)| v == venue).count();
        assert_eq!(rows, 1, "{venue}: {rows} pinned rows");
    }
}

/// What vike makes real is a figure the venue lets a program set: every `LEVERAGE_VENUES` row (the side of what
/// vike SENDS) is a venue whose own row says `VenueApi`.
#[test]
fn the_venue_lets_a_program_set_what_vike_makes_real() {
    for row in crate::LEVERAGE_VENUES {
        assert_eq!(leverage_scope_for(row.venue).setter, VenueApi, "{}", row.venue);
    }
}

/// `NothingToSet` is exactly the grains that hold no figure, so the two columns cannot disagree.
#[test]
fn nothing_to_set_is_exactly_the_grains_without_a_figure() {
    for venue in VENUES {
        let row = leverage_scope_for(venue);
        let no_figure = matches!(row.grain, WholeAccountMarginModel | FullyCollateralized);
        assert_eq!(row.setter == NothingToSet, no_figure, "{venue}: {row:?}");
    }
}
