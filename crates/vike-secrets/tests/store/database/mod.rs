//! The settings database is the credential store — the proof, not the smoke test.
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` is accepted and the owner's ruling makes
//! its credential half non-severable. This file is the evidence for the store, and every test here
//! exists because a GREEN RUN IS NOT THE PROOF: each one asserts a property that could be false while
//! every other test in the crate stayed green.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`all_67_key_names_survive_the_round_trip`] | a writer that files the 10 names of the old `VENUE × TIER × SUFFIX` grid and silently drops the 57 bespoke ones |
//! | [`a_create_over_a_current_store_changes_no_byte`] | a no-op run that rewrites, reorders or grows the store |
//! | [`no_sidecar_survives_a_clean_close`] | WAL by default, which constraint 1 was amended to forbid |
//! | [`the_modes_are_0600_in_a_0700_directory`] | the umask's answer — MEASURED as 0664 on the live box |
//! | [`a_box_with_no_database_has_no_credentials`] | a reader creeping back behind the absent store |
//! | [`a_dry_run_creates_nothing_at_all`] | a "preview" that performs the irreversible act it exists to let somebody avoid |
//! | [`the_dry_run_predicts_exactly_what_the_apply_does`] | a second decision behind the preview, describing a run that is not the one that follows |
//! | [`a_name_addressed_write_files_a_deployment_row_and_mints_no_account`] | a writer that still files a NEW name under an account its tier token implies — and MINTS that account and chooses its tier (decision 0156 item 6) |
//! | [`the_owner_addressed_writer_files_a_field_against_an_existing_account`] | an account's credential written anywhere but against its `(account_id, field)` — or under a name that carries a tier |
//! | [`an_unknown_account_is_refused_and_no_account_is_created`] | a credential write that INVENTS the account it was addressed to |
//! | [`init_on_an_existing_store_is_a_no_op_and_moves_no_byte`] | a `secrets init` that re-creates, truncates or rewrites a store the operator already has |
//!
//! # The fixture is the REAL store's shape
//!
//! [`LIVE_CREDENTIAL_KEYS`] is the key NAME list read off the live box on 2026-09-14 — 67 names —
//! and it is here rather than a tidy ten because **57 of those 67 were outside the enumerable
//! `VENUE × TIER × SUFFIX` grid** the workspace then composed names from: the dukascopy
//! `_LOGIN`/`_PASSWORD`/`_SERVER` triples, hyperliquid's `_PRIVATE_KEY`/`_ACCOUNT_ADDRESS`, fxcm's
//! four, ibkr's seven, ctrader, ig, oanda, the polymarket family, the platform keys and the data-API
//! keys. A fixture built from the grid would have proved the store works on the 15% of it that is
//! easy. (Since decision 0156 item 6 no reader composes a credential name at all; the names are
//! still what the name-addressed writer and the name-keyed reads round-trip.)
//!
//! ⚠ Values are obviously fake and are never asserted on beyond "the value came back unchanged".

use std::collections::BTreeSet;
use std::path::Path;

use vike_secrets::{Backend, Source, Table};

use crate::support::sql::{has_text_venue, stamp_version, user_version};
use crate::support::{Fixture, fake_value, key_names};

mod dry_run;
mod fixture;
mod init;
mod intents_and_delete;
mod read_path;
mod refusals;
mod report;
mod roundtrip;
mod schema2;
mod watch;
mod writes;

use fixture::*;
