//! The settings database is the credential store — the proof, not the smoke test.
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` is accepted and the owner's ruling makes
//! its credential half non-severable. This file is the evidence for the store, and every test here
//! exists because a GREEN RUN IS NOT THE PROOF: each one asserts a property that could be false while
//! every other test in the crate stayed green.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`all_67_key_names_survive_the_round_trip`] | a writer that files the 10 names `vike_model::credential_keys` can enumerate and silently drops the 57 bespoke ones |
//! | [`a_create_over_a_current_store_changes_no_byte`] | a no-op run that rewrites, reorders or grows the store |
//! | [`no_sidecar_survives_a_clean_close`] | WAL by default, which constraint 1 was amended to forbid |
//! | [`the_modes_are_0600_in_a_0700_directory`] | the umask's answer — MEASURED as 0664 on the live box |
//! | [`a_box_with_no_database_has_no_credentials`] | a reader creeping back behind the absent store |
//! | [`a_dry_run_creates_nothing_at_all`] | a "preview" that performs the irreversible act it exists to let somebody avoid |
//! | [`the_dry_run_predicts_exactly_what_the_apply_does`] | a second decision behind the preview, describing a run that is not the one that follows |
//! | [`a_second_spelling_is_filed_as_an_alias_and_both_names_still_answer`] | a store that REFUSES the second spelling of one tier (`ALPACA_DEMO_*` beside the hand-mapped `ALPACA_SANDBOX_*`) — one account, one field, `credential_one_live_value` |
//! | [`a_write_that_collides_names_the_collision_rather_than_the_caller`] | a refusal that names one of the two colliding keys and leaves the operator to guess the other |
//! | [`the_canonical_spelling_takes_the_live_row_even_when_it_arrives_second`] | which of two spellings holds the live row decided by write ORDER rather than by which one spells its tier |
//! | [`a_mainnet_spelled_key_is_unrecognised_and_never_an_alias_of_the_live_one`] | a retired `{VENUE}_MAINNET_*` name folding onto the venue's live account — the tier spelling the owner deleted on 2026-10-09 |
//! | [`an_undiscriminated_key_gets_the_same_verdict_in_either_write`] | one store and one key getting a third account in one write and a refusal in the next, decided by which write it arrived in |
//! | [`init_on_an_existing_store_is_a_no_op_and_moves_no_byte`] | a `secrets init` that re-creates, truncates or rewrites a store the operator already has |
//!
//! # The fixture is the REAL store's shape
//!
//! [`LIVE_CREDENTIAL_KEYS`] is the key NAME list read off the live box on 2026-09-14 — 67 names —
//! and it is here rather than a tidy ten because **57 of those 67 are outside
//! `vike_model::credential_keys`' enumerable `VENUE × TIER × SUFFIX` grid**: the dukascopy
//! `_LOGIN`/`_PASSWORD`/`_SERVER` triples, hyperliquid's `_PRIVATE_KEY`/`_ACCOUNT_ADDRESS`, fxcm's
//! four, ibkr's seven, ctrader, ig, oanda, the polymarket family, the platform keys and the data-API
//! keys. A fixture built from the grid would have proved the store works on the 15% of it that is
//! easy.
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
mod read_path;
mod refusals;
mod report;
mod roundtrip;
mod schema2;
mod two_spellings;
mod writes;

use fixture::*;
