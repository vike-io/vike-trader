//! The account plane: the `account` table readers, the book-column write and the lifecycle.

use super::*;

mod book;
mod edit;
mod fact;
mod lane_twins;
mod model;

pub use book::{BookSource, BookWrite, VENUE_ACCOUNT_ID_MAX_BYTES, normalized_venue_account_id};
pub use edit::{
    ACCOUNT_SYMBOL_MAX_BYTES, AccountEdit, AccountWrite, normalized_account_label,
    normalized_account_symbol,
};
pub use fact::AccountFact;
pub use lane_twins::AccountRowLaneTwinIgnoredLeverage;
pub use model::{
    AccountKeys, AccountLeverage, AccountRow, AccountRowActiveTier, AccountRowMaxExposure,
    AccountRows, AccountRowsUnknown, VenueRow, read_venues,
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
