use super::*;
use crate::{caps_for, venue_amend::AmendSemantics, venue_amend::amend_semantics};

/// The whole matrix, pinned verbatim: one row per roster venue, one cell per kind. A cell that moves
/// is a decision, so it moves here first. `N` native, `C` cancel-then-new, `-` refused.
#[test]
fn the_amend_matrix_is_pinned() {
    use AmendMode::{CancelThenNew as C, Native as N, Refused as X};
    //                        limit stop  stop_limit take_profit
    #[rustfmt::skip]
    let rows: &[(&str, [AmendMode; 4])] = &[
        ("binance",     [N, C, X, X]),
        ("bybit",       [N, C, X, X]),
        ("okx",         [N, C, X, X]),
        ("aster",       [N, C, X, X]),
        ("deribit",     [N, N, N, N]),
        ("hyperliquid", [N, N, N, N]),
        ("oanda",       [N, N, X, X]),
        ("ig",          [N, N, X, X]),
        ("fxcm",        [N, N, X, X]),
        ("dukascopy",   [N, N, X, X]),
        ("ibkr",        [N, N, X, X]),
        ("ctrader",     [N, N, X, X]),
        ("polymarket",  [C, X, X, X]),
        ("alpaca",      [X, X, X, X]),
        // vike:new-venue:row ("{venue}",  [C, X, X, X]), // TODO(new-venue: {venue}): the scaffolded caps row rests market and limit with no native amend, so a limit is cancel-then-new and the rest refused: move with the capability row
    ];
    for (venue, want) in rows {
        for (kind, want) in AmendKind::ALL.iter().zip(want) {
            assert_eq!(amend_mode(venue, *kind), *want, "{venue} / {}", kind.word());
        }
    }
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one pinned row per roster venue");
    for &v in vike_model::VENUES {
        assert!(rows.iter().any(|(rv, _)| *rv == v), "{v} has no pinned amend row");
    }
}

/// The rule is read against the two rows it derives from, so a capability row that moves cannot
/// leave this answer behind.
#[test]
fn the_mode_agrees_with_the_capability_rows() {
    for &venue in vike_model::VENUES {
        let caps = caps_for(venue);
        for kind in AmendKind::ALL {
            let mode = amend_mode(venue, kind);
            let placeable = caps.supported_order_kinds.contains(&kind.word());
            match mode {
                AmendMode::Refused => {
                    assert!(
                        !placeable || REFUSED_VENUES.contains(&venue),
                        "{venue} / {}: refused although the venue places it",
                        kind.word()
                    );
                }
                AmendMode::CancelThenNew => {
                    assert!(
                        placeable,
                        "{venue} / {}: cannot be replaced, it cannot be placed",
                        kind.word()
                    );
                }
                AmendMode::Native => {
                    assert!(
                        placeable && caps.supports_modify,
                        "{venue} / {}: native but the adapter wires no amend",
                        kind.word()
                    );
                    assert_ne!(
                        amend_semantics(venue),
                        AmendSemantics::Unsupported,
                        "{venue}: native, yet the amend table says no amend leaves the process"
                    );
                }
            }
        }
    }
}

/// A kind that does not rest has no amend kind, and an unknown venue is refused (fail closed).
#[test]
fn a_market_order_and_an_unknown_venue_have_no_amend() {
    assert_eq!(AmendKind::of(&OrderType::Market), None);
    assert_eq!(AmendKind::of(&OrderType::Unrecognized("iceberg".into())), None);
    assert_eq!(amend_mode_for("binance", &OrderType::Market), None);
    for venue in ["sim", "paper", "nasdaq", ""] {
        for kind in AmendKind::ALL {
            assert_eq!(amend_mode(venue, kind), AmendMode::Refused, "{venue}");
        }
    }
}

/// Every exception names a pair that exists: a stale row (a venue renamed, a kind dropped from its
/// row) would otherwise sit there doing nothing.
#[test]
fn every_exception_row_names_a_live_pair() {
    for (venue, kind) in NO_NATIVE_KIND {
        assert!(vike_model::VENUES.contains(venue), "{venue}");
        let caps = caps_for(venue);
        assert!(caps.supports_modify, "{venue}: listed as 'amends, but not {}'", kind.word());
        assert!(caps.supported_order_kinds.contains(&kind.word()), "{venue} / {}", kind.word());
    }
    for venue in REFUSED_VENUES {
        assert!(vike_model::VENUES.contains(venue), "{venue}");
    }
}

/// The words differ where the behaviour differs, and a stop's names the property the operator cares
/// about (no unprotected moment) only when it is true.
#[test]
fn the_words_say_what_each_mode_does() {
    let stop_first = amend_words(AmendMode::CancelThenNew, AmendKind::Stop, true);
    assert!(stop_first.contains("never without a stop"), "{stop_first}");
    let entry_stop = amend_words(AmendMode::CancelThenNew, AmendKind::Stop, false);
    assert!(!entry_stop.contains("never without a stop"), "{entry_stop}");
    assert!(entry_stop.contains("neither"), "{entry_stop}");
    let limit = amend_words(AmendMode::CancelThenNew, AmendKind::Limit, false);
    assert!(limit.contains("PLACE IN THE QUEUE"), "{limit}");
    assert!(amend_words(AmendMode::Native, AmendKind::Limit, false).contains("in place"));
    assert!(amend_words(AmendMode::Refused, AmendKind::Stop, true).contains("cancel it"));
}
