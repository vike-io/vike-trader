use super::*;

/// The whole matrix, pinned VERBATIM — the playbook's STEP 1 requirement. A row that changes
    /// without this copy changing is a silent behaviour change.
    ///
    /// ⚠ `#[rustfmt::skip]`: the last line of this literal is a `just new-venue` marker, and rustfmt
    /// re-indents such a marker once a generated row ending in a trailing `//` comment lands above
    /// it — `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const PINNED: &[(&str, usize, Naming, BareSymbol)] = &[
        ("binance",     2, Naming::PerpSuffix,    BareSymbol::Unambiguous),
        ("bybit",       2, Naming::PerpSuffix,    BareSymbol::Ambiguous),
        ("okx",         4, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("deribit",     3, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("oanda",       2, Naming::NoDerivative,  BareSymbol::Unambiguous),
        ("ig",          4, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("fxcm",        0, Naming::Unaddressable, BareSymbol::Unambiguous),
        ("dukascopy",   2, Naming::NoDerivative,  BareSymbol::Unambiguous),
        ("polymarket",  1, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("ibkr",        2, Naming::VenueNative,   BareSymbol::Ambiguous),
        ("ctrader",     2, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("alpaca",      2, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("aster",       2, Naming::PerpSuffix,    BareSymbol::Unambiguous),
        ("hyperliquid", 2, Naming::VenueNative,   BareSymbol::Unambiguous),
        // vike:new-venue:row ("{venue}", 0, Naming::Unaddressable, BareSymbol::Unmeasured), // TODO(new-venue: {venue}): pin whatever the arm above declares
    ];

#[test]
fn addressing_matrix_is_pinned() {
    for (venue, class_count, naming, bare) in PINNED {
        let row = addressing_for(venue);
        assert_eq!(row.classes.len(), *class_count, "{venue}: class count drifted");
        assert_eq!(row.naming, *naming, "{venue}: naming drifted");
        assert_eq!(row.bare_symbol, *bare, "{venue}: bare-symbol verdict drifted");
    }
}

/// This module's own source, read at RUNTIME rather than `include_str!`ed — the idiom
/// `crates/vike-model/src/venues/mod.rs` uses for its roster derivation, and it keeps this file out
/// of `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s ratchet.
fn own_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("addressing.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Completeness vs the canonical roster, the playbook shape: **every roster venue has a NAMED
/// arm, even when its value equals the fallback** — *"the named row is the declaration that the
/// venue was CLASSIFIED, not forgotten"*. Adding a venue to `vike_model::VENUES` fails here
/// until its row exists.
///
/// ⚠ The check is a SOURCE scan and not `assert_ne!(row, UNCLASSIFIED)`, and the difference is
/// the convention: a venue is entitled to a row whose VALUE equals the fallback (a fresh bridge
/// addresses nothing and has been measured by nobody, which is precisely what the scaffolded row
/// says). A value comparison cannot tell that row from an absent one, so it would refuse the
/// scaffold's own correct output.
#[test]
fn every_roster_venue_has_a_named_row() {
    let src = own_source();
    for &venue in vike_model::VENUES {
        assert!(
            PINNED.iter().any(|(v, ..)| *v == venue),
            "roster venue {venue} has no pinned addressing row"
        );
        assert!(
            src.contains(&format!("\"{venue}\" => VenueAddressing {{"))
                || src.contains(&format!("\"{venue}\" => VenueAddressing::")),
            "roster venue {venue} has no NAMED arm in `addressing_for` — it would fall through \
                 to the refusing fallback, and a named row is what proves a venue was classified \
                 rather than forgotten"
        );
    }
    assert_eq!(
        PINNED.len(),
        vike_model::VENUES.len(),
        "the pin carries a row for a venue that is not on the roster (or is short one)"
    );
}

/// The scan above must be able to FAIL — without this it answers "named" for a venue that is not
/// there and the completeness test is green over an empty table.
#[test]
fn the_named_arm_scan_can_actually_fail() {
    let src = own_source();
    assert!(src.contains("\"bybit\" => VenueAddressing {"), "the scan cannot see a real arm");
    assert!(
        !src.contains("\"no-such-venue\" => VenueAddressing"),
        "the scan matches a venue that has no arm"
    );
}

/// ⚠ **The fallback REFUSES.** This is the assertion that separates this table from
/// `session_calendar_for`'s fail-permissive shape, which 0061 names as the thing not to copy.
#[test]
fn an_unknown_venue_addresses_nothing_and_must_be_claimed() {
    let row = addressing_for("no-such-venue");
    assert_eq!(row, VenueAddressing::UNCLASSIFIED);
    assert!(row.must_claim(), "an unclassified venue must never answer permissive");
    assert!(row.classes.is_empty());
    for class in AssetClass::ALL {
        assert!(!row.addresses(*class), "an unclassified venue addresses nothing");
    }
}

/// `must_claim` is the refusing direction: only a POSITIVE unambiguous measurement lets a bare
/// symbol through. Both other states demand a claim.
#[test]
fn must_claim_fails_closed() {
    let unmeasured = VenueAddressing {
        classes: CRYPTO_SPOT_AND_PERP,
        naming: Naming::PerpSuffix,
        bare_symbol: BareSymbol::Unmeasured,
    };
    assert!(unmeasured.must_claim());
    assert!(addressing_for("bybit").must_claim(), "the measured ambiguity demands a claim");
    assert!(addressing_for("ibkr").must_claim());
    assert!(!addressing_for("binance").must_claim());
    assert!(!addressing_for("okx").must_claim());
}

/// A venue that declares itself unaddressable must address no class — otherwise the column says
/// two different things.
#[test]
fn an_unaddressable_venue_addresses_no_class() {
    for &venue in vike_model::VENUES {
        let row = addressing_for(venue);
        if row.naming == Naming::Unaddressable {
            assert!(
                row.classes.is_empty(),
                "{venue} declares Unaddressable but names {} class(es)",
                row.classes.len()
            );
        } else {
            assert!(
                !row.classes.is_empty(),
                "{venue} names a Naming but addresses no class — say Unaddressable instead"
            );
        }
    }
}

/// `addresses` answers off the row's own set, and answers `false` for a class the venue does not
/// mint. The bybit row is the one a route consults.
#[test]
fn addresses_answers_from_the_row() {
    let bybit = addressing_for("bybit");
    assert!(bybit.addresses(AssetClass::CryptoSpot));
    assert!(bybit.addresses(AssetClass::CryptoPerp));
    assert!(!bybit.addresses(AssetClass::CryptoFuture));
    assert!(!bybit.addresses(AssetClass::Equity));
    assert!(addressing_for("okx").addresses(AssetClass::Option));
    assert!(!addressing_for("fxcm").addresses(AssetClass::Fx));
}
