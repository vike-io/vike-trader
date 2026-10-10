//! **Every venue link in the settings store is read BY NUMBER** — ruling 3 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, ordered finished by the
//! owner on 2026-09-30.
//!
//! `account`, `credential` and `venue_setting` carry `venue_id` alone, so no statement can read a
//! venue there but by its number. `venue_arming` keeps a text `venue` column beside its number, and
//! the only way to prove a statement used the number is to make that text LIE and ask again: the
//! `…_by_venue_id` tests plant real rows through the public writers on a store born on the shipped
//! shape, rewrite every text `venue` cell to a string that names no venue, and ask.
//! Covered that way: the three readers (`read_accounts`, both halves of `read_settings`), every
//! branch of the account label guard (the label, the unlabelled and the book collision), the two
//! book-holder checks (`set_venue_account_id`'s and a re-activation's), the venue setting writer's
//! lookup, the one-row arming writer's `UPDATE`, the credential filer's fallback account lookup
//! (`AccountResolver::load`) and the arming fold's two reads.

use vike_secrets::{
    AccountEdit, ArmingRow, BookSource, DbErrorKind, SettingsSource, StoredSettings,
};

use crate::support::Fixture;
use crate::support::sql::has_text_venue;

mod by_venue_id;
mod converted_readers;
mod fixture;

use fixture::*;
