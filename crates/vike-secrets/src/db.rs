//! The settings DATABASE: `<project>/settings/db/vike.db` — the credential store's new home.
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` is accepted, and this module is its
//! credential half: the database EXISTS and is FILLED here, and `crate::store` reads from it.
//! Nothing in this module ever writes to `secrets.env` or `node.env` — see *The one rule that
//! outranks everything here*, below.
//!
//! # Two tables, and the predicate they buy
//!
//! ```sql
//! CREATE TABLE credential (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
//! CREATE TABLE node_key   (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
//! ```
//!
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` is ratified on the measured property
//! that two key classes with divergent growth may not share one namespace: the credential store
//! holds the venue grid (a new venue adds twelve names) and leaking one means somebody signs orders
//! with real money; the node pair is four names, grows with neither, and leaking one means somebody
//! reaches a data service. Two TABLES re-express that split, and what they buy is the **static
//! predicate — no key name is looked up in two namespaces** — **not** access control: SQLite has no
//! per-table grant, so one handle reaches both and a process holding the handle holds everything.
//! 0054's constraint 1 carries that correction; do not restate it as isolation.
//!
//! The predicate is ENFORCED rather than assumed: [`migrate`] refuses
//! ([`Ambiguity::WrongTable`]) when a name it is about to write already sits in the other table, so
//! a run under a wider or narrower `is_node_key` cannot quietly give one name two homes.
//!
//! # The columns, argued rather than gold-plated
//!
//! `name` and `value`, and nothing else. Three columns were considered and refused:
//!
//! * **a timestamp** — the change journal already owns *when* and *by whom*
//!   (`vike_model::change_journal`, which 0054 carries forward as an append-only JSONL file
//!   deliberately). A column here would be a second, weaker answer to a question that has an owner,
//!   and it would make [`migrate`] non-idempotent at the BYTE level for no gain.
//! * **a tier/venue decomposition** — 57 of the 67 names on the live box are OUTSIDE
//!   `vike_model::credential_keys`' enumerable `VENUE × TIER × SUFFIX` grid (the bespoke FX and
//!   on-chain shapes: `DUKASCOPY_DEMO1_SERVER`, `HYPERLIQUID_LIVE_PRIVATE_KEY`,
//!   `FXCM_DEMO_CONNECTION`, `IBKR_DEMO_BACKEND`, `POLY_SIGNATURE_TYPE`, the data-API keys). A
//!   schema that decomposed the name would have to refuse or mangle the majority of the real store.
//! * **a per-row note**, the TOML comment's replacement 0054 flags as owed. That debt belongs to the
//!   SETTINGS half, which has hand-edited files with `# raised for the weekend` in them; the
//!   credential store is a key list, and inventing the column here would decide the settings half's
//!   question in the wrong record.
//!
//! `STRICT` is the point of the exercise rather than a flourish: 0054's argument is *"a schema is a
//! gate; a directory is not"*, and a non-STRICT SQLite table accepts anything in any column.
//!
//! The schema VERSION is [`SCHEMA_VERSION`] in `PRAGMA user_version` — SQLite's own one-integer
//! slot, so versioning costs no table and no row. A database whose `user_version` is not this one is
//! an ERROR and never an empty map, which is the distinction 0054 says the boot seam owes.
//!
//! # ⚠ THE INVARIANT THE EXISTENCE PROBE RESTS ON: a database exists ⇒ a migration finished
//!
//! `crate::store::Backend` decides which store answers from ONE `is_file` on one path, so an empty
//! or half-written database is not a degraded answer — it is a WRONG one that lasts forever: the
//! credential file beside it stops being read, `resolve_project` returns an empty map, and an empty
//! map is not an error downstream but the LIVE GATE. Every venue drops to paper with `secrets.env`
//! sitting on disk looking correct.
//!
//! Two things buy the invariant, and both are in this module:
//!
//! * [`migrate`] **opens no write connection when there is nothing to migrate**
//!   ([`MigrationOutcome::NothingToMigrate`]) — creating the store is the harmful act, so the arm
//!   that would have nothing to put in it does not create one;
//! * `PRAGMA user_version` is stamped **after the insert transaction commits**
//!   ([`stamp_schema_version`]), so a crash, a full disk or a read-only remount leaves
//!   `user_version = 0` — which [`check_schema_version`] already refuses LOUDLY.
//!
//! What remains outside the invariant is stated rather than papered over: a file somebody else puts
//! at that path is a database this code did not write, and it is refused by
//! [`check_schema_version`] (any other `user_version`) or by the engine (not a database at all) —
//! loudly, in both cases, and never as an empty map.
//!
//! # `journal_mode = DELETE`, not WAL
//!
//! `open_for_write` sets it and VERIFIES the engine agreed. 0054's constraint 1 was AMENDED for
//! this: WAL needs `-wal` and `-shm` sidecars beside the file, which makes the daemon-down read
//! impossible in the read-only `settings/` the deployed unit mounts (SQLite's own words: *"It is not
//! possible to open read-only WAL databases"*), turns ONE plaintext credential artifact into two,
//! and turns one `chmod 600` into a check over a set. DELETE leaves nothing at rest. What WAL would
//! have bought — concurrent writers, write throughput — is nothing here: this store takes tens of
//! writes a day. The narrow `ReadWritePaths=<root>/settings/db` grant still stays, because DELETE
//! mode writes a rollback journal beside the database WHILE a transaction is in flight; what changed
//! is that nothing is left there afterwards, which
//! `crates/vike-secrets/tests/migration/database/roundtrip.rs`'s `no_sidecar_survives_a_clean_close` proves
//! rather than trusts.
//!
//! # The modes are set, never inherited
//!
//! **0600 on the file, in a 0700 directory** — the posture the credential file has today, and
//! `crate::env_write`'s `save_credentials` is the precedent: it sets an explicit mode on a store it
//! creates, *never the umask's answer*. That is not caution. MEASURED on the live box 2026-09-13
//! (0054, *What must land*): the umask there produces **0664**, so a database created without an
//! explicit mode lands group- and world-readable with every venue key in it.
//!
//! The file is therefore PRE-CREATED empty at 0600 before the engine is handed the path — a
//! zero-length file is a valid empty SQLite database — so there is no window in which rows exist
//! under the umask's mode. The directory is created through `DirBuilder::mode(0o700)`, which applies
//! to the components this code creates and leaves an existing directory alone.
//!
//! ⚠ **An EXISTING file's mode is REPORTED, never repaired.** `crate::store::permission_warning` is
//! the same check the file store gets, for the same reason the root `CLAUDE.md` gives: a finding is
//! never a refusal, because refusing over a 0644 store strands somebody mid-setup with every venue
//! on paper.
//!
//! ⚠ **SQLite's own ROLLBACK JOURNAL is now MEASURED rather than assumed**, and this paragraph used
//! to record it as 0054's OPEN QUESTION — *the unix VFS derives the journal's creation mode from
//! the database file's own mode (`findCreateFileMode`), which would mean a 0600 store carries 0600
//! across, but that has NOT been measured here and is not leaned on.* It is measured now, in flight,
//! from inside a transaction that is holding a real row:
//! `db_tests::the_rollback_journal_is_0600_while_a_transaction_is_in_flight` `stat`s
//! `vike.db-journal` while it exists and PINS the answer. That matters because the journal holds
//! PAGES of the database — i.e. plaintext venue credentials — for the duration of every write, and
//! an engine upgrade that changed the derivation would otherwise widen a credential artifact in
//! silence. The pinned answer is **0600**, inherited from the database file, which is exactly why
//! the file is pre-created at 0600 rather than left to the umask. `no_sidecar_survives_a_clean_close`
//! remains the AT-REST half; this is the in-flight half.
//!
//! # The one rule that outranks everything here
//!
//! **The credential file is the operator's only copy of live venue keys.** [`migrate`] READS it —
//! through `crate::store::resolve`, the one parser — and writes ELSEWHERE. It does not tidy it,
//! normalise it, re-render it, or remove it, on success or on any other outcome, and there is no
//! flag that makes it. Retiring the file is an OPERATOR act; the most this code does is count what
//! it copied. `crates/vike-ops/tests/credentials/credential_writer_gate.rs` pins the set of files allowed to
//! write that store, and this one is deliberately not among them — nothing here opens either file
//! for writing at all.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// `OptionalExtension` is the trait that turns `QueryReturnedNoRows` into `Ok(None)`. It is imported
// rather than matched by hand because "no such row" is an ANSWER on the account-book write path —
// `DbErrorKind::NoSuchAccount` — and a hand-written match on one engine error variant beside a
// `map_err` that swallows every other one is how that answer would quietly become `Sqlite`.
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};

// The account-label grammar's two constants, IMPORTED rather than re-spelled. They were a second
// copy here until `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`
// (accepted 2026-09-20) admitted the `vike-model` edge; the duplication's only stated reason was
// that this crate could not name the crate that owns them, and that reason is gone. Imported
// rather than reached for fully-qualified because both appear inside inline format captures in
// `DbErrorKind`'s `Display`, which need an identifier in scope.
use vike_model::accounts::account_keys::{MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL};

use crate::store::{SecretMap, SecretsError};

mod accounts;
mod error;
mod migrate;
mod open;
mod read;
#[cfg(feature = "test-support")]
mod test_support;
mod venues;

pub use accounts::{
    ACCOUNT_TABLE_SCHEMA, Account, AccountEdit, AccountKeys, AccountWrite, Accounts, BookSource,
    BookWrite, NoAccountTable, VENUE_ACCOUNT_ID_MAX_BYTES, VenueRow, edit_account,
    normalized_account_label, normalized_venue_account_id, read_account_keys, read_accounts,
    read_venues, set_venue_account_id,
};
pub use error::{DbError, DbErrorKind, Table};
#[cfg(feature = "test-support")]
pub use migrate::SCHEMA_1;
pub(crate) use migrate::upsert_rows;
pub use migrate::{
    Ambiguity, MigrateError, Migration, MigrationOutcome, MigrationPlan, MovedRows, PlannedOutcome,
    SourceReport, migrate, move_pending_rows, preview,
};
pub use open::{READABLE_SCHEMA_VERSIONS, SCHEMA_VERSION, database_present, is_sqlite_file};
pub(crate) use open::{open_for_read, open_for_write, open_for_write_within};
pub use read::{
    read_credentials_demo_only, read_present_names_scoped, read_table, read_table_scoped,
};
#[cfg(feature = "test-support")]
pub use test_support::{create_empty_store_for_test, plant_schema_1};
pub(crate) use venues::{ensure_venue_id_columns, ensure_venue_rows};

// ---------------------------------------------------------------------------------------------
// What this module's own suite MEASURES rather than assumes
// ---------------------------------------------------------------------------------------------

/// Four properties of this module that nothing above it can see, and that no integration test can
/// reach: the ROLLBACK JOURNAL's mode while a transaction is in flight, the ORDER in which a
/// database becomes readable, WHO MAY CREATE ONE, and that the two migration entry points share ONE
/// classifier.
///
/// * **The journal.** `journal_mode = DELETE` writes `vike.db-journal` beside the database for the
///   duration of every write transaction, and that journal holds PAGES of the database — which here
///   means plaintext venue credentials. Nothing AT REST is the property
///   `no_sidecar_survives_a_clean_close` already proves; this is the window while the write is
///   happening. SQLite's unix VFS derives a journal's creation mode from the database file's own
///   mode (`findCreateFileMode`), which would mean a 0600 database carries 0600 across. That had
///   never been measured here. It is measured now, in flight, from the same process — and PINNED, so
///   an engine upgrade that changed it becomes a red test rather than a silent widening of a file
///   full of credentials. ⚠ Those two tests are `#[cfg(unix)]`: Windows has no mode bits, and the
///   equivalent question there is an ACL query this crate has no business answering — the same
///   posture `crate::store::permission_warning` and `create_file_private` already take.
/// * **The order.** [`open_for_write`] leaves `user_version` at 0 and only [`stamp_schema_version`]
///   sets it, which is what keeps an interrupted migration from impersonating a finished one. That
///   is a relationship between two private functions, so it is asserted here and not from `tests/`.
/// * **Who may create one.** [`migrate`] is the only function that may bring a database into
///   existence, because it is the only one that reads the whole of both files first — see
///   [`upsert_rows`]' invariant section. [`upsert_rows`] refuses the one race on which it otherwise
///   could, and [`upsert_rows`] is crate-private, so the state that exercises it is unreachable
///   from `tests/`.
/// * **ONE classifier.** [`migrate`] and [`preview`] must be the same decision plus or minus the
///   write. [`plan`] is crate-private, so from `tests/` the only reachable version of that claim is
///   behavioural — and a behavioural equivalence over fixtures would stay green the day somebody
///   adds a second classifier that happens to agree on the cases those fixtures cover.
///   `the_two_entry_points_share_one_classifier` asserts the structure instead.
#[cfg(test)]
mod open_tests;

#[cfg(test)]
mod venues_tests;

#[cfg(test)]
mod migrate_tests;
