use super::*;

/// Every known venue in the registry, paired with its expected row. The completeness test
    /// below asserts this covers the canonical roster (`crate::venues::VENUES`) exactly.
    ///
    /// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    /// re-indented by rustfmt once a row ending in a trailing `//` comment is generated above it,
    /// which defeats `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const MATRIX: &[(&str, VenueMarginSupport)] = &[
        ("binance", BINANCE),
        ("bybit", BYBIT),
        ("okx", OKX),
        ("deribit", DERIBIT),
        ("oanda", OANDA),
        ("ig", IG),
        ("fxcm", FXCM),
        ("dukascopy", DUKASCOPY),
        ("polymarket", POLYMARKET),
        ("ibkr", IBKR),
        ("ctrader", CTRADER),
        ("alpaca", ALPACA),
        ("aster", ASTER),
        ("hyperliquid", HYPERLIQUID),
        // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): declared-row completeness
    ];

/// The full matrix, pinned verbatim (like the tif matrix test). Any drift in a venue row is
/// loud here. Each tuple is (venue, offered_modes, switch_mechanism, iso_adj) — the mode vike
/// PRODUCES is not this table's subject and is pinned by `venue_caps`'
/// `default_margin_mode_reflects_current_behavior`.
#[test]
fn margin_matrix_is_pinned() {
    use MarginMode::{Cash, Cross, Isolated};
    use SwitchMechanism::{
        AccountLevel, AtLeverageSet, NotApplicable, PerOrderField, PerSymbolEndpoint,
    };
    #[rustfmt::skip]
        let rows: &[(&str, &[MarginMode], SwitchMechanism, bool)] = &[
            // crypto perps — each exposes isolated via a different mechanism
            ("binance",     &[Cross, Isolated],       PerSymbolEndpoint, true),
            ("aster",       &[Cross, Isolated],       PerSymbolEndpoint, false),
            ("okx",         &[Cash, Cross, Isolated], PerOrderField,     true),  // adapter honors margin_mode (unset=cross)
            ("bybit",       &[Cross, Isolated],       AccountLevel,      true),
            ("hyperliquid", &[Cross, Isolated],       AtLeverageSet,     false),
            // deribit — single cross model (portfolio margin is a maint-rate variation within cross)
            ("deribit",     &[Cross],                 NotApplicable,     false),
            // polymarket — fully collateralized, no margin axis (never liquidates)
            ("polymarket",  &[Cash],                  NotApplicable,     false),
            // FX/CFD + equity — shared account margin, no isolated concept: single-mode cross
            ("oanda",       &[Cross],                 NotApplicable,     false),
            ("ig",          &[Cross],                 NotApplicable,     false),
            ("fxcm",        &[Cross],                 NotApplicable,     false),
            ("dukascopy",   &[Cross],                 NotApplicable,     false),
            ("ctrader",     &[Cross],                 NotApplicable,     false),
            ("alpaca",      &[Cross],                 NotApplicable,     false),
            ("ibkr",        &[Cross],                 NotApplicable,     false),
            // vike:new-venue:row ("{venue}",  &[Cross],                 NotApplicable,     false), // TODO(new-venue: {venue}): pin the vendor doc
        ];
    for (venue, modes, mech, iso_adj) in rows {
        let got = venue_margin_support(venue);
        assert_eq!(got.offered_modes, *modes, "{venue} offered_modes");
        assert_eq!(got.switch_mechanism, *mech, "{venue} switch_mechanism");
        assert_eq!(got.isolated_wallet_adjustable, *iso_adj, "{venue} isolated_wallet_adjustable");
    }
    assert_eq!(rows.len(), MATRIX.len(), "matrix test row count == registry size");
}

/// Completeness: every registry venue resolves to its declared const (no missing row), and the
/// registry equals the consts (so the pinned matrix above is authoritative).
#[test]
fn registry_maps_every_known_venue() {
    for (venue, want) in MATRIX {
        assert_eq!(venue_margin_support(venue), *want, "{venue}");
    }
}

/// Completeness vs the canonical roster: every [`crate::venues::VENUES`] entry has a NAMED
/// declared row in `MATRIX`, and the registry serves exactly it. Adding a venue to the roster
/// fails here until its row exists.
#[test]
fn every_roster_venue_has_a_declared_row() {
    assert_eq!(MATRIX.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
    for &v in crate::venues::VENUES {
        let (_, want) = MATRIX
            .iter()
            .find(|(rv, _)| *rv == v)
            .unwrap_or_else(|| panic!("no VenueMarginSupport row declared for roster venue {v}"));
        assert_eq!(venue_margin_support(v), *want, "{v}: registry must serve its declared row");
    }
}

/// An unknown venue is the conservative cross-only fixed row (fail-closed), and equals Default.
#[test]
fn unknown_venue_is_conservative_cross_only() {
    let c = venue_margin_support("nasdaq");
    assert_eq!(c, VenueMarginSupport::UNKNOWN);
    assert_eq!(c, VenueMarginSupport::default());
    assert_eq!(c.offered_modes, &[MarginMode::Cross]);
    assert_eq!(c.switch_mechanism, SwitchMechanism::NotApplicable);
    assert!(!c.isolated_wallet_adjustable);
    assert!(!c.is_switchable());
}

/// `is_switchable`: only the venues that offer >1 mode AND a real mechanism. The 5 crypto-perp
/// venues are switchable; deribit + the fixed-mode FX/equity/polymarket venues are not.
#[test]
fn switchable_matches_multi_mode_venues() {
    for v in ["binance", "aster", "okx", "bybit", "hyperliquid"] {
        assert!(venue_margin_support(v).is_switchable(), "{v} offers a mode switch");
    }
    for v in ["deribit", "polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr"] {
        assert!(!venue_margin_support(v).is_switchable(), "{v} is fixed-mode");
    }
}

/// The switch-mechanism split, pinned per family: OKX is the lone PER-ORDER field (the hardcode
/// step 2 flips); binance/aster are per-symbol; bybit account-level; hyperliquid
/// at-leverage-set; everyone else NotApplicable.
#[test]
fn switch_mechanisms_are_grouped_correctly() {
    assert_eq!(venue_margin_support("okx").switch_mechanism, SwitchMechanism::PerOrderField);
    for v in ["binance", "aster"] {
        assert_eq!(venue_margin_support(v).switch_mechanism, SwitchMechanism::PerSymbolEndpoint);
    }
    assert_eq!(venue_margin_support("bybit").switch_mechanism, SwitchMechanism::AccountLevel);
    assert_eq!(
        venue_margin_support("hyperliquid").switch_mechanism,
        SwitchMechanism::AtLeverageSet
    );
    for v in ["deribit", "polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr"] {
        assert_eq!(venue_margin_support(v).switch_mechanism, SwitchMechanism::NotApplicable, "{v}");
    }
}

/// OKX is the only venue exposing the `Cash` mode (spot/margin `tdMode:"cash"`) alongside
/// cross+isolated; the other perp venues offer cross+isolated; polymarket is cash-only.
#[test]
fn cash_mode_is_okx_and_polymarket() {
    assert!(venue_margin_support("okx").offered_modes.contains(&MarginMode::Cash));
    assert_eq!(venue_margin_support("polymarket").offered_modes, &[MarginMode::Cash]);
    for v in ["binance", "aster", "bybit", "hyperliquid"] {
        assert!(!venue_margin_support(v).offered_modes.contains(&MarginMode::Cash), "{v}");
        assert!(venue_margin_support(v).offered_modes.contains(&MarginMode::Isolated), "{v}");
    }
}

/// `isolated_wallet_adjustable` — the step-1 user-confirmed `true` set is binance/bybit/okx;
/// every other venue (including aster/hyperliquid, held conservatively) is `false`.
#[test]
fn isolated_wallet_adjustable_is_the_confirmed_set() {
    for v in ["binance", "bybit", "okx"] {
        assert!(venue_margin_support(v).isolated_wallet_adjustable, "{v} adjustable");
    }
    for v in [
        "aster",
        "hyperliquid",
        "deribit",
        "polymarket",
        "oanda",
        "ig",
        "fxcm",
        "dukascopy",
        "alpaca",
        "ibkr",
    ] {
        assert!(!venue_margin_support(v).isolated_wallet_adjustable, "{v} not adjustable");
    }
}

/// MarginMode serde round-trips (the enum carries `serde` per the design). Default is Cross;
/// exactly three variants exist.
#[test]
fn margin_mode_serde_and_default() {
    assert_eq!(MarginMode::default(), MarginMode::Cross);
    for m in [MarginMode::Cash, MarginMode::Cross, MarginMode::Isolated] {
        let s = serde_json::to_string(&m).expect("serialize");
        let back: MarginMode = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, m);
    }
    assert_eq!(serde_json::to_string(&MarginMode::Cross).unwrap(), "\"Cross\"");
    assert_eq!(serde_json::to_string(&MarginMode::Cash).unwrap(), "\"Cash\"");
}

#[test]
fn default_is_cross_and_predicates_hold() {
    assert_eq!(MarginMode::default(), MarginMode::Cross);
    assert!(MarginMode::default().is_cross());
    assert!(!MarginMode::default().is_isolated());
    assert!(MarginMode::Isolated.is_isolated());
    assert!(!MarginMode::Cash.is_cross());
}
