//! The PURE read side: for each roster venue and each tier, WHICH ACCOUNT ROW a cell is and which of
//! its credential FIELDS the store holds.
//!
//! # A cell is an account ROW (decision 0156 item 6)
//!
//! The tier is written in ONE place, `account.tier`, and a credential belongs to its ACCOUNT — the
//! store's `credential.account_id` + `field`. So a (venue, tier) cell for the account label the strip
//! selected is "the account row of `(venue, label, tier)`" ([`TierCell`]):
//!
//! * [`TierCell::Account`] — that row exists; [`AccountCell`] carries its id, whether it is
//!   active, and which of the venue's REQUIRED account fields
//!   (`vike_venues::credential_schema::schema_for(venue).required(FieldOwner::Account)`) it holds —
//!   field words only, never a value;
//! * [`TierCell::NoAccount`] — no row: no credential can be attached, because a credential write
//!   never creates an account (`vike_secrets::save_owned_credentials` refuses one);
//! * [`TierCell::Ambiguous`] — more than one row answers and none is the one active row, so a form
//!   would be a guess (a store older than the labelled-row rule: give each a label).
//!
//! ⚠ **No name is read, composed or parsed to decide any of this.** The data is the three store
//! reads a composition root performs — `vike_secrets::CredentialStore::accounts`, `account_keys`
//! and `credential_book` — folded by [`AccountGrids`]. The one use of a NAME left is the field
//! REMOVE's (a soft retire is addressed by the row's stored name, taken from `account_keys`'
//! listing — [`HeldFields::stored_name`]).
//!
//! # ⚠ The ACCOUNT dimension
//!
//! [`AccountGrids`] carries one grid per account LABEL the store's rows carry, plus the default
//! (unlabelled) account's; the view renders one at a time, picked in the strip.

use vike_model::AccountTier;

mod accounts;

pub use accounts::{AccountGrids, HeldFields};

/// **How much of one account row's REQUIRED credential the store holds.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// Every required account field is held (non-blank).
    Complete,
    /// Some required fields are held and some are not — a half-written set.
    Partial,
    /// No required field is held.
    Empty,
}

/// **One account row as a cell sees it** — identity and field PRESENCE, never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountCell {
    /// `account.id` — the identity a Save addresses (`CredentialOwner::Account`).
    pub id: i64,
    /// `account.active`. An inactive row still owns its credentials and may be edited; the arming
    /// reader treats it as absent.
    pub active: bool,
    /// The venue's REQUIRED account fields this row holds, in schema order. Field words only.
    pub held_required: Vec<&'static str>,
    /// The venue's REQUIRED account fields this row does NOT hold, in schema order.
    pub missing_required: Vec<&'static str>,
}

impl AccountCell {
    /// Complete when nothing required is missing, partial when some required fields are held and
    /// some are not, empty otherwise.
    #[must_use]
    pub fn presence(&self) -> Presence {
        match (self.held_required.is_empty(), self.missing_required.is_empty()) {
            (_, true) => Presence::Complete,
            (false, false) => Presence::Partial,
            (true, false) => Presence::Empty,
        }
    }
}

/// **One (venue, tier) cell, for the account label being shown.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TierCell {
    /// No account row at `(venue, label, tier)`: the account must be created with its tier first.
    NoAccount,
    /// More than one row answers `(venue, label, tier)` and none is the ONE active row — the ids,
    /// so the sentence can name them.
    Ambiguous(Vec<i64>),
    /// The account row this cell is.
    Account(AccountCell),
}

/// The cells of one venue for one account label, one per tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VenueCredStatus {
    /// The [`vike_model::VENUES`] id.
    pub venue: String,
    /// The paper tier's cell. The paper simulator reads no credential, so the panel offers no form
    /// here whatever the cell holds.
    pub paper: TierCell,
    /// The demo tier's cell.
    pub demo: TierCell,
    /// The live tier's cell.
    pub live: TierCell,
}

impl VenueCredStatus {
    /// A venue with no account row at any tier — what a label no row carries renders as.
    #[must_use]
    pub fn absent(venue: &str) -> Self {
        VenueCredStatus {
            venue: venue.to_string(),
            paper: TierCell::NoAccount,
            demo: TierCell::NoAccount,
            live: TierCell::NoAccount,
        }
    }

    /// The cell of `tier`.
    #[must_use]
    pub fn cell(&self, tier: AccountTier) -> &TierCell {
        match tier {
            AccountTier::Paper => &self.paper,
            AccountTier::Demo => &self.demo,
            AccountTier::Live => &self.live,
        }
    }

    /// Whether `tier`'s account row holds every REQUIRED account field. Never for paper (it reads
    /// no credential) and never for a venue with no account form.
    #[must_use]
    pub fn is_complete(&self, tier: AccountTier) -> bool {
        tier != AccountTier::Paper
            && crate::keys::has_account_form(&self.venue)
            && matches!(self.cell(tier), TierCell::Account(a) if a.presence() == Presence::Complete)
    }
}

/// **Which venues this box holds credentials for AT ALL** — one slug per venue whose DEFAULT
/// (unlabelled) account holds a complete credential at a demo or live row.
///
/// The question `docs/decisions/0066`'s decision 9 made a surface need: a credentialed venue's
/// Instruments row must say whether the locally-credentialed refresh it names is ARMED.
///
/// ⚠ **DERIVED from the same grid the panel renders** ([`AccountGrids::default_grid`]), never asked
/// a second way: a row claiming "your alpaca keys are saved" beside a panel showing no complete
/// cell would be two answers to one question.
///
/// ⚠ **The DEFAULT account only**: an Instruments row is a VENUE row and has no account to be about.
/// A box whose only alpaca credentials sit on a labelled account reads as unconfigured here —
/// declared rather than hidden.
///
/// ⚠ It is a PRESENCE claim and not a VERIFICATION one, and it is EMPTY when the store could not be
/// read (no number where none was measured).
#[must_use]
pub fn credentialed_venues(grids: &AccountGrids) -> Vec<String> {
    if !grids.health().is_readable() {
        return Vec::new();
    }
    grids
        .default_grid()
        .iter()
        .filter(|row| AccountTier::ALL.iter().any(|tier| row.is_complete(*tier)))
        .map(|row| row.venue.clone())
        .collect()
}

#[path = "status_tests.rs"]
#[cfg(test)]
mod status_tests;
