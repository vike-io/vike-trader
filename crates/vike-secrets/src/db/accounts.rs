//! The account plane: the `account` table readers, the book-column write and the lifecycle.

use super::*;

mod book;
mod edit;
mod model;

pub use book::{
    BookSource, BookWrite, VENUE_ACCOUNT_ID_MAX_BYTES, normalized_venue_account_id,
    set_venue_account_id,
};
pub use edit::{AccountEdit, AccountWrite, edit_account, normalized_account_label};
pub use model::{
    Account, AccountKeys, Accounts, ActiveTier, MaxExposure, NoAccountTable, VenueRow,
    read_account_keys, read_accounts, read_venues,
};
