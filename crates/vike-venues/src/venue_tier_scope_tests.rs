use super::*;
use vike_model::VENUES;

/// **The verbatim matrix pin**: the whole table in roster order. A venue that moves to the
/// venue-wide scope changes here too, in a diff a reviewer reads as "this venue now trades ONE tier
/// per box".
#[test]
fn the_tier_scope_matrix_is_pinned() {
    use TierScope::{Account, Venue};
    let matrix: Vec<(&str, TierScope)> = VENUES.iter().map(|v| (*v, venue_tier_scope(v))).collect();
    assert_eq!(
        matrix,
        vec![
            ("binance", Account),
            ("bybit", Account),
            ("okx", Account),
            ("deribit", Account),
            ("oanda", Account),
            ("ig", Account),
            ("fxcm", Account),
            ("dukascopy", Account),
            ("polymarket", Account),
            ("ibkr", Venue),
            ("ctrader", Account),
            ("alpaca", Account),
            ("aster", Account),
            ("hyperliquid", Account),
        ],
        "a venue's tier scope moved — decision 0140 is the one record that moved one"
    );
}

/// **Completeness**: every roster venue is a NAMED row, so adding a bridge fails here until its
/// scope is classified — even when the answer is the fallback's `Account`.
#[test]
fn every_roster_venue_has_a_named_tier_scope_row() {
    for venue in VENUES {
        assert!(
            named_row(venue).is_some(),
            "{venue} rides the unknown-venue fallback — give it a NAMED row, even if it is \
             `Account`"
        );
    }
    assert_eq!(named_row("no-such-venue"), None);
    assert_eq!(
        venue_tier_scope("no-such-venue"),
        TierScope::Account,
        "0119's rule is the fallback"
    );
}
