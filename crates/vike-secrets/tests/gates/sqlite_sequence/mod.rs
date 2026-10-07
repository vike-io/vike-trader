//! **`sqlite_sequence` must survive a table rebuild** — §7 item 4 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, in its own words:
//! *"Remove the top account, rebuild the table, assert the next id is not reused."*
//!
//! §4.1 is the hazard this exists for, and it is the half a reader will not otherwise meet:
//! *"The high-water mark lives in a row of `sqlite_sequence`, and `DROP TABLE` deletes it. Nothing
//! in `crates/vike-secrets` knows this table exists today. On THIS migration there is no prior mark
//! to lose (the constraint is being introduced), so copying rows with their ids sets the sequence
//! to `max(id)`, which is correct. **The hazard is every FUTURE rebuild of these tables**: one that
//! replays surviving rows after the top row was removed rewinds the mark and silently un-does the
//! guarantee."*
//!
//! That sentence is the whole of it: a rebuild that copies rows WITH THEIR IDS sets the mark to
//! `max(id)` of what it copied, which is correct only while the top row is among them. Remove the
//! top row first and the mark rewinds to the survivor's id, and the next insert is handed a number
//! an operator already wrote down against a different account.
//!
//! # ⚠ Where this gate lives — the ruling §7 item 4 leaves open, and it was got WRONG first
//!
//! The obstacle is real: `crate::schema::reshape_into` is PRIVATE and un-re-exported, so nothing
//! under `tests/` can call it. **That is not the same as being unable to run the real reshape.**
//! `vike_secrets::migrate` is the PRODUCTION entry point that performs it — `fill_into` calls
//! `reshape_into` whenever it finds a store below the current schema — and `vike_secrets::
//! plant_schema_1` (behind the crate's `test-support` feature) plants the schema-1 store it runs
//! on. So [`the_real_reshape_does_not_rewind_an_armed_tables_mark`] drives the crate's own rebuild
//! through the same call a binary makes, which is a STRONGER witness than reaching past the
//! module's front door.
//!
//! ⚠ **This shipped first as a `#[cfg(test)]` module in `src/db/`, and that home was MEASURED to be
//! wrong** — five gates in `crates/vike-ops/tests/` fire on it, all correctly, because a file under
//! `crates/vike-secrets/src/` is held to the credential store's rules whether or not `#[cfg(test)]`
//! will delete it:
//!
//! * `compile_time_path_gate`'s `compile_time_paths_do_not_grow` — [`source_files`] resolves this
//!   crate's own directory from `CARGO_MANIFEST_DIR`, i.e. the tree the compiler was run in. That
//!   is exactly the defect that gate exists for, and in `src/` it is indistinguishable from a
//!   binary baking in its build checkout.
//! * `credential_source_roster_gate`'s `every_file_name_the_store_crate_spells_is_pinned` —
//!   [`TABLE_DROP_PIN`] spells `db.rs`, `schema.rs` and `profile_store.rs`, and a file NAME inside
//!   the store crate is a place a credential value can come from until somebody says otherwise.
//! * the same gate's `every_ingress_site_in_the_store_is_pinned` — `read_dir`, `read_to_string`
//!   and `Connection::open` are byte-ingress sites, and in `src/` each needs a declared row.
//! * the same gate's `the_test_cut_hides_no_production_item` — its `#[cfg(test)]` cut looks for a
//!   `}` at column zero, and `#[cfg(test)] mod name;` (a DECLARATION, not a block) never gives it
//!   one, so the cut could no longer prove what it was hiding.
//! * `credential_writer_gate`'s `every_credential_writer_caller_is_pinned`.
//!
//! Each of those is a row somebody could have written in another crate's gate. None should have
//! been: the scan half of this file has no business in `src/` at all, and once it moves, the
//! behavioural half loses nothing by coming with it.
//!
//! # ⚠ The debt is DERIVED rather than pinned, and stage 4 PAID it on 2026-09-23
//!
//! This gate shipped GREEN against a schema where `account` was `id INTEGER PRIMARY KEY` with no
//! `AUTOINCREMENT` anywhere near it, so §7 item 4's assertion — *the next id is not reused* — was
//! FALSE and a red test cannot be landed at all (`crates/vike-secrets/tests/gates/store_link.rs`
//! says so in its own words, and `crates/vike-secrets/tests/gates/null_discriminator/mod.rs` shipped as
//! a ratchet for the same reason).
//!
//! The resolution was stronger than a pinned expectation, because the expectation is DERIVED from
//! the shipped schema rather than written down:
//! [`section_7_item_4_remove_the_top_account_rebuild_and_create_again`] asks [`armed_tables`]
//! whether `account` declares `AUTOINCREMENT` and asserts REUSE when it does not, NON-REUSE when it
//! does. So:
//!
//! | what stage 4 lands | this gate |
//! |---|---|
//! | nothing (the state this file landed against) | green — reuse MEASURED, not assumed |
//! | `AUTOINCREMENT` + a rebuild that carries the mark — **where the tree is now** | green, and §7 item 4's assertion is the live one |
//! | `AUTOINCREMENT` + a writer that defeats it (an explicit `id` in `edit_account`'s `INSERT`) | **RED** |
//!
//! The third row is not hypothetical — it is one of this gate's kill proofs, run against the real
//! `edit_account`.
//!
//! [`SEQUENCE_PIN`] was the staleness half and did its job: `account` was pinned
//! [`Sequence::Owed`], the shipped `DDL` armed it, and
//! [`every_pinned_verdict_matches_the_schema`] went red naming stage 4 — which is how stage 4 was
//! stopped from landing silently. Every row there now reads [`Sequence::Armed`].
//!
//! # Declared scope — what a green here does NOT cover
//!
//! * ⚠ **This bullet read *"No production path rebuilds an ARMED table today"* and stage 4
//!   falsified it**: `crate::schema::rebuild_table_from_ddl` is the one rebuild procedure in this
//!   crate, and both migrations that call it —
//!   `crate::schema::migrate_sim_tier_to_paper` and
//!   `crate::schema::migrate_tables_onto_autoincrement` — now touch armed tables. The live
//!   behaviour that had no assertion now has one:
//!   [`the_paper_tier_rebuild_does_not_rewind_the_account_marks`], which drives the REAL repair
//!   path over a store whose mark is above its top surviving row. What still covers the rebuilds
//!   nobody has written yet is [`every_table_drop_in_this_crates_source_is_classified`]: a rebuild
//!   needs a table drop, this crate's `src/` carries exactly the ones pinned, and a new one
//!   reddens until its author writes down what happens to the mark. That is a classification duty,
//!   not a proof — and it has already caught one, see [`TABLE_DROP_PIN`].
//! * **[`a_rebuild_that_does_not_carry_the_mark_rewinds_it`] measures the ENGINE**, in the same
//!   spirit as `crates/vike-secrets/src/db/open_tests.rs`'s `the_schema_stamp_and_the_ddl_are_both_transactional`:
//!   it is pinned rather than assumed because every claim above rests on it. It is not a claim
//!   about a caller.
//! * **The scan reads `src/` only.** The rebuild helper in THIS file performs a table drop of its
//!   own and is deliberately out of scope: a harness is not production.

mod behaviour;
mod engine_seam;
mod pin;
mod schema_gate;
mod source_ratchet;

use std::collections::BTreeSet;

use crate::ddl_parser::tables;

// -------------------------------------------------------------------------------------------
// The DDL derivation
// -------------------------------------------------------------------------------------------

/// Every table in `ddl` whose body declares `AUTOINCREMENT` — the tables SQLite keeps a
/// `sqlite_sequence` row for, and therefore the tables a rebuild can rewind.
fn armed_tables(ddl: &str) -> BTreeSet<String> {
    tables(ddl)
        .into_iter()
        .filter(|(_, body)| body.to_uppercase().contains("AUTOINCREMENT"))
        .map(|(name, _)| name)
        .collect()
}

/// The single `CREATE TABLE` statement for `table`, lifted out of the shipped [`DDL`] rather than
/// re-spelled, so a fixture that plants one table plants the one the store actually ships.
fn create_statement(ddl: &str, table: &str) -> String {
    let needle = format!("CREATE TABLE IF NOT EXISTS {table} ");
    let at = ddl.find(&needle).unwrap_or_else(|| panic!("`DDL` declares no table `{table}`"));
    let rest = &ddl[at..];
    let end = rest.find(';').expect("a CREATE TABLE statement ends");
    format!("{};", &rest[..end])
}
