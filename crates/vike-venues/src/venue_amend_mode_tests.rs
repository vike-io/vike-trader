use super::*;
use crate::{capabilities_for, venue_amend::AmendSemantics, venue_amend::amend_semantics};

/// The whole matrix, pinned verbatim: one row per roster venue, one cell per kind. A cell that moves
/// is a decision, so it moves here first. `N` native, `C` cancel-then-new, `-` refused.
#[test]
fn the_amend_matrix_is_pinned() {
    use AmendMode::{CancelThenNew as C, Native as N, Refused as X};
    //                        limit stop  stop_limit take_profit
    #[rustfmt::skip]
    // a blank line apart where neighbouring rows flip in separate commits, so each cherry-picks alone
    let rows: &[(&str, [AmendMode; 4])] = &[
        ("binance",     [N, C, C, C]),

        ("bybit",       [N, N, N, N]),

        ("okx",         [N, N, N, N]),
        ("aster",       [N, C, X, X]),
        ("deribit",     [N, N, N, N]),
        ("hyperliquid", [N, N, N, N]),
        ("oanda",       [N, N, X, C]),
        ("ig",          [N, N, X, X]),
        ("fxcm",        [N, N, X, X]),
        ("dukascopy",   [N, N, N, N]),

        ("ibkr",        [N, N, N, X]),
        ("ctrader",     [N, N, N, X]),
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

/// The TRIGGER move's matrix (a modify that names `new_trigger_price`), pinned verbatim beside the one
/// above: a limit has no trigger, a stop's trigger is its one price (its cell is the matrix's), a
/// stop-limit or a take-profit replaced by cancel-then-new is replaced at the new trigger, and one
/// amended in place whose adapter overrides no `modify_trigger` has its trigger move refused in words
/// (decision 0244, item 8), never made a cancel-then-new.
#[test]
fn the_trigger_amend_matrix_is_pinned() {
    use AmendMode::{CancelThenNew as C, Native as N, Refused as X};
    //                        limit stop  stop_limit take_profit
    #[rustfmt::skip]
    // a blank line apart where neighbouring rows flip in separate commits, so each cherry-picks alone
    let rows: &[(&str, [AmendMode; 4])] = &[
        ("binance",     [X, C, C, C]),

        ("bybit",       [X, N, N, N]),

        ("okx",         [X, N, N, N]),
        ("aster",       [X, C, X, X]),
        ("deribit",     [X, N, X, X]),
        ("hyperliquid", [X, N, X, X]),
        ("oanda",       [X, N, X, C]),
        ("ig",          [X, N, X, X]),
        ("fxcm",        [X, N, X, X]),
        ("dukascopy",   [X, N, N, N]),

        ("ibkr",        [X, N, X, X]),
        ("ctrader",     [X, N, X, X]),
        ("polymarket",  [X, X, X, X]),
        ("alpaca",      [X, X, X, X]),
        // vike:new-venue:row ("{venue}",  [X, X, X, X]), // TODO(new-venue: {venue}): the scaffolded caps row rests no trigger kind: move with the capability row
    ];
    for (venue, want) in rows {
        for (kind, want) in AmendKind::ALL.iter().zip(want) {
            assert_eq!(trigger_amend_mode(venue, *kind), *want, "{venue} / {}", kind.word());
        }
        assert_eq!(
            trigger_amend_mode(venue, AmendKind::Stop),
            amend_mode(venue, AmendKind::Stop),
            "{venue}: a stop's trigger is its price"
        );
    }
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one pinned row per roster venue");
}

/// The rule is read against the two rows it derives from, so a capability row that moves cannot
/// leave this answer behind.
#[test]
fn the_mode_agrees_with_the_capability_rows() {
    for &venue in vike_model::VENUES {
        let capabilities = capabilities_for(venue);
        for kind in AmendKind::ALL {
            let mode = amend_mode(venue, kind);
            let placeable = capabilities.supported_order_kinds.contains(&kind.word());
            match mode {
                AmendMode::Refused => {
                    // every venue refuses a trailing amend, placed or not (the test below)
                    let trailing = matches!(
                        kind,
                        AmendKind::Trailing
                            | AmendKind::TrailingLimit
                            | AmendKind::TrailingTakeProfit
                    );
                    assert!(
                        !placeable || trailing || REFUSED_VENUES.contains(&venue),
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
                        placeable && capabilities.supports_modify,
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

/// A market-to-limit rests as the limit its remainder becomes, so its amend is the limit's. Where a row
/// lists the kind, that limit is amended in place: a cancel-then-new replacement keeps the kind
/// (`vike_execution::Engine::plan_replacement`), and a new market-to-limit would execute at market.
#[test]
fn a_market_to_limit_rests_as_its_limit_and_is_never_placed_again() {
    assert_eq!(AmendKind::of(&OrderType::MarketToLimit), Some(AmendKind::Limit));
    for &venue in vike_model::VENUES {
        if capabilities_for(venue).supported_order_kinds.contains(&"market_to_limit") {
            assert_eq!(amend_mode(venue, AmendKind::Limit), AmendMode::Native, "{venue}");
        }
    }
}

/// No venue modifies a resting trailing order: no adapter moves one in place, and a cancel-then-new would
/// restart its trail from the price at placement. The three trailing kinds (the trailing take-profit of
/// the attributes wave included) are `Refused` on every venue, whatever its row lists; an adapter that
/// proves an in-place amend changes `amend_mode` itself, not a row.
#[test]
fn a_trailing_order_is_never_modified_at_a_venue() {
    for &venue in vike_model::VENUES {
        for kind in [AmendKind::Trailing, AmendKind::TrailingLimit, AmendKind::TrailingTakeProfit] {
            assert_eq!(amend_mode(venue, kind), AmendMode::Refused, "{venue} / {}", kind.word());
        }
    }
}

/// Each trailing kind the model knows (`OrderType::is_trailing`, so a new one is covered on arrival) rests
/// as its own amend kind, is never amended, and its trigger (its activation) no modify moves, on any venue.
#[test]
fn a_trailing_kind_rests_as_its_own_kind_and_its_activation_never_moves() {
    let trailing: Vec<&OrderType> = OrderType::KNOWN.iter().filter(|t| t.is_trailing()).collect();
    assert_eq!(trailing.len(), 3, "{trailing:?}");
    for order_type in trailing {
        let kind = AmendKind::of(order_type).expect("a trailing kind rests");
        assert_eq!(kind.word(), order_type.as_str());
        assert_eq!(amend_mode_for("ibkr", order_type), Some(AmendMode::Refused), "{order_type:?}");
        for &venue in vike_model::VENUES {
            assert_eq!(amend_mode(venue, kind), AmendMode::Refused, "{venue}");
            assert_eq!(trigger_amend_mode(venue, kind), AmendMode::Refused, "{venue}");
        }
    }
}

/// Every exception names a pair that exists: a stale row (a venue renamed, a kind dropped from its
/// row) would otherwise sit there doing nothing.
#[test]
fn every_exception_row_names_a_live_pair() {
    for (venue, kind, moves) in NO_NATIVE_KIND {
        assert!(vike_model::VENUES.contains(venue), "{venue}");
        let capabilities = capabilities_for(venue);
        assert!(
            capabilities.supports_modify,
            "{venue}: listed as 'amends, but not {}'",
            kind.word()
        );
        assert!(
            capabilities.supported_order_kinds.contains(&kind.word()),
            "{venue} / {}",
            kind.word()
        );
        if *moves == Moves::Trigger {
            // a trigger row is about a kind with a trigger of its own that amends in place otherwise
            assert!(matches!(kind, AmendKind::StopLimit | AmendKind::TakeProfit), "{venue}");
            assert_eq!(amend_mode(venue, *kind), AmendMode::Native, "{venue} / {}", kind.word());
        }
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
