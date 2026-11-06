//! The credential panel's PURE derivations — what a dot means, what a count counts, and what the
//! detail pane says about a venue. Everything here is a fold over
//! [`crate::status::VenueCredStatus`] (each tier's account row and which of its REQUIRED fields the
//! store holds) and [`crate::keys`] (which venues have a form at all); nothing here reads a file,
//! an environment or a socket, and nothing here can see a credential VALUE — `VenueCredStatus`
//! carries account ids and field WORDS.
//!
//! # ⚠ Two facts, two renderings — this module holds the FIRST one only
//!
//! The panel shows two different things that both look like "status", and rendering them through
//! one glyph is the defect this module exists to make impossible:
//!
//! * **Credential PRESENCE** — "does the account row of this (venue, label, tier) hold every field
//!   the venue's loader requires". That is [`TierState`], derived here, and it is what the rail's
//!   three dots mean. It says nothing about whether the venue is reachable, armed, or up.
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
//! Everything above folds the account rows and the credential book a root read. A store that
//! exists and cannot be opened answers no rows and no fields — byte-identical to an empty store.
//! Folded blind, every cell in this panel then reads as having no account and every count reads
//! `0 set` — measured numbers about a file that was never read.
//!
//! The root `CLAUDE.md` names that failure exactly: *"A store that EXISTS and cannot be read is an
//! ERROR, not 'no credentials'. Those two must never look the same to an operator, because a
//! permissions bug wearing the 'not configured' answer looks exactly like a correct fresh install
//! while every venue drops to paper for a different reason."*
//!
//! So [`StoreHealth`] is an INPUT to every fold here, [`TierState::Unknown`] is the per-cell
//! answer and [`CredentialSummary::configured`] is an `Option` — the same
//! no-number-where-none-is-known discipline the Backend segment's badge already obeys. An ABSENT
//! store is NOT this case: its empty answer is a real measurement and `0 set` is true.

use std::collections::HashMap;

use crate::keys::has_account_form;
use crate::status::{Presence, TierCell, VenueCredStatus};
use vike_model::AccountTier;
use vike_model::feed_status::ConnectionState;
use vike_secrets::credentials::StoreHealth;
use vike_ui_theme::maps::{self, MapRow};

/// The three tiers, in the order the panel renders them (and the order the rail's `P D L` header
/// names) — `vike_model::AccountTier`, the one tier vocabulary. The chip and header text is the tier's
/// own word (`AccountTier::as_str`), never a second spelling.
pub const TIERS: [AccountTier; 3] = AccountTier::ALL;

/// What ONE (venue, tier) cell is, as the rail's dot and the detail pane's status cell render it.
///
/// ⚠ **FOUR states, not two, and the two additions are each the point of their own defect.** A
/// hollow ring means *the account exists and its required fields are not all stored*; a cell where
/// no credential can be attached at all is a different sentence, and rendering it as the same
/// hollow ring told an operator to go and fill in a form that does not exist. The detail pane
/// therefore spells [`Self::NotConfigurable`] out in words. [`Self::Unknown`] is the fourth: a cell
/// whose store never opened, which is not `NotSet` and must not render as it — see this module's
/// own doc.
///
/// A PARTIAL set (some required fields held, some not) is `NotSet` here — it cannot arm — and the
/// detail pane names which fields are missing ([`crate::status::Presence`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierState {
    /// The account row of this cell holds every REQUIRED account field, non-blank.
    Configured,
    /// The account row EXISTS and its required fields are not all stored (none, or some).
    NotSet,
    /// No credential can be attached here: the PAPER tier (the simulator reads none), a venue with
    /// no account form, no account row at this (venue, label, tier), or more than one row with no
    /// single active one. The cell offers no form.
    ///
    /// ⚠ The paper and no-form answers are properties of the TIER and the SCHEMA, not of the store,
    /// so they are still correct when the store could not be opened — which is why [`tier_state`]
    /// decides them FIRST.
    NotConfigurable,
    /// The credential store EXISTS and could not be opened ([`StoreHealth::Unreadable`]), so
    /// whether this cell has an account row and which fields it holds was never measured.
    /// Rendering it as [`Self::NotSet`] or [`Self::NotConfigurable`] is the defect this variant
    /// exists for.
    Unknown,
}

impl TierState {
    /// This state's row of `ui-theme.toml`'s `tier_state` map: `colour` the mark's colour, `flag` whether the
    /// mark is dimmed (by `value::connections::NOT_CONFIGURABLE_DIM`), `word` the detail pane's words.
    /// Exhaustive: a state without a row does not compile, and
    /// `every_tier_state_has_its_own_row_and_every_row_its_state` holds the other direction. The mark's
    /// GLYPH is not a row field and stays with `crate::view`.
    #[must_use]
    pub(crate) fn row(self) -> &'static MapRow {
        match self {
            TierState::Configured => &maps::tier_state::CONFIGURED,
            TierState::NotSet => &maps::tier_state::NOT_SET,
            TierState::NotConfigurable => &maps::tier_state::NOT_CONFIGURABLE,
            TierState::Unknown => &maps::tier_state::UNKNOWN,
        }
    }

    /// The detail pane's status cell, in words: its row's `word`. ⚠ Never a value, and there is nothing
    /// here that could become one: this function's whole input is a four-variant enum.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.row().word.unwrap_or("")
    }
}

/// [`TierState`] for one cell — the join of the TIER (paper reads no credential), the SCHEMA
/// (`crate::keys::has_account_form`), the STORE's own health and the cell's account row.
///
/// ⚠ **The order of the guards is load-bearing.** `NotConfigurable` for the paper tier and for a
/// venue with no account form is decided FIRST because those are facts about the tier and the
/// schema, true whether or not any store was read. Only then does an unreadable store turn the
/// remaining cells [`TierState::Unknown`]: those are the ones whose answer — which row, which
/// fields — would otherwise have come out of a read that never happened. Last, the row: none (or
/// an ambiguous pair) is `NotConfigurable`, and a row is `Configured` exactly when it holds every
/// REQUIRED account field.
#[must_use]
pub fn tier_state(row: &VenueCredStatus, tier: AccountTier, health: &StoreHealth) -> TierState {
    if tier == AccountTier::Paper || !has_account_form(&row.venue) {
        return TierState::NotConfigurable;
    }
    if !health.is_readable() {
        return TierState::Unknown;
    }
    match row.cell(tier) {
        TierCell::NoAccount | TierCell::Ambiguous(_) => TierState::NotConfigurable,
        TierCell::Account(a) if a.presence() == Presence::Complete => TierState::Configured,
        TierCell::Account(_) => TierState::NotSet,
    }
}

/// The credentials tab's counts — what the segmented control's badge and the status line render.
///
/// ⚠ Every field is a COUNT OF CELLS THIS PANEL IS SHOWING, for the account it is showing, and
/// nothing else. `configured` counts (venue, tier) cells whose account row holds every required
/// field; it is not a count of venues, not a count of keys, and not a count of anything that is
/// armed — an inactive account row is counted like an active one and this number does not move.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CredentialSummary {
    /// Venue rows in the grid — `vike_model::VENUES`'s length on a real box, or the fixture's length
    /// in a harness that injected a subset. Known in every state: the roster is a compiled-in
    /// list, not something the store says.
    pub venues: usize,
    /// (venue, tier) cells whose account row holds every required field — **`None` when the store
    /// could not be OPENED**, because then nothing was counted and a `0` would be a number this
    /// panel does not have. An ABSENT store is `Some(0)`: that zero was measured.
    pub configured: Option<usize>,
    /// (venue, tier) cells a credential can be attached to — the denominator `configured` is out
    /// of: the cells with an account row, plus, under an unreadable store, every demo/live cell of a
    /// venue with a form (whether a row stands there was not measured, so none is excluded).
    pub configurable: usize,
    /// Whether the store the bools above came from actually opened. Carried ON the summary so no
    /// renderer can fold the counts without it.
    pub health: StoreHealth,
}

/// Fold [`CredentialSummary`] over the grid the panel is rendering, for a store of this health.
///
/// ⚠ `configurable` keeps counting under an unreadable store and `configured` stops. That is not
/// an inconsistency: a cell whose row was never read may hold an account, so it stays in the
/// denominator, while the numerator is a claim about what the store holds and is withheld.
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

    /// This fact's row of `ui-theme.toml`'s `connection` map: `colour` the dot's and the `Status` cell's
    /// colour, `word` the cell's words. The status strip's dot reads it too
    /// (`crates/vike-app-core/src/ui/status_dot.rs`'s `dot_color_for`), so the strip and this tool cannot
    /// disagree about a feed. Exhaustive: a state without a row does not compile, and
    /// `every_feed_fact_has_its_own_row_and_every_row_its_fact` holds the other direction.
    #[must_use]
    pub fn row(self) -> &'static MapRow {
        match self {
            FeedFact::State(ConnectionState::Connected) => &maps::connection::CONNECTED,
            FeedFact::State(ConnectionState::Connecting) => &maps::connection::CONNECTING,
            FeedFact::State(ConnectionState::Disconnected) => &maps::connection::DISCONNECTED,
            FeedFact::State(ConnectionState::Error) => &maps::connection::ERROR,
            FeedFact::State(ConnectionState::Unknown) => &maps::connection::UNKNOWN,
            FeedFact::NoProducer => &maps::connection::NO_PRODUCER,
        }
    }

    /// The detail pane's `Status` cell, in words — the producer's own state, or the sentence that
    /// says there is no producer. Never "Unknown" for a venue that was never asked. Its row's `word`.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.row().word.unwrap_or("")
    }
}

#[path = "summary_tests.rs"]
#[cfg(test)]
mod summary_tests;
