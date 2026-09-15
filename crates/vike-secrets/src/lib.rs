//! `vike-secrets` — the credential store: ONE file, in the project.
//!
//! ```text
//! <project>/settings/secrets.env      every API key, every venue, every checkout of this project
//! ```
//!
//! That is the whole story. [`resolve`] opens a path the caller names, [`resolve_project`] asks for
//! the project's own, and [`workspace_dotenv_path`] is how a caller with no opinion learns where
//! that is. There is no precedence and no second location, so there is nothing to guess at and
//! nothing to debug: point at the file, edit the file.
//!
//! # ⚠ …and since `docs/decisions/0054`, ONE STORE rather than one file
//!
//! ```text
//! <project>/settings/db/vike.db       the settings database: `credential`, `account`,
//!                                     `venue_setting` and `node_key`
//! ```
//!
//! ⚠ That line read *"`credential` and `node_key` tables"* until schema 2 landed. Two more exist
//! now, and they are at opposite ends of being useful: [`Account`] has this reader and
//! `venue_setting` is deliberately EMPTY — `crate::schema`'s module doc argues the sequencing rule
//! that keeps it so.
//!
//! 0054 is accepted and its credential half is **not severable** by the owner's ruling. [`migrate`]
//! fills `credential` and `node_key` from the two files — that read *"those two tables"* while the
//! block above listed exactly two, and widening the block moved the antecedent rather than the
//! fact; `venue_setting` is created empty and `account`'s rows are derived by the classifier, so
//! neither is filled FROM a file — `vike-cli secrets migrate` is what calls it, and
//! [`preview`] is that same decision with the write left out, for the `--dry-run` an irreversible
//! first run deserves; [`resolve_project`] and [`resolve_node_keys`] then
//! read the tables when that database exists and the files when it does not. `crate::db` carries the
//! schema, the `journal_mode = DELETE` argument and the modes; [`Backend`] carries the one that
//! matters most here — **the choice is per-RUN, not per-KEY**, so the sentence above survives
//! verbatim with "file" read as "store": still no precedence, still no second location, still one
//! home per name. A per-key fallback would have been the ladder
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids, and it is not what this does.
//!
//! **What has NOT happened, and is not this crate's act to perform:** `secrets.env` is not deleted,
//! moved, truncated or rewritten by any of it. [`migrate`] reads it and writes elsewhere. Retiring
//! the file is an operator act, and [`ShadowedStore`] is how a reader is told the file has stopped
//! being read.
//!
//! # The `account` table has a reader — [`resolve_accounts_in`]
//!
//! Schema 2 gave the account a ROW, and until this reader existed the only `SELECT … FROM account`
//! in the tree was inside the migration that fills it. [`resolve_accounts_in`] is the seam that
//! lets a caller ask the store *which accounts exist for this venue, and what is each one's
//! identity* — [`Account::id`], the permanent opaque handle, rather than a number parsed back out
//! of a key name.
//!
//! ⚠ **This said "it changes no behaviour on its own: nothing mounts, arms or signs from it yet",
//! and that stopped being true on 2026-09-15.** `vike_mount`'s dukascopy arm resolves WHICH ACCOUNT
//! — and therefore which LEGAL ENTITY an order reaches — from these rows, by the credential-key
//! OWNER PREFIX each one owns ([`resolve_account_keys_in`] is the other half). A composition root
//! reads both and hands the pair down; nothing below a binary opens the store for it.
//!
//! ⚠ **It answers in THREE states, not two**, and [`Accounts`] is an enum for that reason alone. A
//! box on [`Backend::Files`] — or one whose database predates the table — has no `account` table at
//! all, and answering *no accounts* there would be an assertion about a store holding sixteen of
//! them under their legacy key names. [`NoAccountTable`] is that third state, and it names the
//! store that IS answering so the caller can fall back to
//! `vike_model::account_keys::accounts_in_store`, which is what every caller uses today.
//!
//! The reader selects from `account` alone and never from `credential`, so nothing reachable from
//! it — no row, no error, no `Debug` — can be a credential value.
//!
//! ⚠ **That purity is also its limit, and [`resolve_account_keys_in`] is the companion it forces.**
//! An [`Account`] is `(id, venue, tier, label, venue_account_id, …)`, and for the pair this schema's
//! hardest case is about — dukascopy's two demo rows — every one of those cells is identical except
//! `id`. So a human rendering built from the reader ALONE shows the two accounts somebody must
//! choose between as two lines differing by an opaque integer. The fact that separates them is in
//! the `credential` table: each row's own key NAMES, `DUKASCOPY_DEMO1_*` against
//! `DUKASCOPY_DEMO2_*`. [`resolve_account_keys_in`] returns those names (and the owner prefixes
//! they imply) per `account.id` — `name` and `field` only, never `value` — so a listing can be
//! acted on. [`AccountKeys`] carries the whole argument.
//!
//! ⚠ **[`Account::id`] is stable for the life of ONE database file, not forever.** It is a SQLite
//! rowid with no `AUTOINCREMENT`, assigned in the order the migration meets credential names, and
//! this tree documents a recovery that DELETES the database and migrates again — after which the
//! same accounts come back numbered differently and hold no book at all. A `venue_account_id` an
//! operator wrote against the old ids would then name the wrong row, which on dukascopy is the
//! wrong legal entity. The field's own doc carries the two halves and the rule: after any
//! re-migration, identify every row again from its key names rather than from a remembered id.
//!
//! # …and ONE of its columns has a writer — [`set_venue_account_id_in`]
//!
//! `account.venue_account_id` is the BOOK, as the venue names it, and
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 recorded it as a column
//! nothing in this tree wrote. [`set_venue_account_id_in`] writes it for ONE row, addressed by
//! [`Account::id`], from a value an OPERATOR supplies.
//!
//! ⚠ **It is one of the column's THREE sources and the narrowest, and confusing them is the thing
//! to avoid.** The other two are the migration's FOLD of the ten stored keys that already are the
//! book (§11 step 3) and the venue's own HANDSHAKE (§12) — neither is built, and §7 fixes the
//! sequencing that keeps the fold from shipping first. What this door is for is the case neither
//! can reach: a book that is in no store and derivable from nothing in one, which today is
//! dukascopy's two demo accounts — `(dukascopy, demo, label = NULL)` twice over, distinguishable
//! only by `id`, with numbers that were read off the venue by logging in.
//!
//! ⚠ **…and since 2026-09-15 the HANDSHAKE source is built too, through the SAME function.**
//! [`BookSource`] is the parameter that tells it which claim it is recording: `Operator` is the
//! paragraph above, `Handshake` is a fold of what a venue's own authenticated session answered —
//! and it is the only thing in this tree that writes [`Account::last_verified_at`], a column that
//! had no writer anywhere until then. A second write FUNCTION was the obvious shape and is the one
//! `crates/vike-ops/tests/credential_writer_gate.rs` exists to refuse, so there is still one writer.
//!
//! ⚠ It does NOT reach here from a live daemon, and that is a measured constraint rather than a
//! preference: the shipped unit runs under `ProtectSystem=strict` with only
//! `<project>/settings/state` writable, so a mount-time `UPDATE` of this database fails `EROFS`. A
//! mount PARKS what it learned (`vike_model::account_confirmation`) and `vike-cli secrets confirm`
//! folds it from a process that is not sandboxed.
//!
//! It is a targeted `UPDATE` of one column of one row: no `credential` row is read or written, no
//! account is created, no database is created, and neither credential FILE is touched. On a
//! [`Backend::Files`] box it REFUSES ([`DbErrorKind::NoDatabase`]) rather than falling back —
//! a file store has no `account` table, and a per-key fallback is exactly what [`Backend`] forbids.
//! `crate::db::set_venue_account_id`'s own doc carries the refusal list, including the one that
//! matters most: a row that already names a DIFFERENT book is not overwritten without the caller
//! saying so out loud.
//!
//! ⚠ **A `None` value is a CLEAR, and it is there because two of those refusals would otherwise be
//! a DEAD END.** If the pair is written the wrong way round, each row names the other's book —
//! and correcting either one is then refused in BOTH directions: `replace` gets past *this row
//! already names a different book*, and ruling 11's one-account-per-book check immediately finds
//! the other row. Every ordering of two writes hits it. Clearing one row first is the third move
//! that makes the repair reachable, and it asserts nothing about a broker: `NULL` is the state
//! every migrated row is already in.
//!
//! `<project>` is resolved at RUNTIME by walking UP for a project marker — see
//! [`project_settings_dir`] for the two markers and their dispositions — and
//! `VIKE_SETTINGS_DIR` ([`SETTINGS_DIR_ENV`]) names the directory outright when a deployment wants
//! to. That value arrives as a PARAMETER: this crate performs no environment read of its own, so
//! nothing here joins the settings registry's `Layer::Library` work-list.
//!
//! # Absent credentials ARE the live gate
//!
//! No store ⇒ an EMPTY map ⇒ every venue loader returns `None` ⇒ every venue stays paper. That is
//! the designed behaviour, not a degradation, and it is why [`resolve`] treats an absent file as an
//! answer rather than an error. A store that EXISTS and cannot be read is the opposite case and does
//! error: "not configured" and "cannot open" must never look the same to an operator.
//!
//! # Writing the store
//!
//! Nothing here ever REGENERATES, reorders or deletes the store — it is the user's only copy of
//! live venue credentials. [`upsert_env`]/[`save_credentials`] are the one sanctioned write, and
//! they are a byte-preserving UPSERT: named keys are replaced in place, new ones appended, and
//! every comment, blank line and unrelated key survives verbatim, written back atomically. That is
//! what makes the store a safe home for a credential that ROTATES (a venue's OAuth grant) as well
//! as for one a human typed. See `env_write`'s module doc; a caller reaching for `fs::write` on
//! this file is the bug it exists to prevent.
//!
//! ⚠ **Since the database landed, a production writer calls [`save_credentials_to_store`] instead**
//! — the same upsert, routed to the store that ANSWERS. A write that names the file while the
//! database shadows it succeeds, changes the file, is journalled, reports success, and is read by
//! nothing. [`save_credentials_to_store`] and [`resolve_store_in`] take the same two arguments and
//! ask the same [`backend_in`], so the writer and the reader cannot disagree; the file branch is
//! [`save_credentials`] verbatim, so the byte-preservation rule above is unchanged wherever it still
//! applies. `crates/vike-ops/tests/credential_writer_gate.rs`'s `WRITER_CALLERS` is the pinned set
//! of files allowed to call either.
//!
//! ⚠ **The gate and a botched UPGRADE produce the same empty map**, which is why [`resolve`]'s
//! absent arm also reports [`legacy_store_warning`]: a `<project>/.env` — the store's predecessor —
//! left in place while the new one was never created. That is a FINDING and never a refusal, because
//! a `.env` also has a legitimate second life as a systemd `EnvironmentFile`. See
//! [`LegacyStoreWarning`], and `docs/ops/upgrading.md` for the whole upgrade path.
//!
//! # Layering
//!
//! **NO `vike-*` dependency, and exactly ONE external crate** (`tempfile` is a dev-dep for the
//! project-walk tests). Two consumers need this tree and must not drag each other in:
//! `vike-bridge-core`, which owns the venue transport stack, and `vike-cli`, which is
//! DataFusion-free, transport-free and rides the FAST CI lane and would otherwise have had to link
//! `ureq`/`tungstenite`/`rustls` to read a `KEY=VALUE` file. That property is untouched: neither
//! consumer gains a transport, a TLS stack or a query engine from the one edge below.
//!
//! ⚠ **This section read "**ZERO dependencies** — no `vike-*` crate and no external crate either"
//! until 2026-09-13**, and the external half stopped being true that day.
//! `docs/decisions/0054-settings-move-into-one-database.md` was accepted, moving settings,
//! credentials and node keys into one SQLite database with the credential half ruled NOT severable
//! — so the store this module owns acquires a database home, and `rusqlite` (`bundled`,
//! `default-features = false`) is declared here ahead of any migration code. The claim is corrected
//! rather than deleted because it was load-bearing in five other files, and because what it bought
//! is only half gone: the fast-lane argument survives intact, while "this crate widens NOTHING"
//! does not — the edge costs four packages on `vike-cli`'s normal tree and the first C compilation
//! in that graph. `crates/vike-secrets/Cargo.toml` carries the rest, the root manifest carries the
//! engine argument, and `crates/vike-boot/tests/dependency_floor.rs`'s `FLOOR_EXCEPTIONS` is the
//! gate row that admits it WITHOUT weakening the floor's criterion for anything else.
//!
//! The consequence for paths is that this crate never resolves a directory it was not given:
//! [`project_settings_dir`] walks up from a `&Path` the caller supplies, and the only `std::env`
//! contact anywhere here is [`workspace_dotenv_path`]'s `current_dir`.
//!
//! `vike_bridge_core::credentials` re-exports [`parse_dotenv`], [`workspace_dotenv_path`] and
//! [`load_workspace_dotenv`] under their historical paths, so all ~179 existing call sites are
//! unchanged.
//!
//! # Redaction
//!
//! Matching `vike_bridge_core::credentials::Credentials` exactly — a manual `Debug` that redacts,
//! with tests asserting it. [`SecretMap`] has no `Display` at all and a `Debug` that prints key
//! NAMES and never a value; reaching the plaintext requires [`SecretMap::into_map`], whose name
//! says so. [`SecretsError`] carries a path and an OS reason, never file contents.

mod db;
mod dotenv;
mod env_write;
mod schema;
mod store;

pub use db::{
    ACCOUNT_TABLE_SCHEMA, Account, AccountKeys, Accounts, Ambiguity, BookSource, BookWrite,
    DbError, DbErrorKind, MigrateError, Migration, MigrationOutcome, MigrationPlan, NoAccountTable,
    PlannedOutcome, READABLE_SCHEMA_VERSIONS, SCHEMA_VERSION, SourceReport, Table,
    VENUE_ACCOUNT_ID_MAX_BYTES, database_present, is_sqlite_file, migrate,
    normalized_venue_account_id, preview, read_account_keys, read_accounts, read_table,
    set_venue_account_id,
};
#[cfg(feature = "test-support")]
pub use db::{SCHEMA_1, plant_schema_1};
pub use dotenv::{
    DB_DIR, DB_FILE, NODE_FILE, SECRETS_FILE, SETTINGS_DIR, SETTINGS_DIR_ENV, STATE_DIR,
    db_path_for, db_path_in, load_workspace_dotenv, load_workspace_dotenv_from, node_path_for,
    node_path_in, parse_dotenv, project_secrets_path, project_secrets_path_from,
    project_settings_dir, project_settings_dir_for, project_settings_dir_from, secrets_path_in,
    settings_dir_of_store, settings_dir_or_last_resort, workspace_db_path_from,
    workspace_dotenv_path, workspace_dotenv_path_from, workspace_node_path_from,
    workspace_settings_dir_from,
};
pub use env_write::{save_credentials, upsert_env};
pub use schema::{
    ACCOUNT_TIERS, AccountKey, Classification, FileComments, PendingMove, Placement, RowReport,
    SchemaRefusal, scan_comments,
};
pub use store::{
    Backend, Finding, LEGACY_STORE_FILE, LegacyStoreWarning, NodeKeySource, PermissionWarning,
    Resolved, SecretMap, SecretsError, ShadowedStore, Source, backend_at, backend_in,
    legacy_node_key_notice, legacy_store_warning, permission_warning, resolve,
    resolve_account_keys, resolve_account_keys_in, resolve_accounts, resolve_accounts_in,
    resolve_node_keys, resolve_project, resolve_store_in, save_credentials_to_store,
    set_venue_account_id_in, workspace_backend_from,
};
