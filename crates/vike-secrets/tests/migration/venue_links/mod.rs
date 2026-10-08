//! **Every venue link in the settings store is read BY NUMBER** — ruling 3 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, ordered finished by the
//! owner on 2026-09-30.
//!
//! The text `venue` column was still WRITTEN for one release, so that a rollback to the release
//! before could read a store the first release migrated. The only way to prove a statement used the
//! number was therefore to make the text LIE and ask again: the `…_by_venue_id` tests plant real
//! rows through the public writers, rewrite every text `venue` cell to a string that names no
//! venue, and ask. ⚠ **The plan's second release DROPPED that column from `account`, `credential`
//! and `venue_setting`**, so on those three there is nothing left to lie with and no statement can
//! read a venue but by its number; the same tests now prove the property through `venue_arming`,
//! the one table that keeps a text column until Plan B deletes it, and through the absence of any
//! text column elsewhere (`the_contracting_write_removes_the_text_column_and_readers_still_answer`,
//! with `a_text_venue_with_no_number_refuses_the_contraction_by_name` for the one row the drop
//! would otherwise erase).
//! Covered that way: the three readers (`read_accounts`, both halves of `read_settings`), every
//! branch of the account label guard (the label, the unlabelled and the book collision), the two
//! book-holder checks (`set_venue_account_id`'s and a re-activation's), the venue setting writer's
//! lookup, the one-row arming writer's `UPDATE`, decision 0095's ceiling migration, the credential
//! filer's fallback account lookup (`AccountResolver::load`), the arming fold's two reads, and trap
//! 5's collision check.
//!
//! The other half of the file is the store the number is MISSING from — older shapes a read-only
//! reader and decision 0095's boot-path migration must still answer for, because neither can wait
//! for the write funnel to carry the store — and the pass that carries it.

use vike_secrets::{
    AccountEdit, ArmingRow, BookSource, DbErrorKind, SettingsSource, StoredSettings,
};

use crate::support::Fixture;
use crate::support::sql::{
    foreign_key_violations, has_column, has_text_venue, is_not_null, object_sql, plant_ddl,
    stamp_version,
};

mod by_venue_id;
mod converted_readers;
mod decision_0095;
mod fixture;
mod refusals;
mod repair;
mod text_column_move;

use fixture::*;
