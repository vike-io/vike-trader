//! `vike-secrets` — the credential store: ONE database, in the project.
//!
//! ```text
//! <project>/settings/db/vike.db       the settings database: `credential`, `account`,
//!                                     `venue_setting`, `venue` and `node_key`
//! ```
//!
//! That is the whole story. [`resolve_project`] asks for the project's own store, and
//! [`resolve_store_in`] for a caller that already holds the settings directory. There is no
//! precedence and no second location.
//!
//! # ⚠ The credential FILE store is GONE (2026-10-07)
//!
//! Until the owner's order of 2026-10-07 (*"we don't use any files anymore, we use sqlite: remove
//! the file plane"*), a project with NO database was answered by `<project>/settings/secrets.env`
//! and `node.env`: parsed on every read, rewritten by a byte-preserving upsert on every write. That
//! plane — the `Files` arm of [`Backend`], the file reader, the file writer (`env_write.rs`, with
//! `save_credentials` and `upsert_env`) — is deleted. Its precondition was `vike-cli secrets migrate
//! --init`, which creates the store directly in the database
//! (`docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`).
//!
//! What a box with no database gets now: an EMPTY map from every read (the live gate — every venue
//! paper), a REFUSAL from every write naming [`CREATE_STORE_REMEDY`], and — if a `secrets.env` or
//! `node.env` is still on disk — an [`UnreadCredentialFile`] finding saying, out loud, that the file
//! is NOT READ and that `vike-cli secrets migrate` carries it in. That carry is the one reader of a
//! credential file left in the workspace, read-only, and lives beside [`migrate`].
//!
//! ⚠ That line read *"`credential` and `node_key` tables"* until schema 2 landed, then *"Two more
//! exist now"* naming `account` and `venue_setting` until the settings-store-plane spec's stage 2
//! added a fifth: `venue`. **This paragraph is self-aware about rotting once already in exactly this
//! shape and rotted a second time anyway — read that as the reason to keep it current, not as
//! permission to let it happen a third.** The three beyond the original pair are at three different
//! ends of being useful: [`Account`] has this reader; `venue_setting` has its own,
//! [`venue_setting::load_venue_settings`] into [`venue_setting::VenueSettings`], which every venue
//! mount reads (⚠ from 2026-09-22 until decision 0095's Task 7 the store ALSO folded its rows back
//! into the legacy credential names through [`venue_setting::venue_setting_names`], inside
//! [`resolve_store_in`] and [`resolve_store_scoped_in`]; that fold is retired, and a credential row
//! under one of those names now refuses the daemons' start. Before either, this clause said the
//! table was *deliberately EMPTY*; that stopped being true when `vike-cli secrets
//! move-venue-config` shipped the writer that fills it); and `venue`
//! is a
//! PROJECTION of the compiled `vike_model::VENUES` roster with no reader of its own AT
//! ALL — it exists only so `account`/`credential`/`venue_arming`/`venue_setting`'s own nullable
//! `venue_id` columns have a foreign-key target, and `crate::schema::DDL`'s own module doc carries
//! that argument in full.
//!
//! 0054 is accepted and its credential half is **not severable** by the owner's ruling. [`migrate`]
//! fills `credential` and `node_key` from the two files — that read *"those two tables"* while the
//! block above listed exactly two, and widening the block moved the antecedent rather than the
//! fact; `venue_setting` is created empty and `account`'s rows are derived by the classifier, so
//! neither is filled FROM a file — `vike-cli secrets migrate` is what calls it, and
//! [`preview`] is that same decision with the write left out, for the `--dry-run` an irreversible
//! first run deserves; [`resolve_project`] and [`resolve_node_keys`] then
//! read the tables when that database exists and answer EMPTY when it does not. `crate::db` carries
//! the schema, the `journal_mode = DELETE` argument and the modes; [`Backend`] carries the one that
//! matters most here — **the choice is per-RUN, not per-KEY**: still no precedence, still no second
//! location, still one home per name. A per-key fallback would have been the ladder
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids, and it is not what this does.
//!
//! **What has NOT happened, and is not this crate's act to perform:** `secrets.env` is not deleted,
//! moved, truncated or rewritten by any of it. [`migrate`] reads it and writes elsewhere. Retiring
//! the file is an operator act, and [`ShadowedStore`] (beside a database) and
//! [`UnreadCredentialFile`] (with none) are how a reader is told the file is not read.
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
//! and that stopped being true on 2026-09-15.** The dukascopy mount (`vike_mount`'s dukascopy arm
//! then, `crates/bridges/dukascopy/src/mount.rs` since the venue mount contract) resolves WHICH
//! ACCOUNT — and therefore which LEGAL ENTITY an order reaches — from these rows, by the
//! credential-key OWNER PREFIX each one owns ([`resolve_account_keys_in`] is the other half). A
//! composition root reads both and hands the pair down; nothing below a binary opens the store for
//! it.
//!
//! ⚠ **It answers in THREE states, not two**, and [`Accounts`] is an enum for that reason alone. A
//! box with no database ([`Backend::Absent`]) — or one whose database predates the table — has no
//! `account` table at all, and answering *no accounts* there would be an assertion about a table
//! nobody could read. [`NoAccountTable`] is that third state, and it names why.
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
//! rowid, assigned in the order the migration meets credential names. ⚠ **This paragraph used to
//! say *"and since the account lifecycle landed, not unconditionally even within one file"* and
//! listed TWO things that free a number; stage 4 removed the second.** What is left:
//!
//! * the recovery this tree documents — DELETE the database and migrate again, after which the same
//!   accounts come back numbered differently and hold no book at all. `AUTOINCREMENT` cannot reach
//!   this case: the high-water mark lives in the file that was deleted;
//! * ~~**[`AccountEdit::Remove`]**, which deletes a row. Without `AUTOINCREMENT` a new row is
//!   handed `max(rowid) + 1`, so removing the row with the LARGEST id frees that id for the next
//!   `Add`.~~ **CLOSED** by the spec's §4.1 (`id INTEGER PRIMARY KEY AUTOINCREMENT`, landed by
//!   §9's stage 4): the engine keeps the mark, a `DELETE` does not move it, and
//!   `crates/vike-secrets/tests/accounts/lifecycle.rs`'s
//!   `a_removed_id_is_never_handed_to_the_next_created_account` is the pin. The remaining duty is
//!   on every REBUILD of these tables, which must carry the mark across —
//!   `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs`.
//!
//! A `venue_account_id` an operator wrote against the old ids would then name the wrong row, which
//! on dukascopy is the wrong legal entity. The field's own doc carries all of it and the rule is one
//! sentence: **identify a row by its credential key names, never by a remembered id** — which is why
//! every verb echoes those names before it acts.
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
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` exists to refuse, so there is still one writer.
//!
//! ⚠ It does NOT reach here from a live daemon, and that is a measured constraint rather than a
//! preference: the shipped unit runs under `ProtectSystem=strict` with only
//! `<project>/settings/state` writable, so a mount-time `UPDATE` of this database fails `EROFS`. A
//! mount PARKS what it learned (`vike_model::accounts::account_confirmation`) and `vike-cli secrets confirm`
//! folds it from a process that is not sandboxed.
//!
//! It is a targeted `UPDATE` of one column of one row: no `credential` row is read or written, no
//! account is created, no database is created, and neither credential FILE is touched. On a
//! [`Backend::Absent`] box it REFUSES ([`DbErrorKind::NoDatabase`]) — there is no store at all.
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
//! the designed behaviour, not a degradation, and it is why a box with no database is an answer
//! rather than an error. A store that EXISTS and cannot be read is the opposite case and does
//! error: "not configured" and "cannot open" must never look the same to an operator.
//!
//! # Writing the store
//!
//! Nothing here ever REGENERATES, reorders or deletes the store — it may be the user's only copy of
//! live venue credentials. [`save_credentials_to_store`] is the one sanctioned credential write: an
//! UPSERT of the named rows in one transaction, every other row untouched. It asks the same
//! [`backend_in`] [`resolve_store_in`] asks, so the writer and the reader cannot disagree, and with
//! no database it REFUSES rather than writing anywhere else.
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s `WRITER_CALLERS` is the pinned
//! set of files allowed to call it.
//!
//! ⚠ **The gate and a botched UPGRADE produce the same empty map**, which is why the absent store
//! reports what it can see: an [`UnreadCredentialFile`] when a `secrets.env`/`node.env` is still on
//! disk, and [`legacy_store_warning`] when a `<project>/.env` — the store's oldest predecessor — is.
//! Both are FINDINGS and never refusals. See `docs/ops/upgrading.md` for the whole upgrade path.
//!
//! # Layering
//!
//! **NOTHING ABOVE RANK 10 — the vocabulary floor — and exactly ONE external crate** (`tempfile`
//! is a dev-dep for the
//! project-walk tests). That is tier 15's rule and it is machine-checked:
//! `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary`. Two consumers need this tree and must
//! not drag each other in:
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
//! ⚠ **The `vike-*` half fell too, on 2026-09-20** — the sentence above kept reading "NO `vike-*`
//! dependency" after
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` admitted `vike-model`,
//! and this section is one of ~19 places in this crate and its two neighbours that said so. 0072's
//! own *What dies* claimed they went with the walk; they did not, which is why this amendment is
//! dated separately from the one above it.
//!
//! ⚠ **Do not replace that clause with "it declares `vike-model`" either** — that is a fact about
//! one EDGE, and the tree has already ruled on the sentence it should be. Tier 15's own description
//! carried the same stricter wording and was repaired on 2026-09-23 for exactly this reason, when
//! four pure-compute crates joined the band and would each have needed an exception row: the rule
//! is AT MOST THE FLOOR, so the property to state is the RANK. It survives the next edge and the
//! next occupant, and `every_tier_15_crate_names_nothing_above_the_vocabulary` enforces it.
//!
//! The four properties the old rule actually protected all
//! survive under it: no transport, no TLS stack, no query engine, and no
//! new package in `vike-cli`'s graph — `vike-model` is already a normal dependency of all ten
//! crates that declare this one. What the edge BOUGHT is that `dotenv.rs`'s project walk stopped
//! being a
//! second spelling of `vike_model::paths::state_path`'s; `crates/vike-bridge-core/tests/settings_dir_spellings.rs`
//! is the retired pin that certified the merge and now holds the property it bought.
//!
//! The consequence for paths is that this crate never resolves a directory it was not given:
//! [`project_settings_dir`] walks up from a `&Path` the caller supplies, and the only `std::env`
//! contact anywhere here is [`workspace_dotenv_path`]'s `current_dir`.
//!
//! `vike_bridge_core::credentials` re-exports [`workspace_dotenv_path`] and
//! [`load_workspace_dotenv`] under their historical paths, so the existing call sites are
//! unchanged; the names are historical, and the loader reads the database.
//!
//! # Redaction
//!
//! Matching `vike_bridge_core::credentials::Credentials` exactly — a manual `Debug` that redacts,
//! with tests asserting it. [`SecretMap`] has no `Display` at all and a `Debug` that prints key
//! NAMES and never a value; reaching the plaintext requires [`SecretMap::into_map`], whose name
//! says so. [`SecretsError`] carries a path and an OS reason, never file contents.

mod db;
mod dotenv;
pub mod live_means_mainnet;
pub mod profile_store;
mod schema;
mod settings;
mod store;
pub mod venue_links;
pub mod venue_setting;

pub use db::{
    ACCOUNT_TABLE_SCHEMA, Account, AccountEdit, AccountKeys, AccountWrite, Accounts, Ambiguity,
    BookSource, BookWrite, DbError, DbErrorKind, MigrateError, Migration, MigrationOutcome,
    MigrationPlan, MovedRows, NoAccountTable, PlannedOutcome, READABLE_SCHEMA_VERSIONS,
    SCHEMA_VERSION, SourceReport, Table, VENUE_ACCOUNT_ID_MAX_BYTES, VenueRow, WhenNothingToCarry,
    database_present, edit_account, migrate, move_pending_rows, normalized_account_label,
    normalized_venue_account_id, preview, read_account_keys, read_accounts,
    read_credentials_demo_only, read_present_names_scoped, read_table, read_table_scoped,
    read_venues, set_venue_account_id,
};
#[cfg(feature = "test-support")]
pub use db::{SCHEMA_1, create_empty_store_for_test, plant_schema_1};
pub use dotenv::{
    DB_DIR, DB_FILE, NODE_FILE, SECRETS_FILE, SETTINGS_DIR_ENV, db_path_for, db_path_in,
    load_workspace_dotenv, load_workspace_dotenv_from, node_path_for, node_path_in,
    project_secrets_path, project_secrets_path_from, project_settings_dir,
    project_settings_dir_for, project_settings_dir_from, secrets_path_in, settings_dir_of_store,
    settings_dir_or_last_resort, workspace_db_path_from, workspace_dotenv_path,
    workspace_dotenv_path_from, workspace_node_path_from, workspace_settings_dir_from,
};
pub use schema::{
    ACCOUNT_TIERS, AccountKey, Classification, DDL, FileComments, PAPER_TIER, PendingMove,
    Placement, RowReport, SIM_KEY_TOKEN, SchemaRefusal, account_tier_named,
    account_tier_of_key_token, key_token_of_account_tier, scan_comments,
};
pub use settings::{
    Adoption, ArmingRow, ProfileRiskRow, ProfileRiskSource, ProfileRiskWritten, RowChange,
    RowWriteError, RowWritten, SETTINGS_SECTIONS, SettingRow, SettingsSource, SettingsWritten,
    StoredProfileRisk, StoredSettings, VenueSettingRefusal, VenueSettingRow, clear_adoption,
    read_profile_risk, read_profile_risk_in, read_settings, read_settings_in, section_is_known,
    set_venue_setting_in, set_venue_setting_in_journalled, write_adoption, write_profile_risk,
    write_profile_risk_in, write_setting_row_in, write_settings, write_settings_in,
};
#[cfg(feature = "test-support")]
pub use settings::{hold_write_lock, plant_settings_rows};

pub use store::{
    AccountJournal, Backend, CREATE_STORE_REMEDY, CredentialJournal, Finding, JournalAppendError,
    KeyScope, LEGACY_STORE_FILE, LegacyStoreWarning, Lookup, PermissionWarning, Resolved,
    ScopedSecrets, SecretMap, SecretsError, ShadowedStore, Source, UndeclaredKey,
    UnreadCredentialFile, backend_at, backend_in, edit_account_in, edit_account_in_journalled,
    legacy_store_warning, permission_warning, present_names_scoped_in, read_venues_in,
    resolve_account_keys, resolve_account_keys_in, resolve_accounts, resolve_accounts_in,
    resolve_node_keys, resolve_project, resolve_project_scoped, resolve_store_demo_only_in,
    resolve_store_in, resolve_store_scoped_in, save_credentials_to_store,
    save_credentials_to_store_journalled, set_venue_account_id_in, unread_credential_file,
    withheld_by_demo_scope, workspace_backend_from,
};
