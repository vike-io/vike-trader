//! The credential store moves into the database — the proof, not the smoke test.
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` is accepted and the owner's ruling makes
//! its credential half non-severable. This file is the evidence for the landing, and every test here
//! exists because a GREEN RUN IS NOT THE PROOF: each one asserts a property that could be false while
//! every other test in the crate stayed green.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`all_67_key_names_survive_the_round_trip`] | a migration that carries the 10 names `vike_model::credential_keys` can enumerate and silently drops the 57 bespoke ones |
//! | [`the_source_files_are_byte_identical_afterwards`] | a migration that "tidies" the operator's only copy of their live venue keys |
//! | [`twice_is_the_same_as_once`] | a second run that rewrites, reorders or grows the store |
//! | [`no_sidecar_survives_a_clean_close`] | WAL by default, which constraint 1 was amended to forbid |
//! | [`the_modes_are_0600_in_a_0700_directory`] | the umask's answer — MEASURED as 0664 on the live box |
//! | [`a_box_with_no_database_is_unchanged`] | a stage-2 read path that made an unmigrated box worse |
//! | [`the_database_answers_wholly_and_the_file_does_not`] | half from each, the one outcome the brief singles out |
//! | [`a_dry_run_creates_nothing_at_all`] | a "preview" that performs the irreversible act it exists to let somebody avoid |
//! | [`the_dry_run_predicts_exactly_what_the_apply_does`] | a second classifier behind the preview, describing a migration that is not the one that follows |
//! | [`a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer`] | a store that REFUSES TO EXIST on every box holding both `{VENUE}_LIVE_*` and `{VENUE}_MAINNET_*` — one account, one field, `credential_one_live_value` |
//! | [`two_spellings_that_disagree_are_refused_and_both_names_are_in_the_message`] | a refusal that names one of the two colliding keys and leaves the operator to guess the other |
//! | [`the_dry_run_predicts_the_alias_and_predicts_the_collision`] | a preview blind to every decision the ROW classifier makes — the shape that said "would be UPGRADED" above an apply that failed |
//! | [`the_canonical_spelling_takes_the_live_row_even_when_it_arrives_second`] | which of two spellings holds the live row decided by migration ORDER rather than by which one spells its tier |
//! | [`a_rollback_line_for_a_key_added_in_the_same_run_is_not_also_refused`] | a combined upgrade+add run printing a refusal for a key it actually landed |
//! | [`an_undiscriminated_key_gets_the_same_verdict_in_either_run`] | one store and one key getting a third account in one run and a refusal in the next, decided by which run it arrived in |
//!
//! # The fixture is the REAL store's shape
//!
//! [`LIVE_CREDENTIAL_KEYS`] is the key NAME list read off the live box on 2026-09-14 — 67 names, at
//! mode 600 — and it is here rather than a tidy ten because **57 of those 67 are outside
//! `vike_model::credential_keys`' enumerable `VENUE × TIER × SUFFIX` grid**: the dukascopy
//! `_LOGIN`/`_PASSWORD`/`_SERVER` triples, hyperliquid's `_PRIVATE_KEY`/`_ACCOUNT_ADDRESS`, fxcm's
//! four, ibkr's seven, ctrader, ig, oanda, the polymarket family, the platform keys and the data-API
//! keys. A fixture built from the grid would have proved the migration works on the 15% of the store
//! that is easy.
//!
//! ⚠ Values are obviously fake and are never asserted on beyond "the value came back unchanged".

use std::collections::BTreeSet;
use std::path::Path;

use vike_secrets::{Backend, NodeKeySource, Source, Table};

use crate::support::sql::{has_text_venue, stamp_version, user_version};
use crate::support::{Fixture, fake_value, key_names};

mod dry_run;
mod fixture;
mod read_path;
mod refusals;
mod report;
mod roundtrip;
mod schema2;
mod two_spellings;
mod venue_id;
mod writes;

use fixture::*;
