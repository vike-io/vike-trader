//! Unit tests for `summary`'s pure folds: tier states, the credential summary, the feed fact, and their map rows.

use super::*;
use crate::grid::AccountManagerTierCellAccount;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::theme::ThemePaletteId;

/// An account row holding the required fields `held` and lacking the required fields `missing`.
fn account(id: i64, held: &[&'static str], missing: &[&'static str]) -> AccountManagerTierCell {
    AccountManagerTierCell::Account(AccountManagerTierCellAccount {
        id,
        active: true,
        held_required: held.to_vec(),
        missing_required: missing.to_vec(),
    })
}

fn row(
    venue: &str,
    paper: AccountManagerTierCell,
    demo: AccountManagerTierCell,
    live: AccountManagerTierCell,
) -> AccountManagerGridRow {
    AccountManagerGridRow { venue: venue.to_string(), paper, demo, live }
}

/// The states are genuinely distinct: a cell with no account row (and the paper tier, which reads
/// no credential) is not configurable, an account holding none or only some of its required
/// fields is not set, and a complete one is configured. Reddens on `tier_state` collapsing the
/// first two — which is the exact rendering the detail pane exists to stop.
#[test]
fn a_cell_with_no_account_is_not_the_same_as_one_that_is_unset() {
    let ok = StoreHealth::Readable;
    let r = row(
        "binance",
        account(1, &["API_KEY", "API_SECRET"], &[]),
        AccountManagerTierCell::NoAccount,
        account(2, &["API_KEY"], &["API_SECRET"]),
    );
    // ⚠ A COMPLETE paper row still reads NotConfigurable: the paper simulator reads no credential,
    // so a dot there would be a measurement of something nothing uses.
    assert_eq!(
        tier_state(&r, AccountTier::Paper, &ok),
        AccountManagerTierCellState::NotConfigurable
    );
    assert_eq!(
        tier_state(&r, AccountTier::Demo, &ok),
        AccountManagerTierCellState::NotConfigurable
    );
    assert_eq!(
        tier_state(&r, AccountTier::Live, &ok),
        AccountManagerTierCellState::NotSet,
        "a PARTIAL set is unset"
    );

    let set = row(
        "binance",
        AccountManagerTierCell::NoAccount,
        account(3, &[], &["API_KEY", "API_SECRET"]),
        account(4, &["API_KEY", "API_SECRET"], &[]),
    );
    assert_eq!(
        tier_state(&set, AccountTier::Demo, &ok),
        AccountManagerTierCellState::NotSet,
        "an EMPTY set"
    );
    assert_eq!(tier_state(&set, AccountTier::Live, &ok), AccountManagerTierCellState::Configured);

    let ambiguous = row(
        "dukascopy",
        AccountManagerTierCell::NoAccount,
        AccountManagerTierCell::Ambiguous(vec![5, 6]),
        AccountManagerTierCell::NoAccount,
    );
    assert_eq!(
        tier_state(&ambiguous, AccountTier::Demo, &ok),
        AccountManagerTierCellState::NotConfigurable
    );
}

/// ⚠ **A store that could not be OPENED yields UNKNOWN cells, never `not set` and never "no
/// account"** — and the paper tier keeps saying it reads no credential, because that answer comes
/// from the tier and no store failure can take it away.
///
/// Reddens on `tier_state` ignoring its health argument, which is the state this panel was in:
/// every cell would read as measured about a file nothing ever read.
#[test]
fn an_unreadable_store_makes_a_cell_unknown_rather_than_unset() {
    let bad = StoreHealth::Unreadable("permission denied".into());
    // An unopenable store answers no rows — exactly the grid an unconfigured box produces. The
    // cells cannot tell the two apart; that is the whole reason health is a separate input.
    let blank = AccountManagerGridRow::absent("binance");
    for tier in [AccountTier::Demo, AccountTier::Live] {
        assert_eq!(tier_state(&blank, tier, &bad), AccountManagerTierCellState::Unknown, "{tier}");
        assert_eq!(
            tier_state(&blank, tier, &StoreHealth::Readable),
            AccountManagerTierCellState::NotConfigurable,
            "{tier}: the SAME row reads `no account` when the store answered"
        );
    }
    assert_eq!(
        tier_state(&blank, AccountTier::Paper, &bad),
        AccountManagerTierCellState::NotConfigurable
    );
    assert_ne!(
        AccountManagerTierCellState::Unknown.label(),
        AccountManagerTierCellState::NotSet.label()
    );
}

/// The summary's denominator is the cells a credential can be attached to — the account rows —
/// not `3 * venues`. Reddens on a count that multiplies the roster by the tier list, which is how
/// a panel reports "1 of 42 set" on a box that is fully configured.
#[test]
fn the_summary_counts_cells_that_exist_not_rows_times_tiers() {
    let full = || account(1, &["API_KEY", "API_SECRET"], &[]);
    let rows = vec![
        row("binance", full(), full(), account(2, &["API_KEY"], &["API_SECRET"])),
        row(
            "dukascopy",
            AccountManagerTierCell::NoAccount,
            account(3, &["LOGIN", "PASSWORD"], &[]),
            AccountManagerTierCell::NoAccount,
        ),
    ];
    let s = credential_summary(&rows, &StoreHealth::Readable);
    assert_eq!(s.venues, 2);
    assert_eq!(s.configurable, 3, "binance's demo and live rows plus dukascopy's demo row");
    assert_eq!(s.configured, Some(2), "binance DEMO, dukascopy DEMO — paper never counts");
}

/// An empty grid counts nothing rather than dividing by a roster it was not given.
#[test]
fn an_empty_grid_summarises_to_zero() {
    assert_eq!(
        credential_summary(&[], &StoreHealth::Readable),
        AccountManagerCredentialSummary {
            configured: Some(0),
            ..AccountManagerCredentialSummary::default()
        }
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
    let rows =
        vec![AccountManagerGridRow::absent("binance"), AccountManagerGridRow::absent("dukascopy")];

    let empty = credential_summary(&rows, &StoreHealth::Readable);
    assert_eq!(empty.configured, Some(0), "an ABSENT store genuinely holds nothing");
    assert_eq!(empty.configurable, 0, "…and no account row stands anywhere");

    let unreadable =
        credential_summary(&rows, &StoreHealth::Unreadable("permission denied".into()));
    assert_eq!(unreadable.configured, None, "…and an UNOPENED one has no number to give");

    // The roster is not the store's to withhold, and a cell whose row was never read stays in the
    // denominator: each venue's demo and live cells, never its paper one.
    assert_eq!(unreadable.venues, empty.venues);
    assert_eq!(unreadable.configurable, 4, "two venues, demo and live each");
    assert_ne!(unreadable, empty, "the two summaries must not compare equal");
}

/// ⚠ A venue with NO producer is not `Unknown`, and the two render different sentences.
/// Reddens on `AccountManagerFeedFact::of` folding absence into `DataClientConnectionState::default()`, which is what
/// the grid this replaces did.
#[test]
fn a_venue_with_no_producer_is_distinguishable_from_one_that_has_not_reported() {
    let mut live = HashMap::new();
    live.insert("binance".to_string(), DataClientConnectionState::Connected);
    live.insert("bybit".to_string(), DataClientConnectionState::Unknown);

    assert_eq!(
        AccountManagerFeedFact::of("binance", &live),
        AccountManagerFeedFact::State(DataClientConnectionState::Connected)
    );
    assert!(AccountManagerFeedFact::of("binance", &live).has_producer());

    assert_eq!(
        AccountManagerFeedFact::of("bybit", &live),
        AccountManagerFeedFact::State(DataClientConnectionState::Unknown)
    );
    assert!(AccountManagerFeedFact::of("bybit", &live).has_producer());

    assert_eq!(AccountManagerFeedFact::of("deribit", &live), AccountManagerFeedFact::NoProducer);
    assert!(!AccountManagerFeedFact::of("deribit", &live).has_producer());
    assert_ne!(
        AccountManagerFeedFact::of("deribit", &live).label(),
        AccountManagerFeedFact::of("bybit", &live).label()
    );
}

/// Which colours and words a feed fact wears, pinned to what the status strip's dot and the detail pane's
/// `Status` cell painted and said before they moved to `ui-theme.toml`'s `connection` map: connected the
/// status green, connecting the status amber, an error the status red, and everything else — a feed that
/// is down, one that has not reported, a venue with no producer — the status grey; on every theme. Changing
/// a row of the table changes the dot and the cell, and this test is what says so by name.
#[test]
fn a_feed_fact_wears_the_colour_and_the_words_the_connection_map_gives_it() {
    for id in ThemePaletteId::ALL {
        let t = ThemeTokens::from_appearance(&ThemeAppearance {
            theme: id,
            ..ThemeAppearance::default()
        });
        for (fact, colour, words) in [
            (
                AccountManagerFeedFact::State(DataClientConnectionState::Connected),
                ThemeStatus::Ok,
                "Connected",
            ),
            (
                AccountManagerFeedFact::State(DataClientConnectionState::Connecting),
                ThemeStatus::Warning,
                "Connecting",
            ),
            (
                AccountManagerFeedFact::State(DataClientConnectionState::Disconnected),
                ThemeStatus::Muted,
                "Disconnected",
            ),
            (
                AccountManagerFeedFact::State(DataClientConnectionState::Error),
                ThemeStatus::Error,
                "Error",
            ),
            (
                AccountManagerFeedFact::State(DataClientConnectionState::Unknown),
                ThemeStatus::Muted,
                "Unknown (producer has not reported)",
            ),
            (
                AccountManagerFeedFact::NoProducer,
                ThemeStatus::Muted,
                "no feed producer in this build",
            ),
        ] {
            assert_eq!(fact.row().colour.resolve(&t), colour.color(), "{id:?} {fact:?}: colour");
            assert_eq!(fact.label(), words, "{fact:?}: the words");
        }
    }
}

/// The map is the Gateway screen's own: every feed fact reads the row of its own name, no two share
/// one, and no row of the `connection` map is left without a fact.
#[test]
fn every_feed_fact_has_its_own_row_and_every_row_its_fact() {
    let all = [
        (AccountManagerFeedFact::State(DataClientConnectionState::Connected), "CONNECTED"),
        (AccountManagerFeedFact::State(DataClientConnectionState::Connecting), "CONNECTING"),
        (AccountManagerFeedFact::State(DataClientConnectionState::Disconnected), "DISCONNECTED"),
        (AccountManagerFeedFact::State(DataClientConnectionState::Error), "ERROR"),
        (AccountManagerFeedFact::State(DataClientConnectionState::Unknown), "UNKNOWN"),
        (AccountManagerFeedFact::NoProducer, "NO_PRODUCER"),
    ];
    for (fact, key) in all {
        assert_eq!(fact.row().key, key, "{fact:?} reads another row");
        if let AccountManagerFeedFact::State(state) = fact {
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
    for id in ThemePaletteId::ALL {
        let t = ThemeTokens::from_appearance(&ThemeAppearance {
            theme: id,
            ..ThemeAppearance::default()
        });
        for (state, colour, dimmed, words) in [
            (AccountManagerTierCellState::Configured, ThemeStatus::Ok, None, "configured"),
            (AccountManagerTierCellState::NotSet, ThemeStatus::Muted, None, "not set"),
            (
                AccountManagerTierCellState::NotConfigurable,
                ThemeStatus::Muted,
                Some(true),
                "not configurable",
            ),
            (
                AccountManagerTierCellState::Unknown,
                ThemeStatus::Warning,
                None,
                "unknown — store unreadable",
            ),
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
        (AccountManagerTierCellState::Configured, "CONFIGURED"),
        (AccountManagerTierCellState::NotSet, "NOT_SET"),
        (AccountManagerTierCellState::NotConfigurable, "NOT_CONFIGURABLE"),
        (AccountManagerTierCellState::Unknown, "UNKNOWN"),
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
