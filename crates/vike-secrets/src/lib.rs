//! `vike-secrets` — the credential store: ONE database, in the project.
//!
//! ```text
//! <project>/settings/db/vike.db       the settings database: `credential`, `account`,
//!                                     `venue_setting`, `venue` and `hub_key`
//! ```
//!
//! That is the whole story. [`CredentialStore`] is the ONE handle every read goes through: a
//! [`StoreLocation`] (the settings directory a root resolved once) and a [`CredentialScope`], with
//! no I/O until a read. There is no precedence and no second location.
//!
//! # One handle, one scope, every read under it
//!
//! The scope is a property of the HANDLE ([`CredentialScope::NoLiveKeys`] is the process that may
//! hold no live or real-money key, switched on by the ROOT from [`NO_LIVE_KEYS_ENV`]), and it is
//! applied in the statement of every read the handle offers: [`CredentialStore::read_all`],
//! [`CredentialStore::read`], [`CredentialStore::present_names`],
//! [`CredentialStore::account_keys`] and [`CredentialStore::hub_keys`]. There is no public read of
//! credential rows beside them, so the scope cannot be read around by picking another door. A read
//! never logs: its answer carries every finding as data and [`Resolved::report`] (or
//! [`ScopedSecrets::report`]) is the ONE place a finding becomes a line.
//!
//! Every public WRITER records its change in the change journal beside the store
//! ([`save_credentials_to_store`], [`retire_credential_in_store`], [`edit_account_in`],
//! [`set_venue_account_id_in`], [`set_venue_setting_in`]); the record-less bodies are public to a
//! test only, through the `test-support` feature.
//!
//! # The settings database is the only store
//!
//! Settings and credentials live ONLY in SQLite (`docs/decisions/0054-settings-move-into-one-database.md`,
//! `docs/decisions/0086-settings-live-only-in-the-database.md`). `vike-cli secrets init`
//! creates the EMPTY store on a fresh box ([`create_store`]), and `vike-cli secrets set` writes a
//! key into it. Nothing converts a store of an older shape
//! (`docs/decisions/0117-there-are-no-migrations.md`).
//!
//! What a box with no database gets: an EMPTY map from every read (the live gate — every venue
//! paper) and a REFUSAL from every write naming [`CREATE_STORE_REMEDY`].
//!
//! ⚠ The block at the top read *"`credential` and `hub_key` tables"* until schema 2 landed, then *"Two more
//! exist now"* naming `account` and `venue_setting` until the settings-store-plane spec's stage 2
//! added a fifth: `venue`. **This paragraph is self-aware about rotting once already in exactly this
//! shape and rotted a second time anyway — read that as the reason to keep it current, not as
//! permission to let it happen a third.** The three beyond the original pair are at three different
//! ends of being useful: [`Account`] has this reader; `venue_setting` has its own,
//! [`venue_setting::load_venue_settings`] into [`venue_setting::VenueSettings`], which every venue
//! mount reads (⚠ from 2026-09-22 until decision 0095's Task 7 the store ALSO folded its rows back
//! into the legacy credential names, inside the store's whole-map and scoped reads; that fold is
//! retired, and a credential row
//! under one of those names is read by nothing); and `venue` is a PROJECTION of the compiled
//! `vike_model::VENUES` roster with no reader of its own AT ALL — it exists only so `account`/`credential`/`venue_setting`'s own nullable
//! `venue_id` columns have a foreign-key target, and `crate::schema::DDL`'s own module doc carries
//! that argument in full.
//!
//! 0054 is accepted and its credential half is **not severable** by the owner's ruling. [`create_store`]
//! creates the database (asked to) and refuses one at any other schema — `vike-cli secrets init`
//! is what calls it, and [`preview_create_store`] is that same decision with the write left out,
//! for the `--dry-run` an irreversible act deserves; [`CredentialStore::read_all`] and
//! [`CredentialStore::hub_keys`] then read the tables when that database exists and answer EMPTY when it does not. `crate::db` carries the schema, the `journal_mode = DELETE` argument and the modes;
//! [`Backend`] carries the one that matters most here — **the choice is per-RUN, not per-KEY**: no
//! precedence, no second location, one home per name. A per-key fallback would have been the
//! ladder `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids, and it is not what
//! this does.
//!
//! # The `account` table has a reader — [`CredentialStore::accounts`]
//!
//! The schema gives the account a ROW, and until this reader existed the only `SELECT … FROM
//! account` in the tree was inside the writer that fills it. [`CredentialStore::accounts`] is the seam
//! that lets a caller ask the store *which accounts exist for this venue, and what is each one's
//! identity* — [`Account::id`], the permanent opaque handle, rather than a number parsed back out
//! of a key name.
//!
//! ⚠ **It ARMS every venue.** Each account trades at its row's tier while active (decision 0119), and
//! its credentials are the rows filed against it (decision 0156 item 6): a composition root reads
//! the rows, their key names ([`CredentialStore::account_keys`]) and their credentials
//! ([`CredentialStore::credential_book`]) once and hands the three down as one
//! [`account_directory::AccountDirectory`]; nothing below a binary opens the store for it.
//!
//! ⚠ **It answers in THREE states, not two**, and [`Accounts`] is an enum for that reason alone. A
//! box with no database ([`Backend::Absent`]) has no `account` table at all, and answering *no
//! accounts* there would be an assertion about a table nobody could read. [`NoAccountTable`] is
//! that third state, and it names why.
//!
//! The reader selects from `account` alone and never from `credential`, so nothing reachable from
//! it — no row, no error, no `Debug` — can be a credential value.
//!
//! ⚠ **That purity is also its limit, and [`CredentialStore::account_keys`] is the companion it forces.**
//! An [`Account`] is `(id, venue, tier, label, venue_account_id, …)`, and for the pair this schema's
//! hardest case is about — dukascopy's two demo rows — every one of those cells is identical except
//! `id`. So a human rendering built from the reader ALONE shows the two accounts somebody must
//! choose between as two lines differing by an opaque integer — which is why decision 0156 item 6
//! gives such accounts LABELS. [`CredentialStore::account_keys`] returns each row's credential key
//! NAMES per `account.id` — `name` and `field` only, never `value` — so a listing can show what an
//! account holds. [`AccountKeys`] carries the argument.
//!
//! ⚠ **[`Account::id`] is stable for the life of ONE database file, not forever.** It is a SQLite
//! rowid, assigned in the order accounts are created. ⚠ **This paragraph
//! used to say *"and since the account lifecycle landed, not unconditionally even within one file"* and
//! listed TWO things that free a number; stage 4 removed the second.** What is left:
//!
//! * a store rebuilt from scratch (the database deleted, `secrets init` run again and the keys set
//!   again), after which the same accounts come back numbered differently and hold no book at
//!   all. `AUTOINCREMENT` cannot reach this case: the high-water mark lives in the file that was
//!   deleted;
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
//! to avoid.** The other two are a FOLD of the ten stored keys that already are the book (§11
//! step 3, never built: there are no migrations) and the venue's own HANDSHAKE (§12). What this
//! door is for is the case neither can reach: a book that is in no store and derivable from nothing in one, which today is
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
//! mount PARKS what it learned ([`account_confirmation`]) and `vike-cli secrets confirm`
//! folds it from a process that is not sandboxed.
//!
//! It is a targeted `UPDATE` of one column of one row: no `credential` row is read or written, no
//! account is created, and no database is created. On a
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
//! every row starts in.
//!
//! `<project>` is resolved at RUNTIME by walking UP for a project marker — see
//! `vike_paths::state_path::project_settings_dir` for the two markers and their dispositions — and
//! `VIKE_SETTINGS_DIR` ([`SETTINGS_DIR_ENV`]) names the directory outright when a deployment wants
//! to. That value arrives as a PARAMETER: this crate performs no environment read of its own, so
//! nothing here joins the settings registry's `Layer::Library` work-list. A root that runs no boot
//! hands its whole environment sweep to [`open_credential_store`], which takes that value and
//! `VIKE_NO_LIVE_KEYS` ([`NO_LIVE_KEYS_ENV`]) out of the map and opens the store.
//!
//! # Absent credentials ARE the live gate
//!
//! No store ⇒ an EMPTY map ⇒ every venue loader returns `None` ⇒ no `demo` or `live` account runs. That is
//! the designed behaviour, not a degradation, and it is why a box with no database is an answer
//! rather than an error. A store that EXISTS and cannot be read is the opposite case and does
//! error: "not configured" and "cannot open" must never look the same to an operator.
//!
//! # Writing the store
//!
//! Nothing here ever REGENERATES, reorders or deletes the store — it may be the user's only copy of
//! live venue credentials. [`save_credentials_to_store`] is the one sanctioned credential write: an
//! UPSERT of the named rows in one transaction, every other row untouched, then its record in the
//! change journal. It asks the same [`backend_in`] [`CredentialStore::backend`] asks, so the writer
//! and the reader cannot disagree, and with no database it REFUSES rather than writing anywhere
//! else.
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s `WRITER_CALLERS` is the pinned
//! set of files allowed to call it.
//!
//! # Layering
//!
//! **NOTHING ABOVE RANK 10 — the vocabulary floor — but the change journal one rung over it, and
//! ONE external crate of weight, `rusqlite`, plus the `tracing` facade** (`tempfile` and
//! `vike-log`'s capture are dev-deps for the project-walk and log-line tests). Its floor edges are
//! `vike-model`, `vike-util` and `vike-paths`; the journal edge (`vike-change-journal`, rank 12) is
//! the one declared row of tier 15's exception table (the last paragraph of this section). That is
//! tier 15's rule plus that row, and both are machine-checked:
//! `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary`. Two consumers need this tree and must
//! not drag each other in:
//! `vike-bridge-core`, which owns the venue transport stack, and `vike-cli`, which is
//! DataFusion-free, transport-free and rides the FAST CI lane and would otherwise have had to link
//! `ureq`/`tungstenite`/`rustls` to read the store. That property is untouched: neither
//! consumer gains a transport, a TLS stack or a query engine from the one edge below.
//!
//! ⚠ **This section read "**ZERO dependencies** — no `vike-*` crate and no external crate either"
//! until 2026-09-13**, and the external half stopped being true that day.
//! `docs/decisions/0054-settings-move-into-one-database.md` was accepted, moving settings,
//! credentials and hub keys into one SQLite database with the credential half ruled NOT severable
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
//! crates that declare this one. What the edge BOUGHT is that `store_locator.rs`'s project walk
//! stopped being a second spelling of `vike_paths::state_path`'s; `crates/vike-bridge-core/tests/settings_dir_spellings.rs`
//! is the retired pin that certified the merge and now holds the property it bought.
//!
//! The consequence for paths is that this crate never resolves a directory it was not given:
//! [`project_settings_dir_for`] walks up from a `&Path` the caller supplies, and the only `std::env`
//! contact anywhere here is the `current_dir` of the `workspace_*` resolvers
//! ([`workspace_settings_dir_from`], and the crate-private one `create_store` uses).
//!
//! ⚠ **…and `tracing` joined on 2026-10-11**, the FACADE alone (the macros; no subscriber, no
//! appender, no `vike-log`). Measured by what it bought: the scoped loaders (now
//! [`CredentialStore::read`] and [`ScopedSecrets::report`]) and [`warn_once`], the one
//! per-process dedupe of a store finding, moved down from `vike-bridge-core` so a binary that needs
//! ONE API key (the `vike-backfill` vendor bins) loads it without linking the transport stack. The
//! facade is already linked by every binary that logs; it is no transport, no TLS stack and no
//! query engine, which are the properties above.
//! The READS still return a finding as DATA (a caller that runs before a subscriber exists, such as
//! `vike_boot`, must not have a line chosen for it); only [`Resolved::report`] and
//! [`ScopedSecrets::report`] log, and both log a permission finding through the one [`warn_once`].
//!
//! ⚠ **…and the change journal is one rung ABOVE the floor.** It was this crate's `change_journal`
//! module and moved out whole into `vike-change-journal` (layer 12), which every public writer here
//! records into. That one edge is a declared row of
//! `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s `TIER_15_EXCEPTIONS`; the journal
//! names only `vike-model`, `vike-util`, `serde` and `serde_json`, so the properties above survive
//! it.
//!
//! # The light half that moved down (decision 0135)
//!
//! Four public modules came down from `vike-bridge-core` on 2026-10-12, because each names only
//! `std`, `vike-model`, `tracing` and this crate, and the consumers that wanted nothing else from
//! that crate (`vike-cli`, `vike-gateway-panel`, `vike-desktop`) were linking it, and with it a
//! transport stack, for them: [`credentials`] (the generic HMAC loader, `StoreHealth` and
//! `attribution_code_from`; the whole-map loaders it carried
//! became [`CredentialStore`]),
//! [`account_directory`], [`venue_passphrase`] and [`key_permissions`]. Each is reached by its module
//! path only; nothing re-exports them at the crate root. They add no dependency: the band rule above
//! holds unchanged. (The HALT sentinel's FILE half came down with them and went on to `vike-tradehub-core`,
//! the composition layer that resolves it; this crate keeps no halt module.)
//!
//! # Redaction
//!
//! Matching [`credentials::Credentials`] exactly — a manual `Debug` that redacts,
//! with tests asserting it. [`SecretMap`] has no `Display` at all and a `Debug` that prints key
//! NAMES and never a value; reaching the plaintext requires [`SecretMap::into_map`], whose name
//! says so. [`SecretsError`] carries a path and an OS reason, never file contents.

#![warn(unreachable_pub)]

pub mod account_confirmation;
pub mod account_credentials;
pub mod account_directory;
pub mod credentials;
mod db;
pub mod key_permissions;
pub mod profile_store;
mod schema;
mod settings;
mod store;
mod store_locator;
pub mod venue_passphrase;
pub mod venue_setting;

#[cfg(feature = "test-support")]
pub use db::create_empty_store_for_test;
pub use db::{
    Account, AccountEdit, AccountKeys, AccountLeverage, AccountWrite, Accounts, ActiveTier,
    BookSource, BookWrite, CredentialOwner, DbError, DbErrorKind, IgnoredLiveLeverage, LANE_TWINS,
    LEVERAGE_VENUES, LaneTwin, LeverageMeans, LeverageVenue, MaxExposure, NoAccountTable,
    OwnedWrite, SCHEMA_VERSION, StoreCreation, StoreCreationPlan, StoreWatch, Table, UpsertIntent,
    VENUE_ACCOUNT_ID_MAX_BYTES, VenueRow, account_credential_name, create_store, database_present,
    is_lane_twin_label, lane_twin, lane_twin_symbol, leverage_refusal, leverage_venue,
    leverage_venue_at, normalized_account_label, normalized_venue_account_id, preview_create_store,
    read_venues, row_leverage_refusal, venue_credential_name,
};
// The raw row readers and the two raw account writers: a TEST's, never production's (which reads
// through `CredentialStore`, under its scope, and writes through the journalled writers below).
#[cfg(any(test, feature = "test-support"))]
pub use db::{
    edit_account, read_account_keys, read_accounts, read_present_names_scoped, read_table,
    read_table_scoped, set_venue_account_id,
};
pub use schema::DDL;
#[cfg(any(test, feature = "test-support"))]
pub use settings::set_venue_setting_in_unjournalled;
pub use settings::{
    Adoption, ProfileRiskRow, ProfileRiskSource, RowChange, RowWriteError, RowWritten,
    SETTINGS_SECTIONS, SettingRow, SettingsSource, SettingsWritten, StoredProfileRisk,
    StoredSettings, VenueSettingRefusal, VenueSettingRow, read_settings, read_settings_in,
    section_is_known, set_venue_setting_in, write_setting_row_in, write_settings,
    write_settings_in,
};
#[cfg(feature = "test-support")]
pub use settings::{hold_write_lock, plant_settings_rows};

pub use store::{
    AccountJournal, Backend, CREATE_STORE_REMEDY, CredentialJournal, CredentialScope,
    CredentialStore, Finding, JournalAppendError, KeyScope, Lookup, NO_LIVE_KEYS_ENV,
    PermissionWarning, Resolved, ScopeError, ScopedSecrets, SecretMap, SecretsError, Source,
    StoreLocation, UndeclaredKey, backend_at, backend_in, edit_account_in, permission_warning,
    read_venues_in, retire_credential_in_store, save_credentials_to_store, save_owned_credentials,
    set_venue_account_id_in, warn_once,
};
// The record-less write bodies: a TEST's, never production's (every public writer above journals).
#[cfg(any(test, feature = "test-support"))]
pub use store::{
    edit_account_in_unjournalled, retire_credential_in_store_unjournalled,
    save_credentials_to_store_unjournalled, save_owned_credentials_unjournalled,
    set_venue_account_id_in_unjournalled,
};
#[cfg(feature = "test-support")]
pub use store::{smoke_directory, smoke_store, smoke_vars};
pub use store_locator::{
    DB_DIR, DB_FILE, SETTINGS_DIR_ENV, db_path_for, db_path_in, open_credential_store,
    project_settings_dir_for, workspace_settings_dir_from,
};
