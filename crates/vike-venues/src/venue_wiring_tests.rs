//! The pins of [`super::VENUE_WIRING`]: roster completeness, the verbatim matrix, the two orders
//! (moved here from `vike-tradehub`'s table tests with the table), and the `const` lookup the
//! bridges' probe symbols are declared through.

use super::{VENUE_WIRING, Wiring, wired_symbol, wiring_for};
use vike_model::VENUES;

/// One `VENUE_WIRING` row as the matrix pin reads it: the venue, its published class string and its engine.
type Pinned = (&'static str, &'static str, Option<(&'static str, u8)>);

/// The rows that carry an engine, as `(venue, symbol, engine_rank)`, in row (MOUNT) order.
fn engine_rows() -> Vec<(&'static str, &'static str, u8)> {
    VENUE_WIRING.iter().filter_map(|&(v, w)| w.engine().map(|(s, r)| (v, s, r))).collect()
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
/// It is the union of what `vike-docs`' `WIRING` and `vike-tradehub`'s per-venue `WiredMarket`
/// consts said on the day they were merged; a change here is a behaviour change.
#[rustfmt::skip]
#[test]
fn current_wiring_matrix_is_pinned() {
    #[rustfmt::skip]
    let want: &[Pinned] = &[
        ("binance",     "live-exec-recon",       Some(("BTCUSDT", 0))),
        ("bybit",       "live-exec-recon",       Some(("BTCUSDT", 1))),
        ("okx",         "live-exec-recon",       Some(("BTC-USDT-SWAP", 2))),
        ("deribit",     "live-exec-recon",       Some(("BTC-PERPETUAL", 5))),
        ("hyperliquid", "live-exec-recon",       Some(("BTC", 3))),
        ("aster",       "live-exec-recon",       Some(("BTCUSDT.P", 4))),
        ("alpaca",      "live-exec-recon",       Some(("AAPL", 6))),
        ("ctrader",     "live-exec-recon",       Some(("EURUSD", 7))),
        ("ig",          "live-exec-recon",       Some(("CS.D.EURUSD.MINI.IP", 8))),
        ("oanda",       "live-exec-recon",       Some(("EURUSD", 9))),
        ("ibkr",        "feature-gated",         Some(("AAPL.SMART.USD", 10))),
        ("polymarket",  "feature-gated",         Some(("", 11))),
        ("fxcm",        "feature-gated",         Some(("EURUSD", 12))),
        ("dukascopy",   "sidecar-exec-no-recon", None),
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

/// Every engine row names its symbol, except polymarket's deliberately account-wide empty one.
#[test]
fn every_engine_row_names_its_symbol_except_polymarkets_account_wide_mount() {
    for (venue, symbol, _) in engine_rows() {
        if venue == "polymarket" {
            assert!(symbol.is_empty(), "polymarket's mount is account-wide (empty symbol)");
        } else {
            assert!(!symbol.is_empty(), "wired venue {venue} must name its mounted symbol");
        }
    }
}

/// Exactly the two classes the daemon mounts carry an engine.
#[test]
fn only_the_two_mounted_classes_carry_an_engine() {
    for &(venue, w) in VENUE_WIRING {
        let mounted = matches!(w, Wiring::LiveExecRecon { .. } | Wiring::FeatureGated { .. });
        assert_eq!(w.engine().is_some(), mounted, "{venue}: {w:?}");
    }
}

/// The four strings `venues.json` publishes. Renaming one is a published-value change.
#[test]
fn the_published_wiring_strings_are_pinned() {
    assert_eq!(Wiring::LiveExecRecon { symbol: "X", engine_rank: 0 }.as_str(), "live-exec-recon");
    assert_eq!(Wiring::FeatureGated { symbol: "X", engine_rank: 0 }.as_str(), "feature-gated");
    assert_eq!(Wiring::SidecarExecNoRecon.as_str(), "sidecar-exec-no-recon");
    assert_eq!(Wiring::PaperOnly.as_str(), "paper-only");
}

/// The `const` lookup agrees with the table row for row, refuses prefixes and strays, and is
/// usable in a `const` item (the shape the bridges declare `PROBE_SYMBOL` in).
#[test]
fn the_const_lookup_agrees_with_the_table() {
    for &(venue, w) in VENUE_WIRING {
        assert_eq!(wired_symbol(venue), w.engine().map(|(s, _)| s), "{venue}");
    }
    assert_eq!(wired_symbol("polymarket"), Some(""));
    assert_eq!(wired_symbol("dukascopy"), None);
    assert_eq!(wired_symbol("binanc"), None, "a prefix is not a match");
    assert_eq!(wired_symbol("not-a-venue"), None);
    const AT_COMPILE_TIME: Option<&str> = wired_symbol("okx");
    assert_eq!(AT_COMPILE_TIME, Some("BTC-USDT-SWAP"));
}
