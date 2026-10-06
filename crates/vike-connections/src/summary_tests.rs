//! Unit tests for `summary`'s pure folds: tier states, the credential summary, the feed fact, and their map rows.

use super::*;
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::theme::ThemeId;

fn row(venue: &str, sim: bool, demo: bool, live: bool) -> VenueCredStatus {
    VenueCredStatus { venue: venue.to_string(), sim, demo, live }
}

/// The three states are genuinely three: dukascopy's SIM has no tier, its DEMO can be unset,
/// and a stored DEMO is configured. Reddens on `tier_state` collapsing the first two — which
/// is the exact rendering the detail pane exists to stop.
#[test]
fn a_tier_that_does_not_exist_is_not_the_same_as_one_that_is_unset() {
    let ok = StoreHealth::Readable;
    let unset = row("dukascopy", false, false, false);
    assert_eq!(tier_state(&unset, "SIM", &ok), TierState::NotConfigurable);
    assert_eq!(tier_state(&unset, "LIVE", &ok), TierState::NotConfigurable);
    assert_eq!(tier_state(&unset, "DEMO", &ok), TierState::NotSet);

    let set = row("dukascopy", false, true, false);
    assert_eq!(tier_state(&set, "DEMO", &ok), TierState::Configured);
    // ⚠ A stored bool for a tier that does not EXIST still reads NotConfigurable: the write
    // table is the authority on whether a form can be offered, and a dot the operator cannot
    // act on is the thing being removed.
    let impossible = row("dukascopy", true, false, true);
    assert_eq!(tier_state(&impossible, "SIM", &ok), TierState::NotConfigurable);
}

/// ⚠ **A store that could not be OPENED yields UNKNOWN cells, never `not set`** — and the
/// tier that does not exist keeps saying so, because that answer comes from the write table
/// and no store failure can take it away.
///
/// Reddens on `tier_state` ignoring its health argument, which is the state this panel was in:
/// every cell would read `not set` about a file nothing ever read.
#[test]
fn an_unreadable_store_makes_a_cell_unknown_rather_than_unset() {
    let bad = StoreHealth::Unreadable("permission denied".into());
    // The loader hands back an EMPTY map when the store will not open, so every bool is false
    // — exactly the row an unconfigured box produces. The bools cannot tell the two apart;
    // that is the whole reason health is a separate input.
    let blank = row("binance", false, false, false);
    for tier in TIERS {
        assert_eq!(tier_state(&blank, tier, &bad), TierState::Unknown, "{tier}");
        assert_eq!(
            tier_state(&blank, tier, &StoreHealth::Readable),
            TierState::NotSet,
            "{tier}: the SAME row reads `not set` when the store answered"
        );
    }
    let duka = row("dukascopy", false, false, false);
    assert_eq!(tier_state(&duka, "SIM", &bad), TierState::NotConfigurable);
    assert_ne!(TierState::Unknown.label(), TierState::NotSet.label());
}

/// The summary's denominator is CONFIGURABLE cells, not `3 * venues` — dukascopy contributes
/// one, binance three. Reddens on a count that multiplies the roster by the tier list, which
/// is how a panel reports "1 of 42 set" on a box that is fully configured.
#[test]
fn the_summary_counts_cells_that_exist_not_rows_times_tiers() {
    let rows = vec![row("binance", true, false, true), row("dukascopy", false, true, false)];
    let s = credential_summary(&rows, &StoreHealth::Readable);
    assert_eq!(s.venues, 2);
    assert_eq!(s.configurable, 4, "binance's three plus dukascopy's one DEMO");
    assert_eq!(s.configured, Some(3), "binance SIM + LIVE, dukascopy DEMO");
}

/// An empty grid counts nothing rather than dividing by a roster it was not given.
#[test]
fn an_empty_grid_summarises_to_zero() {
    assert_eq!(
        credential_summary(&[], &StoreHealth::Readable),
        CredentialSummary { configured: Some(0), ..CredentialSummary::default() }
    );
}

/// ⚠ **THE FINDING THIS MODULE'S DOC IS ABOUT.** A box with NOTHING configured and a box whose
/// store could not be opened hand this fold the SAME all-false rows. The first has a measured
/// zero; the second has no number at all, and a UI that prints `0 set` for it states a
/// measurement it never made — a permissions bug wearing the not-configured answer, which the
/// root `CLAUDE.md`'s "Credentials & the live gate" forbids by name.
///
/// Reddens on `configured` going back to a plain `usize`, which is the shape that makes the
/// two indistinguishable downstream no matter how carefully the renderer is written.
#[test]
fn an_unreadable_store_has_no_count_while_an_empty_one_has_a_measured_zero() {
    let rows = vec![row("binance", false, false, false), row("dukascopy", false, false, false)];

    let empty = credential_summary(&rows, &StoreHealth::Readable);
    assert_eq!(empty.configured, Some(0), "an ABSENT store genuinely holds nothing");

    let unreadable =
        credential_summary(&rows, &StoreHealth::Unreadable("permission denied".into()));
    assert_eq!(unreadable.configured, None, "…and an UNOPENED one has no number to give");

    // Everything the write table knows survives: the roster and the denominator are not the
    // store's to withhold.
    assert_eq!(unreadable.venues, empty.venues);
    assert_eq!(unreadable.configurable, empty.configurable, "binance's three + dukascopy's one");
    assert_eq!(unreadable.configurable, 4);
    assert_ne!(unreadable, empty, "the two summaries must not compare equal");
}

/// ⚠ A venue with NO producer is not `Unknown`, and the two render different sentences.
/// Reddens on `FeedFact::of` folding absence into `ConnectionState::default()`, which is what
/// the grid this replaces did.
#[test]
fn a_venue_with_no_producer_is_distinguishable_from_one_that_has_not_reported() {
    let mut live = HashMap::new();
    live.insert("binance".to_string(), ConnectionState::Connected);
    live.insert("bybit".to_string(), ConnectionState::Unknown);

    assert_eq!(FeedFact::of("binance", &live), FeedFact::State(ConnectionState::Connected));
    assert!(FeedFact::of("binance", &live).has_producer());

    assert_eq!(FeedFact::of("bybit", &live), FeedFact::State(ConnectionState::Unknown));
    assert!(FeedFact::of("bybit", &live).has_producer());

    assert_eq!(FeedFact::of("deribit", &live), FeedFact::NoProducer);
    assert!(!FeedFact::of("deribit", &live).has_producer());
    assert_ne!(FeedFact::of("deribit", &live).label(), FeedFact::of("bybit", &live).label());
}

/// Which colours and words a feed fact wears, pinned to what the status strip's dot and the detail pane's
/// `Status` cell painted and said before they moved to `ui-theme.toml`'s `connection` map: connected the
/// status green, connecting the status amber, an error the status red, and everything else — a feed that
/// is down, one that has not reported, a venue with no producer — the status grey; on every theme. Changing
/// a row of the table changes the dot and the cell, and this test is what says so by name.
#[test]
fn a_feed_fact_wears_the_colour_and_the_words_the_connection_map_gives_it() {
    use ConnectionState as C;
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        for (fact, colour, words) in [
            (FeedFact::State(C::Connected), Status::Ok, "Connected"),
            (FeedFact::State(C::Connecting), Status::Warning, "Connecting"),
            (FeedFact::State(C::Disconnected), Status::Muted, "Disconnected"),
            (FeedFact::State(C::Error), Status::Error, "Error"),
            (FeedFact::State(C::Unknown), Status::Muted, "Unknown (producer has not reported)"),
            (FeedFact::NoProducer, Status::Muted, "no feed producer in this build"),
        ] {
            assert_eq!(fact.row().colour.resolve(&t), colour.color(), "{id:?} {fact:?}: colour");
            assert_eq!(fact.label(), words, "{fact:?}: the words");
        }
    }
}

/// The map is the Connections tool's own: every feed fact reads the row of its own name, no two share
/// one, and no row of the `connection` map is left without a fact.
#[test]
fn every_feed_fact_has_its_own_row_and_every_row_its_fact() {
    use ConnectionState as C;
    let all = [
        (FeedFact::State(C::Connected), "CONNECTED"),
        (FeedFact::State(C::Connecting), "CONNECTING"),
        (FeedFact::State(C::Disconnected), "DISCONNECTED"),
        (FeedFact::State(C::Error), "ERROR"),
        (FeedFact::State(C::Unknown), "UNKNOWN"),
        (FeedFact::NoProducer, "NO_PRODUCER"),
    ];
    for (fact, key) in all {
        assert_eq!(fact.row().key, key, "{fact:?} reads another row");
        if let FeedFact::State(state) = fact {
            assert_eq!(
                key,
                format!("{state:?}").to_uppercase(),
                "{fact:?}: the row is named for the state"
            );
        }
        assert_eq!(
            all.iter().filter(|(other, _)| other.row() == fact.row()).count(),
            1,
            "{fact:?}'s row is shared"
        );
    }
    for row in maps::connection::ALL {
        assert!(all.iter().any(|(fact, _)| fact.row() == *row), "{} is no feed fact", row.key);
    }
}

/// Which colours and words a credential tier wears, pinned to what the rail's dot, the detail pane's tier
/// row and the legend painted before they moved to `ui-theme.toml`'s `tier_state` map: configured the
/// status green, not set the status grey, a tier that does not exist the same grey DIMMED (the row's
/// `flag`; the dimming is `crate::view`'s), and a store nobody could open the status amber; on every theme.
#[test]
fn a_tier_wears_the_colour_and_the_words_the_tier_state_map_gives_it() {
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        for (state, colour, dimmed, words) in [
            (TierState::Configured, Status::Ok, None, "configured"),
            (TierState::NotSet, Status::Muted, None, "not set"),
            (TierState::NotConfigurable, Status::Muted, Some(true), "not configurable"),
            (TierState::Unknown, Status::Warning, None, "unknown — store unreadable"),
        ] {
            assert_eq!(state.row().colour.resolve(&t), colour.color(), "{id:?} {state:?}: colour");
            assert_eq!(state.row().flag, dimmed, "{id:?} {state:?}: dimmed");
            assert_eq!(state.label(), words, "{state:?}: the words");
        }
    }
}

/// The map is the credential panel's own: every tier state reads the row of its own name, no two share
/// one, and no row of the `tier_state` map is left without a state.
#[test]
fn every_tier_state_has_its_own_row_and_every_row_its_state() {
    let all = [
        (TierState::Configured, "CONFIGURED"),
        (TierState::NotSet, "NOT_SET"),
        (TierState::NotConfigurable, "NOT_CONFIGURABLE"),
        (TierState::Unknown, "UNKNOWN"),
    ];
    for (state, key) in all {
        assert_eq!(state.row().key, key, "{state:?} reads another row");
        assert_eq!(
            all.iter().filter(|(other, _)| other.row() == state.row()).count(),
            1,
            "{state:?}'s row is shared"
        );
    }
    for row in maps::tier_state::ALL {
        assert!(all.iter().any(|(state, _)| state.row() == *row), "{} is no tier state", row.key);
    }
}
