//! ⚠ **TRANSITIONAL — this whole module is deleted by the migration's final task.**
//!
//! [`LEGACY_ARMS`] names the venues whose `make_engine_for_account` legacy arm still exists; each
//! port removes its venue. [`LEGACY_REGISTRY`] is this crate's own registry for its tests — and
//! `vike-run`'s, which names no bridge and so holds no registry — kept row-for-row in step with
//! `vike_tradehub::registry::REGISTRY` by every port, so the roster tests run against each venue's
//! contract implementation the moment it lands. `crates/vike-tradehub/tests/daemon/registry.rs`
//! and this module's own test hold both registries' `Legacy` rows equal to [`LEGACY_ARMS`].

use crate::VenueRow;

/// The venues whose legacy arm still exists.
pub const LEGACY_ARMS: &[&str] = &[
    "binance",
    "bybit",
    "okx",
    "deribit",
    "oanda",
    "ig",
    "fxcm",
    "dukascopy",
    "polymarket",
    "ibkr",
    "ctrader",
    "alpaca",
    "aster",
    "hyperliquid",
];

/// This crate's registry for its own tests. Roster order.
pub const LEGACY_REGISTRY: &[VenueRow] = &[
    VenueRow::Legacy("binance"),
    VenueRow::Legacy("bybit"),
    VenueRow::Legacy("okx"),
    VenueRow::Legacy("deribit"),
    VenueRow::Legacy("oanda"),
    VenueRow::Legacy("ig"),
    VenueRow::Legacy("fxcm"),
    VenueRow::Legacy("dukascopy"),
    VenueRow::Legacy("polymarket"),
    VenueRow::Legacy("ibkr"),
    VenueRow::Legacy("ctrader"),
    VenueRow::Legacy("alpaca"),
    VenueRow::Legacy("aster"),
    VenueRow::Legacy("hyperliquid"),
];

#[cfg(test)]
mod transition_tests {
    use super::*;

    /// A port deletes an arm AND flips the row, or this is red.
    #[test]
    fn the_legacy_rows_are_exactly_the_venues_that_still_have_an_arm() {
        let mut rows: Vec<&str> = LEGACY_REGISTRY
            .iter()
            .filter_map(|r| match r {
                VenueRow::Legacy(v) => Some(*v),
                _ => None,
            })
            .collect();
        let mut arms = LEGACY_ARMS.to_vec();
        rows.sort_unstable();
        arms.sort_unstable();
        assert_eq!(rows, arms, "a ported venue leaves LEGACY_ARMS and flips its row together");
    }

    /// Every roster venue has exactly one row.
    #[test]
    fn the_transitional_registry_partitions_the_roster() {
        let mut seen: Vec<&str> = LEGACY_REGISTRY.iter().map(VenueRow::venue).collect();
        let mut roster = vike_model::VENUES.to_vec();
        seen.sort_unstable();
        roster.sort_unstable();
        assert_eq!(seen, roster);
    }
}
