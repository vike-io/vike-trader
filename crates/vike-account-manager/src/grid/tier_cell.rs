//! The three tier-cell types: what ONE (venue, label, tier) cell of the grid is — no account row,
//! several (ambiguous), or the one account row with which of its REQUIRED fields the store holds.
//! Field words and ids only, never a value. The fold that builds them is `accounts`; the meaning of
//! a cell (a dot, a count) is `crate::summary`'s, which this module never names.

/// **How much of one account row's REQUIRED credential the store holds.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountManagerTierCellPresence {
    /// Every required account field is held (non-blank).
    Complete,
    /// Some required fields are held and some are not — a half-written set.
    Partial,
    /// No required field is held.
    Empty,
}

/// **One account row as a cell sees it** — identity and field PRESENCE, never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountManagerTierCellAccount {
    /// `account.id` — the identity a Save addresses (`CredentialOwner::Account`).
    pub id: i64,
    /// `account.active`. An inactive row still owns its credentials and may be edited; the arming
    /// reader treats it as absent.
    pub active: bool,
    /// The venue's REQUIRED account fields this row holds, in schema order. Field words only.
    pub(crate) held_required: Vec<&'static str>,
    /// The venue's REQUIRED account fields this row does NOT hold, in schema order.
    pub missing_required: Vec<&'static str>,
}

impl AccountManagerTierCellAccount {
    /// Complete when nothing required is missing, partial when some required fields are held and
    /// some are not, empty otherwise.
    #[must_use]
    pub fn presence(&self) -> AccountManagerTierCellPresence {
        match (self.held_required.is_empty(), self.missing_required.is_empty()) {
            (_, true) => AccountManagerTierCellPresence::Complete,
            (false, false) => AccountManagerTierCellPresence::Partial,
            (true, false) => AccountManagerTierCellPresence::Empty,
        }
    }
}

/// **One (venue, tier) cell, for the account label being shown.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountManagerTierCell {
    /// No account row at `(venue, label, tier)`: the account must be created with its tier first.
    NoAccount,
    /// More than one row answers `(venue, label, tier)` and none is the ONE active row — the ids,
    /// so the sentence can name them.
    Ambiguous(Vec<i64>),
    /// The account row this cell is.
    Account(AccountManagerTierCellAccount),
}
