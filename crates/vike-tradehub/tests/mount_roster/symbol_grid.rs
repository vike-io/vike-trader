//! The grid-source partition over the REAL registry. It lived in
//! `crates/vike-mount/src/symbol_grid_tests.rs` until the venue mount contract finished
//! (docs/decisions/0096): each row is a bridge's declaration now, and only the crate holding the
//! registry can read all of them.

use vike_bridge_core::venue_mount::DeclaredGridSource;
use vike_mount::declared_grid_source;
use vike_tradehub::registry::REGISTRY;

/// Every roster venue is CLASSIFIED — the capability-map completeness gate. A new bridge crate
/// joining `vike_model::VENUES` must state where its arm's per-symbol grid comes from (even
/// when the answer equals the fallback), rather than silently riding `NoGrid`.
///
/// NON-VACUOUS in the direction that matters: `declared_grid_source` has a catch-all `_` arm,
/// so a missing row could never be caught by exhaustiveness. This asserts the NAMED partition
/// instead — the `InHand` set is spelled out here, so an arm that stops holding its table in
/// hand (or a new one that starts) turns this red.
#[test]
fn every_roster_venue_declares_a_grid_source() {
    let sorted = |want: DeclaredGridSource| {
        let mut v: Vec<&str> = vike_model::VENUES
            .iter()
            .copied()
            .filter(|venue| declared_grid_source(REGISTRY, venue) == want)
            .collect();
        v.sort_unstable(); // roster ORDER is not the subject here; membership is
        v
    };
    assert_eq!(
        sorted(DeclaredGridSource::InHand),
        vec!["ctrader", "hyperliquid"],
        "exactly these arms hold a whole-venue instrument table at mount time; adding one is a \
             deliberate change to what make_engine_with_legs wires"
    );
    // …and no roster venue is unclassified by accident: the PerSymbolFetch family is named too,
    // so the remainder is a DECLARED `NoGrid` set rather than an assumed one.
    assert_eq!(
        sorted(DeclaredGridSource::PerSymbolFetch),
        vec!["alpaca", "aster", "binance", "bybit", "deribit", "okx"],
        "these arms already make a symbol-scoped blocking pre-fetch for the mounted symbol; a \
             declared leg would cost one more at mount time, so they stay on the scalar fallback"
    );
    // ⚠ ibkr is `NoGrid` HERE and `PerSymbolFetch` in its bridge. The default build this module
    // runs in registers it `FeatureAbsent` (its bridge is behind this crate's `ibkr` feature), and
    // a build without the bridge answers generic, venue-free facts
    // (`crates/vike-mount/src/registry.rs`'s `ABSENT_BOOK`, and `NoGrid`). The real row is pinned
    // by `crates/bridges/vike-ibkr/src/mount_tests.rs`'s
    // `the_declaration_is_the_rows_the_mount_crate_carried`.
    assert_eq!(
        sorted(DeclaredGridSource::NoGrid),
        vec!["dukascopy", "fxcm", "ibkr", "ig", "oanda", "polymarket"],
        "these arms fetch no instrument grid at all, so a leg inherits nothing from the mount"
    );
    // vike:new-venue:note add "{venue}" to the sorted vec above that matches the grid source its mount DECLARES (a scaffolded mount declares `NoGrid`) — this assertion compares against a SORTED literal, so the scaffold cannot append the row for you: crates/vike-tradehub/tests/mount_roster/symbol_grid.rs's `every_roster_venue_declares_a_grid_source`
}
