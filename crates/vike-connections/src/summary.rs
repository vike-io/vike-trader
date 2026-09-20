//! The credential panel's PURE derivations — what a dot means, what a count counts, and what the
//! detail pane says about a venue. Everything here is a fold over
//! [`crate::status::VenueCredStatus`] (the store-presence grid) and [`crate::view::edit_fields`]
//! (the per-venue key table); nothing here reads a file, an environment or a socket, and nothing
//! here can see a credential VALUE — `VenueCredStatus` carries three `bool`s and a venue name.
//!
//! # ⚠ Two facts, two renderings — this module holds the FIRST one only
//!
//! The panel shows two different things that both look like "status", and rendering them through
//! one glyph is the defect this module exists to make impossible:
//!
//! * **Credential PRESENCE** — "is every key this cell's form writes present and non-blank in the
//!   store, for the account being shown". That is [`TierState`], derived here, and it is what the
//!   rail's three dots mean. It says nothing about whether the venue is reachable, armed, or up.
//! * **The LIVE FEED's connection state** — `vike_model::feed_status::ConnectionState`, parsed
//!   from a bridge feed's own status string by the binary and handed to the view as a map. That is
//!   a different fact with a different producer, it is absent for every venue with no feed
//!   producer in this build, and it is rendered in the DETAIL pane's `Status` row where it can be
//!   labelled as what it is.
//!
//! A configured venue can read `Disconnected` and an unconfigured one can read `Connected` (a
//! keyless market-data feed needs no credential at all), so the two are not even correlated.
//! [`FeedFact`] is the honest rendering of the second: it carries `NoProducer` as a distinct
//! answer from `Unknown`, because "nothing in this build produces a status for this venue" and
//! "the producer has not said yet" are different sentences and only one of them is a fault.
//!
//! # ⚠ A THIRD thing that is not a fact at all: a store nobody could open
//!
//! Everything above folds `VenueCredStatus` bools, and those come from a map the loader built.
//! `vike_bridge_core::credentials::load_workspace_secrets_from_env` is documented INFALLIBLE: a
//! store that exists and cannot be opened logs `tracing::error!` and returns an EMPTY map,
//! byte-identical to an absent one. Folded blind, every cell in this panel then reads `not set`
//! and every count reads `0 set` — measured numbers about a file that was never read.
//!
//! The root `CLAUDE.md` names that failure exactly: *"A store that EXISTS and cannot be read is an
//! ERROR, not 'no credentials'. Those two must never look the same to an operator, because a
//! permissions bug wearing the 'not configured' answer looks exactly like a correct fresh install
//! while every venue drops to paper for a different reason."*
//!
//! So [`StoreHealth`] is an INPUT to every fold here, [`TierState::Unknown`] is the per-cell
//! answer and [`CredentialSummary::configured`] is an `Option` — the same
//! no-number-where-none-is-known discipline the Backend segment's badge already obeys. An ABSENT
//! store is NOT this case: its empty map is a real measurement and `0 set` is true.

use std::collections::HashMap;

use crate::status::VenueCredStatus;
use crate::view::edit_fields;
use vike_model::feed_status::ConnectionState;

pub use vike_bridge_core::credentials::StoreHealth;

/// The three credential tiers, in the order the panel renders them (and the order the rail's
/// `S D L` header names). The strings are the ones [`crate::view::edit_fields`] and
/// [`crate::status::credential_status`] both key on — `vike_model::credential_keys::
/// CREDENTIAL_TIERS`' spelling.
pub const TIERS: [&str; 3] = ["SIM", "DEMO", "LIVE"];

/// What ONE (venue, tier) cell is, as the rail's dot and the detail pane's status cell render it.
///
/// ⚠ **FOUR states, not two, and the two additions are each the point of their own defect.** A
/// hollow ring means *nothing is stored for a tier that exists*; a venue with no such tier at all
/// is a different sentence, and rendering it as the same hollow ring told an operator to go and
/// fill in a form that does not exist. The detail pane therefore spells [`Self::NotConfigurable`]
/// out in words. [`Self::Unknown`] is the fourth: a cell whose store never opened, which is not
/// `NotSet` and must not render as it — see this module's own doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierState {
    /// Every key this cell's form writes is present and non-blank in the store, for the account
    /// being shown. `VenueCredStatus`'s own bool, unmodified.
    Configured,
    /// This tier EXISTS for this venue (its form has fields) and nothing is stored for it.
    NotSet,
    /// This venue has no such tier — [`crate::view::edit_fields`] returns an empty list, which is
    /// how that table says so, and the cell offers no form.
    ///
    /// ⚠ This answer is a property of the WRITE TABLE, not of the store, so it is still correct
    /// when the store could not be opened — which is why [`tier_state`] decides it FIRST.
    NotConfigurable,
    /// The credential store EXISTS and could not be opened
    /// ([`StoreHealth::Unreadable`]), so whether this cell's keys are present was never measured.
    /// Rendering it as [`Self::NotSet`] is the defect this variant exists for.
    Unknown,
}

impl TierState {
    /// The detail pane's status cell, in words. ⚠ Never a value, and there is nothing here that
    /// could become one: this function's whole input is a four-variant enum.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TierState::Configured => "configured",
            TierState::NotSet => "not set",
            TierState::NotConfigurable => "not configurable",
            TierState::Unknown => "unknown — store unreadable",
        }
    }
}

/// This row's stored-presence bool for one tier. `SIM`/`DEMO`/`LIVE` are the only tiers; any other
/// spelling reads `false`, which is the same answer the grid gives a cell it cannot light.
#[must_use]
pub fn configured_for(row: &VenueCredStatus, tier: &str) -> bool {
    match tier {
        "SIM" => row.sim,
        "DEMO" => row.demo,
        "LIVE" => row.live,
        _ => false,
    }
}

/// [`TierState`] for one cell — the join of the READ side (`row`, which tiers are stored), the
/// WRITE side ([`edit_fields`], which tiers EXIST) and the STORE's own health. The first two
/// tables are independent and held equal by
/// `crates/vike-connections/tests/editor_key_shapes.rs`; this is the one place the panel asks them
/// the same question at once.
///
/// ⚠ **The order of the two guards is load-bearing.** `NotConfigurable` is decided FIRST because
/// it is a fact about the write table and is true whether or not any store was read — a venue with
/// no SIM tier has none on a box whose `secrets.env` is a directory. Only then does an unreadable
/// store turn the remaining cells [`TierState::Unknown`]: those are the ones whose answer would
/// otherwise have come out of a map that was never filled.
#[must_use]
pub fn tier_state(row: &VenueCredStatus, tier: &str, health: &StoreHealth) -> TierState {
    if edit_fields(&row.venue, tier).is_empty() {
        return TierState::NotConfigurable;
    }
    if !health.is_readable() {
        return TierState::Unknown;
    }
    if configured_for(row, tier) { TierState::Configured } else { TierState::NotSet }
}

/// The credentials tab's counts — what the segmented control's badge and the status line render.
///
/// ⚠ Every field is a COUNT OF ROWS THIS PANEL IS SHOWING, for the account it is showing, and
/// nothing else. `configured` counts (venue, tier) cells whose every key is present in the store;
/// it is not a count of venues, not a count of keys, and not a count of anything that is armed —
/// a `policy.venues.<venue>` ceiling can hold every one of them at paper and this number does not
/// move.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CredentialSummary {
    /// Venue rows in the grid — `vike_model::VENUES`'s length on a real box, or the fixture's length
    /// in a harness that injected a subset. Known in every state: the roster is a compiled-in
    /// list, not something the store says.
    pub venues: usize,
    /// (venue, tier) cells whose every key is present and non-blank in the store — **`None` when
    /// the store could not be OPENED**, because then nothing was counted and a `0` would be a
    /// number this panel does not have. An ABSENT store is `Some(0)`: that zero was measured.
    pub configured: Option<usize>,
    /// (venue, tier) cells that EXIST at all — the denominator `configured` is out of. Known in
    /// every state, for the same reason `venues` is: it comes from [`edit_fields`], the WRITE
    /// table, which no store can make unreadable.
    pub configurable: usize,
    /// Whether the store the bools above came from actually opened. Carried ON the summary so no
    /// renderer can fold the counts without it.
    pub health: StoreHealth,
}

/// Fold [`CredentialSummary`] over the grid the panel is rendering, for a store of this health.
///
/// ⚠ `configurable` keeps counting under an unreadable store and `configured` stops. That is not
/// an inconsistency: the denominator is the write table's answer and the numerator is the store's,
/// and only one of them went missing.
#[must_use]
pub fn credential_summary(rows: &[VenueCredStatus], health: &StoreHealth) -> CredentialSummary {
    let mut configured = 0usize;
    let mut s = CredentialSummary {
        venues: rows.len(),
        health: health.clone(),
        ..CredentialSummary::default()
    };
    for row in rows {
        for tier in TIERS {
            match tier_state(row, tier, health) {
                TierState::Configured => {
                    configured += 1;
                    s.configurable += 1;
                }
                TierState::NotSet | TierState::Unknown => s.configurable += 1,
                TierState::NotConfigurable => {}
            }
        }
    }
    s.configured = health.is_readable().then_some(configured);
    s
}

/// What the panel can honestly say about a venue's LIVE FEED — the second of the two facts this
/// module's doc separates.
///
/// ⚠ [`Self::NoProducer`] is a distinct variant from `State(ConnectionState::Unknown)` ON PURPOSE.
/// The old grid collapsed them: a venue absent from the status map rendered the `Unknown` the
/// `Default` impl produces, which reads as "we asked and got no answer" when the truth is
/// "nothing in this build produces a status for this venue at all" — for most of the roster
/// (every exec-only venue, every FX venue) that is the permanent and correct state, not a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedFact {
    /// A feed producer for this venue is wired into this build and reports this state.
    State(ConnectionState),
    /// No feed producer for this venue exists in this build — the venue is absent from the status
    /// map the binary assembles, so there is no state to report and none is invented.
    NoProducer,
}

impl FeedFact {
    /// Read one venue out of the status map the binary hands the view. Absence is
    /// [`Self::NoProducer`]; it is never folded into a `ConnectionState`.
    #[must_use]
    pub fn of(venue: &str, live: &HashMap<String, ConnectionState>) -> Self {
        match live.get(venue) {
            Some(state) => FeedFact::State(*state),
            None => FeedFact::NoProducer,
        }
    }

    /// Whether this venue has a feed producer at all — what the detail pane's
    /// `feed producer in this build` badge is derived from, so the badge is a statement about this
    /// BUILD rather than about the venue's health.
    ///
    /// ⚠ **`true` for EVERY reported state, `Disconnected` included** — a producer that is down is
    /// still a producer. That is why the badge may not be worded as an activity: `crate::view`'s
    /// `venue_detail` carries the argument at the call, and it used to read `Streaming feed`
    /// directly above a `Status (live feed)  Disconnected` row.
    #[must_use]
    pub fn has_producer(self) -> bool {
        matches!(self, FeedFact::State(_))
    }

    /// The detail pane's `Status` cell, in words — the producer's own state, or the sentence that
    /// says there is no producer. Never "Unknown" for a venue that was never asked.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FeedFact::State(ConnectionState::Connected) => "Connected",
            FeedFact::State(ConnectionState::Connecting) => "Connecting",
            FeedFact::State(ConnectionState::Error) => "Error",
            FeedFact::State(ConnectionState::Disconnected) => "Disconnected",
            FeedFact::State(ConnectionState::Unknown) => "Unknown (producer has not reported)",
            FeedFact::NoProducer => "no feed producer in this build",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            unreadable.configurable, empty.configurable,
            "binance's three + dukascopy's one"
        );
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
}
