//! The ONE canonical venue roster — THE authority on "every venue vike has a bridge crate for".
//!
//! Four per-venue capability tables grew on the same pattern — [`crate::venue_caps::caps_for`],
//! [`crate::venue_margin_support::venue_margin_support`], [`crate::fees::fee_schedule_for`], and
//! `vike_bridge_core::tif::venue_tif` — and each carried its OWN private copy of the venue-name
//! list inside its rows and completeness tests. That is the copy-drift disease one level up:
//! adding a venue forced nothing globally (and the lists HAD already drifted — cTrader was absent
//! from three of the four). This const is the single list they all test against instead.
//!
//! **The contract:** every per-venue capability table's completeness test iterates [`VENUES`] and
//! asserts a declared row exists for each entry — so adding a venue here fails EVERY table's tests
//! until its rows land, and a new bridge crate cannot ship without declaring its capabilities.
//! Registry FALLBACK arms (`VenueCaps::UNSUPPORTED`, `VenueMarginSupport::UNKNOWN`, `TifOutcome::
//! NotEmitted`, `FeeSchedule::Free`) stay for genuinely unknown strings, but no roster venue may
//! silently ride one — a roster venue's row is always NAMED, even when its value equals the
//! fallback (the named row is the declaration that the venue was CLASSIFIED, not forgotten).
//!
//! `vike-catalog`'s `BRIDGE_VENUES` is the sibling slug list on the catalog side (it predates this
//! const and cannot be imported here — vike-catalog depends on vike-model, not vice versa);
//! keeping the two in sync is part of adding a venue.

/// Every venue with a `crates/bridges/<venue>` crate, by canonical lowercase venue id (each
/// crate's `VENUE` const — `vike-ibkr`'s slug is `"ibkr"`). Add a venue here when its bridge
/// crate lands; every capability table's completeness test will then demand its rows.
///
/// ⚠ `#[rustfmt::skip]` because the last line of this literal is a `just new-venue` marker, and a
/// marker with no sibling element below it is what rustfmt re-indents once a generated row lands
/// above it (the row ends in a trailing `//` comment, so rustfmt aligns the marker to that
/// comment's column — MEASURED at the tail of an array, a struct literal, a match block, a call
/// argument list and a plain block alike). A MOVED marker renders the NEXT venue's row at the
/// mangled indent and defeats `--remove`'s comparison. `crates/vike-ops/tests/new_venue_gate.rs`'s
/// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows` is the gate.
#[rustfmt::skip]
pub const VENUES: &[&str] = &[
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
    // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): keep the roster ordered by when the bridge landed
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// The roster is exactly the `crates/bridges/*` set — DERIVED from the tree, never a pinned
    /// count. Every bridge crate's package name is `vike-<venue id>` (`crates/bridges/vike-ibkr`
    /// → package `vike-ibkr` → id `"ibkr"`, the one directory whose name is not the id), so the
    /// directory walk yields the canonical ids directly.
    ///
    /// This test used to be `assert_eq!(VENUES.len(), 14)`, which gated the WRONG direction: a new
    /// bridge crate whose roster entry was forgotten left the length at 14 and passed green, while
    /// a correctly-added venue turned it red until someone bumped the number. A declared number
    /// checked against itself gates nothing — the whole point of this const is that adding a venue
    /// fails every capability table's completeness test, and that only holds if the roster is
    /// derived from the crates rather than hand-declared alongside them.
    #[test]
    fn roster_matches_the_bridge_crates() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("bridges");
        let mut found: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|e| e.expect("readable dir entry").path())
            .filter(|p| p.join("Cargo.toml").is_file())
            .map(|p| {
                let manifest = fs::read_to_string(p.join("Cargo.toml")).expect("read manifest");
                // `[package]` is the first table in every bridge manifest, so the first `name =`
                // line is the package name.
                let name = manifest
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("name = "))
                    .unwrap_or_else(|| panic!("no package name in {}", p.display()))
                    .trim()
                    .trim_matches('"')
                    .to_string();
                name.strip_prefix("vike-")
                    .unwrap_or_else(|| panic!("bridge package {name} must be named vike-<venue>"))
                    .to_string()
            })
            .collect();
        found.sort();
        let mut roster: Vec<String> = VENUES.iter().map(|v| v.to_string()).collect();
        roster.sort();
        assert_eq!(
            roster, found,
            "VENUES must be exactly the crates/bridges/* set (one id per bridge crate)"
        );
    }

    #[test]
    fn no_duplicates() {
        let mut sorted = VENUES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), VENUES.len(), "duplicate venue id in VENUES");
    }

    /// Canonical ids are lowercase ASCII (they key credentials, hist-store partitions, and every
    /// capability registry).
    #[test]
    fn ids_are_lowercase_ascii() {
        for v in VENUES {
            assert!(!v.is_empty());
            assert!(
                v.chars().all(|c| c.is_ascii_lowercase()),
                "venue id {v:?} must be lowercase ascii"
            );
        }
    }
}
