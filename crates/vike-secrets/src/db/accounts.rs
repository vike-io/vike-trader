//! The account plane: the `account` table readers, the book-column write and the lifecycle.

use super::*;

mod book;
mod edit;
mod leverage_venues;
mod model;

pub use book::{BookSource, BookWrite, VENUE_ACCOUNT_ID_MAX_BYTES, normalized_venue_account_id};
pub use edit::{AccountEdit, AccountWrite, normalized_account_label};
pub use leverage_venues::{
    LEVERAGE_VENUES, LeverageMeans, LeverageVenue, leverage_refusal, leverage_venue,
    leverage_venue_at,
};
pub use model::{
    Account, AccountKeys, AccountLeverage, Accounts, ActiveTier, MaxExposure, NoAccountTable,
    VenueRow, read_venues,
};
// Reached by production only through `crate::CredentialStore` and the journalled writers; public
// to a test through the `test-support` feature (see `crate::db`'s matching note).
#[cfg(feature = "test-support")]
pub use book::set_venue_account_id;
#[cfg(not(feature = "test-support"))]
pub(crate) use book::set_venue_account_id;
#[cfg(feature = "test-support")]
pub use edit::edit_account;
#[cfg(not(feature = "test-support"))]
pub(crate) use edit::edit_account;
#[cfg(feature = "test-support")]
pub use model::{read_account_keys, read_accounts};
#[cfg(not(feature = "test-support"))]
pub(crate) use model::{read_account_keys, read_accounts};
