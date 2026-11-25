//! The PURE read side: for each roster venue and each tier, WHICH ACCOUNT ROW a cell is and which of
//! its credential FIELDS the store holds.
//!
//! # A cell is an account ROW (decision 0156 item 6)
//!
//! The tier is written in ONE place, `account.tier`, and a credential belongs to its ACCOUNT — the
//! store's `credential.account_id` + `field`. So a (venue, tier) cell for the account label the strip
//! selected is "the account row of `(venue, label, tier)`" ([`AccountManagerTierCell`]):
//!
//! * [`AccountManagerTierCell::Account`] — that row exists; [`AccountManagerTierCellAccount`] carries its id, whether it is
//!   active, and which of the venue's REQUIRED account fields
//!   (`vike_venues::credential_schema::schema_for(venue).required(KeyFieldOwner::Account)`) it holds —
//!   field words only, never a value;
//! * [`AccountManagerTierCell::NoAccount`] — no row: no credential can be attached, because a credential write
//!   never creates an account (`vike_secrets::save_owned_credentials` refuses one);
//! * [`AccountManagerTierCell::Ambiguous`] — more than one row answers and none is the one active row, so a form
//!   would be a guess (a store older than the labelled-row rule: give each a label).
//!
//! ⚠ **No name is read, composed or parsed to decide any of this.** The data is the three store
//! reads a composition root performs — `vike_secrets::CredentialStore::accounts`, `account_keys`
//! and `credential_registry` — folded by [`AccountManagerGrids`]. The one use of a NAME left is the field
//! REMOVE's (a soft retire is addressed by the row's stored name, taken from `account_keys`'
//! listing — [`AccountManagerGridHeldFields::stored_name`]).
//!
//! # ⚠ The ACCOUNT dimension
//!
//! [`AccountManagerGrids`] carries one grid per account LABEL the store's rows carry, plus the default
//! (unlabelled) account's; the view renders one at a time, picked in the strip.

use vike_model::AccountTier;

mod accounts;
mod tier_cell;

pub use accounts::{AccountManagerGridHeldFields, AccountManagerGrids};
pub use tier_cell::{
    AccountManagerTierCell, AccountManagerTierCellAccount, AccountManagerTierCellPresence,
};

/// The cells of one venue for one account label, one per tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountManagerGridRow {
    /// The [`vike_model::VENUES`] id.
    pub venue: String,
    /// The paper tier's cell. The paper simulator reads no credential, so the panel offers no form
    /// here whatever the cell holds.
    pub paper: AccountManagerTierCell,
    /// The demo tier's cell.
    pub demo: AccountManagerTierCell,
    /// The live tier's cell.
    pub live: AccountManagerTierCell,
}

impl AccountManagerGridRow {
    /// A venue with no account row at any tier — what a label no row carries renders as.
    #[must_use]
    pub fn absent(venue: &str) -> Self {
        AccountManagerGridRow {
            venue: venue.to_string(),
            paper: AccountManagerTierCell::NoAccount,
            demo: AccountManagerTierCell::NoAccount,
            live: AccountManagerTierCell::NoAccount,
        }
    }

    /// The cell of `tier`.
    #[must_use]
    pub fn cell(&self, tier: AccountTier) -> &AccountManagerTierCell {
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
            && crate::form::has_account_form(&self.venue)
            && matches!(self.cell(tier), AccountManagerTierCell::Account(a) if a.presence() == AccountManagerTierCellPresence::Complete)
    }
}

/// **Which venues this box holds credentials for AT ALL** — one slug per venue whose DEFAULT
/// (unlabelled) account holds a complete credential at a demo or live row.
///
/// The question `docs/decisions/0066`'s decision 9 made a surface need: a credentialed venue's
/// Instruments row must say whether the locally-credentialed refresh it names is ARMED.
///
/// ⚠ **DERIVED from the same grid the panel renders** ([`AccountManagerGrids::default_grid`]), never asked
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
pub fn venues_with_credentials(grids: &AccountManagerGrids) -> Vec<String> {
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

#[path = "grid_tests.rs"]
#[cfg(test)]
mod grid_tests;
