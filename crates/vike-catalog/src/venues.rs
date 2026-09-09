//! The declarative bridge-venue registry: the one flat list of every venue that has a
//! `crates/bridges/<venue>` crate. `CatalogProvider` impls are wired ad hoc per binary (each
//! provider is constructed by hand where it's needed, e.g. `vike-app`'s `spawn_catalog_fetcher`
//! builds its own `Vec<Box<dyn CatalogProvider>>` at runtime — there is no central provider
//! registry to enumerate), so this const is deliberately NOT derived from `CatalogProvider`
//! impls; it is the flat venue-slug list other crates (e.g. `vike-connections`' credential-status
//! grid) need without depending on every bridge crate.

/// The authoritative bridge-venue list (`crates/bridges/*`, one crate per venue — `vike-ibkr`'s
/// slug is `"ibkr"`). Add a venue here when its bridge crate lands; every other crate that needs
/// "all known venues" (e.g. `vike-connections`) should read this instead of keeping its own copy.
///
/// RE-POINTED (#498, done): this is now literally the canonical `vike_model::VENUES` roster —
/// vike-catalog depends on vike-model, so the catalog-side slug list and the model-side roster are
/// ONE const and can never drift. The [`tests::mirrors_the_canonical_roster`] set-equality guard
/// below fails loudly if a future edit ever re-points this at a private copy. This list had drifted
/// once before the re-point — hyperliquid's bridge merged (#362) without being added here, so the
/// venue never appeared in the vike-connections credential grid; sharing the roster makes that
/// class of drift structurally impossible.
pub const BRIDGE_VENUES: &[&str] = vike_model::VENUES;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_core_venues() {
        for v in ["binance", "bybit", "okx", "deribit", "oanda", "ig", "fxcm", "dukascopy"] {
            assert!(BRIDGE_VENUES.contains(&v), "missing venue: {v}");
        }
    }

    // vike:new-venue:note bump the pinned length below and add `"{venue}"` to its literal list — this is a HAND-MAINTAINED COUNT over a const that is literally `vike_model::VENUES`, so it fails on a correctly-added venue and passes on a forgotten one (the exact anti-pattern crates/vike-model/src/venues.rs's `roster_matches_the_bridge_crates` was rewritten to escape): crates/vike-catalog/src/venues.rs's `contains_all_fourteen_current_venues`
    #[test]
    fn contains_all_fourteen_current_venues() {
        assert_eq!(BRIDGE_VENUES.len(), 14);
        for v in [
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
        ] {
            assert!(BRIDGE_VENUES.contains(&v), "missing venue: {v}");
        }
    }

    #[test]
    fn no_duplicates() {
        let mut sorted = BRIDGE_VENUES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), BRIDGE_VENUES.len(), "duplicate venue slug in BRIDGE_VENUES");
    }

    /// The anti-drift guard (#498): the catalog-side slug list must be SET-EQUAL to the canonical
    /// `vike_model::VENUES` roster. Today they are the SAME const so this holds trivially — but the
    /// test stays a real gate: if a future edit ever re-points `BRIDGE_VENUES` back at a private
    /// copy, any added/removed/renamed venue on either side trips here (the exact drift that once
    /// dropped hyperliquid from the credential grid before the two lists were unified).
    #[test]
    fn mirrors_the_canonical_roster() {
        let mut ours = BRIDGE_VENUES.to_vec();
        ours.sort_unstable();
        let mut roster = vike_model::VENUES.to_vec();
        roster.sort_unstable();
        assert_eq!(ours, roster, "BRIDGE_VENUES must be set-equal to vike_model::VENUES");
    }
}
