//! The pins of [`super::VENUE_WIRING`]: roster completeness, the verbatim matrix, the two orders
//! (moved here from `vike-tradehub`'s table tests with the table), and the mount lookups.

use super::{EngineMount, VENUE_WIRING, VenueWiring, mount_of, mounts_a_symbol, wiring_for};
use vike_model::VENUES;

/// One `VENUE_WIRING` row as the matrix pin reads it: the venue, its published class string and its engine.
type Pinned = (&'static str, &'static str, Option<(EngineMount, u8)>);

/// The rows that carry an engine, as `(venue, mount, engine_rank)`, in row (MOUNT) order.
fn engine_rows() -> Vec<(&'static str, EngineMount, u8)> {
    VENUE_WIRING.iter().filter_map(|&(v, w)| w.engine().map(|(m, r)| (v, m, r))).collect()
}

/// One row per `VENUES` entry, no duplicate, no non-roster stray: adding a venue to the roster
/// reddens this until its row exists (moved from `vike-docs`' `wiring_map_is_roster_complete`).
#[test]
fn the_wiring_table_is_roster_complete() {
    assert_eq!(
        VENUE_WIRING.len(),
        VENUES.len(),
        "one VENUE_WIRING row per roster venue, no strays"
    );
    for &venue in VENUES {
        assert!(
            wiring_for(venue).is_some(),
            "roster venue {venue} has no VENUE_WIRING row — add one in \
             crates/vike-venues/src/venue_wiring.rs"
        );
    }
    let mut ids: Vec<&str> = VENUE_WIRING.iter().map(|&(v, _)| v).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), VENUE_WIRING.len(), "duplicate venue id in VENUE_WIRING");
}

/// The verbatim matrix, in MOUNT order: the class string `venues.json` publishes and the engine.
/// It is the union of what `vike-docs`' `WIRING` and `vike-tradehub`'s per-venue `RuntimeMarket`
/// consts said on the day they were merged, less the symbols (owner, 2026-11-13: an account's own row
/// states its symbol); a change here is a behaviour change. The `Pair` rows are the venues whose
/// symbols' names stated their money on that day (`EURUSD`, the IG epic around it).
#[rustfmt::skip]
#[test]
fn current_wiring_matrix_is_pinned() {
    use EngineMount::{Account, Pair, Symbol};
    #[rustfmt::skip]
    let want: &[Pinned] = &[
        ("binance",     "live-exec-recon",       Some((Symbol, 0))),
        ("bybit",       "live-exec-recon",       Some((Symbol, 1))),
        ("okx",         "live-exec-recon",       Some((Symbol, 2))),
        ("deribit",     "live-exec-recon",       Some((Symbol, 5))),
        ("hyperliquid", "live-exec-recon",       Some((Symbol, 3))),
        ("aster",       "live-exec-recon",       Some((Symbol, 4))),
        ("alpaca",      "live-exec-recon",       Some((Symbol, 6))),
        ("ctrader",     "live-exec-recon",       Some((Pair, 7))),
        ("ig",          "live-exec-recon",       Some((Pair, 8))),
        ("oanda",       "live-exec-recon",       Some((Pair, 9))),
        ("ibkr",        "feature-gated",         Some((Symbol, 10))),
        ("polymarket",  "feature-gated",         Some((Account, 11))),
        ("fxcm",        "feature-gated",         Some((Pair, 12))),
        ("dukascopy",   "sidecar-exec-recon",    Some((Pair, 13))),
        // vike:new-venue:row ("{venue}", "paper-only", None), // TODO(new-venue: {venue}): move with the table's placeholder row
    ];
    let got: Vec<Pinned> =
        VENUE_WIRING.iter().map(|&(v, w)| (v, w.as_str(), w.engine())).collect();
    assert_eq!(got, want);
}

/// MOUNT order: the order `vike_tradehub_core::assemble_runtime` mounts venues in and `armed_live_venues` claims
/// live-account locks in. Pinned before decision 0098 moved the table; it must not move. Every
/// feature-gated row is included here — which of them a build mounts is `vike_tradehub_core::wired_markets`'
/// answer over the daemon's registry (`vike-tradehub`'s `wired_markets` tests pin that filter).
#[test]
fn the_engines_are_mounted_in_this_order() {
    let got: Vec<&str> = engine_rows().iter().map(|&(v, ..)| v).collect();
    assert_eq!(
        got,
        [
            "binance",
            "bybit",
            "okx",
            "deribit",
            "hyperliquid",
            "aster",
            "alpaca",
            "ctrader",
            "ig",
            "oanda",
            "ibkr",
            "polymarket",
            "fxcm",
            "dukascopy",
        ]
    );
}

/// ENGINE order: the order the core holds the default engines in (`CoreSnapshot::portfolio`'s
/// `venues`, and so every per-venue `py_sum`) and lists their default reconcile legs in.
/// Ascending `engine_rank`, the smallest being the primary; distinct ranks; the first row carries
/// the smallest.
#[test]
fn the_core_holds_the_default_engines_in_rank_order() {
    let mut ranked = engine_rows();
    ranked.sort_by_key(|&(.., r)| r);
    let got: Vec<&str> = ranked.iter().map(|&(v, ..)| v).collect();
    assert_eq!(
        got,
        [
            "binance",
            "bybit",
            "okx",
            "hyperliquid",
            "aster",
            "deribit",
            "alpaca",
            "ctrader",
            "ig",
            "oanda",
            "ibkr",
            "polymarket",
            "fxcm",
            "dukascopy",
        ]
    );

    let mut ranks: Vec<u8> = engine_rows().iter().map(|&(.., r)| r).collect();
    let n = ranks.len();
    ranks.sort_unstable();
    ranks.dedup();
    assert_eq!(
        ranks.len(),
        n,
        "two rows share an engine_rank: their order would fall to the table"
    );
    assert_eq!(
        engine_rows().first().map(|&(.., r)| r),
        engine_rows().iter().map(|&(.., r)| r).min(),
        "the FIRST engine row mounts first and is held as the primary engine: it carries the \
         smallest rank"
    );
}

/// Every engine row's account states a symbol, except polymarket's deliberately account-wide mount.
#[test]
fn every_engine_row_mounts_a_symbol_except_polymarkets_account_wide_mount() {
    for (venue, mount, _) in engine_rows() {
        assert_eq!(mount == EngineMount::Account, venue == "polymarket", "{venue}: {mount:?}");
        assert_eq!(mounts_a_symbol(venue), venue != "polymarket", "{venue}");
    }
}

/// Exactly the three classes the daemon mounts carry an engine.
#[test]
fn only_the_three_mounted_classes_carry_an_engine() {
    for &(venue, w) in VENUE_WIRING {
        let mounted = matches!(
            w,
            VenueWiring::LiveExecutionReconcile { .. }
                | VenueWiring::FeatureGated { .. }
                | VenueWiring::SidecarExecutionReconcile { .. }
        );
        assert_eq!(w.engine().is_some(), mounted, "{venue}: {w:?}");
    }
}

/// The four strings `venues.json` publishes. Renaming one is a published-value change.
#[test]
fn the_published_wiring_strings_are_pinned() {
    let mount = EngineMount::Symbol;
    assert_eq!(
        VenueWiring::LiveExecutionReconcile { mount, engine_rank: 0 }.as_str(),
        "live-exec-recon"
    );
    assert_eq!(VenueWiring::FeatureGated { mount, engine_rank: 0 }.as_str(), "feature-gated");
    assert_eq!(
        VenueWiring::SidecarExecutionReconcile { mount, engine_rank: 0 }.as_str(),
        "sidecar-exec-recon"
    );
    assert_eq!(VenueWiring::PaperOnly.as_str(), "paper-only");
}

/// **No roster venue is classed as unreconciled any more**: the class that said so (`sidecar-exec-no-recon`, dukascopy's)
/// was replaced when the sidecar learned to answer what the venue lists, so the one venue whose exec is an owned child
/// process is a `SidecarExecutionReconcile` row and every engine row is in the default reconcile set. A venue that is
/// really unreconciled again would need a class of its own, argued where the row is.
#[test]
fn the_one_child_process_venue_is_classed_as_reconciled() {
    assert_eq!(
        wiring_for("dukascopy"),
        Some(VenueWiring::SidecarExecutionReconcile { mount: EngineMount::Pair, engine_rank: 13 })
    );
    assert!(
        VENUE_WIRING.iter().all(|&(_, w)| w.as_str() != "sidecar-exec-no-recon"),
        "the retired class string must not be published by any row"
    );
}

/// The mount lookup agrees with the table row for row and refuses prefixes and strays; a venue it does not
/// know needs a symbol.
#[test]
fn the_mount_lookup_agrees_with_the_table() {
    for &(venue, w) in VENUE_WIRING {
        assert_eq!(mount_of(venue), w.engine().map(|(m, _)| m), "{venue}");
    }
    assert_eq!(mount_of("polymarket"), Some(EngineMount::Account));
    assert_eq!(mount_of("dukascopy"), Some(EngineMount::Pair));
    assert_eq!(mount_of("binanc"), None, "a prefix is not a match");
    assert_eq!(mount_of("not-a-venue"), None);
    assert!(mounts_a_symbol("not-a-venue"), "what does not say otherwise needs a symbol");
}
