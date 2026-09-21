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
//! `crates/vike-secrets/tests/database_migration.rs`'s `no_sidecar_survives_a_clean_close` proves
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
//! it copied. `crates/vike-ops/tests/credential_writer_gate.rs` pins the set of files allowed to
//! write that store, and this one is deliberately not among them — nothing here opens either file
//! for writing at all.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// `OptionalExtension` is the trait that turns `QueryReturnedNoRows` into `Ok(None)`. It is imported
// rather than matched by hand because "no such row" is an ANSWER on the account-book write path —
// `DbErrorKind::NoSuchAccount` — and a hand-written match on one engine error variant beside a
// `map_err` that swallows every other one is how that answer would quietly become `Sqlite`.
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};

use crate::store::{SecretMap, SecretsError};

/// The schema this code WRITES. Lives in `PRAGMA user_version`.
///
/// ⚠ **It is no longer the only one this code READS** — see [`READABLE_SCHEMA_VERSIONS`], which is
/// the half that keeps an already-migrated box working the day this constant moves.
pub const SCHEMA_VERSION: i64 = 2;

/// **Every schema this code can read.** A database carrying any other number is
/// [`DbErrorKind::SchemaVersion`] — loudly unreadable rather than quietly empty, which is the
/// difference between "this box is not configured" and "this box is configured and cannot
/// authenticate".
///
/// # ⚠ Why this list exists at all, and why a bare version bump was unshippable
///
/// [`check_schema_version`] was equality against [`SCHEMA_VERSION`] with no upgrade arm, and BOTH
/// live boxes hold a schema-1 database. Bumping the constant alone would mean: the v2 binary opens
/// the v1 store, `check_schema_version` errors, and
/// `vike_bridge_core::credentials::load_workspace_secrets_at` — the infallible wrapper EVERY
/// composition root reaches through — logs one line and returns an EMPTY map. An empty credential
/// map is not an error downstream; it is the LIVE GATE. Every venue drops to paper with
/// `secrets.env` sitting on disk looking correct, and the release deploys automatically on a green
/// tag. `crates/vike-secrets/tests/database_migration.rs`'s
/// `an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one` already
/// pins that mechanism, in the opposite direction.
///
/// So the version bump and the reshape are **not one event**. A v2 binary READS a v1 store — the
/// read path branches on the version it found ([`read_table_on`]) — and [`migrate`] is what
/// performs the in-place upgrade, with `--dry-run` in front of it. Deploy order is then free: no
/// box is ever running a binary that cannot read what is on its disk.
///
/// ⚠ **Reading a v1 store is not the same as writing one.** Nothing here creates a schema-1
/// database any more, and [`upsert_rows`] against one is refused rather than half-supported: see
/// [`DbErrorKind::WriteToOlderSchema`].
pub const READABLE_SCHEMA_VERSIONS: [i64; 2] = [1, SCHEMA_VERSION];

/// The two credential namespaces, as tables.
///
/// A closed enum rather than a `&str` parameter on purpose: the table name is interpolated into SQL
/// (SQLite cannot bind an identifier), so making it impossible to name a table that is not one of
/// these two is what keeps that interpolation safe by construction rather than by review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Table {
    /// Venue credentials — everything the credential store holds today.
    Credential,
    /// The node keys `docs/decisions/0051-node-keys-live-in-their-own-store.md` split out.
    NodeKey,
}

impl Table {
    /// The SQL identifier. A `&'static str` from a closed enum — never operator input.
    #[must_use]
    pub fn sql_name(self) -> &'static str {
        match self {
            Table::Credential => "credential",
            Table::NodeKey => "node_key",
        }
    }

    /// The other one. Used by [`migrate`]'s two-namespace check.
    #[must_use]
    fn other(self) -> Table {
        match self {
            Table::Credential => Table::NodeKey,
            Table::NodeKey => Table::Credential,
        }
    }
}

impl std::fmt::Display for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.sql_name())
    }
}

/// The database did not open, did not answer, or is not this schema.
///
/// Carries the PATH and a reason, **never a row value** — every variant below is built from a path,
/// a table name, a key NAME or an engine error, and no arm of its `Display` formats a credential.
/// Same contract as [`SecretsError`], and for the same reason: an operator has to be able to read
/// "the store did not open" out of a daemon log without that log becoming a credential.
#[derive(Debug)]
pub struct DbError {
    /// The database file.
    pub path: PathBuf,
    /// What went wrong.
    pub kind: DbErrorKind,
}

/// Why [`DbError`].
#[derive(Debug)]
pub enum DbErrorKind {
    /// Creating `settings/db`, or pre-creating the database file at its mode, failed.
    Io(std::io::Error),
    /// The engine refused: open, pragma, prepare, or a statement.
    Sqlite(rusqlite::Error),
    /// `PRAGMA journal_mode = DELETE` was set and the engine answered something else. Treated as an
    /// error rather than shrugged at, because a WAL database in `settings/db` is the one shape
    /// 0054's constraint 1 was amended to forbid, and it announces itself only as sidecar files
    /// nobody looks at.
    JournalMode {
        /// What the engine said it was using.
        answered: String,
    },
    /// A database exists at this path and is not this schema. Never downgraded to an empty map.
    SchemaVersion {
        /// `PRAGMA user_version` as read.
        found: i64,
        /// [`SCHEMA_VERSION`].
        expected: i64,
    },
    /// A WRITE would have given a name a second namespace: it is already a row in the other table.
    ///
    /// The same static predicate [`Ambiguity::WrongTable`] enforces on the migration path, enforced
    /// on the write path, and for the same reason — a name has ONE home
    /// (`docs/decisions/0051-node-keys-live-in-their-own-store.md`), and a writer that could create
    /// a second one would break the invariant the two tables exist to buy while every reader stayed
    /// green.
    WrongTable {
        /// The key name. Never its value.
        key: String,
        /// Where the name already is.
        found_in: Table,
        /// Where this write wanted to put it.
        wanted: Table,
    },
    /// A WRITE reached the database's path and found nothing there — it was REMOVED between the
    /// moment [`crate::store::save_credentials_to_store`] chose the backend and the moment
    /// [`upsert_rows`] opened it.
    ///
    /// **The write is refused and the database this call had begun to create is rolled back.** See
    /// [`upsert_rows`]' invariant section for why creating it instead — which is what this code did
    /// until it was measured — retires every other key on the box.
    VanishedDatabase,
    /// `PRAGMA foreign_keys = ON` was set and the engine did not take it, so schema 2's two
    /// `REFERENCES` clauses would enforce nothing. Treated like the journal mode above: a pragma
    /// that did not take is silent everywhere else, so it is checked rather than assumed.
    ForeignKeys,
    /// A WRITE reached a database at an OLDER schema this code can READ but will not write.
    ///
    /// ⚠ The asymmetry is deliberate. [`READABLE_SCHEMA_VERSIONS`] is what keeps an unmigrated box
    /// working under a new binary; writing to that box is a different act, because schema 2's
    /// `credential.field` is `NOT NULL` and there is nowhere in schema 1's table to put the
    /// classification a schema-2 row needs. Half-supporting it — writing `name`/`value` and calling
    /// the row infrastructure — would file a VENUE credential as a deployment-level one and
    /// silently detach it from its account, which is worse than the refusal.
    WriteToOlderSchema {
        /// The key name. Never its value.
        key: String,
        /// The schema the database carries.
        found: i64,
    },
    /// A WRITE would have inserted a credential name this database has never held, and the caller
    /// supplied no classifier to say WHICH ACCOUNT it belongs to.
    ///
    /// Not reachable for a name already in the store: replacing a known key's value needs no
    /// classification, because the row already carries one. See [`upsert_rows`].
    Unclassified {
        /// The key name. Never its value.
        key: String,
    },
    /// **A WRITE was REFUSED by the schema-2 fill, and this carries the fill's own reason.**
    ///
    /// ⚠ It exists because this path used to report every such refusal as
    /// [`DbErrorKind::Unclassified`], whose message says *no classifier was supplied* — and a
    /// classifier had been supplied in every one of those cases. The cause an operator was handed
    /// was therefore false, and it pointed at the caller rather than at the key: a
    /// `{VENUE}_MAINNET_*` written beside a live `{VENUE}_LIVE_*` with a different value
    /// ([`crate::SchemaRefusal::CollidingLiveValues`]) reported *supplied no classifier*, which is
    /// not a thing an operator can act on and is not what happened.
    ///
    /// [`crate::SchemaRefusal`] names a KEY and never a value, so the whole refusal is safe to
    /// print.
    Refused {
        /// The fill's own refusal, whose `Display` states the key, the reason and the repair.
        refusal: crate::schema::SchemaRefusal,
    },
    /// **No settings database at all** — [`crate::store::Backend::Files`] answers on this box, and
    /// a file store has no `account` table for [`set_venue_account_id`] to write.
    ///
    /// Built by `crate::store`, never by this module: the choice of store is that module's, and
    /// this arm exists so the refusal names the file that IS answering rather than arriving as an
    /// engine error about a path nothing created. A per-KEY fallback to that file is forbidden
    /// (`crate::store::Backend`), so there is no half-answer to give.
    NoDatabase {
        /// The credential file that answers here instead.
        file: PathBuf,
    },
    /// **A SETTINGS-mirror write reached a project with no database at all.**
    ///
    /// Refused rather than served by creating one, and the reason is not tidiness: the mere
    /// EXISTENCE of `<project>/settings/db/vike.db` is the whole of [`crate::store::Backend`]'s
    /// per-run choice, so a settings writer that created the file would make every credential in
    /// `secrets.env` unread on that box in the same act — the LIVE GATE, silently, from a command
    /// about settings. `vike-cli secrets migrate` is the one thing that may create the store.
    NoSettingsDatabase,
    /// **An ADOPTION reached a store whose settings tables do not exist.**
    ///
    /// The seal says *resolve every settings key from the rows*, so sealing a store that has no
    /// `setting` table would seal the box into resolving from nothing — every ceiling absent, every
    /// venue `paper`, and `is_declared()` false. Refused rather than served by creating empty
    /// tables, which would produce exactly that state with a success message on it.
    ///
    /// ⚠ It is reachable on a perfectly healthy box: `vike-cli secrets migrate` creates BOTH
    /// settings tables through the shared DDL batch, so the ordinary shape of this refusal is a
    /// store migrated before those tables joined it. `vike-cli config mirror` is the repair.
    NoSettingsTables,

    /// A WRITE to the `account` table reached a database at a schema OLDER than
    /// [`ACCOUNT_TABLE_SCHEMA`], which carries no such table.
    ///
    /// The write twin of [`read_accounts`]' [`NoAccountTable::OlderSchema`] branch, and it exists
    /// for the same reason: [`READABLE_SCHEMA_VERSIONS`] keeps a schema-1 store working under a new
    /// binary, so a box that has simply not run `vike-cli secrets migrate` would otherwise be handed
    /// the engine's own `no such table: account` as if the store had malfunctioned.
    NoAccountTable {
        /// The schema the database carries, as `PRAGMA user_version` reported it.
        found: i64,
    },
    /// [`set_venue_account_id`] was given an `id` no `account` row carries.
    ///
    /// **Never a create.** `id` is the identity, and a row invented for a mistyped id is a book
    /// landing on an account nobody has.
    NoSuchAccount {
        /// The id that was asked for.
        id: i64,
    },
    /// [`set_venue_account_id`] would have replaced a row's KNOWN book with a DIFFERENT one, and
    /// the caller did not say to.
    ///
    /// ⚠ The most consequential refusal on this path. A `venue_account_id` decides which broker an
    /// order routes to — dukascopy's two demo accounts are two legal entities — so re-pointing an
    /// account at another book is an act an operator states rather than one a typo performs.
    /// The stored number is named (it is an account number, not a secret); the OFFERED one is not,
    /// because nothing here can know that a value the operator put in this flag was not a secret
    /// they meant for another one.
    BookAlreadyKnown {
        /// The row.
        id: i64,
        /// Its venue.
        venue: String,
        /// Its tier.
        tier: String,
        /// The book it already names.
        current: String,
    },
    /// Ruling 11 (`docs/superpowers/specs/2026-09-14-the-credential-schema.md` §8): another ACTIVE
    /// account of the same venue already names this book.
    ///
    /// Two engines on one ledger each read *"I hold 1"* while the venue holds 2, and reconcile's
    /// auto-applied `PositionDrift` then rewrites each engine's size onto a total that includes the
    /// other's. The partial index `account_one_account_per_book` is what enforces it; this arm is
    /// that refusal with both row ids in it.
    BookHeldByAnother {
        /// The row the write aimed at.
        id: i64,
        /// The venue both rows belong to.
        venue: String,
        /// The ACTIVE row that already names this book.
        holder: i64,
    },
    /// **Another process is holding the store**, and the wait ([`BUSY_TIMEOUT`]) ran out.
    ///
    /// ⚠ It is a NAMED arm rather than a [`DbErrorKind::Sqlite`] because of what the engine's own
    /// words do to an operator: `database is locked` names no repair, does not say whether anything
    /// was half-written, and reads like corruption of the one file holding this box's credentials.
    /// It is none of that — it is the migrator, another `set-book`, or a daemon reading credentials,
    /// for the milliseconds a statement takes.
    ///
    /// **Nothing was written.** Every write in this module is inside one transaction, so a refusal
    /// at any point rolls the whole of it back; there is no partial state to repair and the answer
    /// is to run the command again. Built by [`DbError::sql`], so every statement here reaches it.
    StoreBusy,
    /// The offered `venue_account_id` is not a value [`normalized_venue_account_id`] will take.
    ///
    /// ⚠ **It does not echo the token**, and the refusal is deliberately the one arm here with no
    /// data at all: a value that failed this predicate is a value nobody has classified, and the
    /// commonest way to reach it is pasting something into the wrong flag.
    BookMalformed,
    /// **A writer died mid-write and its SQLite ROLLBACK JOURNAL is still on disk**, so a
    /// READ-ONLY open of the store is refused whole — `SQLITE_READONLY_ROLLBACK`.
    ///
    /// ⚠ It is a NAMED arm for the same reason [`StoreBusy`](DbErrorKind::StoreBusy) is, only
    /// worse: the engine's own words for this one are *attempt to write a readonly database*,
    /// which an operator whose file modes are perfect reads as the single thing it is not. Nothing
    /// tried to write. `journal_mode = DELETE` leaves `<db>-journal` beside the database for the
    /// duration of every write and removes it on a clean close, so a crash, an OOM kill or power
    /// loss inside that window leaves one behind; the engine must REPLAY it before anything may
    /// read the file, replaying is a write, and a read-only connection may not write.
    ///
    /// **Nothing is corrupt and nothing is lost.** The journal holds exactly the pages needed to
    /// put the database back to its last committed state, and the first read-WRITE open performs
    /// that replay and deletes it — no verb, no flag, no repair tool.
    ///
    /// ⚠ **A DAEMON CANNOT DO IT.** It opens this store read-only and the deployed unit mounts its
    /// settings directory read-only in the service's own mount namespace, so the replay fails
    /// there too (MEASURED: a write attempt from inside the namespace fails at
    /// `PRAGMA journal_mode = DELETE` with the same extended code). The repair is an OPERATOR
    /// SHELL, where the directory is writable — which is why the message names a command rather
    /// than a setting, and why a restart on its own achieves nothing.
    ///
    /// Classified on the EXTENDED code, not on `rusqlite::ErrorCode::ReadOnly`: that primary code
    /// also covers a genuinely read-only file, a read-only directory and a moved database, and
    /// telling an operator with a `chmod 400` store that *a writer died mid-write* would be this
    /// arm committing the defect it exists to fix. Built by [`DbError::sql`], so every statement
    /// in this module reaches it.
    ReadOnlyRollback,
    /// [`edit_account`] was given a LABEL its own predicate will not take
    /// ([`normalized_account_label`]).
    ///
    /// ⚠ **It does not echo the token**, the same rule [`DbErrorKind::BookMalformed`] obeys and for
    /// a sharper reason on this path: the commonest way to reach it is pasting into the wrong flag,
    /// and on the surfaces that reach an account verb the flag beside this one carries a credential
    /// VALUE. A refusal that quoted what it was handed would be the one channel on this surface
    /// that prints an operator's secret back at them.
    AccountLabelMalformed,
    /// [`AccountEdit::Create`] would have made a SECOND row at a `(venue, tier)` that already has
    /// an UNLABELLED one — planting `crate::SchemaRefusal::AmbiguousAccount` for the next
    /// credential key that arrives for that venue.
    ///
    /// ⚠ **`UNIQUE (venue, tier, label)` does not refuse this and cannot**: NULLs are distinct in a
    /// SQLite index, and [`crate::schema::DDL`]'s own doc says that index *"starts biting the moment
    /// an account is LABELLED"*. What breaks instead is `AccountResolver`'s `by_key`, whose
    /// `(venue, tier, None, None)` entry then names two rows and whose `ambiguous_unlabelled` set
    /// makes the NEXT write for that venue refuse. So a create verb that allowed it would not be
    /// creating an account, it would be arming a refusal for a write nobody has made yet.
    AmbiguousUnlabelledAccount {
        /// The venue both rows would belong to.
        venue: String,
        /// The tier both rows would belong to.
        tier: String,
        /// The UNLABELLED row that is already there.
        holder: i64,
    },
    /// [`AccountEdit::Create`] or [`AccountEdit::Rename`] would have given a `(venue, tier)` a
    /// label another row of that venue and tier already carries — `UNIQUE (venue, tier, label)`,
    /// asked as a question so the answer can name the other row.
    AccountLabelTaken {
        /// The venue.
        venue: String,
        /// The tier.
        tier: String,
        /// The label.
        label: String,
        /// The row that already carries it.
        holder: i64,
    },
    /// The label would COLLIDE with another ACTIVE row's `venue_account_id` at the same venue.
    ///
    /// ⚠ Not a schema constraint — there is none to lean on — but a live misroute all the same:
    /// `vike_mount::dukascopy`'s `resolve_account` matches a policy address against
    /// `venue_account_id` FIRST and then `label`, so two rows answering one address surface as
    /// `DukascopyRefusal::Ambiguous` at the NEXT mount rather than here. Refused at the edit, where
    /// the operator who made it is standing.
    AccountLabelHeldAsBook {
        /// The venue.
        venue: String,
        /// The label that was offered.
        label: String,
        /// The ACTIVE row that already names this string as its BOOK.
        holder: i64,
    },
    /// [`AccountEdit::Remove`] was asked to DELETE a row that still owns live `credential` rows.
    ///
    /// ⚠ **The engine already refuses this** — `credential.account_id REFERENCES account(id)`
    /// carries no `ON DELETE` clause, so it is `NO ACTION`, and [`open_for_write`] sets AND VERIFIES
    /// `PRAGMA foreign_keys = ON`. What the engine cannot do is NAME them: its own words are
    /// `FOREIGN KEY constraint failed`, which tells an operator holding sixty-odd keys nothing at
    /// all. So the verb asks first, through the same statement [`read_account_keys`] uses — `SELECT
    /// name, field, account_id`, with no `value` column in it — and the foreign key is left as the
    /// authority that still holds if this pre-check is ever wrong. The same two-layer shape
    /// [`DbErrorKind::BookHeldByAnother`] has.
    AccountHasCredentials {
        /// The row.
        id: i64,
        /// Its venue.
        venue: String,
        /// Its tier.
        tier: String,
        /// The live credential key NAMES filed against it — **names, never values**, from a
        /// statement that does not select one.
        keys: Vec<String>,
    },
    /// [`AccountEdit::Rename`] was asked to change the label of a row whose credential keys SPELL
    /// the old one.
    ///
    /// ⚠ **The rename would create a SECOND account row the next time one of those keys is
    /// written.** A labelled key is `{VENUE}_{TIER}_{FIELD}__{LABEL}`;
    /// `crate::schema::Classification::owner_prefix` is `None` for it by construction (the label
    /// sits after the field, so the field is not a suffix), so the store finds its account by
    /// `(venue, tier, label)` — and `vike_bridge_core::credentials::classify_credential_name` still
    /// derives the OLD label from the unchanged key name. The only alternatives are rewriting the
    /// credential key names, which is the whole-store rewrite `docs/decisions/0036` forbids, or
    /// letting the divergence happen silently. Every migrated row on every box is unlabelled and
    /// renames freely.
    AccountKeysPinTheLabel {
        /// The row.
        id: i64,
        /// The label its keys spell.
        label: String,
        /// The live credential key NAMES that spell it — names, never values.
        keys: Vec<String>,
    },
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.path.display();
        match &self.kind {
            DbErrorKind::Io(e) => write!(f, "settings database {p} could not be created: {e}"),
            DbErrorKind::Sqlite(e) => write!(f, "settings database {p} could not be read: {e}"),
            DbErrorKind::JournalMode { answered } => write!(
                f,
                "settings database {p} refused journal_mode=DELETE and is using {answered:?} \
                 instead; a WAL database here cannot be read while the settings directory is \
                 mounted read-only, and it leaves a second plaintext credential artifact at rest"
            ),
            // ⚠ ZERO IS THE HALF-FINISHED MIGRATION, AND IT IS UNRECOVERABLE — so the ONE message
            // an operator ever sees has to say both, rather than leaving the story in a doc comment
            // beside code they are not reading. `stamp_schema_version` runs AFTER the insert
            // transaction commits (the module doc argues why), so a process killed between the two
            // leaves the rows on disk with the stamp unwritten: every reader then refuses LOUDLY,
            // which is the designed behaviour and is also a box whose credentials are unreadable
            // until somebody deletes a file by hand. There is no resume path and no repair verb;
            // saying only "version 0, not 1" hands that operator a number and no way out.
            DbErrorKind::SchemaVersion { found, expected } if *found == 0 => write!(
                f,
                "settings database {p} carries schema version 0, not {expected} — it was never \
                 finished, so nothing will read it. The migration commits its rows and THEN stamps \
                 the version; a process killed between those two steps (or a file at this path \
                 that this project did not write) leaves exactly this. It cannot be resumed and \
                 there is no repair command: delete {p} and run the migration again. The \
                 credential files beside it are untouched — nothing here has ever written, moved \
                 or deleted one — so they still hold everything this database was built from. ⚠ \
                 The rebuilt store RE-NUMBERS its account rows (`account.id` is a rowid, assigned \
                 in the order the migration meets the key names) and carries over no \
                 venue_account_id, so any book that was written by hand must be re-identified from \
                 `vike-cli secrets accounts` and written again — never from a note that names an \
                 old id."
            ),
            DbErrorKind::SchemaVersion { found, expected } => write!(
                f,
                "settings database {p} carries schema version {found}, not one this binary reads \
                 ({READABLE_SCHEMA_VERSIONS:?}, and it writes {expected}) — refusing to read it \
                 rather than answering with an empty credential map"
            ),
            DbErrorKind::NoSettingsDatabase => write!(
                f,
                "there is no settings database at {p}, and the settings mirror will not create \
                 one: the mere existence of that file is what makes the database — rather than \
                 `secrets.env` — answer for every credential on this box, so creating it here \
                 would take every venue to paper from a command about settings. Run `vike-cli \
                 secrets migrate` (with `--dry-run` first), then retry. NOTHING WAS WRITTEN."
            ),
            DbErrorKind::NoSettingsTables => write!(
                f,
                "settings database {p} carries no `setting`/`venue_arming` tables, so there is \
                 nothing to adopt: sealing it would make this box resolve every setting from zero \
                 rows — no ceiling, no dead-man, and every venue capped `paper` with nothing \
                 recording that an arming was ever stated. Run `vike-cli config mirror` first, \
                 then `vike-cli config compare`. NOTHING WAS WRITTEN."
            ),
            DbErrorKind::ForeignKeys => write!(
                f,
                "settings database {p} did not take `PRAGMA foreign_keys = ON`, so a credential \
                 could be written naming an account row that does not exist and nothing would \
                 object. Nothing was written."
            ),
            DbErrorKind::WriteToOlderSchema { key, found } => write!(
                f,
                "settings database {p} carries schema version {found} and this write wants to add \
                 {key}, which needs schema {SCHEMA_VERSION}'s account classification — there is \
                 nowhere in the older table to record it. NOTHING WAS WRITTEN and nothing was \
                 changed. Run `vike-cli secrets migrate` (with `--dry-run` first) to bring this \
                 store to the current schema, then retry."
            ),
            DbErrorKind::Unclassified { key } => write!(
                f,
                "settings database {p}: {key} is not yet in this store and the caller supplied no \
                 account classification for it, so the row could only be filed as a \
                 deployment-level credential belonging to no venue and no account. Refusing rather \
                 than guessing; nothing was written."
            ),
            DbErrorKind::WrongTable { key, found_in, wanted } => write!(
                f,
                "settings database {p}: {key} is already in the {found_in} table and this write \
                 wants it in {wanted} — a name has ONE home (decision 0051), and nothing here will \
                 give it two. Nothing was written."
            ),
            DbErrorKind::VanishedDatabase => write!(
                f,
                "settings database {p} was chosen for this write and had been removed by the time \
                 the write opened it. NOTHING WAS WRITTEN and no database was left behind — \
                 creating one here would have held only this write's keys and retired every other \
                 credential on this box. The credential files beside it are untouched; re-run the \
                 write, which will resolve the store again."
            ),
            DbErrorKind::Refused { refusal } => {
                write!(f, "settings database {p}: {refusal}. NOTHING WAS WRITTEN.")
            }
            DbErrorKind::NoDatabase { file } => write!(
                f,
                "there is no settings database at {p}, so this box has no `account` table to write \
                 — the credential store {} answers here, and a file store keeps its accounts in the \
                 key NAMES rather than in rows. NOTHING WAS WRITTEN and no database was created. \
                 Run `vike-cli secrets migrate --dry-run`, then `vike-cli secrets migrate`, and try \
                 again.",
                file.display()
            ),
            DbErrorKind::NoAccountTable { found } => write!(
                f,
                "settings database {p} carries schema version {found}, which predates the account \
                 table (schema {ACCOUNT_TABLE_SCHEMA}) — there is no row here to write. NOTHING WAS \
                 WRITTEN. Run `vike-cli secrets migrate` to bring this store to the current schema."
            ),
            DbErrorKind::NoSuchAccount { id } => write!(
                f,
                "settings database {p} holds no account with id {id}. NOTHING WAS WRITTEN and no \
                 account was created — the id IS the identity, so an id that names no row is a \
                 typo rather than a new account. `vike-cli secrets accounts` lists the ids this \
                 store holds."
            ),
            DbErrorKind::BookAlreadyKnown { id, venue, tier, current } => write!(
                f,
                "account {id} ({venue}/{tier}) in {p} already names venue account {current}, and \
                 this would write a DIFFERENT one. A venue_account_id decides which BROKER an order \
                 routes to, so it is never replaced silently — a mistyped id is exactly how a book \
                 lands on the wrong account. NOTHING WAS WRITTEN. If {current} is the wrong number, \
                 say so with --replace; if you meant another row, `vike-cli secrets accounts` lists \
                 them."
            ),
            // ⚠ THE REPAIR IS NAMED AS A COMMAND THAT EXISTS, and it did not used to be. This
            // message read "deactivate or correct the other", and neither act was reachable: no
            // verb in this tree deactivates an account row, and CORRECTING the other row is refused
            // by this same check whenever the two are SWAPPED — which is the commonest way to
            // arrive here and the one case where the operator is stuck in both directions. `--clear`
            // is the third move that breaks the cycle; see `set_venue_account_id`'s CLEAR section.
            DbErrorKind::BookHeldByAnother { id, venue, holder } => write!(
                f,
                "account {holder} in {p} is an ACTIVE {venue} account that already names this venue \
                 account, and account {id} is another one — two accounts of one venue may not name \
                 one book (the schema's `account_one_account_per_book` index). Two engines on one \
                 ledger each read their own size while the venue holds the sum. NOTHING WAS \
                 WRITTEN. If the two rows hold each other's books, clear one and then write \
                 both:\n  vike-cli secrets set-book --id {holder} --clear\n  vike-cli secrets \
                 set-book --id {id} --venue-account-id <this number> --replace\n  vike-cli secrets \
                 set-book --id {holder} --venue-account-id <the other number>\nIf account {holder} \
                 is simply the wrong row, `--clear` it and leave it blank."
            ),
            DbErrorKind::StoreBusy => write!(
                f,
                "settings database {p} is being held by another process and did not come free \
                 within {}s — NOTHING WAS WRITTEN and nothing was half-written, because every write \
                 here is one transaction that rolls back whole. This is not corruption: it is a \
                 concurrent `vike-cli secrets migrate`, another `set-book`, or a daemon reading its \
                 credentials. Wait for that to finish and run the same command again.",
                BUSY_TIMEOUT.as_secs()
            ),
            DbErrorKind::BookMalformed => write!(
                f,
                "that is not a usable venue account id, so nothing was written to {p}. It must be \
                 ONE token (the identifier the venue itself answers with — a number, a login, an \
                 address), made only of printable ASCII, with no spaces, no line breaks, no control \
                 characters and at most {VENUE_ACCOUNT_ID_MAX_BYTES} bytes. ⚠ A value that LOOKS \
                 right is usually an invisible character that came along with a paste — a \
                 byte-order mark or a zero-width space from a web page. One at either end is \
                 trimmed and accepted; one in the MIDDLE is what this refuses, so RETYPE the \
                 identifier rather than pasting it again. The value is deliberately not echoed here."
            ),
            DbErrorKind::AccountLabelMalformed => write!(
                f,
                "that is not a usable account label, so nothing was written to {p}. A label is \
                 A-Z and 0-9 only, at most {ACCOUNT_LABEL_MAX_LEN} characters, and never \
                 `{RESERVED_ACCOUNT_LABEL}` — that is the account an UNLABELLED key already \
                 addresses, so a row carrying it would be a second answer to one question. It is \
                 case-SENSITIVE and deliberately not repaired: a label this store uppercased on \
                 your behalf is a label that then does not match what you wrote in policy.toml. \
                 The value is deliberately not echoed here — the flag beside this one carries a \
                 credential."
            ),
            DbErrorKind::AmbiguousUnlabelledAccount { venue, tier, holder } => write!(
                f,
                "account {holder} in {p} is already an UNLABELLED {venue}/{tier} row, and this \
                 would make a second one. NOTHING WAS WRITTEN. Two unlabelled rows of one \
                 (venue, tier) cannot be told apart by any cell they carry, so the NEXT credential \
                 key written for {venue} would be refused as ambiguous — creating this row would \
                 arm a refusal for a write nobody has made yet. Give this account a --label, or \
                 use account {holder}."
            ),
            DbErrorKind::AccountLabelTaken { venue, tier, label, holder } => write!(
                f,
                "account {holder} in {p} is already {venue}/{tier} labelled {label}. NOTHING WAS \
                 WRITTEN — a label is how policy.toml addresses an account, so two rows may not \
                 answer to one. Pick another label, or edit account {holder}."
            ),
            DbErrorKind::AccountLabelHeldAsBook { venue, label, holder } => write!(
                f,
                "account {holder} in {p} is an ACTIVE {venue} account whose venue_account_id is \
                 already `{label}`, and a mount resolves a policy address against the BOOK before \
                 the label. NOTHING WAS WRITTEN: a row labelled {label} beside it would make \
                 `policy.accounts.{venue}.{label}` name two accounts, which the mount refuses as \
                 ambiguous and takes to PAPER. Pick another label."
            ),
            DbErrorKind::AccountHasCredentials { id, venue, tier, keys } => write!(
                f,
                "account {id} ({venue}/{tier}) in {p} still owns {} live credential {}: {}. \
                 NOTHING WAS WRITTEN and nothing was deleted — removing the row would leave those \
                 keys naming an account that does not exist, which the schema's foreign key \
                 refuses anyway and which nothing here will do quietly. Key NAMES only are shown; \
                 no value was read. Either DEACTIVATE this account instead (the reversible act — \
                 every consumer already reads a deactivated row exactly as it reads a deleted one, \
                 and the row survives as evidence), or remove those keys first.",
                keys.len(),
                if keys.len() == 1 { "key" } else { "keys" },
                keys.join(", ")
            ),
            DbErrorKind::AccountKeysPinTheLabel { id, label, keys } => write!(
                f,
                "account {id} in {p} is labelled {label} and its credential keys SPELL that label: \
                 {}. NOTHING WAS WRITTEN. Renaming the row would not rename the keys — nothing in \
                 this workspace rewrites a credential store wholesale — and the classifier would \
                 then read {label} out of those names again and CREATE A SECOND ACCOUNT the next \
                 time one of them is written. Key NAMES only are shown; no value was read. A row \
                 with no labelled keys renames freely; every migrated row is unlabelled.",
                keys.join(", ")
            ),
            DbErrorKind::ReadOnlyRollback => write!(
                f,
                "a writer of the settings database was killed mid-write and left SQLite's \
                 rollback journal at {p}-journal. NOTHING IS CORRUPT and nothing is lost: that \
                 journal holds the pages needed to put the database back to its last committed \
                 state, and the engine REPLAYS it — and deletes it — on the first read-WRITE open. \
                 What was refused is the READ-ONLY open, which may not write and therefore may not \
                 replay. ⚠ The engine's own words for this are `attempt to write a readonly \
                 database`; nothing tried to write, and your file permissions are not the problem. \
                 ⚠ A DAEMON CANNOT REPAIR THIS ITSELF — it opens this store read-only and its \
                 settings directory is read-only in its own mount namespace, so restarting it just \
                 fails the same open again. Repair it from an OPERATOR SHELL, where the directory \
                 is writable: ANY `vike-cli secrets` command opens the store read-write and \
                 replays the journal, so `vike-cli secrets list` is enough. Then restart the \
                 daemon."
            ),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            DbErrorKind::Io(e) => Some(e),
            DbErrorKind::Sqlite(e) => Some(e),
            _ => None,
        }
    }
}

impl DbError {
    pub(crate) fn io(path: &Path, e: std::io::Error) -> Self {
        DbError { path: path.to_path_buf(), kind: DbErrorKind::Io(e) }
    }

    /// ⚠ **CLASSIFIES rather than wraps**, and the cases it pulls out are the two on-disk states an
    /// operator meets that are NOT malfunctions of their own store. Every statement in this module
    /// reaches [`DbErrorKind::Sqlite`] through here, so putting the branches in the constructor is
    /// what makes each of them a named answer at every call site at once rather than at the handful
    /// somebody remembered.
    ///
    /// **A lock another process is holding.** What the operator was handed before: `settings
    /// database …/vike.db could not be read: database is locked` — the engine's own six words,
    /// which name no repair, do not say the write was refused WHOLE, and read exactly like
    /// corruption to somebody who has never seen SQLite's locking model. It is none of those
    /// things: it is a second process holding the store for the few milliseconds a write takes, and
    /// the repair is to run the command again.
    ///
    /// **A rollback journal a killed writer left behind.** Same defect, one layer worse: the
    /// engine's words are *attempt to write a readonly database* about a READ that wrote nothing,
    /// on a store whose permissions are perfect. [`DbErrorKind::ReadOnlyRollback`] carries the
    /// argument and the repair.
    ///
    /// ⚠ The rollback case is matched on the EXTENDED code, never on the primary
    /// `rusqlite::ErrorCode::ReadOnly`. That primary code is the whole `SQLITE_READONLY_*` family —
    /// a read-only file, a read-only directory, a database moved out from under an open handle —
    /// and answering all of them with *a writer died mid-write* would hand the next operator
    /// exactly the kind of confident wrong sentence this constructor exists to remove.
    pub(crate) fn sql(path: &Path, e: rusqlite::Error) -> Self {
        let named = |kind| DbError { path: path.to_path_buf(), kind };
        if let rusqlite::Error::SqliteFailure(inner, _) = &e {
            if matches!(
                inner.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) {
                return named(DbErrorKind::StoreBusy);
            }
            if inner.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK {
                return named(DbErrorKind::ReadOnlyRollback);
            }
        }
        named(DbErrorKind::Sqlite(e))
    }
}

/// A database failure, rendered as the credential-store failure every existing caller already
/// handles.
///
/// ⚠ The bridge is deliberate, and it is why no call site outside this crate changes shape.
/// [`SecretsError`] is *"the store EXISTS and could not be read"*, which is exactly what every
/// [`DbError`] variant is; `crate::store::resolve`'s ABSENT arm stays the live gate, and a database
/// that is corrupt, is not this schema, or refused its journal mode now arrives as the same LOUD
/// error a `chmod 000` credential file already produced. The message is preserved verbatim, so
/// nothing about the database's failure is flattened into "could not be read: Other".
impl From<DbError> for SecretsError {
    fn from(e: DbError) -> Self {
        let path = e.path.clone();
        SecretsError { path, source: std::io::Error::other(e.to_string()) }
    }
}

/// Is there a database at `path`?
///
/// **The whole of the stage-2 decision, and deliberately one `is_file` on one path.** See
/// `crate::store::Backend`, which is where the argument for a per-RUN choice over a per-KEY
/// fallback lives.
#[must_use]
pub fn database_present(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------------

/// **How long any statement here waits for a lock another connection is holding** before the engine
/// gives up and returns `SQLITE_BUSY`.
///
/// ⚠ **This is a PIN, not a new behaviour, and the difference is the reason it is written down.**
/// MEASURED in rusqlite 0.40's `InnerConnection::open_with_flags`: every connection it opens is
/// already given `sqlite3_busy_timeout(db, 5000)`, so this store has never been without one. What it
/// has been without is a timeout it CHOSE — rusqlite's own `Connection::busy_timeout` doc says the
/// default "may be subject to change", and a silent change to how long a live daemon's credential
/// read waits for the migrator's lock is not a thing this store should learn about from a dependency
/// bump. Setting it explicitly costs one call and makes the number this file's answer.
///
/// It is set on the READ path as well as the write path deliberately. The contended moment is a
/// process booting (reading credentials) while an operator runs `secrets migrate` or `set-book`;
/// waiting a few seconds for a sub-millisecond write is strictly better than failing, and the
/// failure it replaces is the LIVE GATE — a credential read that errors is a box with no keys.
///
/// ⚠ **A busy timeout does NOT fix a DEFERRED read-then-write, and reaching for one instead of
/// `TransactionBehavior::Immediate` is the mistake it invites.** SQLite returns `SQLITE_BUSY`
/// IMMEDIATELY — without consulting the busy handler at all — when a connection holding a SHARED
/// lock tries to promote to RESERVED while another connection already holds RESERVED, because
/// sleeping there could deadlock both sides. So the timeout below is what covers a writer waiting
/// for a writer; [`set_venue_account_id`]'s `Immediate` is what stops this module's one
/// read-then-write from being that unpromotable case.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5_000);

/// Open READ-ONLY, verifying the schema version. Never creates anything.
///
/// Read-only is not a nicety: the deployed daemon mounts `settings/` read-only apart from one narrow
/// grant, an operator reading the store with the daemon down may have no write access to the
/// directory at all, and a reader that could CREATE the file would turn "this box has no database"
/// into "this box has an empty database" — which under `crate::store::Backend` is the difference
/// between the file answering and every venue silently going to paper.
/// Returns the connection and the schema version it carries — one of
/// [`READABLE_SCHEMA_VERSIONS`], because every caller of this has to branch on it.
pub(crate) fn open_for_read(path: &Path) -> Result<(Connection, i64), DbError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| DbError::sql(path, e))?;
    // The pin, before the first statement — see [`BUSY_TIMEOUT`].
    conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| DbError::sql(path, e))?;
    let version = check_schema_version(path, &conn)?;
    Ok((conn, version))
}

/// Open READ-WRITE, creating the directory, the file, the modes and the schema when they are absent.
///
/// The order is load-bearing and is the module doc's *the modes are set, never inherited*: directory
/// (0700), then an empty file (0600), then the engine — so no row has ever existed under the umask's
/// answer. Returns whether this call created the database.
///
/// # ⚠ It does NOT stamp [`SCHEMA_VERSION`], and that is the whole of the crash story
///
/// `PRAGMA user_version` is written by the CALLER, **after** its own transaction commits
/// ([`stamp_schema_version`]). This function used to stamp it here, between the schema batch and the
/// caller's inserts, and every failure after that point — a full disk, a read-only remount, `SIGKILL`
/// — left a file that `database_present` calls a database, that `check_schema_version` accepts, and
/// that holds NO ROWS. `crate::store::Backend` then answers `Database` for every process on the box
/// forever and the credential file beside it is never read again, which is not an error downstream
/// but the LIVE GATE: every venue silently on paper with `secrets.env` sitting there looking correct.
///
/// With the stamp deferred, that same crash leaves `user_version = 0`, which
/// [`check_schema_version`] already treats as the loud [`DbErrorKind::SchemaVersion`]. The unfinished
/// database announces itself instead of impersonating a finished one.
/// Returns the connection, whether this call created the database, and the schema version the
/// database carries — [`SCHEMA_VERSION`] on a create, the stored one otherwise.
///
/// ⚠ `PRAGMA foreign_keys` is SET AND VERIFIED here, and it is not decoration: it is **OFF by
/// default, per connection**, so schema 2's `credential.account_id REFERENCES account(id)` and
/// `account.parent_id REFERENCES account(id)` would enforce NOTHING without it — the spec's *a
/// schema is a gate* claim would be false for both columns. Same set-then-believe-the-engine shape
/// as the journal mode above, and for the same reason: a pragma that did not take is silent
/// everywhere else. It is set BEFORE any transaction because SQLite is a no-op for it inside one.
pub(crate) fn open_for_write(path: &Path) -> Result<(Connection, bool, i64), DbError> {
    let created = !path.exists();
    if let Some(dir) = path.parent() {
        create_dir_private(dir).map_err(|e| DbError::io(path, e))?;
    }
    if created {
        create_file_private(path).map_err(|e| DbError::io(path, e))?;
    }
    let conn = Connection::open(path).map_err(|e| DbError::sql(path, e))?;
    // The pin, before the first statement — see [`BUSY_TIMEOUT`]. ⚠ Ahead of the journal-mode
    // pragma below on purpose: that pragma takes a RESERVED lock, so a migrator holding one is
    // enough to make this open fail before any of this function's own checks have run.
    conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| DbError::sql(path, e))?;

    // Set it, then BELIEVE THE ENGINE rather than the request. A journal mode that did not take is
    // the failure this pragma exists to prevent, and it is silent everywhere else.
    let answered: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .map_err(|e| DbError::sql(path, e))?;
    if !answered.eq_ignore_ascii_case("delete") {
        return Err(DbError {
            path: path.to_path_buf(),
            kind: DbErrorKind::JournalMode { answered },
        });
    }

    conn.execute_batch("PRAGMA foreign_keys = ON;").map_err(|e| DbError::sql(path, e))?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(|e| DbError::sql(path, e))?;
    if fk != 1 {
        return Err(DbError { path: path.to_path_buf(), kind: DbErrorKind::ForeignKeys });
    }

    let version = if created {
        conn.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(path, e))?;
        SCHEMA_VERSION
    } else {
        check_schema_version(path, &conn)?
    };
    Ok((conn, created, version))
}

/// Declare the database FINISHED — the last write of a run that created one.
///
/// Separated from [`open_for_write`] deliberately: see that function's crash section. Until this
/// runs, `PRAGMA user_version` is `0` and every reader refuses the file loudly, so the window
/// between "the schema exists" and "the rows are committed" cannot be mistaken for a configured box.
/// The write half of [`migrate`], in its own function so the connection is DROPPED before its
/// caller can unlink the file — on Windows an open handle refuses the removal, so a cleanup written
/// inline would silently leave exactly the database it was added to remove.
///
/// Returns whether this call CREATED the database. ⚠ That answer comes from the OPEN and not from
/// the plan's read-only probe, and the two can genuinely differ: a sibling process may have created
/// the file in between. The open is the authority because it is the call that would have done the
/// creating, so the stamp below is decided by what actually happened rather than by what was
/// predicted — which is also exactly what a preview cannot promise, see [`preview`].
/// # ⚠ THREE acts, and which transaction each one is in
///
/// * **the RESHAPE** (schema 1 → 2) and **the STAMP** ride the SAME transaction as the inserts.
///   MEASURED in `db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`: `PRAGMA
///   user_version` and DDL are both rolled back with the transaction that set them, so a box is
///   either fully at 2 or fully at 1 and there is no state in between. That is stricter than
///   schema 1 needed and the reason is the shape of schema 2: `SELECT name, value FROM credential`
///   — the exact statement a schema-1 binary runs — is STILL VALID SQL against the schema-2 table,
///   so a half-applied reshape stamped 1 over schema-2 tables would be ACCEPTED by an older binary
///   and answered from. Atomicity removes that state rather than documenting it.
/// * **the CREATE's stamp** also moved inside the transaction, and the crash story it was written
///   for is UNCHANGED. [`open_for_write`] still applies the DDL outside the transaction and still
///   does not stamp, so a kill between the schema batch and the commit still leaves
///   `user_version = 0` — the loud, unresumable state [`check_schema_version`] explains. What the
///   move removes is the window that used to sit AFTER the commit, in which the rows were durable
///   and the version was not.
fn write_pending(
    planned: &Plan,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> Result<(bool, Option<crate::schema::RowReport>), DbError> {
    let (mut conn, created, version) = open_for_write(&planned.db)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(&planned.db, e))?;

    let report = fill_into(&tx, planned, classify, version, created)
        .map_err(|e| DbError::sql(&planned.db, e))?;

    // ⚠ INSIDE the transaction — see this function's doc. On a create the version is still withheld
    // by `open_for_write`, so the unfinished-store story is unchanged.
    if created || version != SCHEMA_VERSION {
        // ONE spelling of the stamp, reached through the transaction — `Transaction` derefs to
        // `Connection`, so this is the same call `db_tests` makes and there is no second way to
        // declare a database finished.
        stamp_schema_version(&planned.db, &tx)?;
    }
    tx.commit().map_err(|e| DbError::sql(&planned.db, e))?;
    Ok((created, report))
}

fn stamp_schema_version(path: &Path, conn: &Connection) -> Result<(), DbError> {
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(|e| DbError::sql(path, e))
}

/// **Everything a run WRITES into an open transaction** — the reshape when one is due, the node
/// rows, and the schema-2 fill — and the one place it is spelled.
///
/// ⚠ It is a free function over a `Transaction` rather than a step inside [`write_pending`] for the
/// same reason [`plan`] is a free function: [`preview`] has to perform it too, against a database
/// that is not the operator's, and a second spelling here would be a dry run describing a
/// classification the apply does not make. [`preview_rows`] is that caller. The STAMP and the
/// COMMIT stay with [`write_pending`], because those are the two acts a preview must not have.
fn fill_into(
    tx: &Transaction<'_>,
    planned: &Plan,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
    version: i64,
    created: bool,
) -> rusqlite::Result<Option<crate::schema::RowReport>> {
    let mut report = if version < SCHEMA_VERSION && !created {
        Some(crate::schema::reshape_into(tx, &planned.comments, classify)?)
    } else {
        None
    };

    // The node table is `(name, value)` in every schema — 0051's pair is not an account's — so it
    // is written exactly as it always was.
    let mut credentials: BTreeMap<String, String> = BTreeMap::new();
    for ((table, name), value) in &planned.pending {
        match table {
            Table::NodeKey => {
                tx.execute("INSERT INTO node_key (name, value) VALUES (?1, ?2)", (name, value))?;
            }
            Table::Credential => {
                credentials.insert(name.clone(), value.clone());
            }
        }
    }
    if !credentials.is_empty() || report.is_none() {
        let fill = crate::schema::write_rows(tx, &credentials, &planned.comments, classify)?;
        match &mut report {
            Some(r) => r.absorb(fill),
            None => report = Some(fill),
        }
    }
    Ok(report)
}

/// **Run the real classifier over a REPLICA of this store, in memory, and report what it did.**
///
/// The dry run's missing half. [`plan`] predicts the DECISIONS that are made before a row is
/// written — which table a name belongs to, which keys are ambiguous, what is already stored — and
/// those were the whole of what `--dry-run` reported. They are not the whole of what an upgrade
/// does: [`crate::schema::write_rows`] makes a second set of decisions per row (which account, and
/// what to do when two names resolve to one), and every one of its refusals was invisible to the
/// preview. A reviewer measured the consequence end to end on a store holding both
/// `ASTER_LIVE_API_KEY` and `ASTER_MAINNET_API_KEY`: the dry run said the store *would be upgraded*
/// and that *every key name would still answer exactly as it does today*, and the apply that
/// followed it failed. A preview that cannot see the one failure the reshape introduces is not a
/// preview.
///
/// # Why a REPLICA, and why in memory
///
/// [`preview`]'s own list of three cheaper previews rules out the two obvious shapes, and both
/// arguments still hold: opening the real database for writing creates the file and the schema
/// before any transaction can be rolled back, and copying it to a temp directory writes a second
/// PLAINTEXT copy of every live venue key under the umask. An in-memory database is neither. It has
/// no path, no mode and no umask; nothing is created, nothing is opened for writing, and the bytes
/// it holds are bytes this process already has — [`plan`] read the whole store into `stored` before
/// this function exists. `temp_store = MEMORY` is set so the engine cannot spill them to a file
/// under memory pressure, which is the one way an in-memory database can reach a disk.
///
/// # ⚠ What the replica is NOT
///
/// It is rebuilt from `(name, value)`, so the surrogate `account.id` and `credential.id` values it
/// derives are its own. Nothing in the report depends on them — [`crate::schema::RowReport`] names
/// accounts by `(venue, tier)` and keys by name — but a caller must not read a predicted id as the
/// id the apply will assign.
///
/// ⚠ **A refusal that fails the whole run comes back in the INDICATIVE mood**, and that is a
/// declared residual rather than an oversight: the message is
/// [`crate::schema::reshape_into`]'s own, so it says *NOTHING WAS WRITTEN and this store is still
/// at its old schema* where a preview would rather say *would not be*. Both sentences are TRUE of
/// a dry run — nothing was written, and the store is indeed still at its old schema — and the half
/// that matters, the KEY it could not carry, is named either way. Re-spelling it in the conditional
/// would mean a second message to keep in step with the one the apply prints, which is the defect
/// this whole function exists to remove.
fn preview_rows(
    planned: &Plan,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> rusqlite::Result<Option<crate::schema::RowReport>> {
    let mut mem = Connection::open_in_memory()?;
    // Never a spill file: an in-memory database's temporary B-trees would otherwise land in
    // `SQLITE_TMPDIR` under the umask, holding plaintext credentials. Same hazard `reshape_into`
    // refuses a `VACUUM` for.
    //
    // ⚠ Both pragmas are set as SQL and then VERIFIED, exactly as `open_for_write` sets and
    // verifies its own: a pragma that did not take is silent, and both of these are load-bearing —
    // one keeps plaintext off the disk and the other makes schema 2's `REFERENCES` clauses a gate
    // rather than a comment, which is what lets a preview predict a dangling reference.
    mem.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = ON;")?;
    let temp_store: i64 = mem.query_row("PRAGMA temp_store", [], |r| r.get(0))?;
    let foreign_keys: i64 = mem.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if temp_store != 2 || foreign_keys != 1 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
            Some(format!(
                "the dry run's in-memory replica did not take its pragmas (temp_store={temp_store}, \
                 foreign_keys={foreign_keys}); refusing to classify credentials in it rather than \
                 risk spilling them to a temp file"
            )),
        ));
    }
    let stored_of = |table: Table| -> BTreeMap<String, String> {
        planned
            .stored
            .iter()
            .filter(|((t, _), _)| *t == table)
            .map(|((_, n), v)| (n.clone(), v.clone()))
            .collect()
    };

    let tx = mem.transaction()?;
    let version = planned.version.unwrap_or(SCHEMA_VERSION);
    if !planned.exists {
        // Nothing to replicate — the apply would create the tables and fill them from the files.
        tx.execute_batch(crate::schema::DDL)?;
    } else if version < SCHEMA_VERSION {
        // The shape the reshape will find. `SCHEMA_1` is the real DDL rather than an approximation
        // of it, for the reason `plant_schema_1` gives.
        tx.execute_batch(SCHEMA_1)?;
        for (table, rows) in [
            (Table::Credential, stored_of(Table::Credential)),
            (Table::NodeKey, stored_of(Table::NodeKey)),
        ] {
            let sql = format!("INSERT INTO {} (name, value) VALUES (?1, ?2)", table.sql_name());
            for (name, value) in &rows {
                tx.execute(&sql, (name, value))?;
            }
        }
    } else {
        // A store already at this schema. Its account rows are a pure function of its key NAMES and
        // this same classifier, so they are re-derived rather than copied — which is what lets this
        // work from `(name, value)` at all. The report of THAT fill is discarded: it describes work
        // the store has already had done to it.
        tx.execute_batch(crate::schema::DDL)?;
        for (name, value) in &stored_of(Table::NodeKey) {
            tx.execute("INSERT INTO node_key (name, value) VALUES (?1, ?2)", (name, value))?;
        }
        crate::schema::write_rows(&tx, &stored_of(Table::Credential), &planned.comments, classify)?;
    }

    let report = fill_into(&tx, planned, classify, version, !planned.exists)?;
    // Nothing is committed and nothing could be: the database has no file. The rollback is stated
    // rather than left to the drop so that the intent is on the page.
    tx.rollback()?;
    Ok(report)
}

/// **Schema 1's two flat tables — the shape this code no longer WRITES and still READS.**
///
/// A test that planted its own approximation of the old DDL would be proving the reshape against a
/// table nobody ever shipped, so the real one is stated here once. `crate::schema::DDL` is the
/// shape every real create uses.
///
/// ⚠ It sat behind `test-support` until [`preview_rows`] needed it: a dry run over an unmigrated
/// store builds the schema-1 shape IN MEMORY so the real reshape can be run against it, and that is
/// production code. The feature still gates [`plant_schema_1`], which is the part that puts a
/// superseded schema on DISK and that nothing in a shipped binary may reach.
pub const SCHEMA_1: &str = "\
CREATE TABLE IF NOT EXISTS credential (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS node_key   (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
";

/// **Plant a finished schema-1 store** — the state both live boxes are in, for a test that has to
/// reshape one.
///
/// Behind `test-support`, and it is the ONE function in this crate that creates a database
/// [`migrate`] did not. It is not a second migration: it writes exactly the rows it is handed into
/// exactly the shape schema 1 had, and it exists because the reshape's whole claim is about a
/// database this code will never write again.
///
/// # Errors
/// The engine, or the filesystem.
#[cfg(feature = "test-support")]
pub fn plant_schema_1(
    path: &Path,
    credentials: &[(String, String)],
    node_keys: &[(String, String)],
) -> Result<(), DbError> {
    if let Some(dir) = path.parent() {
        create_dir_private(dir).map_err(|e| DbError::io(path, e))?;
    }
    create_file_private(path).map_err(|e| DbError::io(path, e))?;
    let mut conn = Connection::open(path).map_err(|e| DbError::sql(path, e))?;
    conn.execute_batch(SCHEMA_1).map_err(|e| DbError::sql(path, e))?;
    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    for (table, rows) in [(Table::Credential, credentials), (Table::NodeKey, node_keys)] {
        let sql = format!("INSERT INTO {} (name, value) VALUES (?1, ?2)", table.sql_name());
        for (name, value) in rows {
            tx.execute(&sql, (name, value)).map_err(|e| DbError::sql(path, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    conn.pragma_update(None, "user_version", 1i64).map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// The version this database carries, refused unless it is one this code can read.
fn check_schema_version(path: &Path, conn: &Connection) -> Result<i64, DbError> {
    let found: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| DbError::sql(path, e))?;
    if READABLE_SCHEMA_VERSIONS.contains(&found) {
        Ok(found)
    } else {
        Err(DbError {
            path: path.to_path_buf(),
            kind: DbErrorKind::SchemaVersion { found, expected: SCHEMA_VERSION },
        })
    }
}

/// `mkdir -p` at 0700 on unix — the components this call creates only. An existing directory's mode
/// is the operator's, and is REPORTED by `crate::store::permission_warning` rather than changed.
#[cfg(unix)]
fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Windows has no mode bits; the equivalent question is an ACL query, which needs a Win32 crate this
/// workspace does not carry. The same no-op posture `crate::store::permission_warning` already takes.
#[cfg(not(unix))]
fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Create the database file EMPTY at 0600, before the engine sees the path.
///
/// A zero-length file is a valid empty SQLite database, so this costs nothing and closes the window
/// in which rows would exist at the umask's mode. `create_new` means a racing sibling that got there
/// first is joined, never clobbered.
#[cfg(unix)]
fn create_file_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
fn create_file_private(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// Every row of one table, as the map the rest of the workspace speaks.
///
/// The WHOLE table, for the callers that need the SET: a live mount composes its key names from
/// `vike_model::credential_keys`' grid, each venue's own `*_env_var_names` and an UNBOUNDED account
/// label, so no list it could hand a reader would be complete; and `vike-cli secrets list` /
/// `config show` have the store itself as their subject. `crate::store::resolve`'s file arm returns
/// the whole file, so on a box with no database this is also the only shape available.
///
/// ⚠ **This doc said a per-key `SELECT` would be "a second shape for callers to reason about with
/// no reader that wants one", and that clause EXPIRED on 2026-09-16**, when the owner ruled that a
/// process must materialise only what it asked for. There is now a reader that wants one:
/// [`read_table_scoped`], reached through `crate::store::resolve_project_scoped`, whose whole
/// subject is a caller needing one or two names and holding nothing else.
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` had already named that as the debt
/// the database left behind — *"Only the scoped read supplies it."*
///
/// This function did not change and no caller of it should move without a reason: the set is a
/// legitimate need, and forcing a live daemon through a hand-assembled name list would trade a
/// bounded blast radius for a silent arming defect.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub fn read_table(path: &Path, table: Table) -> Result<SecretMap, DbError> {
    let (conn, version) = open_for_read(path)?;
    read_table_on(path, &conn, table, version)
}

/// **Only the DECLARED names of one table** — the scoped twin of [`read_table`], and the one place
/// the narrowing is a genuinely narrower QUERY rather than a filter.
///
/// Owner ruling, 2026-09-16: restrict what a process materialises. A row outside `scope` is never
/// selected, so it never enters this process's address space at all — which is the difference
/// between this arm and `crate::store::resolve_scoped`'s file arm, where the whole file must be
/// parsed before anything can be dropped (that type's own ⚠ section says so).
///
/// # ⚠ It must answer EXACTLY as [`read_table`] does for the names it was given
///
/// One statement per declared name, carrying the SAME `WHERE` predicate and the SAME ordering
/// [`read_table_on`] builds — from the same two helpers, so the two cannot drift. That is not
/// tidiness: the widened `superseded_at` predicate is what keeps a TIER ALIAS
/// (`{VENUE}_MAINNET_API_KEY` beside its `_LIVE_` twin, filed superseded because
/// `crate::schema`'s `credential_one_live_value` admits one live row per `(account_id, field)`)
/// in the map, and the `id` ordering is what makes the last-wins fold deterministic. A scoped
/// `SELECT` that spelled a flat `superseded_at IS NULL` would silently drop a key the operator
/// wrote — the exact defect that predicate was widened to fix, reintroduced one function over.
/// `crates/vike-secrets/tests/scoped_read.rs`'s
/// `a_scoped_read_answers_exactly_like_the_whole_table_read_name_for_name` folds both readers over
/// the same planted store and compares, rather than asserting it.
///
/// Per-name rather than one `name IN (…)`: the scopes this serves are one to six names, the
/// `credential_one_live_name` index makes each lookup a seek, and a bound list has a parameter
/// ceiling this would then have to chunk around. The name is a BOUND parameter either way — only
/// `table.sql_name()`, a `&'static str` from a closed enum, is interpolated.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub fn read_table_scoped(
    path: &Path,
    table: Table,
    scope: &crate::store::KeyScope,
) -> Result<SecretMap, DbError> {
    let (conn, version) = open_for_read(path)?;
    read_table_scoped_on(path, &conn, table, version, scope)
}

/// The `WHERE` predicate (no `WHERE` keyword) that selects the rows a read may see, or `""`.
///
/// Extracted so [`read_table_on`] and [`read_table_scoped_on`] cannot spell it differently — see
/// [`read_table_scoped`] for what a divergence would cost.
fn live_predicate(table: Table, version: i64) -> &'static str {
    match (table, version) {
        // Schema 1 has no `superseded_at` column, so naming it would be a prepare-time error.
        (_, 1) | (Table::NodeKey, _) => "",
        (Table::Credential, _) => {
            "superseded_at IS NULL \
               OR NOT EXISTS (SELECT 1 FROM credential live \
                              WHERE live.name = credential.name AND live.superseded_at IS NULL)"
        }
    }
}

/// ⚠ `ORDER BY name, id` and not `name` alone. The rows are folded into a `BTreeMap`, so the LAST
/// row of a name wins, and [`live_predicate`]'s widened clause can return more than one row for a
/// name that has no live one. `id` is monotonic per INSERT, so the newest row answers —
/// deterministic on every box and on every run, which row order is not.
///
/// The scoped reader binds one name at a time, so this reduces to `ORDER BY id` there; it is the
/// same string rather than a second one for the reason [`live_predicate`] is shared.
fn order_clause(table: Table, version: i64) -> &'static str {
    match table {
        Table::Credential if version != 1 => " ORDER BY name, id",
        _ => " ORDER BY name",
    }
}

/// [`read_table_scoped`] on an open connection.
fn read_table_scoped_on(
    path: &Path,
    conn: &Connection,
    table: Table,
    version: i64,
    scope: &crate::store::KeyScope,
) -> Result<SecretMap, DbError> {
    let mut out = BTreeMap::new();
    if scope.is_empty() {
        return Ok(SecretMap::new(out));
    }
    let live = live_predicate(table, version);
    let where_clause = if live.is_empty() {
        " WHERE name = ?1".to_string()
    } else {
        format!(" WHERE name = ?1 AND ({live})")
    };
    // `table.sql_name()` is a `&'static str` from a closed enum — see `Table`. The NAME is a bound
    // parameter, so no caller-supplied text reaches the statement.
    let sql = format!(
        "SELECT value FROM {}{where_clause}{}",
        table.sql_name(),
        order_clause(table, version)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    for name in scope.as_set() {
        let rows = stmt
            .query_map([name.as_str()], |r| r.get::<_, String>(0))
            .map_err(|e| DbError::sql(path, e))?;
        // LAST wins, matching the `BTreeMap` fold `read_table_on` performs over the same ordering.
        let mut last = None;
        for row in rows {
            last = Some(row.map_err(|e| DbError::sql(path, e))?);
        }
        if let Some(value) = last {
            out.insert(name.clone(), value);
        }
    }
    Ok(SecretMap::new(out))
}

/// # ⚠ The version branch, and why the READERS CANNOT TELL THE TWO APART
///
/// Schema 2 keeps `credential.name` and `credential.value` and — because §11's steps 3 and 4 are
/// deliberately not performed (`crate::schema`'s module doc argues the sequencing rule that forbids
/// them) — it still holds a row for EVERY live name. So the two spellings below select the same set
/// over the same store, and that is the whole of the "the readers must not notice" claim:
/// `crates/vike-secrets/tests/database_migration.rs`'s
/// `a_reshaped_store_answers_byte_for_byte_like_the_flat_one_it_came_from` compares the two maps
/// directly rather than asserting it.
///
/// The ONE difference is the `WHERE`, and it is load-bearing rather than tidy: schema 2 can hold
/// SUPERSEDED rows (§4.2's rollback copies), which carry the SAME `name` as the live value that
/// replaced them. Without the clause a superseded `ASTER_*` value and its live replacement would
/// collapse into one `BTreeMap` key and the winner would be decided by row order — i.e. a MAINNET
/// key on the one venue in this store that trades real money, chosen arbitrarily.
///
/// # ⚠ …and the one row that is superseded and STILL ANSWERS
///
/// The clause is *this name has a live row and this is not it*, not *superseded rows are invisible*,
/// and the difference is a whole key. A tier ALIAS — `{VENUE}_MAINNET_API_KEY` beside its
/// `{VENUE}_LIVE_API_KEY` twin — is filed `superseded_at IS NOT NULL` because
/// `credential_one_live_value` admits ONE live row per `(account_id, field)` and the two spellings
/// derive the same one (`crate::schema::RowReport::aliases` carries that argument in full). Its
/// NAME, though, appears on no live row at all. A flat `superseded_at IS NULL` therefore DROPPED it
/// from the map — a name the operator wrote in `secrets.env`, present in the database, absent from
/// `crate::resolve_project` and from `vike-cli secrets list`, with nothing saying so. So the `WHERE`
/// admits a superseded row whose name no live row carries, which is exactly the set the flat clause
/// was reaching for.
///
/// The §4.2 rollback copies are untouched by that widening and cannot be reached by it: a rollback
/// copy is only ever written for a name that HAS a live row (`crate::schema::write_rows` refuses
/// the rest — `SchemaRefusal::SupersededKeyIsNotInTheStore`), so the `NOT EXISTS` is false for
/// every one of them.
///
/// ⚠ **It is a DEVIATION from §11's printed read** (*"`SELECT name, value FROM credential WHERE
/// superseded_at IS NULL` for the 50"*), and it is stated here rather than edited into a signed
/// spec. §11 is describing the mirror-period RENDERER over a store in which every name has a live
/// row, which was true of every shape that spec enumerates; the tier alias is a name that does not,
/// and the flat clause would answer for it with nothing. The two agree everywhere §11 was looking.
fn read_table_on(
    path: &Path,
    conn: &Connection,
    table: Table,
    version: i64,
) -> Result<SecretMap, DbError> {
    // `table.sql_name()` is a `&'static str` from a closed enum — see `Table`. No operator input
    // reaches this string, and the `WHERE` below is a literal chosen by a match on an integer.
    // Both clauses come from the helpers the SCOPED reader also calls, so the two cannot drift —
    // `read_table_scoped`'s doc carries what a divergence would cost.
    let predicate = live_predicate(table, version);
    let live = if predicate.is_empty() { String::new() } else { format!(" WHERE {predicate}") };
    let order = order_clause(table, version);
    let sql = format!("SELECT name, value FROM {}{live}{order}", table.sql_name());
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, value) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert(name, value);
    }
    Ok(SecretMap::new(out))
}

// ---------------------------------------------------------------------------------------------
// Reading the ACCOUNT table
// ---------------------------------------------------------------------------------------------

/// **The schema that first carried an `account` table.**
///
/// Named rather than spelled `2` at the one branch that needs it, because the branch is not *is
/// this the current schema* — [`SCHEMA_VERSION`] already answers that — but *is this old enough
/// that the table does not exist*. [`READABLE_SCHEMA_VERSIONS`] deliberately keeps a schema-1 store
/// readable, so the two questions have different answers on a box that has not run
/// `vike-cli secrets migrate` yet, and conflating them is how [`read_accounts`] would have handed
/// that box a raw `no such table: account` out of the engine.
pub const ACCOUNT_TABLE_SCHEMA: i64 = 2;

/// **One account of one venue, as the `account` table holds it** — the row that replaced the key
/// NAME as the answer to *which account is this*.
///
/// The columns are [`crate::schema::DDL`]'s, minus `notes`, and the omission is
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.3's rule rather than an
/// oversight: *notes are for humans, and code NEVER reads them — the moment code parses a note it
/// is not a note*, it is an undeclared schema with no validation and no gate. A reader that
/// RETURNED the column would be the invitation to parse it, and a fact code needs gets a column of
/// its own. A human-facing renderer that prints provenance adds it back as a display-only field and
/// argues for itself; nothing on the arming side needs it.
///
/// ⚠ **No value, no secret, no credential — by construction rather than by care.** An account row
/// carries identity and nothing else; the values live in `credential`, which [`read_accounts`]
/// never selects from. That is what makes `Debug` here safe to log verbatim, the same contract
/// [`DbError`] holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Account {
    /// **The identity** — opaque, and stable **for the life of ONE database file**. The owner's
    /// ruling at the spec's signature is that this is what identifies an account;
    /// [`Account::label`] is not.
    ///
    /// ⚠ **"Permanent, never reused" is the guarantee WITHIN a file, and this doc used to state it
    /// without the qualifier.** The scope is not a technicality — it is the difference between a
    /// number an operator may write down and one they may not:
    ///
    /// * **within a file it is permanent until something REMOVES a row — and since the account
    ///   lifecycle landed, something can.** This bullet used to read *"genuinely permanent"* and
    ///   justified it with *"nothing in this crate ever `UPDATE`s `account.id` or deletes a row"*.
    ///   The first half still holds; the second stopped being true the day [`AccountEdit::Remove`]
    ///   shipped, and the sentence did not move with it. What is left: `crate::schema`'s
    ///   `AccountResolver::resolve` takes its ids from `last_insert_rowid()`, nothing ever `UPDATE`s
    ///   an id, and a re-run of the migration RESOLVES existing rows (by owner prefix, then by
    ///   `(venue, tier, label)`) rather than re-creating them — so an id is stable across restarts,
    ///   re-migrations of the SAME file, and every other write this crate performs.
    ///
    ///   ⚠ **But `account.id INTEGER PRIMARY KEY` carries no `AUTOINCREMENT`** (the schema has none
    ///   anywhere), so SQLite hands a new row `max(rowid) + 1`. Delete the row holding the LARGEST
    ///   id and the next `Add` is handed that same number. Concretely: remove account 16, add an
    ///   account, and the new one IS account 16. Nothing is corrupted and no write goes to the
    ///   wrong row — but a number an operator, a runbook or a wire client REMEMBERED from before
    ///   the removal now names a different account. That is why the verbs echo the row's credential
    ///   KEY NAMES before acting (`echo_row`) instead of trusting the number, and why the rule below
    ///   is unchanged rather than merely advisable;
    /// * **across a RE-CREATED file it is not.** `account.id INTEGER PRIMARY KEY` is the SQLite
    ///   rowid with no `AUTOINCREMENT`, and a fresh migration assigns ids in the order
    ///   `AccountResolver` meets credential names — `BTreeMap` order over the store's key names. Add
    ///   a key, remove one, or rename a venue, and the same accounts come back numbered
    ///   differently.
    ///
    /// ⚠ **That second case is REACHABLE, and this tree documents the route.** The unrecoverable
    /// half-migration ([`DbErrorKind::SchemaVersion`] with `found == 0`) has exactly one repair and
    /// it is *delete the database and run the migration again*. A `venue_account_id` an operator
    /// wrote against the OLD ids is not carried across that — the column is filled by
    /// [`set_venue_account_id`] and by nothing a migration performs — so after a re-migration the
    /// books are absent, and if they are re-entered from a note that says *account 2 is 1234567*,
    /// account 2 may now be the other broker. **Re-read `vike-cli secrets accounts` and identify
    /// each row again after any re-migration**; the row's own credential key names
    /// ([`read_account_keys`]) are what identify it, never a remembered id.
    pub id: i64,
    /// A `vike_model::VENUES` id.
    pub venue: String,
    /// One of [`crate::schema::ACCOUNT_TIERS`] — the arming ceiling's vocabulary, lowercase.
    pub tier: String,
    /// The operator's name for the ROLE, and **`None` is the ordinary answer**.
    ///
    /// ⚠ A reader may not synthesise one. Every account a migration creates carries `NULL` here —
    /// `crate::schema`'s `AccountResolver` writes whatever the classifier supplied, and the owner
    /// refused the provisional `DEMO1`/`DEMO2` spellings outright at signature: *labels are
    /// informative and optional, `id` is the identity*. Rendering `"DEFAULT"` for a `None` would
    /// put back the string `vike_model::account_keys::AccountLabel::parse` refuses as reserved, and
    /// rendering an index token would put an identity back into the one column this schema exists
    /// to stop carrying one.
    pub label: Option<String>,
    /// **The BOOK, as the venue names it** — `None` until the venue has been asked.
    ///
    /// ⚠ It is `None` on every row a MIGRATION writes, not merely on dukascopy's two.
    /// [`crate::schema`]'s module doc states why: §11's steps 3 and 4 are not performed, so the ten
    /// book keys are still `credential` rows carrying their legacy names and nothing folds them
    /// here. A caller that needs one of those books today must read the legacy name; a caller that
    /// reads this column must treat `None` as *not yet known* and never as *this account has no
    /// book*.
    ///
    /// ⚠ **It is no longer `None` on every row of every store**, and the difference is one source
    /// out of three. [`set_venue_account_id`] fills it for a book an OPERATOR supplies, which is
    /// the only way the two dukascopy demo accounts can be told apart at all — their numbers are in
    /// no store and derivable from nothing in one. The other two sources of §4.5 are still owed and
    /// still unwritten: the migration's fold of the ten stored book keys (§11 step 3, sequenced
    /// behind the map renderer §7 requires) and the venue's own handshake (§12). So a `Some` here
    /// means *somebody or something told this store the book*, and this column does not record
    /// which — [`Account::last_verified_at`] is the column that does, and [`BookSource`] is how
    /// [`set_venue_account_id`] is told which of the two it is performing.
    pub venue_account_id: Option<String>,
    /// A sub-account's master. `None` is the ordinary answer and means *not a sub-account*.
    pub parent_id: Option<i64>,
    /// `false` = the operator no longer uses this account. See [`Accounts::active_for_venue`].
    pub active: bool,
    /// **Set by a successful authenticated SESSION**, and by nothing else. `None` on every migrated
    /// row, and on every row an operator has written a book onto by hand.
    ///
    /// ⚠ This doc read *"this column has no writer in the tree today"* until 2026-09-15, and that
    /// is the gap [`BookSource::Handshake`] closes: the writer is [`set_venue_account_id`] under
    /// that arm, reached by a fold of what a venue's own handshake answered. [`BookSource::Operator`]
    /// leaves it untouched, deliberately — see that enum, which carries the whole argument.
    ///
    /// The value is an RFC 3339 instant the CALLER formats
    /// (`vike_model::time::epoch_ms_to_utc_timestamp`); this crate carries no time dependency and
    /// parses nothing here. ⚠ The instant is the HANDSHAKE's, never the fold's — the column answers
    /// *when did this credential last authenticate*, and a fold two days later stamping its own
    /// `now` would answer *when was the CLI run* while looking like the first.
    pub last_verified_at: Option<String>,
}

/// **Why a store cannot answer the account question AT ALL** — which is a different answer from
/// *this store holds no accounts*.
///
/// The distinction is the whole reason [`Accounts`] is an enum. [`crate::store::Backend`] is a
/// per-RUN choice and a box that has not migrated has no `account` table on it; collapsing that
/// into an empty list would hand every caller the sentence *this venue has no accounts* about a
/// store that is holding its credentials perfectly well under their legacy names. That is the
/// shape of the failure §1 of the spec is about — a store answering confidently about something it
/// cannot see — and it is the same posture this crate already takes between an ABSENT store (the
/// live gate, an empty map) and one that EXISTS and will not open ([`SecretsError`], loud).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAccountTable {
    /// [`crate::store::Backend::Files`] — there is no settings database on this box, so the
    /// credential FILE answers and the accounts live in the key NAMES.
    /// `vike_model::account_keys::accounts_in_store` is the reader that applies to that store.
    FileStore {
        /// The credential file that is answering instead.
        file: PathBuf,
    },
    /// A settings database at a schema OLDER than [`ACCOUNT_TABLE_SCHEMA`] — readable (that is what
    /// [`READABLE_SCHEMA_VERSIONS`] buys) and carrying no such table. `vike-cli secrets migrate` is
    /// what moves it.
    OlderSchema {
        /// The database that was opened.
        path: PathBuf,
        /// The schema it carries, as `PRAGMA user_version` reported it.
        found: i64,
    },
}

impl std::fmt::Display for NoAccountTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoAccountTable::FileStore { file } => write!(
                f,
                "the credential store {} is a FILE, which carries no account table — the accounts \
                 of a file store are in the key names",
                file.display()
            ),
            NoAccountTable::OlderSchema { path, found } => write!(
                f,
                "the settings database {} is at schema {found}, which predates the account table \
                 (schema {ACCOUNT_TABLE_SCHEMA}) — `vike-cli secrets migrate` is what moves it",
                path.display()
            ),
        }
    }
}

/// **What a store can say about the accounts it holds.**
///
/// Two arms and not an `Option<Vec<_>>`, because the caller that merges them is the caller this
/// type exists to stop: [`Accounts::Known`] with an empty vector is *the table is there and holds
/// nothing*, and [`Accounts::Unanswerable`] is *this store cannot be asked*. See
/// [`NoAccountTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounts {
    /// The store carries an `account` table and these are its rows, **`id`-ordered** — every row,
    /// inactive ones included, because filtering is the caller's decision and a silent one here
    /// would retire an account nobody asked to retire.
    Known(Vec<Account>),
    /// The store has no `account` table. Carries WHICH store, and why.
    Unanswerable(NoAccountTable),
}

impl Accounts {
    /// The rows, or `None` when the store could not be asked.
    #[must_use]
    pub fn known(&self) -> Option<&[Account]> {
        match self {
            Accounts::Known(rows) => Some(rows),
            Accounts::Unanswerable(_) => None,
        }
    }

    /// Why the store could not be asked, or `None` when it answered.
    #[must_use]
    pub fn unanswerable(&self) -> Option<&NoAccountTable> {
        match self {
            Accounts::Known(_) => None,
            Accounts::Unanswerable(why) => Some(why),
        }
    }

    /// **Every ACTIVE account of one venue**, `id`-ordered — or `None` when the store could not be
    /// asked.
    ///
    /// ⚠ `Some(&[])` and `None` are DIFFERENT ANSWERS and a caller may not merge them: the first
    /// is *this store knows its accounts and this venue has none*, the second is *ask the key
    /// names instead*. The `active` filter is in the NAME rather than behind a flag, so a call site
    /// cannot arm a retired account by forgetting an argument; [`Accounts::Known`] is where a
    /// caller that genuinely wants the inactive rows goes.
    #[must_use]
    pub fn active_for_venue(&self, venue: &str) -> Option<Vec<&Account>> {
        Some(self.known()?.iter().filter(|a| a.active && a.venue == venue).collect())
    }
}

/// **Every row of the `account` table** — the reader that asks the DATABASE which accounts exist,
/// rather than parsing them out of credential key names.
///
/// The twin of [`read_table`] for the third table, and deliberately the same shape: the WHOLE
/// table, because a per-venue `SELECT` would be a second query shape for callers to reason about
/// and the table is sixteen rows on the live box. Filtering is [`Accounts::active_for_venue`], over
/// the rows this returned.
///
/// # What it never does
///
/// * **It never touches `credential`.** No value of any kind is selected, so no error, no `Debug`
///   and no log line reachable from here can become a credential.
/// * **It never writes.** [`open_for_read`] opens `SQLITE_OPEN_READ_ONLY` and creates nothing —
///   which matters more here than usual, because a reader that could create the file would turn
///   *this box has no database* into *this box has an empty database*, and under
///   [`crate::store::Backend`] that is the difference between the credential file answering and
///   every venue silently dropping to paper.
/// * **It never answers `Known(vec![])` for a store that has no table.** See [`NoAccountTable`].
///
/// # The version branch
///
/// A schema-1 database is READABLE by design ([`READABLE_SCHEMA_VERSIONS`]) and has no `account`
/// table, so the `SELECT` below would fail at PREPARE with the engine's own `no such table`. That
/// arrives as [`DbErrorKind::Sqlite`] — an opaque malfunction — for a store that is simply older
/// than the table. The branch turns it into the classification it actually is.
pub fn read_accounts(path: &Path) -> Result<Accounts, DbError> {
    let (conn, version) = open_for_read(path)?;
    if version < ACCOUNT_TABLE_SCHEMA {
        return Ok(Accounts::Unanswerable(NoAccountTable::OlderSchema {
            path: path.to_path_buf(),
            found: version,
        }));
    }
    // ⚠ `ORDER BY id` and not by `(venue, tier)`: `id` is the identity (the owner's ruling), it is
    // monotonic per INSERT, and it is the one column guaranteed present and distinct on every row —
    // so the order is stable across boxes and across runs, which a nullable `label` is not.
    let mut stmt = conn
        .prepare(
            "SELECT id, venue, tier, label, venue_account_id, parent_id, active, last_verified_at \
             FROM account ORDER BY id",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Account {
                id: r.get(0)?,
                venue: r.get(1)?,
                tier: r.get(2)?,
                label: r.get(3)?,
                venue_account_id: r.get(4)?,
                parent_id: r.get(5)?,
                // `STRICT` types the column INTEGER and the DDL's `CHECK (active IN (0, 1))` is
                // what makes this comparison exact rather than a truthiness convention.
                active: r.get::<_, i64>(6)? != 0,
                last_verified_at: r.get(7)?,
            })
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(Accounts::Known(out))
}

/// **What tells one account row from another when the row itself does not** — the credential key
/// NAMES it owns, and the owner PREFIXES those names imply.
///
/// # Why a listing needs this at all
///
/// `(id, venue, tier, label, venue_account_id)` is the whole of an [`Account`], and for the pair
/// this schema's hardest case is about — dukascopy's two demo rows, `(dukascopy, demo, NULL)`
/// twice over, both books `NULL` until somebody writes them — every one of those cells is
/// IDENTICAL except `id`. A listing built from [`Account`] alone therefore renders the two rows the
/// operator must tell apart as two indistinguishable lines differing by an opaque integer, and
/// [`set_venue_account_id`] addresses a row by exactly that integer. There is no way to choose.
///
/// The discriminating fact is already in the store and is not in the `account` table: each row's
/// own `credential` rows carry their LEGACY NAMES (§4.1 of
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md`: *`name` is the only record*), so
/// `DUKASCOPY_DEMO1_LOGIN` belongs to one row and `DUKASCOPY_DEMO2_LOGIN` to the other. That is
/// what an operator recognises, and it is what this type carries.
///
/// # ⚠ NAMES ONLY — never a value, and structurally so
///
/// [`read_account_keys`] selects `name` and `field` and nothing else. `credential.value` is not in
/// the statement, so no row, no `Debug`, no error and no rendering reachable from this type can be
/// a credential — the same construction [`Account`] holds by never touching the table at all.
/// A caller may print every field of this verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountKeys {
    /// The OWNER PREFIXES — each credential name with its own `field` suffix removed, deduplicated
    /// and sorted. `DUKASCOPY_DEMO1_`, `BINANCE_LIVE_`.
    ///
    /// The same derivation `crate::schema`'s `AccountResolver::load` performs to build `by_prefix`,
    /// and it is deliberately the same one: that map is how a re-run of the migration finds the
    /// account an earlier run created, so a prefix here is the identity the STORE itself uses.
    ///
    /// ⚠ **More than one is normal, not a fault.** A legacy tier spelling and its canonical twin
    /// are one account with two prefixes (`ASTER_MAINNET_` and `ASTER_LIVE_`, unified because
    /// `AccountRef::tier` normalizes `MAINNET` onto `LIVE`), and both belong on the row.
    ///
    /// Empty when no live credential row names this account — an account row can outlive the last
    /// key that created it, and an empty list says so rather than pretending.
    pub prefixes: Vec<String>,
    /// Every live credential key NAME filed against this account, sorted.
    ///
    /// The evidence behind [`AccountKeys::prefixes`], for the case where a prefix is not enough —
    /// and the thing an operator actually recognises from their own `secrets.env`.
    pub names: Vec<String>,
}

/// **The credential key NAMES each `account` row owns** — the discriminator [`read_accounts`]
/// cannot return, keyed by [`Account::id`].
///
/// See [`AccountKeys`] for what this is for and why a listing is unusable without it. A row with no
/// live credential rows is simply absent from the map; a caller renders that as an empty
/// [`AccountKeys`] (which is its [`Default`]).
///
/// # What it never does
///
/// * **It never selects a value.** The statement is `SELECT name, field, account_id FROM
///   credential`, so a credential value is not merely omitted from the result — it is not read.
/// * **It never writes**, and it never creates: [`open_for_read`]'s `SQLITE_OPEN_READ_ONLY`, for
///   the reason spelled at [`read_accounts`].
/// * **It skips SUPERSEDED rows** (`superseded_at IS NULL`), exactly as `AccountResolver::load`
///   does — a name that has been rotated out is not evidence about who the row is today.
///
/// A schema older than [`ACCOUNT_TABLE_SCHEMA`] has no `account` table and therefore no
/// `credential.account_id`; it answers with an EMPTY map rather than an error, because its caller
/// has already been told the real answer by [`read_accounts`] — which returns
/// [`Accounts::Unanswerable`] for that store and is the authority on it.
pub fn read_account_keys(path: &Path) -> Result<BTreeMap<i64, AccountKeys>, DbError> {
    let (conn, version) = open_for_read(path)?;
    if version < ACCOUNT_TABLE_SCHEMA {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT name, field, account_id FROM credential \
             WHERE superseded_at IS NULL AND account_id IS NOT NULL ORDER BY name",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })
        .map_err(|e| DbError::sql(path, e))?;

    let mut out: BTreeMap<i64, AccountKeys> = BTreeMap::new();
    let mut prefixes: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let (name, field, id) = row.map_err(|e| DbError::sql(path, e))?;
        // The SAME derivation `AccountResolver::load` performs — see `AccountKeys::prefixes`. A
        // classifier whose `field` is not a suffix of the name yields no prefix (the LABELLED
        // grammar, legitimately), and the NAME below still carries the evidence.
        if let Some(prefix) = name.strip_suffix(field.as_str()) {
            prefixes.entry(id).or_default().insert(prefix.to_string());
        }
        out.entry(id).or_default().names.push(name);
    }
    for (id, set) in prefixes {
        out.entry(id).or_default().prefixes = set.into_iter().collect();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing — the BOOK column
// ---------------------------------------------------------------------------------------------

/// The longest `venue_account_id` this store will accept.
///
/// Measured against the widest shape any roster venue actually names a book with: a
/// hyperliquid/aster EVM address is 42 characters (`0x` + 20 bytes hex), and every other venue in
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §7 answers with something shorter —
/// dukascopy's measured numbers are seven digits, IBKR's is `DU` plus six. The cap is not a wire
/// constraint and nothing downstream depends on it; it is a floor under [`normalized_venue_account_id`]
/// so a whole file pasted into the flag cannot become a row.
pub const VENUE_ACCOUNT_ID_MAX_BYTES: usize = 128;

/// **Invisible at both edges of a paste** — whitespace, control characters, and the FORMAT
/// characters neither of those two predicates covers.
///
/// ⚠ **`char::is_control` is category Cc ALONE and `char::is_whitespace` is the `White_Space`
/// property, so between them they classify none of Cf** — and `str::trim` strips only the second.
/// U+FEFF (a byte-order mark), U+200B (a zero-width space) and U+200E (a left-to-right mark)
/// therefore survive both a `trim` and a `is_whitespace() || is_control()` test, which is exactly
/// what [`normalized_venue_account_id`] used to apply. A value pasted off a venue's own page with a
/// leading BOM would have been STORED with it: identical to another row's book on every screen an
/// operator can read, and UNEQUAL to it in `account_one_account_per_book`, so the one-account-per-
/// book rule would be defeated by a character nobody can see. That is the failure this predicate
/// exists for, and it is why the cure is not "strip the whitespace harder".
///
/// The list is the Cf characters that can plausibly ride along on a copy — the bidi marks and
/// embeddings, the zero-width family, the word joiner and invisible operators, the soft hyphen and
/// the Mongolian vowel separator. It does not need to be the whole of Cf: an INTERIOR one is
/// refused by [`normalized_venue_account_id`]'s ASCII-graphic rule whether it is named here or not,
/// so this list only has to cover what a paste can leave at an EDGE.
fn is_invisible(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{00AD}'                  // SOFT HYPHEN
            | '\u{061C}'                // ARABIC LETTER MARK
            | '\u{180E}'                // MONGOLIAN VOWEL SEPARATOR
            | '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | '\u{202A}'..='\u{202E}'   // the bidi embeddings and overrides
            | '\u{2060}'..='\u{2064}'   // WORD JOINER and the invisible operators
            | '\u{2066}'..='\u{2069}'   // the bidi isolates
            | '\u{FEFF}'                // ZERO WIDTH NO-BREAK SPACE — a pasted byte-order mark
        )
}

/// **The ONE predicate for what may be a `venue_account_id`** — the cleaned value, or `None`.
///
/// Surrounding INVISIBLES are TRIMMED rather than refused ([`is_invisible`] — whitespace, control
/// characters AND the format characters `str::trim` leaves behind): an operator reads the number off
/// the venue's own page and pastes it, and a trailing space or a leading byte-order mark is not a
/// different account. Everything else is a refusal, and the reason is that this column is not free
/// text — it is the value `account_one_account_per_book` compares two accounts by, so `"1234567 "`
/// and `"1234567"` colliding or NOT colliding would be decided by an invisible byte.
///
/// **What survives the trim must be ASCII GRAPHIC end to end** (`0x21..=0x7E`), which is a stronger
/// rule than the `is_whitespace() || is_control()` one it replaces and is stronger on purpose. It
/// refuses, in one test: interior whitespace (a book identifier is one token — two tokens means a
/// label was pasted beside the number), every control character, every FORMAT character (an
/// interior zero-width space is the attack the trim above cannot reach), and every non-ASCII
/// character at all — which closes the homoglyph case, where a Cyrillic `о` renders exactly like a
/// Latin `o` and compares unequal in the index.
///
/// ⚠ **The ASCII rule is a REFUSAL, not a mangling, and widening it is a one-line change with a
/// measurement behind it.** Every book shape §7 of
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` measured is ASCII — a decimal
/// account number, an `0x` EVM address, a `DU`-prefixed IBKR id, a venue login — so a venue that
/// genuinely answers with a non-ASCII identifier is a case nobody has met yet. It arrives here as a
/// loud refusal rather than as a row that quietly does not compare equal to itself, which is the
/// right way round for a column that decides which broker an order reaches.
///
/// Also refused: empty after the trim, and anything over [`VENUE_ACCOUNT_ID_MAX_BYTES`] (measured
/// AFTER the trim, so a padded paste is not refused for the padding).
///
/// ⚠ It is `pub` so the CLI's early refusal and this module's write-path refusal are ONE predicate
/// with two messages. They deliberately do NOT share a message: the CLI names the flag the operator
/// typed, and this module names the store — and neither ever echoes the offending token, because
/// nothing here can know that a value which failed this predicate was not a secret pasted into the
/// wrong flag.
#[must_use]
pub fn normalized_venue_account_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(is_invisible);
    if trimmed.is_empty() || trimmed.len() > VENUE_ACCOUNT_ID_MAX_BYTES {
        return None;
    }
    if !trimmed.chars().all(|c| c.is_ascii_graphic()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// **What [`set_venue_account_id`] did** — the row AS IT WAS BEFORE the write, and what it holds now.
///
/// The `before` row is the point of the type. A `venue_account_id` decides which BROKER an order
/// routes to — dukascopy's two demo accounts are Dukascopy Bank SA and Dukascopy Europe IBS AS, two
/// legal entities — so a caller must be able to say WHICH ROW it changed in the same breath as
/// saying it changed one, out of the transaction that actually performed the write rather than out
/// of a read it did beforehand.
///
/// ⚠ No value, no secret, no credential — the same property [`Account`] has, and by the same
/// construction: this type is built from the `account` table alone, and `venue_account_id` is an
/// account number the venue echoes on its own wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookWrite {
    /// The account row as the write transaction found it — including its previous
    /// [`Account::venue_account_id`], which is `None` on every row a migration wrote.
    pub before: Account,
    /// The value the row carries now, after [`normalized_venue_account_id`] — or `None` when the
    /// call was a CLEAR and the row is back to *not yet known*.
    pub venue_account_id: Option<String>,
    /// `false` when the row already held exactly this BOOK and no book was written. Idempotent
    /// rather than an error: re-running the same command is not a mistake, and refusing it would
    /// make a script that re-asserts a known book fail on its second run. A CLEAR of a row that
    /// already named no book reports `false` for the same reason.
    ///
    /// ⚠ It is a claim about the `venue_account_id` COLUMN alone, so a confirming handshake write
    /// reports `false` here and a `Some` in [`BookWrite::verified_at`] — the row was written, the
    /// book did not move. Read the two together; neither alone is *nothing happened*.
    pub changed: bool,
    /// What this write stamped into `last_verified_at`, or `None` when it stamped nothing.
    ///
    /// `Some` exactly under [`BookSource::Handshake`], carrying the instant that arm supplied. It
    /// is on the result rather than left for the caller to remember, because the confirmation case
    /// — book unchanged, timestamp written — is otherwise indistinguishable from a true no-op in
    /// everything this type reports.
    pub verified_at: Option<String>,
}

/// **WHO is telling the store this book, and therefore whether `last_verified_at` may be stamped.**
///
/// A parameter rather than two functions, and that is the load-bearing choice:
/// `crates/vike-ops/tests/credential_writer_gate.rs` pins the SET of names that write this store,
/// and a second write function for the timestamp is precisely what that gate exists to refuse. One
/// writer, told what it is doing.
///
/// # Why the distinction exists at all
///
/// [`set_venue_account_id`]'s doc used to carry a *What it does NOT write* section saying
/// `last_verified_at` stays untouched, because *"an operator typing a number read off a web page has
/// performed [no authenticated session] from this process — stamping it here would make a
/// hand-entered row indistinguishable from one a handshake confirmed, which is the false confidence
/// §1 of the spec is about."* That argument is not weakened by this enum; it is what the enum
/// ENCODES. [`BookSource::Operator`] is that sentence, and it is still the behaviour of every
/// operator-facing door.
///
/// What changed is that a SECOND caller now exists whose claim is the other one —
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5's handshake writer, folding
/// what a venue answered during a session that really did authenticate. A `bool` would have carried
/// the same bit and none of the argument; a named arm makes the wrong one impossible to pass by
/// accident and impossible to pass without reading why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSource<'a> {
    /// **An OPERATOR supplied this number** — typed at a CLI, read off the venue's own web page.
    /// `last_verified_at` is left exactly as it was: nothing in this process authenticated, and a
    /// hand-entered row that read *verified* would be a row that looks confirmed and is not.
    Operator,
    /// **A VENUE's own successful authenticated handshake answered with it**, at this instant —
    /// an RFC 3339 UTC string the caller formats (this crate has no time dependency).
    ///
    /// Stamps `last_verified_at`, and stamps it **even when the book is unchanged**: *the row
    /// already says what the venue says* is the CONFIRMATION case, and it is the one the column
    /// exists for. A write that only stamped on a change would leave a correctly-configured account
    /// looking never-verified forever.
    Handshake {
        /// The instant the HANDSHAKE succeeded — not the instant of this call. See
        /// [`Account::last_verified_at`].
        verified_at: &'a str,
    },
}

impl BookSource<'_> {
    /// The value to write into `last_verified_at`, or `None` to leave the column alone.
    fn verified_at(&self) -> Option<&str> {
        match self {
            BookSource::Operator => None,
            BookSource::Handshake { verified_at } => Some(verified_at),
        }
    }
}

/// **Write ONE account row's `venue_account_id`** — the column
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 records as having no writer in
/// this tree at all.
///
/// # Which of the column's sources this is, and which it is NOT
///
/// §4.5 names two writers for this column and this function is neither of them, which is the first
/// thing to know before reading it:
///
/// * **the migration's FOLD (§11 step 3)** — ten stored keys that already ARE the book
///   (`OANDA_DEMO_ACCOUNT_ID`, `ALPACA_SANDBOX_ACCOUNT_ID`, `CTRADER_DEMO_ACCOUNT_ID`,
///   `IBKR_DEMO_ACCOUNT`, `HYPERLIQUID_{DEMO,LIVE}_ACCOUNT_ADDRESS`, `ASTER_LIVE_USER`,
///   `IG_DEMO_IDENTIFIER`, `FXCM_DEMO_USER`, `POLY_FUNDER`) move into this column at migration
///   time and no `credential` row is written for them. **That fold is NOT performed by this
///   function and is not shipped**: §7 states its sequencing requirement outright — four of those
///   ten are load-bearing for AUTHENTICATION rather than for labelling, so the map RENDERER that
///   re-synthesizes their legacy names out of this column must be covered BEFORE the fold ships,
///   and §12 records that renderer as owed. `crate::schema`'s migration path is where the fold
///   will live when it does;
/// * **the venue's own HANDSHAKE** — every mount already performs an authenticated handshake that
///   carries the account identifier. ⚠ This read *"and nothing in this tree records what it
///   learned. Also owed (§12)"* until 2026-09-15. It is built: this same function performs it,
///   under [`BookSource::Handshake`], which is why that arm exists rather than a second writer.
///   What reaches it is a fold of the confirmations a mount parks
///   (`vike_model::account_confirmation`), because the deployed daemon's sandbox cannot write this
///   database at all — that module's doc carries the measurement.
///
/// This is the THIRD source, and it exists because one venue's books are reachable by neither: the
/// two dukascopy demo accounts are `(dukascopy, demo, label = NULL)` twice over, they differ only
/// by `id`, and their numbers are not in the credential store and not derivable from anything in
/// it. They were read off the venue by logging in. So an operator supplies them, one row at a time,
/// and this is the door — an operator standing in for the handshake §4.5 describes, by hand.
///
/// ⚠ **The numbers themselves are NOT in this workspace and may never be.** They are one operator's
/// account data: a source literal would be wrong for every other operator and would ship somebody's
/// account numbers into the public mirror.
///
/// # `venue_account_id: None` is a CLEAR, and it is the REPAIR the refusals below need
///
/// `None` puts the column back to `NULL` — *not yet known*, the state every migrated row starts in
/// — and it is the only move on this path that makes no assertion about a broker. It exists because
/// without it two of the refusals below have **no way out**, which is worse than either of them
/// being wrong.
///
/// The case that forced it: an operator writes the pair the wrong way round, so row A names B's
/// book and row B names A's. Correcting either one now fails in BOTH directions —
/// `--replace` gets past [`DbErrorKind::BookAlreadyKnown`] and the holder check then finds the
/// other row and raises [`DbErrorKind::BookHeldByAnother`] — and every ordering of the two writes
/// hits it. Ruling 11's index is right to refuse the intermediate state; what was missing was a
/// third move. `set_venue_account_id(path, a, None, false)` is it: clear one row, write the other,
/// write the first. Three statements, each one a state the index accepts.
///
/// A clear asks NEITHER guard, and both omissions are deliberate — see the comment at the branch.
/// Clearing a row that already names no book is a no-op reported as `changed: false`, the same
/// shape as re-writing a known value.
///
/// # What it refuses, and why each refusal is a refusal rather than a guess
///
/// ⚠ Every refusal in this list is asked of a SET. A CLEAR reaches none of them but the first three
/// (the store, the schema, the id).
///
/// * **a value the predicate will not take** — [`normalized_venue_account_id`];
/// * **a store that has no `account` table** ([`DbErrorKind::NoAccountTable`]) — schema 1 is
///   READABLE by design ([`READABLE_SCHEMA_VERSIONS`]) and carries no such table, so the `SELECT`
///   below would otherwise arrive as an opaque `no such table` for a box that is merely older;
/// * **an `id` no row carries** ([`DbErrorKind::NoSuchAccount`]) — never a create. This function
///   cannot bring an account into existence: `id` is the identity, and inventing a row for an id
///   the operator mistyped is how a book lands on an account nobody has;
/// * **a row that already names a DIFFERENT book** ([`DbErrorKind::BookAlreadyKnown`]), unless the
///   caller passes `replace`. ⚠ This is the interlock that matters. Overwriting a known book with a
///   different number re-points an armed account at another broker, in silence, and a mistyped `id`
///   is exactly how that happens — so the default is refusal, the refusal names the row and the
///   number it already holds, and the caller has to say out loud that the stored number is the
///   wrong one. Re-writing the SAME value is not a change and is reported as `changed: false`;
/// * **a book another ACTIVE row of the same venue already names** ([`DbErrorKind::BookHeldByAnother`])
///   — ruling 11 (§8), whose enforcement is the partial index `account_one_account_per_book`. The
///   pre-check exists only to turn the engine's `UNIQUE constraint failed` into a refusal that
///   names both rows; the INDEX is the authority, and it is what still holds if this check is ever
///   wrong. It is asked only when the target row is ACTIVE, exactly mirroring the index's `WHERE`:
///   §8 makes the constraint partial so that a DEACTIVATED row may keep naming the book of the
///   active row that replaced it, and a stricter check here would refuse that legitimate state;
/// * **a database that VANISHED** between the backend choice and this open
///   ([`DbErrorKind::VanishedDatabase`]) — the same invariant [`upsert_rows`] states at length, and
///   it is not weaker here: [`open_for_write`] creates a database when the path is empty, so
///   without this arm a `set-book` against a box with no database at all would MINT one holding a
///   single account row and nothing else, from which moment [`crate::store::backend_at`] answers
///   `Database` for every process on the box and every credential in the file beside it is retired.
///   [`migrate`] stays the only function here that may bring a database into existence.
///
/// # What it writes besides the book, and what it never writes
///
/// `last_verified_at` is written **only** under [`BookSource::Handshake`], and the whole argument
/// for that split is on [`BookSource`] itself. Under [`BookSource::Operator`] — every
/// operator-facing door, `vike-cli secrets set-book` included — the column stays untouched, exactly
/// as it did before that parameter existed: §4.5 gives it to *a successful authenticated session*,
/// and an operator typing a number read off a web page has performed none from this process.
///
/// ⚠ A `Handshake` write stamps the timestamp **on the unchanged path too** — the
/// `before.venue_account_id == value` early return still writes, and still reports
/// `changed: false` for the BOOK. That is the confirmation case, and it is the case the column
/// exists for; a stamp that required a book change would leave a correctly-configured account
/// reading *never verified* forever. ⚠ It is NOT stamped on a refusal of any kind: a refusal is a
/// transaction that wrote nothing, and a session whose identity claim the store just refused is the
/// single row that must not read *verified*.
///
/// `label` stays untouched under every source, and deliberately: the owner refused the provisional
/// `DEMO1`/`DEMO2` spellings at signature, and this whole column exists so that identity does not
/// have to live in a name.
///
/// Nothing else in the store is read or written: one `UPDATE` of one row, in one transaction, with
/// every `credential` row and both credential FILES untouched.
pub fn set_venue_account_id(
    path: &Path,
    id: i64,
    venue_account_id: Option<&str>,
    replace: bool,
    source: BookSource<'_>,
) -> Result<BookWrite, DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    let value: Option<String> = match venue_account_id {
        Some(raw) => match normalized_venue_account_id(raw) {
            Some(v) => Some(v),
            None => return Err(refuse(DbErrorKind::BookMalformed)),
        },
        // A CLEAR. There is no value to validate, and none of the refusals below apply to it — see
        // the CLEAR section of this function's doc.
        None => None,
    };

    let (mut conn, created, version) = open_for_write(path)?;
    // ⚠ See the refusal list above: the ONLY safe act on this path is to put back what we found.
    // Close the engine BEFORE unlinking — an open handle keeps the file alive on Windows.
    if created {
        drop(conn);
        let _ = std::fs::remove_file(path);
        return Err(refuse(DbErrorKind::VanishedDatabase));
    }
    if version < ACCOUNT_TABLE_SCHEMA {
        return Err(refuse(DbErrorKind::NoAccountTable { found: version }));
    }

    // ⚠ **IMMEDIATE, not the default DEFERRED, and this is the whole of the concurrency story.**
    // Everything below is a read-then-write: the row is SELECTed, the refusals are decided from
    // what it holds, and only then is the UPDATE issued. A DEFERRED transaction takes no lock until
    // its first statement, so it acquires SHARED on that SELECT and must PROMOTE to RESERVED at the
    // UPDATE — and when another connection already holds RESERVED, SQLite refuses that promotion
    // with `SQLITE_BUSY` **immediately, without consulting the busy handler at all**, because
    // sleeping there could deadlock two waiters. So [`BUSY_TIMEOUT`] cannot cover this case and no
    // amount of raising it would: a second writer arriving mid-decision surfaced as a bare
    // `database is locked` no matter what. IMMEDIATE takes RESERVED up front, which is the one form
    // of this transaction the busy handler can actually wait on — and it also makes the decision
    // and the write ATOMIC against another writer rather than merely likely to be.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;

    // The row, read INSIDE the transaction that will write it — so the echo a caller renders is
    // what the write actually saw, not what a read beforehand happened to find.
    let before = tx
        .query_row(
            "SELECT id, venue, tier, label, venue_account_id, parent_id, active, last_verified_at \
             FROM account WHERE id = ?1",
            [id],
            |r| {
                Ok(Account {
                    id: r.get(0)?,
                    venue: r.get(1)?,
                    tier: r.get(2)?,
                    label: r.get(3)?,
                    venue_account_id: r.get(4)?,
                    parent_id: r.get(5)?,
                    active: r.get::<_, i64>(6)? != 0,
                    last_verified_at: r.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    let Some(before) = before else {
        return Err(refuse(DbErrorKind::NoSuchAccount { id }));
    };

    if before.venue_account_id == value {
        // Already exactly this — including a CLEAR of a row that names no book. Nothing to write
        // about the BOOK, and not an error; see `BookWrite::changed`.
        //
        // ⚠ **A HANDSHAKE still stamps here, and this is the CONFIRMATION case** — the row already
        // says what the venue says, which is the single most useful thing `last_verified_at` can
        // record. Before this arm existed the function returned early and a correctly-configured
        // account would have read *never verified* forever, however many sessions it authenticated.
        // `changed` stays `false`, because it is a claim about the BOOK and the book did not move.
        if let Some(verified_at) = source.verified_at() {
            tx.execute("UPDATE account SET last_verified_at = ?2 WHERE id = ?1", (id, verified_at))
                .map_err(|e| DbError::sql(path, e))?;
            tx.commit().map_err(|e| DbError::sql(path, e))?;
            return Ok(BookWrite {
                before,
                venue_account_id: value,
                changed: false,
                verified_at: Some(verified_at.to_string()),
            });
        }
        return Ok(BookWrite {
            before,
            venue_account_id: value,
            changed: false,
            verified_at: None,
        });
    }

    // ⚠ Both guards below are asked only of a SET. A CLEAR is the one act on this path that can
    // never reach a wrong broker: it removes an assertion rather than making one, and the state it
    // leaves — `NULL`, *not yet known* — is the state every migrated row is already in. Requiring
    // `--replace` to clear would put a second word in front of the only move that REPAIRS a wrong
    // write, and asking the holder question would be asking who else names a value there is not.
    if let Some(value) = &value {
        if let Some(current) = &before.venue_account_id {
            // `before.venue_account_id == value` was handled above, so this is a DIFFERENT book.
            if !replace {
                return Err(refuse(DbErrorKind::BookAlreadyKnown {
                    id,
                    venue: before.venue.clone(),
                    tier: before.tier.clone(),
                    current: current.clone(),
                }));
            }
        }

        // Ruling 11's index, asked as a question so the answer can name the other row. Scoped to
        // ACTIVE rows because the index is — see the refusal list.
        if before.active {
            let holder: Option<i64> = tx
                .query_row(
                    "SELECT id FROM account \
                     WHERE venue = ?1 AND venue_account_id = ?2 AND active = 1 AND id <> ?3",
                    (&before.venue, value, id),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::BookHeldByAnother {
                    id,
                    venue: before.venue.clone(),
                    holder,
                }));
            }
        }
    }

    // ONE statement for both columns, so a stamped write cannot half-land: the timestamp says *this
    // book is what the venue answered at that instant*, and a commit carrying one without the other
    // would be a claim nobody made. `COALESCE(?3, last_verified_at)` leaves the column alone under
    // `BookSource::Operator` — a NULL parameter is *not my business*, never *clear it*.
    tx.execute(
        "UPDATE account SET venue_account_id = ?2, \
         last_verified_at = COALESCE(?3, last_verified_at) WHERE id = ?1",
        (id, &value, source.verified_at()),
    )
    .map_err(|e| DbError::sql(path, e))?;
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(BookWrite {
        before,
        venue_account_id: value,
        changed: true,
        verified_at: source.verified_at().map(str::to_string),
    })
}

// ---------------------------------------------------------------------------------------------
// Writing — the account LIFECYCLE
// ---------------------------------------------------------------------------------------------

/// The longest account LABEL this store will accept.
///
/// ⚠ **A SECOND SPELLING of `vike_model::account_keys::MAX_LABEL_LEN`, and the duplication is
/// deliberate rather than an oversight** — this crate declares no `vike-*` dependency at all (that
/// is what lets `vike-model` and the bridges both reach it without a cycle), so the label grammar
/// cannot be imported here. `vike_model::account_keys::AccountLabel::parse` stays the AUTHORITY —
/// it is what every operator-facing surface validates through, and its refusals name the rule that
/// was broken — and [`normalized_account_label`] is the store's own floor under it, exactly as
/// [`normalized_venue_account_id`] is the floor under whatever a caller thinks a book looks like.
///
/// The two are held EQUAL by `crates/vike-bridge-core/tests/account_label_spellings.rs`, which is
/// the only place that can compare them: it depends on both crates and neither depends on the
/// other, the same construction `crates/vike-bridge-core/tests/settings_dir_spellings.rs` uses for
/// the settings-directory resolver's two copies.
pub const ACCOUNT_LABEL_MAX_LEN: usize = 24;

/// The label an UNLABELLED key already addresses, which therefore may never be written into the
/// `label` column. The second spelling of `vike_model::account_keys::RESERVED_DEFAULT_LABEL` — see
/// [`ACCOUNT_LABEL_MAX_LEN`] for why there are two and what holds them equal.
pub const RESERVED_ACCOUNT_LABEL: &str = "DEFAULT";

/// **The store's own floor under an account label** — `Some(label)` when it is one this store will
/// write, `None` when it is not.
///
/// A-Z and 0-9 only, 1..=[`ACCOUNT_LABEL_MAX_LEN`] characters, never [`RESERVED_ACCOUNT_LABEL`].
///
/// ⚠ **It REPAIRS nothing, not even case.** `alt` is refused rather than uppercased, for the reason
/// `vike_model::account_keys::AccountLabel::parse` refuses it: a spelling this store fixed on the
/// operator's behalf is a spelling nobody learns, and the label they then write into
/// `policy.accounts.<venue>.<LABEL>` would match no row. That is a stricter rule than
/// [`normalized_venue_account_id`]'s, which trims invisible characters at the edges — a book is a
/// number read off a venue's page and pasted, a label is a name somebody chose.
#[must_use]
pub fn normalized_account_label(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > ACCOUNT_LABEL_MAX_LEN {
        return None;
    }
    if raw.chars().any(|c| !c.is_ascii_uppercase() && !c.is_ascii_digit()) {
        return None;
    }
    if raw == RESERVED_ACCOUNT_LABEL {
        return None;
    }
    Some(raw.to_string())
}

/// **WHICH act on the `account` table** — the parameter that keeps the lifecycle ONE writer.
///
/// ⚠ **A parameter rather than four functions, and that is the load-bearing choice.**
/// `crates/vike-ops/tests/credential_writer_gate.rs` pins the SET of function names that write this
/// store, and its `GROWTH_GUIDANCE` states the rule at exactly this shape: *"a SECOND FUNCTION for
/// a second column is the shape to refuse here: this one grew a parameter instead."* That was
/// written about [`BookSource`] growing onto [`set_venue_account_id`]; this enum is the same answer
/// one size up. Four `pub fn`s would be four names the gate has to learn, four transactions to keep
/// in step, and four places for the refusals below to diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountEdit<'a> {
    /// **ADD a row.** `label` is `None` for the unlabelled account a venue's plain keys address,
    /// and a second unlabelled row at one `(venue, tier)` is REFUSED —
    /// [`DbErrorKind::AmbiguousUnlabelledAccount`].
    ///
    /// ⚠ **A row is not a CEILING.** `policy.venues.<venue>` is consulted ABOVE the credential read
    /// by `vike_mount::make_engine`, so an account plus its keys leaves the venue on PAPER until a
    /// policy edit. Creating one arms nothing, and a caller's reply must say so.
    Create {
        /// A `vike_model::VENUES` id. ⚠ **Not validated here and cannot be** — this crate declares
        /// no `vike-*` dependency, so the roster is not reachable. Every operator-facing caller
        /// validates against `vike_model::VENUES` before it gets here; what the store enforces is
        /// the schema's `CHECK` on the tier and the guards below on the label.
        venue: &'a str,
        /// One of [`crate::schema::ACCOUNT_TIERS`] — refused by the table's own `CHECK` otherwise.
        tier: &'a str,
        /// The operator's name for the ROLE, or `None` for the unlabelled account.
        label: Option<&'a str>,
    },
    /// **CHANGE one row's label**, and nothing else. Not `id`, not the key prefixes (those are
    /// derived from `credential.name`, which this never touches), not `venue_account_id`.
    ///
    /// ⚠ Refused outright when the row's own credential keys SPELL the old label —
    /// [`DbErrorKind::AccountKeysPinTheLabel`].
    Rename {
        /// The row, by [`Account::id`].
        id: i64,
        /// The new label, or `None` to clear it back to the unlabelled state (which is refused if
        /// that would make a second unlabelled row at this `(venue, tier)`).
        label: Option<&'a str>,
    },
    /// **DEACTIVATE or re-activate a row** — `account.active`, the reversible act, and the one a UI
    /// leads with.
    ///
    /// [`Accounts::active_for_venue`] filters on this column, so every consumer already reads a
    /// deactivated row exactly as it would read a deleted one; and `account_one_account_per_book`'s
    /// `WHERE active = 1` frees the BOOK while the row survives as evidence.
    ///
    /// ⚠ **Two residuals a caller must state rather than let an operator discover.** The arming
    /// snapshot is read ONCE at boot (`vike_mount`'s `AccountDirectory`, carried on
    /// `MountPolicy`), so deactivating an account while a daemon runs changes nothing until it
    /// restarts — the running engines keep their credentials and keep trading. And
    /// `vike-cli secrets confirm`'s fold silently keeps a parked confirmation whose address matches
    /// no active row, which is indistinguishable from what a re-migration looks like.
    SetActive {
        /// The row, by [`Account::id`].
        id: i64,
        /// `false` = the operator no longer uses this account.
        active: bool,
    },
    /// **DELETE a row.** Refused while it still owns live `credential` rows, NAMING them by key
    /// name — [`DbErrorKind::AccountHasCredentials`].
    Remove {
        /// The row, by [`Account::id`].
        id: i64,
    },
}

impl AccountEdit<'_> {
    /// A short, stable word for the act — for a log line, a journal cell and a reply. Total by
    /// construction, so a new variant is a compile error rather than a record with a wrong verb.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            AccountEdit::Create { .. } => "create",
            AccountEdit::Rename { .. } => "rename",
            AccountEdit::SetActive { active: true, .. } => "activate",
            AccountEdit::SetActive { active: false, .. } => "deactivate",
            AccountEdit::Remove { .. } => "remove",
        }
    }
}

/// What [`edit_account`] did — the row before and after, so a caller can echo the change rather
/// than re-reading and hoping it is describing the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountWrite {
    /// The row as the write transaction FOUND it. `None` for an [`AccountEdit::Create`], which
    /// found nothing.
    pub before: Option<Account>,
    /// The row as it stands now. `None` for an [`AccountEdit::Remove`], which left nothing.
    pub after: Option<Account>,
    /// [`AccountEdit::verb`] for the act that was performed.
    pub verb: &'static str,
    /// `false` when the row already held exactly this state and no statement changed anything — a
    /// rename to the label already carried, a deactivate of an already-inactive row.
    ///
    /// Idempotent rather than an error, the same rule [`BookWrite::changed`] states: re-running a
    /// command is not a mistake, and refusing the second run would break a script that re-asserts
    /// a known state.
    pub changed: bool,
    /// The live credential key NAMES this row owned when the transaction looked — **names, never
    /// values**, from the same value-free statement [`read_account_keys`] uses.
    ///
    /// Carried on the RESULT rather than left for the caller to fetch, because the thing an
    /// operator has to check before deactivating an account is exactly this list, and a second
    /// read outside the transaction could describe a different row.
    pub keys: Vec<String>,
}

/// **The ONE writer of the `account` table's lifecycle columns** — create, rename, (de)activate,
/// remove — in ONE transaction, told by [`AccountEdit`] which of the four it is performing.
///
/// [`set_venue_account_id`] is its sibling and writes a DIFFERENT column (`venue_account_id`, plus
/// `last_verified_at` under a handshake); the two are deliberately separate because that one
/// records a claim about a BROKER and these record a claim about an operator's own filing.
///
/// # What it never does
///
/// * **It never creates the settings database.** [`open_for_write`] creates a database when the
///   path is empty, so the `created` branch below UNLINKS what it made and refuses with
///   [`DbErrorKind::VanishedDatabase`] — the same invariant [`upsert_rows`] and
///   [`set_venue_account_id`] hold, and the reason is sharper here than anywhere: the mere
///   EXISTENCE of `<project>/settings/db/vike.db` is the whole of [`crate::store::Backend`]'s
///   per-run choice, so minting one would retire every credential in `secrets.env` on that box in
///   an act nobody asked to be a migration. [`migrate`] stays the only creator.
/// * **It never reads a credential VALUE.** The one `credential` statement it issues is
///   `SELECT name FROM credential WHERE account_id = ?1 AND superseded_at IS NULL` — there is no
///   `value` column in it, so no refusal, no `Debug`, no log line and no reply reachable from
///   [`AccountWrite`] can be a credential.
/// * **It never writes, moves or deletes a credential FILE**, on any path.
/// * **It never touches `venue_account_id` or `last_verified_at`.** A rename changes a LABEL and
///   may change nothing else — that is the rule `crates/vike-model/src/account_confirmation.rs`'s
///   *the address is the KEY PREFIX, never the row id* exists to protect, and it is why the
///   labelled-keys case below is REFUSED rather than cascaded.
///
/// # IMMEDIATE, for [`set_venue_account_id`]'s reason verbatim
///
/// Every arm here is a read-then-write: the row is `SELECT`ed, the refusals are decided from what
/// it holds, and only then is the statement issued. A DEFERRED transaction acquires SHARED on that
/// `SELECT` and must PROMOTE to RESERVED at the write, and SQLite refuses that promotion with
/// `SQLITE_BUSY` **without consulting the busy handler at all** — so [`BUSY_TIMEOUT`] could never
/// cover it. IMMEDIATE takes RESERVED up front, which is also what makes the decision and the write
/// atomic against another writer rather than merely likely to be.
///
/// # What it refuses, and why each refusal is a refusal rather than a guess
///
/// * **a store with no `account` table** ([`DbErrorKind::NoAccountTable`]) — schema 1 is READABLE
///   by design, so the statements below would otherwise arrive as an opaque `no such table`;
/// * **a malformed label** ([`DbErrorKind::AccountLabelMalformed`]), echoing nothing;
/// * **an `id` no row carries** ([`DbErrorKind::NoSuchAccount`]) — never a create. A rename that
///   invented a row for a mistyped id is how an operator ends up editing a stranger;
/// * **a SECOND unlabelled row at one `(venue, tier)`**
///   ([`DbErrorKind::AmbiguousUnlabelledAccount`]) — the sharpest one, and the one no schema
///   constraint can make: it plants `crate::SchemaRefusal::AmbiguousAccount` for the next
///   credential key that arrives, so allowing it would be arming a refusal for a write nobody has
///   made yet;
/// * **a label another row of that `(venue, tier)` carries** ([`DbErrorKind::AccountLabelTaken`])
///   — `UNIQUE (venue, tier, label)` is the authority; this pre-check exists only so the refusal
///   can name the other row, the same two-layer shape [`DbErrorKind::BookHeldByAnother`] has;
/// * **a label another ACTIVE row of that venue already names as its BOOK**
///   ([`DbErrorKind::AccountLabelHeldAsBook`]) — no constraint enforces this one at all, and the
///   failure it prevents is a MOUNT refusing as ambiguous at the next restart;
/// * **a rename of a row whose credential keys SPELL the old label**
///   ([`DbErrorKind::AccountKeysPinTheLabel`]);
/// * **a REMOVE of a row that still owns live credential rows**
///   ([`DbErrorKind::AccountHasCredentials`]), naming them by KEY NAME. The foreign key would
///   refuse it anyway — `credential.account_id REFERENCES account(id)` with no `ON DELETE`, under
///   an [`open_for_write`] that verifies `PRAGMA foreign_keys` took — but its words are
///   `FOREIGN KEY constraint failed`, which names nothing an operator can act on.
///
/// # Errors
/// [`DbError`] for every refusal above, for a store that will not open, and for a statement the
/// engine rejects. **Every one of them wrote nothing**: one transaction, rolled back whole.
pub fn edit_account(path: &Path, edit: AccountEdit<'_>) -> Result<AccountWrite, DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    // The label is validated BEFORE the store is opened, so a malformed one costs no lock and
    // leaves no journal file behind. `None` is a legitimate value on both arms that take one.
    let label: Option<String> = match &edit {
        AccountEdit::Create { label, .. } | AccountEdit::Rename { label, .. } => match label {
            Some(raw) => match normalized_account_label(raw) {
                Some(l) => Some(l),
                None => return Err(refuse(DbErrorKind::AccountLabelMalformed)),
            },
            None => None,
        },
        AccountEdit::SetActive { .. } | AccountEdit::Remove { .. } => None,
    };

    let (mut conn, created, version) = open_for_write(path)?;
    // ⚠ The ONLY safe act on this path is to put back what we found — see the doc's *never creates*
    // section. Close the engine BEFORE unlinking: an open handle keeps the file alive on Windows.
    if created {
        drop(conn);
        let _ = std::fs::remove_file(path);
        return Err(refuse(DbErrorKind::VanishedDatabase));
    }
    if version < ACCOUNT_TABLE_SCHEMA {
        return Err(refuse(DbErrorKind::NoAccountTable { found: version }));
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;

    let verb = edit.verb();
    let out = match edit {
        AccountEdit::Create { venue, tier, .. } => {
            guard_account_label(&tx, path, venue, tier, label.as_deref(), None)?;
            // `label` is written as supplied — `NULL` for the unlabelled account. `venue` and
            // `tier` are the caller's; the table's `CHECK (tier IN …)` is what refuses a tier
            // outside `ACCOUNT_TIERS`, and it arrives as a `DbErrorKind::Sqlite` naming the
            // constraint. `active` takes the column's `DEFAULT 1`; `venue_account_id`, `parent_id`,
            // `last_verified_at` and `notes` stay NULL — a new row knows nothing about its book,
            // and filling one in is `set_venue_account_id`'s job and nobody else's.
            tx.execute(
                "INSERT INTO account (venue, tier, label) VALUES (?1, ?2, ?3)",
                (venue, tier, label.as_deref()),
            )
            .map_err(|e| DbError::sql(path, e))?;
            let id = tx.last_insert_rowid();
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: None, after, verb, changed: true, keys: Vec::new() }
        }
        AccountEdit::Rename { id, .. } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            if before.label == label {
                // Already exactly this label. Not an error — see `AccountWrite::changed`.
                let out = AccountWrite {
                    after: Some(before.clone()),
                    before: Some(before),
                    verb,
                    changed: false,
                    keys,
                };
                return commit_account_write(tx, path, out);
            }
            // ⚠ THE LABELLED-KEYS REFUSAL. A key spelling `__{OLD}` keeps spelling it after a
            // rename — nothing here rewrites a credential name, and nothing in this workspace may —
            // so the classifier would derive the OLD label from it again and CREATE a second row.
            // See `DbErrorKind::AccountKeysPinTheLabel`.
            if let Some(old) = &before.label {
                let suffix = format!("__{old}");
                let pinned: Vec<String> =
                    keys.iter().filter(|k| k.ends_with(&suffix)).cloned().collect();
                if !pinned.is_empty() {
                    return Err(refuse(DbErrorKind::AccountKeysPinTheLabel {
                        id,
                        label: old.clone(),
                        keys: pinned,
                    }));
                }
            }
            guard_account_label(
                &tx,
                path,
                &before.venue,
                &before.tier,
                label.as_deref(),
                Some(id),
            )?;
            tx.execute("UPDATE account SET label = ?2 WHERE id = ?1", (id, label.as_deref()))
                .map_err(|e| DbError::sql(path, e))?;
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: Some(before), after, verb, changed: true, keys }
        }
        AccountEdit::SetActive { id, active } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            if before.active == active {
                let out = AccountWrite {
                    after: Some(before.clone()),
                    before: Some(before),
                    verb,
                    changed: false,
                    keys,
                };
                return commit_account_write(tx, path, out);
            }
            // ⚠ RE-ACTIVATING can collide where deactivating never can: while this row was off,
            // `account_one_account_per_book`'s `WHERE active = 1` let another row take its BOOK.
            // The index is the authority and would refuse the UPDATE; asking first is what lets the
            // refusal name the other row, the same two-layer shape every guard here has.
            if active && let Some(book) = &before.venue_account_id {
                let holder: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM account \
                         WHERE venue = ?1 AND venue_account_id = ?2 AND active = 1 AND id <> ?3",
                        (&before.venue, book, id),
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| DbError::sql(path, e))?;
                if let Some(holder) = holder {
                    return Err(refuse(DbErrorKind::BookHeldByAnother {
                        id,
                        venue: before.venue.clone(),
                        holder,
                    }));
                }
            }
            tx.execute("UPDATE account SET active = ?2 WHERE id = ?1", (id, i64::from(active)))
                .map_err(|e| DbError::sql(path, e))?;
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: Some(before), after, verb, changed: true, keys }
        }
        AccountEdit::Remove { id } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            // ⚠ THE REFUSAL THE WHOLE VERB IS BUILT AROUND, and it is a PRE-CHECK over a
            // second-layer guarantee: the foreign key refuses this delete anyway, but its words
            // name nothing. See `DbErrorKind::AccountHasCredentials`.
            if !keys.is_empty() {
                return Err(refuse(DbErrorKind::AccountHasCredentials {
                    id,
                    venue: before.venue.clone(),
                    tier: before.tier.clone(),
                    keys,
                }));
            }
            // ⚠ A SUB-ACCOUNT's master may not vanish under it either: `account.parent_id
            // REFERENCES account(id)` is the same `NO ACTION` clause, so the engine refuses that
            // too. Nothing writes `parent_id` in this tree today, so there is no pre-check for it
            // and no message to write — the day something does, this is where the refusal goes.
            // ⚠ **THIS STATEMENT IS WHAT MAKES AN `account.id` REUSABLE, and it is the only one in
            // the tree that does.** `account.id INTEGER PRIMARY KEY` carries no `AUTOINCREMENT`
            // (the schema has none anywhere), so SQLite hands a new row `max(rowid) + 1`: deleting
            // the row with the LARGEST id frees that id for the next `Create`. Remove account 16,
            // add an account, and the new one IS account 16.
            //
            // Nothing is corrupted and no write lands on the wrong row — every verb resolves the id
            // inside this same transaction. What changes is what a number MEANS to whoever wrote it
            // down: an operator's note, a runbook, a GUI cell held across frames, a wire client's
            // remembered id. [`Account::id`]'s own doc used to promise ids were never reused within
            // a file and justified it with *"nothing in this crate ever deletes a row"* — this line
            // is the thing that falsified it, so the promise moved rather than the statement.
            //
            // The mitigation in force is the ECHO, not the number: `vike-cli`'s `echo_row` prints
            // the row's credential KEY NAMES before the ceremony, and those are what identify an
            // account. The structural cure is `AUTOINCREMENT`, which SQLite cannot add by `ALTER` —
            // it needs a table rebuild behind the schema version, and that is a change of its own
            // rather than something to smuggle in beside a lifecycle.
            tx.execute("DELETE FROM account WHERE id = ?1", [id])
                .map_err(|e| DbError::sql(path, e))?;
            AccountWrite {
                before: Some(before),
                after: None,
                verb,
                changed: true,
                keys: Vec::new(),
            }
        }
    };
    commit_account_write(tx, path, out)
}

/// One `account` row by id, read INSIDE the transaction that is about to write it — so the echo a
/// caller renders is what the write actually saw, not what a read beforehand happened to find.
fn account_row(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    id: i64,
) -> Result<Option<Account>, DbError> {
    tx.query_row(
        "SELECT id, venue, tier, label, venue_account_id, parent_id, active, last_verified_at \
         FROM account WHERE id = ?1",
        [id],
        |r| {
            Ok(Account {
                id: r.get(0)?,
                venue: r.get(1)?,
                tier: r.get(2)?,
                label: r.get(3)?,
                venue_account_id: r.get(4)?,
                parent_id: r.get(5)?,
                active: r.get::<_, i64>(6)? != 0,
                last_verified_at: r.get(7)?,
            })
        },
    )
    .optional()
    .map_err(|e| DbError::sql(path, e))
}

/// The live credential key NAMES one account owns.
///
/// ⚠ **`SELECT name` — there is no `value` column in this statement**, which is what makes every
/// refusal and every echo built from it structurally incapable of carrying a credential, exactly as
/// [`read_account_keys`] is. `superseded_at IS NULL` matches that reader too: a name rotated out is
/// not evidence about who the row is today.
fn account_key_names(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    id: i64,
) -> Result<Vec<String>, DbError> {
    let mut stmt = tx
        .prepare(
            "SELECT name FROM credential \
             WHERE account_id = ?1 AND superseded_at IS NULL ORDER BY name",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let rows =
        stmt.query_map([id], |r| r.get::<_, String>(0)).map_err(|e| DbError::sql(path, e))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(out)
}

/// The three LABEL guards, asked of the `(venue, tier)` that is about to carry `label`.
///
/// `exclude` is the row being edited, so a rename to the label a row already carries is not refused
/// by the row itself. Each guard asks a question the schema either cannot answer
/// ([`DbErrorKind::AmbiguousUnlabelledAccount`], [`DbErrorKind::AccountLabelHeldAsBook`]) or
/// answers without naming anybody ([`DbErrorKind::AccountLabelTaken`]).
fn guard_account_label(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    venue: &str,
    tier: &str,
    label: Option<&str>,
    exclude: Option<i64>,
) -> Result<(), DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    // No row can carry a negative rowid, so this is "exclude nothing" without a second statement.
    let exclude = exclude.unwrap_or(-1);
    match label {
        // ⚠ THE AMBIGUITY GUARD. `UNIQUE (venue, tier, label)` cannot make this one: NULLs are
        // distinct in a SQLite index, so the engine would take a second unlabelled row happily and
        // `crate::schema`'s `AccountResolver` would then hold two rows under one `by_key` entry —
        // which is what makes the NEXT credential key for that venue refuse as ambiguous.
        None => {
            let holder: Option<i64> = tx
                .query_row(
                    "SELECT id FROM account \
                     WHERE venue = ?1 AND tier = ?2 AND label IS NULL AND id <> ?3",
                    (venue, tier, exclude),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::AmbiguousUnlabelledAccount {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    holder,
                }));
            }
        }
        Some(label) => {
            let holder: Option<i64> = tx
                .query_row(
                    "SELECT id FROM account \
                     WHERE venue = ?1 AND tier = ?2 AND label = ?3 AND id <> ?4",
                    (venue, tier, label, exclude),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::AccountLabelTaken {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    label: label.to_string(),
                    holder,
                }));
            }
            // ⚠ The BOOK collision, scoped to ACTIVE rows exactly as `account_one_account_per_book`
            // is — `vike_mount::dukascopy`'s `resolve_account` filters on `active` before it
            // matches either column, so an inactive row naming this string cannot make a mount
            // ambiguous and refusing for it would refuse a legitimate state.
            let holder: Option<i64> = tx
                .query_row(
                    "SELECT id FROM account \
                     WHERE venue = ?1 AND venue_account_id = ?2 AND active = 1 AND id <> ?3",
                    (venue, label, exclude),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::AccountLabelHeldAsBook {
                    venue: venue.to_string(),
                    label: label.to_string(),
                    holder,
                }));
            }
        }
    }
    Ok(())
}

/// Commit an [`edit_account`] transaction and hand back its result — the two lines every arm ends
/// with, so an early return on a no-op path cannot forget the commit.
fn commit_account_write(
    tx: rusqlite::Transaction<'_>,
    path: &Path,
    out: AccountWrite,
) -> Result<AccountWrite, DbError> {
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// **The UPSERT, database half** — replace exactly the named rows, insert the ones that were absent,
/// leave every other row untouched.
///
/// The same contract `crate::env_write`'s `save_credentials` holds over the file, expressed in the
/// only two statements SQLite needs for it. `crate::store::save_credentials_to_store` is the door;
/// nothing outside this crate calls this directly, and nothing here decides WHICH store is written.
///
/// # What it refuses
///
/// * a name already sitting in the OTHER table ([`DbErrorKind::WrongTable`]) — the static predicate,
///   enforced on the write path as well as on the migration's;
/// * a database that is not [`SCHEMA_VERSION`] — through [`open_for_write`]'s existing check;
/// * a database that VANISHED between the backend choice and this open
///   ([`DbErrorKind::VanishedDatabase`]) — see the invariant below.
///
/// # ⚠ The invariant is *the database holds what the file holds*, and ONLY [`migrate`] may create one
///
/// This function used to state a weaker one — *a database that exists has a finished schema and a
/// stamped version* — and satisfied it on the race path by CREATING and STAMPING. That is the wrong
/// invariant, and the difference is the whole of this section. A write reaches here holding one or
/// two keys; the store it was routed to holds sixty-odd. If the file is removed between
/// `crate::store::backend_in`'s probe and this open, minting a fresh database here leaves one that
/// is schema-complete, version-stamped, accepted by [`check_schema_version`] — and holding ONLY this
/// write's keys. From that moment [`crate::store::backend_at`] answers `Database` for every process
/// on the box, the credential file beside it is never read again, and every other key is gone. The
/// old invariant is satisfied the whole way down; the load-bearing one is violated at the first
/// statement.
///
/// So the rule is now structural rather than repaired after the fact: **[`migrate`] is the only
/// function in this module that may bring a database into existence**, because it is the only one
/// that reads the whole of both files first and can therefore satisfy *the database holds what the
/// file holds*. Every other writer refuses.
///
/// **The rollback deletes only a file this very call created**, on the branch where
/// [`open_for_write`] reports `created` — a path that did not exist moments earlier and has never
/// held a row belonging to anyone. It is not a credential store and it is not the operator's data;
/// leaving it would be the defect. Nothing here touches `secrets.env` or `node.env`, then or ever.
///
/// ONE transaction for the whole batch, which is what makes a rotating pair (cTrader's access +
/// refresh grant, a node key pair) impossible to half-write.
/// # ⚠ The schema-2 shape, and the three things that changed under it
///
/// 1. **`ON CONFLICT(name)` no longer names a unique constraint.** Schema 2's `name` uniqueness is
///    the PARTIAL index `credential_one_live_name … WHERE superseded_at IS NULL` (§4.1 argues why it
///    must be partial: a superseded row carries the same name as the value that replaced it), and
///    SQLite requires an upsert's conflict target to repeat a partial index's `WHERE`. The
///    statement below is an explicit UPDATE of the live row instead, which says the same thing and
///    needs no conflict target at all.
/// 2. **`field` is `NOT NULL`, so a row that is not already there needs a CLASSIFICATION.** That is
///    what `classify` is, and it is `Option` because it is meaningless for [`Table::NodeKey`] —
///    0051's pair belongs to no account and its table is `(name, value)` in every schema. A
///    credential name this store has never held, with no classifier, is
///    [`DbErrorKind::Unclassified`]: refused, never filed as infrastructure. Filing it that way
///    would detach a venue credential from its account in silence, which is the failure this
///    refusal exists to prevent.
/// 3. **Replacing a KNOWN key needs no classifier at all**, and that is what keeps the sharpest
///    writer in the workspace working unchanged: `crates/bridges/ctrader/src/token_store.rs`'s
///    `persist` re-writes a grant the VENUE rotated, under a name the store already holds.
///
/// ⚠ **`superseded_at` is NOT written here, and that is §12's explicit debt rather than an
/// oversight**: *"the sanctioned upsert replaces in place today; marking the old row superseded is
/// a behaviour change to that writer and to `credential_writer_gate.rs`'s vocabulary."* This
/// function still replaces in place, so a rotation loses the old value exactly as it did before
/// schema 2 — no better, no worse.
pub fn upsert_rows(
    path: &Path,
    table: Table,
    updates: &[(String, String)],
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
) -> Result<(), DbError> {
    if updates.is_empty() {
        return Ok(());
    }
    let (mut conn, created, version) = open_for_write(path)?;

    // ⚠ `created` is FALSE on every ordinary call — `crate::store::backend_in` has already seen the
    // file, which is why this function's doc says it creates no database. It can be true only if the
    // file was REMOVED between that probe and this open, and on that path the only safe act is to
    // put back what we found: no database. A write that fails loudly costs the caller a retry; the
    // alternative silently retires every key this write was not about. See the invariant above.
    if created {
        // Close the engine BEFORE unlinking — an open handle keeps the file alive on Windows, and a
        // rollback that quietly did not roll back is exactly the shape this branch exists to refuse.
        drop(conn);
        // Best-effort by necessity, and never a silent success: the error below is returned
        // whatever the unlink does, so a file we somehow could not remove is still reported as a
        // refused write rather than as a store that answered.
        let _ = std::fs::remove_file(path);
        return Err(DbError { path: path.to_path_buf(), kind: DbErrorKind::VanishedDatabase });
    }

    // The two-namespace check, BEFORE anything is written, and over the whole batch — so a refusal
    // names the offending key and leaves the store exactly as it found it.
    {
        let other = table.other();
        let sql = format!("SELECT 1 FROM {} WHERE name = ?1", other.sql_name());
        let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
        for (name, _) in updates {
            let clash = stmt.exists([name.as_str()]).map_err(|e| DbError::sql(path, e))?;
            if clash {
                return Err(DbError {
                    path: path.to_path_buf(),
                    kind: DbErrorKind::WrongTable {
                        key: name.clone(),
                        found_in: other,
                        wanted: table,
                    },
                });
            }
        }
    }

    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    match table {
        // 0051's pair, in every schema: `(name, value)`, no account, no classification.
        Table::NodeKey => {
            let sql = "INSERT INTO node_key (name, value) VALUES (?1, ?2) \
                       ON CONFLICT(name) DO UPDATE SET value = excluded.value";
            let mut stmt = tx.prepare(sql).map_err(|e| DbError::sql(path, e))?;
            for (name, value) in updates {
                stmt.execute((name, value)).map_err(|e| DbError::sql(path, e))?;
            }
        }
        Table::Credential => {
            let mut fresh: BTreeMap<String, String> = BTreeMap::new();
            {
                // ⚠ The `WHERE` is chosen by the SCHEMA and not spelled once: naming
                // `superseded_at` against a schema-1 table is a PREPARE-time error (*no such
                // column*), which would have turned every credential write on an unmigrated box
                // into a hard failure — including the venue's own cTrader grant rotation, whose
                // loss is a lockout at the next restart. A schema-1 table has one row per name by
                // its PRIMARY KEY, so the clause is unnecessary there as well as illegal.
                let sql = if version == 1 {
                    "UPDATE credential SET value = ?2 WHERE name = ?1"
                } else {
                    "UPDATE credential SET value = ?2 WHERE name = ?1 AND superseded_at IS NULL"
                };
                let mut update = tx.prepare(sql).map_err(|e| DbError::sql(path, e))?;
                for (name, value) in updates {
                    let touched =
                        update.execute((name, value)).map_err(|e| DbError::sql(path, e))?;
                    if touched == 0 {
                        // A name this store has never held. It needs a home, and the two ways of
                        // not having one are different refusals with different fixes.
                        if version != SCHEMA_VERSION {
                            return Err(DbError {
                                path: path.to_path_buf(),
                                kind: DbErrorKind::WriteToOlderSchema {
                                    key: name.clone(),
                                    found: version,
                                },
                            });
                        }
                        if classify.is_none() {
                            return Err(DbError {
                                path: path.to_path_buf(),
                                kind: DbErrorKind::Unclassified { key: name.clone() },
                            });
                        }
                        fresh.insert(name.clone(), value.clone());
                    }
                }
            }
            if !fresh.is_empty() {
                let classify = classify.expect("checked above, once per absent name");
                let report = crate::schema::write_rows(
                    &tx,
                    &fresh,
                    &crate::schema::FileComments::default(),
                    classify,
                )
                .map_err(|e| DbError::sql(path, e))?;
                // ⚠ A per-key refusal is fine for a MIGRATION (it carries the rest and names the
                // key) and is NOT fine here: this call has one or two keys in it and its caller
                // believes a successful return means the key landed. A rotating cTrader grant that
                // silently did not land is a lockout at the next restart.
                //
                // ⚠ **The refusal is CARRIED, not discarded.** This returned
                // `DbErrorKind::Unclassified` for every refusal the fill could raise, i.e. it told
                // the operator *the caller supplied no account classification* while holding, in
                // `report.refused`, the real reason — a tier the `CHECK` will not take, an account
                // with two answers, or two spellings of one credential disagreeing about its
                // value. Every one of those names a key and a repair; `Unclassified` names
                // neither and points at the wrong party.
                if let Some(first) = report.refused.first() {
                    return Err(DbError {
                        path: path.to_path_buf(),
                        kind: DbErrorKind::Refused { refusal: first.clone() },
                    });
                }
            }
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    // No stamp here, and no branch that could need one: the only call that reaches this line opened
    // a database that already existed, so `check_schema_version` has already accepted its version
    // and `migrate` is what wrote it. The `created` arm returned above.
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The migration
// ---------------------------------------------------------------------------------------------

/// One reason [`migrate`] refused. **Names a KEY, never a value.**
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ambiguity {
    /// The node file carries a name the caller's predicate does not call a node key. The migration
    /// will not guess whether that is a venue credential filed in the wrong place or a node key the
    /// predicate has not heard of — either guess writes a credential into a namespace nothing will
    /// look for it in.
    UnexpectedNameInNodeFile {
        /// The key name.
        key: String,
    },
    /// The same name is in both files with DIFFERENT values: a half-migrated box, exactly the shape
    /// `crate::store::resolve_node_keys` refuses to merge. Choosing a side here produces a
    /// mismatched pair, whose symptom at the node is an opaque `bad mac`.
    DisagreeingFiles {
        /// The key name.
        key: String,
    },
    /// The file's value differs from the row already in the database. Nothing here can tell which is
    /// newer — the file may have been hand-edited after the move, or the row may have been written
    /// through software — so it refuses rather than clobbering a key an order is signed with.
    ///
    /// ⚠ **This one is a PER-KEY refusal and is reported on [`Migration::refused`], not through
    /// [`MigrateError::Ambiguous`].** The other three arms make the RUN undecidable; this one makes
    /// one KEY undecidable, and a whole-run refusal over it meant an operator who added a brand-new
    /// key in the same edit that left a stale line behind got neither key migrated. The refusal is
    /// unchanged in what it protects — the stored value is never overwritten, and the name is always
    /// printed — it just no longer takes its neighbours with it.
    DisagreesWithDatabase {
        /// The key name.
        key: String,
        /// Which table holds the differing row.
        table: Table,
    },
    /// The name is already in the OTHER table. This is the static predicate being enforced: a name
    /// has one home, and a run under a differently-scoped `is_node_key` may not give it a second.
    WrongTable {
        /// The key name.
        key: String,
        /// Where it already is.
        found_in: Table,
        /// Where this run wanted to put it.
        wanted: Table,
    },
}

impl std::fmt::Display for Ambiguity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ambiguity::UnexpectedNameInNodeFile { key } => write!(
                f,
                "{key} is in {} but the node-key predicate does not claim it — move it to {} if it \
                 is a venue credential, or widen the predicate if it is a node key",
                crate::dotenv::NODE_FILE,
                crate::dotenv::SECRETS_FILE
            ),
            Ambiguity::DisagreeingFiles { key } => write!(
                f,
                "{key} is in BOTH {} and {} with different values — this box is half-migrated; \
                 remove the stale line before migrating",
                crate::dotenv::SECRETS_FILE,
                crate::dotenv::NODE_FILE
            ),
            Ambiguity::DisagreesWithDatabase { key, table } => write!(
                f,
                "{key} already sits in the {table} table with a DIFFERENT value than the file's — \
                 nothing here can tell which is newer, so neither is overwritten"
            ),
            Ambiguity::WrongTable { key, found_in, wanted } => write!(
                f,
                "{key} is already in the {found_in} table and this run wants it in {wanted} — a \
                 name has ONE home (decision 0051), and nothing here will give it two"
            ),
        }
    }
}

/// [`migrate`] did not finish, and NOTHING was written.
#[derive(Debug)]
pub enum MigrateError {
    /// One of the two files exists and could not be read. (An ABSENT file is not this — it is an
    /// empty contribution, the same live gate `crate::store::resolve` implements.)
    Store(SecretsError),
    /// The database could not be opened, is not this schema, or refused `journal_mode=DELETE`.
    Db(DbError),
    /// The inputs do not determine an answer. EVERY finding is reported, not just the first, so one
    /// run tells the operator everything they have to fix.
    ///
    /// ⚠ **WHOLE-RUN findings only** — the three that make the run itself undecidable. A value that
    /// merely disagrees with a row already stored is a per-KEY refusal and rides
    /// [`Migration::refused`] on an otherwise successful run; see
    /// [`Ambiguity::DisagreesWithDatabase`].
    Ambiguous(Vec<Ambiguity>),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::Store(e) => write!(f, "{e}"),
            MigrateError::Db(e) => write!(f, "{e}"),
            MigrateError::Ambiguous(list) => {
                write!(
                    f,
                    "refusing to migrate — {} ambiguous key(s), nothing written:",
                    list.len()
                )?;
                for a in list {
                    write!(f, "\n  - {a}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<SecretsError> for MigrateError {
    fn from(e: SecretsError) -> Self {
        MigrateError::Store(e)
    }
}

impl From<DbError> for MigrateError {
    fn from(e: DbError) -> Self {
        MigrateError::Db(e)
    }
}

/// What one file contributed to one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReport {
    /// The file that was READ. It was not written, moved, truncated or re-rendered.
    pub file: PathBuf,
    /// Where its keys went.
    pub table: Table,
    /// Names read out of this file and destined for this table.
    pub read: usize,
    /// Rows this run actually INSERTED. Zero on every run after the first — that is idempotence.
    pub inserted: usize,
    /// Rows already present with an identical value, so this run left them alone.
    pub already_present: usize,
}

/// **What a [`migrate`] run DID, including the two ways it did nothing.**
///
/// The distinction that matters is the last variant, and it is the reason this is an enum rather
/// than the `bool` it replaced: *the files held nothing, so no database was created*. That case used
/// to be indistinguishable from *the database already had everything*, and the code took the
/// harmful branch on it — it opened a write connection, which CREATES the store. See
/// [`MigrationOutcome::NothingToMigrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The database did not exist and this run created it and filled it.
    Created,
    /// The database existed and this run added rows to it.
    Updated,
    /// **The database existed at an OLDER schema and this run brought it to [`SCHEMA_VERSION`]**,
    /// in place, in one transaction — with or without new rows from the files beside it.
    ///
    /// It is its own outcome rather than folded into [`Self::Updated`] because it is the fact an
    /// operator most needs after running the verb: until it happens, a box whose binary reads
    /// schema 1 happily REFUSES every credential write to it
    /// ([`DbErrorKind::WriteToOlderSchema`]), and *already existed* would have read as *nothing to
    /// do*.
    SchemaUpgraded,
    /// The database existed and already held every key the files carry. Nothing was written and no
    /// connection was opened for writing — that is how *twice is the same as once* is structural
    /// here rather than hoped for.
    ///
    /// ⚠ This variant asserts COMPLETENESS, so it may only be chosen when
    /// [`Migration::refused`] is empty — see [`Self::NothingNewButRefused`].
    AlreadyComplete,
    /// **The database existed, nothing new was pending — and a key was REFUSED.** Not complete, and
    /// the distinction is not pedantry: a file value that disagrees with a stored row is held back
    /// (`Ambiguity::DisagreesWithDatabase`) rather than overwritten, so the store is missing a
    /// value the files carry and the operator has two values for one name.
    ///
    /// ⚠ It exists because [`Self::AlreadyComplete`] was chosen on `pending.is_empty()` ALONE, which
    /// printed *already existed, already complete* directly above a refusal line saying the run had
    /// not carried everything. A runbook or deploy check grepping stdout for `complete` would call
    /// that box converged.
    NothingNewButRefused,
    /// **There was nothing to migrate and NO DATABASE WAS CREATED.**
    ///
    /// Neither file carries a key (both absent, or both empty), and no database exists. The old code
    /// opened a write connection here and left behind a schema-stamped, zero-row `vike.db` — and
    /// from that moment `crate::store::backend_at` answers `Database` for every process on the box,
    /// so the credential file the operator writes afterwards is never read again.
    /// `crate::store::resolve_project` then returns an EMPTY map, which downstream is not an error
    /// but the LIVE GATE: every venue silently on paper while `secrets.env` sits on disk looking
    /// correct.
    ///
    /// **Creating the store IS the harmful act**, so this arm performs none of it: no directory, no
    /// file, no connection. A CLI verb should say *nothing to migrate* and exit successfully.
    NothingToMigrate,
}

impl std::fmt::Display for MigrationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MigrationOutcome::Created => "created",
            MigrationOutcome::Updated => "already existed",
            MigrationOutcome::SchemaUpgraded => {
                "already existed at an OLDER schema and was upgraded in place"
            }
            MigrationOutcome::AlreadyComplete => "already existed, already complete",
            MigrationOutcome::NothingNewButRefused => {
                "already existed; nothing new to carry, and a key was REFUSED"
            }
            MigrationOutcome::NothingToMigrate => "NOT created — there was nothing to migrate",
        })
    }
}

/// What [`migrate`] did. Its `Display` is the operator's report: how many keys, from which file,
/// into which table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The database this run was about. ⚠ It does not necessarily EXIST — see
    /// [`MigrationOutcome::NothingToMigrate`], the one outcome on which this path was deliberately
    /// left untouched.
    pub db: PathBuf,
    /// What happened. See [`MigrationOutcome`].
    pub outcome: MigrationOutcome,
    /// The schema the database carried BEFORE this run, or `None` when there was none.
    pub schema_before: Option<i64>,
    /// The schema it carries now — always [`SCHEMA_VERSION`] on a run that wrote anything.
    pub schema_now: i64,
    /// **What the schema-2 fill DID** — accounts created, rows classified, names the classifier
    /// could not place, and the rows §7 and §6 will move that this change deliberately did not.
    /// `None` on a run that wrote nothing. See [`crate::schema::RowReport`].
    pub rows: Option<crate::schema::RowReport>,
    /// One row per (file, table) pair that could have contributed — including the pairs that
    /// contributed nothing, because an empty node file is a fact worth printing.
    pub sources: Vec<SourceReport>,
    /// Names the credential file AND the node file both carry with an IDENTICAL value.
    ///
    /// ⚠ They are counted under the node file's row and under no other, because the node file is
    /// their home and one row is one key. That makes the credential file's `read` count LOWER than
    /// the number of `KEY=` lines in it — on precisely the half-migrated box where an operator most
    /// wants to reconcile the report against a `grep -c`. So the names are reported here as well,
    /// and the arithmetic closes: the credential file's rows plus this list is what that file holds.
    pub doubly_claimed: Vec<String>,
    /// **Keys this run REFUSED without refusing the run** — see [`Ambiguity::DisagreesWithDatabase`].
    ///
    /// A file value that disagrees with a row already stored is never overwritten and is always
    /// NAMED; what changed is that it no longer takes the rest of the edit down with it. A brand-new
    /// key added in the same edit LANDS, and this list is what the operator has left to reconcile.
    ///
    /// ⚠ A non-empty list means the run did not carry everything the files hold, even though it
    /// returned `Ok`. `MigrateError::Ambiguous` is still the WHOLE-RUN refusal, and still carries
    /// the three findings that make the run itself undecidable (a name the predicate does not claim,
    /// two files disagreeing with each other, a name that would acquire a second namespace).
    pub refused: Vec<Ambiguity>,
    /// **The key NAMES this run INSERTED** — not what the store holds, and not what was read.
    ///
    /// Sorted, and never a value. It exists because a CALLER that records the migration has to name
    /// what the migration did: the ledger record `vike-cli secrets migrate` appends takes key names,
    /// and the only other way to obtain them is to read the whole table back — which on a run that
    /// added one key to a sixty-seven-key store would name all sixty-eight and claim this run wrote
    /// them. That is an append-only record asserting something false, which is worse than one
    /// asserting nothing.
    ///
    /// A key NAME is not a secret: `vike-cli secrets list` prints names by an explicit decision in
    /// the root `CLAUDE.md`, and `vike_model::change_journal`'s `credential_write` records them for
    /// exactly the same reason.
    ///
    /// ⚠ **`inserted_keys.len()` is [`Migration::inserted`] ON EVERY OUTCOME BUT ONE, and this doc
    /// stated it without the exception.** On [`MigrationOutcome::SchemaUpgraded`] the two are
    /// deliberately different and the gap is the size of the store: `inserted()` sums
    /// [`Migration::sources`], which counts what the FILES contributed, and an upgrade contributes
    /// none of those while re-inserting every row in the table — so this list is the union of the
    /// pending names and `crate::schema::RowReport::written_names`, and is the larger of the two by
    /// however many keys the store already held. That is the whole reason the fold exists (see
    /// that field), so the invariant could never have held on the path it was added for. On every
    /// other outcome the per-file breakdown is a different decomposition of the same rows and
    /// `crates/vike-secrets/tests/database_migration.rs` pins them equal.
    pub inserted_keys: Vec<String>,
}

impl Migration {
    /// Total rows this run inserted. `0` means the run was a no-op, which is what a second run is.
    #[must_use]
    pub fn inserted(&self) -> usize {
        self.sources.iter().map(|s| s.inserted).sum()
    }

    /// Did this run CREATE the database? (Was a public `bool` field; the four states it flattened
    /// are now [`MigrationOutcome`], and two of them mean "no database was written".)
    #[must_use]
    pub fn created(&self) -> bool {
        matches!(self.outcome, MigrationOutcome::Created)
    }

    /// Does a database EXIST at [`Migration::db`] as a result of this run having succeeded?
    ///
    /// False on exactly one outcome — [`MigrationOutcome::NothingToMigrate`] — and that is the
    /// invariant the existence-only backend probe rests on: **a database exists ⇒ a migration
    /// finished.**
    #[must_use]
    pub fn database_exists(&self) -> bool {
        !matches!(self.outcome, MigrationOutcome::NothingToMigrate)
    }

    /// Total names read out of the files, across both tables. (Named `keys_read` rather than `read`
    /// so it cannot be mistaken for a file-opening call by the tree scanners that key on that name —
    /// `crates/vike-ops/tests/settings_registry.rs`'s `FILE_OPENERS`.)
    #[must_use]
    pub fn keys_read(&self) -> usize {
        self.sources.iter().map(|s| s.read).sum()
    }
}

impl std::fmt::Display for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "settings database {} ({})", self.db.display(), self.outcome)?;
        if self.outcome == MigrationOutcome::SchemaUpgraded {
            write!(
                f,
                "\n  schema {} -> {}: the account is a ROW now. Nothing was lost and every key \
                 name still answers exactly as it did — the `account`, `credential` and \
                 `venue_setting` tables are the new shape, and the credential file beside this \
                 database was READ ONLY, as always.\n  ⚠ ONE-WAY: a binary that predates schema \
                 {} will REFUSE this store, which downstream is an EMPTY credential map and \
                 therefore every venue on paper. Roll the BINARY back only together with this \
                 store.",
                self.schema_before.unwrap_or(0),
                self.schema_now,
                self.schema_now,
            )?;
        }
        if let Some(rows) = &self.rows {
            write!(f, "\n  {rows}")?;
        }
        for s in &self.sources {
            write!(
                f,
                "\n  {} -> {}: {} key(s) read, {} inserted, {} already present",
                s.file.display(),
                s.table,
                s.read,
                s.inserted,
                s.already_present
            )?;
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) were REFUSED and nothing was overwritten — every other key above \
                 landed:",
                self.refused.len()
            )?;
            for a in &self.refused {
                write!(f, "\n    - {a}")?;
            }
        }
        if !self.doubly_claimed.is_empty() {
            write!(
                f,
                "\n  {} name(s) are in BOTH source files with an identical value and are counted \
                 under the node file's row above, so the credential file holds that many more \
                 `KEY=` lines than its row reports: {}",
                self.doubly_claimed.len(),
                self.doubly_claimed.join(", ")
            )?;
        }
        f.write_str("\n  the source files were READ ONLY — nothing was moved, rewritten or deleted")
    }
}

/// **Everything a migration DECIDES, before anything it WRITES.**
///
/// Crate-private, and deliberately not a public type: it is the shared body of [`migrate`] and
/// [`preview`], not a second answer either of them could drift from. See [`plan`].
struct Plan {
    /// Where the database is, or would be.
    db: PathBuf,
    /// Did one exist when the classification was made? The ONLY fact here a write can invalidate —
    /// see [`preview`]'s *what a dry run cannot predict*.
    exists: bool,
    /// The schema an existing database carries. `None` when there is none.
    version: Option<i64>,
    /// §4.2's and §4.3's evidence, which lives in the credential file's COMMENT lines and which
    /// `crate::dotenv::parse_dotenv` discards. See [`crate::schema::FileComments`] for why a second
    /// read of that file does not make this a second opinion about what a line means.
    comments: crate::schema::FileComments,
    /// The rows a write would INSERT, keyed `(table, name)`. Empty means the write arm opens no
    /// connection at all, which is what makes *twice is the same as once* structural.
    pending: BTreeMap<(Table, String), String>,
    /// One row per (file, table) pair that could have contributed. Assembled HERE rather than after
    /// the write, because it never depended on the write: it is a function of what was read, what is
    /// pending and what was already stored.
    sources: Vec<SourceReport>,
    /// See [`Migration::doubly_claimed`].
    doubly_claimed: Vec<String>,
    /// See [`Migration::refused`] — the per-KEY refusals, which leave the run `Ok`.
    refused: Vec<Ambiguity>,
    /// **Every `(table, name) -> value` the database already holds**, as the read-only probe found
    /// it. Empty when there is none.
    ///
    /// The comparison in step 3 is what it is FOR; [`preview_rows`] is what it is KEPT for. A dry
    /// run has to be able to build the store the apply will be classifying into, and the only way
    /// to do that without opening the real database for writing is to rebuild it somewhere else
    /// out of what was read.
    stored: BTreeMap<(Table, String), String>,
}

/// **The classification, the comparison and the report — everything [`migrate`] does except write.**
///
/// This exists so that [`preview`] is the SAME code path rather than a second implementation of the
/// same rules. A preview that classified separately would be a second opinion about which table a
/// name belongs in, about which keys are ambiguous, and about what is already stored — and the
/// moment those two opinions disagreed, the dry run would be advertising a migration that is not
/// the one the apply performs. `db_tests::the_two_entry_points_share_one_classifier` pins that both
/// call it, and `crates/vike-secrets/tests/database_migration.rs`'s
/// `the_dry_run_predicts_exactly_what_the_apply_does` pins the behaviour over real fixtures.
///
/// **It opens NOTHING for writing.** `open_for_read` carries no `SQLITE_OPEN_CREATE`, so the whole
/// of this function is reachable on a box with no database and leaves it with no database — which is
/// the property the dry run is built on, rather than a rollback that would have to be trusted.
///
/// The steps are [`migrate`]'s own 1, 2, 3 and 5; the numbering there is the authority for what each
/// one is for.
fn plan(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
) -> Result<Plan, MigrateError> {
    let secrets_path = crate::dotenv::workspace_dotenv_path_from(settings_dir);
    let node_path = crate::dotenv::workspace_node_path_from(settings_dir);
    let db_path = crate::dotenv::workspace_db_path_from(settings_dir);

    let secrets = crate::store::resolve(&secrets_path)?.secrets.into_map();
    let node = crate::store::resolve(&node_path)?.secrets.into_map();

    let mut refusals: Vec<Ambiguity> = Vec::new();
    let mut doubly_claimed: Vec<String> = Vec::new();

    // 1. Classify. `wanted[(table, name)] = (value, the file it came from)`.
    let mut wanted: BTreeMap<(Table, String), (String, PathBuf)> = BTreeMap::new();
    for (name, value) in &node {
        if !is_node_key(name) {
            refusals.push(Ambiguity::UnexpectedNameInNodeFile { key: name.clone() });
            continue;
        }
        wanted.insert((Table::NodeKey, name.clone()), (value.clone(), node_path.clone()));
    }
    for (name, value) in &secrets {
        // The same name in both files with different values is a half-migrated box. Whichever side
        // we picked would produce a mismatched pair, so neither is picked.
        if let Some(other) = node.get(name) {
            if other != value {
                refusals.push(Ambiguity::DisagreeingFiles { key: name.clone() });
            } else {
                // An identical value already claimed from the node file is the SAME row, and the
                // node file is the home, so it keeps the attribution — but the credential file DOES
                // hold this line, and a report that never says so under-counts that file against a
                // `grep -c`. See `Migration::doubly_claimed`.
                doubly_claimed.push(name.clone());
            }
            continue;
        }
        let table = if is_node_key(name) { Table::NodeKey } else { Table::Credential };
        wanted.insert((table, name.clone()), (value.clone(), secrets_path.clone()));
    }

    // 2. What is already there. A READ-ONLY open, so a run that turns out to have nothing to do
    //    never opens the database for writing — see step 4 in `migrate`, and `preview`, which never
    //    reaches a step 4 at all.
    let exists = database_present(&db_path);
    let mut version = None;
    let mut stored: BTreeMap<(Table, String), String> = BTreeMap::new();
    if exists {
        let (conn, found) = open_for_read(&db_path)?;
        version = Some(found);
        for table in [Table::Credential, Table::NodeKey] {
            for (name, value) in read_table_on(&db_path, &conn, table, found)?.into_map() {
                stored.insert((table, name), value);
            }
        }
    }

    // 2b. The credential file's COMMENT lines — the one thing the parser above throws away, and
    //     §4.2's rollback values and §4.3's provenance live in nothing else. Read from the SAME
    //     path step 1 read, so there is no second walk; absent is an empty contribution.
    let comments = match std::fs::read_to_string(&secrets_path) {
        Ok(text) => crate::schema::scan_comments(&text),
        // Unreadable is not silently empty anywhere else in this crate either — but step 1 already
        // opened this exact path through `crate::store::resolve` and would have returned its error,
        // so reaching here with an error means the file vanished between the two reads. An empty
        // contribution is then the honest answer: there are no comments to carry.
        Err(_) => crate::schema::FileComments::default(),
    };

    // 3. Compare. Every finding, not the first.
    //
    // ⚠ TWO CLASSES of finding, and they are disposed of differently. See `MigrateError::Ambiguous`
    // and `Migration::refused`.
    let mut pending: BTreeMap<(Table, String), String> = BTreeMap::new();
    let mut already: BTreeSet<(Table, String)> = BTreeSet::new();
    let mut refused: Vec<Ambiguity> = Vec::new();
    for ((table, name), (value, _)) in &wanted {
        if stored.contains_key(&(table.other(), name.clone())) {
            refusals.push(Ambiguity::WrongTable {
                key: name.clone(),
                found_in: table.other(),
                wanted: *table,
            });
            continue;
        }
        match stored.get(&(*table, name.clone())) {
            Some(have) if have == value => {
                already.insert((*table, name.clone()));
            }
            // ⚠ PER-KEY, not whole-run. This used to join `refusals` and abort the run, so an
            // operator who added ONE brand-new key in the same edit that left ONE stale line behind
            // got neither: the new key did not land, and the only way forward was to hand-edit a
            // file to get a DIFFERENT key migrated. The refusal itself is unchanged and is the
            // point — this key is never silently overwritten and it is still NAMED — but it now
            // refuses itself alone, and every unambiguous key beside it lands.
            //
            // ⚠ It is also, by construction, IMPOSSIBLE on the run that CREATES the database:
            // `stored` is populated only `if exists`, so a non-empty `refused` proves a database was
            // already there. The irreversible first run therefore always carries everything the
            // files hold, which is what lets a CLI report a partial run as a finding rather than as
            // a failure.
            Some(_) => {
                refused.push(Ambiguity::DisagreesWithDatabase { key: name.clone(), table: *table })
            }
            None => {
                pending.insert((*table, name.clone()), value.clone());
            }
        }
    }

    if !refusals.is_empty() {
        refusals.sort();
        refusals.dedup();
        return Err(MigrateError::Ambiguous(refusals));
    }
    refused.sort();
    refused.dedup();

    // 5. Report — one row per (file, table) pair that could have contributed. (Step 4 is the WRITE,
    //    and it is the one thing this function does not do.)
    let mut sources = Vec::new();
    for (file, table) in [
        (&secrets_path, Table::Credential),
        (&secrets_path, Table::NodeKey),
        (&node_path, Table::NodeKey),
    ] {
        let keys: Vec<String> = wanted
            .iter()
            .filter(|((t, _), (_, src))| *t == table && src == file)
            .map(|((_, n), _)| n.clone())
            .collect();
        if keys.is_empty() && file == &secrets_path && table == Table::NodeKey {
            // The ordinary post-0051 box: no node key left in the credential store. A zero row here
            // would be noise, and its ABSENCE is the thing worth noticing.
            continue;
        }
        sources.push(SourceReport {
            file: file.clone(),
            table,
            read: keys.len(),
            inserted: keys.iter().filter(|n| pending.contains_key(&(table, (*n).clone()))).count(),
            already_present: keys
                .iter()
                .filter(|n| already.contains(&(table, (*n).clone())))
                .count(),
        });
    }

    doubly_claimed.sort();
    doubly_claimed.dedup();
    Ok(Plan {
        db: db_path,
        exists,
        version,
        comments,
        pending,
        sources,
        doubly_claimed,
        refused,
        stored,
    })
}

/// **Fill the database from the files.** Idempotent, non-destructive, and refusing rather than
/// half-finishing.
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value — the same PARAMETER every other resolver
/// in this crate takes, so this performs no environment read and resolves its three paths through
/// the ONE walk ([`crate::workspace_dotenv_path_from`], [`crate::workspace_node_path_from`],
/// [`crate::workspace_db_path_from`]).
///
/// `is_node_key` decides WHICH TABLE a name belongs to, and it is a parameter because this crate
/// declares no `vike-*` dependency and so cannot see `vike_model::credential_keys`. Pass the UNION
/// predicate (`is_platform_key`, the four-name table), not a per-service one: 0051's narrowing is
/// about which FILE answers for one service's pair at READ time, whereas this is a CLASSIFICATION of
/// every name in the store, and a per-service predicate here would file the other service's pair as
/// a venue credential.
///
/// # What it does
///
/// 1. Reads both files through `crate::store::resolve` — the one parser, so the migration cannot
///    disagree with the reader about what a line means. An ABSENT file contributes nothing; an
///    unreadable one is an error.
/// 2. Classifies: everything in the node file is a node key (and it REFUSES if the predicate
///    disagrees — [`Ambiguity::UnexpectedNameInNodeFile`]); in the credential file, a name the
///    predicate claims goes to `node_key` and every other name to `credential`. That second arm is
///    what DRAINS 0051's legacy home rather than stacking a third level on it.
/// 3. Compares against what is already stored, collecting EVERY finding before writing anything —
///    the three that make the RUN undecidable (returned as [`MigrateError::Ambiguous`], nothing
///    written) and the one that makes a single KEY undecidable (reported on [`Migration::refused`],
///    that key not written and every other key landed).
/// 4. Writes the pending rows in ONE transaction, then stamps [`SCHEMA_VERSION`] — or opens no write
///    connection at all when there is nothing pending, which is how *twice is the same as once* is
///    structural here rather than hoped for.
///
///    ⚠ **With nothing pending and NO DATABASE, it creates none**
///    ([`MigrationOutcome::NothingToMigrate`]). Creating an empty one is the harmful act: from that
///    moment `crate::store::backend_at` answers `Database` for every process on the box and the
///    credential file written afterwards is never read again.
///
/// ⚠ **Steps 1, 2, 3 and the report are [`plan`], verbatim and shared with [`preview`]** — this
/// function is that plan plus step 4. It is one code path rather than two so that a dry run cannot
/// describe a migration different from the one that follows it.
///
/// # What it never does
///
/// Open either file for writing, in any branch. See the module doc.
/// ⚠ **Step 0, added with schema 2: it also UPGRADES a store it finds at an older schema**, in the
/// same transaction as everything else.
///
/// That is a widening of this verb and it is deliberate. The alternative was a second verb, and a
/// second verb would mean an operator whose box is at schema 1 runs `migrate`, is told *already
/// complete*, and is not told that every credential write on that box will now be refused. One
/// verb whose name already means *bring this store to the current schema*, with the `--dry-run`
/// this act has always deserved in front of it, is the smaller surface. `crate::schema`'s
/// `reshape_into` carries the atomicity argument.
///
/// `classify` is the second injected decision, beside `is_node_key` and for the same layering
/// reason: it says which ACCOUNT a credential name belongs to, which needs
/// `vike_model::account_keys` and the venue roster, and this crate declares no `vike-*` dependency.
/// `vike_bridge_core::credentials::classify_credential_name` is the production implementation.
pub fn migrate(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> Result<Migration, MigrateError> {
    let planned = plan(settings_dir, is_node_key)?;
    // The NAMES behind the counts, taken from the plan rather than read back out of the store
    // afterwards — see `Migration::inserted_keys`. A `BTreeMap` keyed `(table, name)` yields them
    // sorted, and the classification puts each name in exactly one table, so there are no
    // duplicates to fold.
    //
    // ⚠ A SCHEMA UPGRADE adds to this set, below, and that is not double-counting: it re-inserts
    // every credential row in the store while inserting no new KEY, so a ledger built from
    // `pending` alone would record NOTHING for the one act that rewrites the whole table and
    // cannot be undone. `crate::schema::RowReport::written_names` is that half.
    let mut inserted_keys: Vec<String> = planned.pending.keys().map(|(_, n)| n.clone()).collect();

    // 4. Write — or do not open for writing at all.
    //
    // ⚠ The `pending.is_empty() && !exists` arm is the one that must NOT open a write connection,
    // because opening one CREATES the database. See `MigrationOutcome::NothingToMigrate`, which
    // carries what that cost.
    // Step 0 — an existing store at an older schema is UPGRADED even when the files carry nothing
    // new, because "nothing to carry" and "this store is the shape this binary writes" are
    // different questions and only the second one decides whether a credential write will land.
    let must_upgrade = planned.version.is_some_and(|v| v != SCHEMA_VERSION);
    let schema_before = planned.version;

    let mut rows = None;
    let outcome = if planned.pending.is_empty() && !must_upgrade {
        if planned.exists {
            if planned.refused.is_empty() {
                MigrationOutcome::AlreadyComplete
            } else {
                MigrationOutcome::NothingNewButRefused
            }
        } else {
            MigrationOutcome::NothingToMigrate
        }
    } else {
        // ⚠ `created` comes from the OPEN and not from `planned.exists`, and the two can genuinely
        // differ: a sibling process may have created the database between the plan's read-only
        // probe and this open. The open is the authority because it is the call that would have
        // done the creating, so the stamp below is decided by what actually happened rather than by
        // what was predicted. That difference is also exactly what a preview cannot promise — see
        // [`preview`].
        // ⚠ **A RUN THAT FAILS LEAVES NO DATABASE BEHIND, and that is a DIFFERENT case from the
        // unfinished one this module already argues.** Draw the line before reading further:
        //
        // * **The process is KILLED** between the commit and [`stamp_schema_version`] — nothing can
        //   prevent that, the rows sit at `user_version = 0`, and every fallible reader refuses
        //   LOUDLY with the recipe ([`check_schema_version`], pinned by
        //   `the_unfinished_database_says_it_cannot_be_resumed_and_names_the_way_out`). Unchanged.
        // * **This function RETURNS `Err`** — a path we control. Leaving the file then converts a
        //   working box into one whose credentials are unreadable until a human deletes a file by
        //   hand, in exchange for nothing: the credential FILES are untouched, so re-running is the
        //   whole repair. `upsert_rows` refuses the same shape and argues it; this is that rule for
        //   the one function whose JOB is to create the file.
        //
        // The guard is measured BEFORE the open, not taken from `open_for_write`'s `created`,
        // because `open_for_write` can fail AFTER `create_file_private` — at `Connection::open`, at
        // the `journal_mode` verification, or in `execute_batch(SCHEMA)` — and on those paths there
        // is no `created` to take. The unlink is `let _ =` deliberately: a cleanup may not replace
        // the operator's real error with a worse one, and it runs only over a path that did not
        // exist moments earlier and has never held a row belonging to anybody.
        let we_created_it = !planned.db.exists();
        match write_pending(&planned, classify) {
            Ok((created, report)) => {
                rows = report;
                if created {
                    MigrationOutcome::Created
                } else if must_upgrade {
                    MigrationOutcome::SchemaUpgraded
                } else {
                    MigrationOutcome::Updated
                }
            }
            Err(e) => {
                if we_created_it {
                    let _ = std::fs::remove_file(&planned.db);
                }
                return Err(e.into());
            }
        }
    };

    if outcome == MigrationOutcome::SchemaUpgraded
        && let Some(r) = &rows
    {
        inserted_keys.extend(r.written_names.iter().cloned());
        inserted_keys.sort();
        inserted_keys.dedup();
    }

    Ok(Migration {
        db: planned.db,
        outcome,
        schema_before,
        schema_now: SCHEMA_VERSION,
        rows,
        sources: planned.sources,
        doubly_claimed: planned.doubly_claimed,
        refused: planned.refused,
        inserted_keys,
    })
}

// ---------------------------------------------------------------------------------------------
// The dry run
// ---------------------------------------------------------------------------------------------

/// What a migration WOULD do. The conditional mood of [`MigrationOutcome`], and a separate type
/// because the indicative one would lie here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedOutcome {
    /// No database exists and there are rows to write, so a run would create one.
    WouldCreate,
    /// A database exists and a run would add rows to it.
    WouldAdd,
    /// **A database exists at an OLDER schema and a run would UPGRADE it in place**, with or
    /// without new rows. The conditional twin of [`MigrationOutcome::SchemaUpgraded`], and the one
    /// an operator most wants to see before running the verb: the upgrade is a ONE-WAY DOOR for
    /// every binary older than this one.
    WouldUpgradeSchema,
    /// A database exists and already holds every key the files carry. A run would open no write
    /// connection at all.
    ///
    /// ⚠ Asserts COMPLETENESS, so it may only be chosen when [`MigrationPlan::refused`] is empty —
    /// see [`Self::NothingNewButRefused`].
    AlreadyComplete,
    /// A database exists, nothing new is pending, and a key WOULD BE refused. The twin of
    /// [`MigrationOutcome::NothingNewButRefused`], and it matters more on this side: a preview is
    /// read to decide whether to run the thing at all.
    NothingNewButRefused,
    /// **Nothing to migrate, and NO DATABASE WOULD BE CREATED.** Neither file carries a key and none
    /// exists. See [`MigrationOutcome::NothingToMigrate`] for why creating one is the harmful act.
    NothingToMigrate,
}

impl std::fmt::Display for PlannedOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PlannedOutcome::WouldCreate => "would be CREATED",
            PlannedOutcome::WouldAdd => "already exists; rows would be ADDED",
            PlannedOutcome::WouldUpgradeSchema => {
                "already exists at an OLDER schema; it would be UPGRADED in place"
            }
            PlannedOutcome::AlreadyComplete => "already exists and already holds every key",
            PlannedOutcome::NothingNewButRefused => {
                "already exists; nothing new to carry, and a key would be REFUSED"
            }
            PlannedOutcome::NothingToMigrate => {
                "would NOT be created — there is nothing to migrate"
            }
        })
    }
}

/// **What [`preview`] found: the same report [`Migration`] carries, in the conditional mood.**
///
/// ⚠ **It is deliberately NOT a [`Migration`]**, and the distinction is the whole reason this type
/// exists rather than a `dry_run: bool` on the other one. [`Migration::database_exists`] is
/// documented as *"as a result of this run having SUCCEEDED"* and [`MigrationOutcome`]'s `Display`
/// says `created`; a preview that returned one would hand its caller a value asserting a database
/// exists when none does, and the caller most likely to be misled is a CLI printing the report
/// straight through. So the report shapes match field for field — which is what makes them
/// comparable — and the two vocabularies do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    /// The database this run would be about. It may not exist, and on
    /// [`PlannedOutcome::NothingToMigrate`] it deliberately would not be made to.
    pub db: PathBuf,
    /// What a run would do. See [`PlannedOutcome`].
    pub outcome: PlannedOutcome,
    /// The schema the database carries today, or `None` when there is none.
    pub schema_before: Option<i64>,
    /// The schema a run would leave it at.
    pub schema_now: i64,
    /// One row per (file, table) pair that could contribute. ⚠ `SourceReport::inserted` reads as
    /// *would be inserted* here — the same type, the same numbers, the conditional mood.
    pub sources: Vec<SourceReport>,
    /// See [`Migration::doubly_claimed`].
    pub doubly_claimed: Vec<String>,
    /// See [`Migration::refused`] — the keys a run would refuse WITHOUT refusing the run.
    ///
    /// ⚠ Non-empty here proves a database already exists (the per-key refusal compares against a
    /// STORED row), so this list is always empty on the one run that is irreversible.
    pub refused: Vec<Ambiguity>,
    /// See [`Migration::inserted_keys`] — the names a run WOULD insert, in the conditional mood.
    ///
    /// Carried here rather than left out because it is the one field an operator can check by eye
    /// against their own file before an irreversible act, and because a preview that reported fewer
    /// fields than the apply is a preview nobody can compare to the apply.
    pub inserted_keys: Vec<String>,
    /// **What the schema-2 fill WOULD do** — the conditional twin of [`Migration::rows`], produced
    /// by running the real classifier over an in-memory REPLICA of this store. See [`preview_rows`]
    /// for how the replica is built and for the measurement that made this field necessary: without
    /// it the preview reported *would be upgraded, every key name would still answer* immediately
    /// before an apply that failed.
    ///
    /// `None` only when there is nothing to do at all ([`PlannedOutcome::NothingToMigrate`]).
    pub rows: Option<crate::schema::RowReport>,
}

impl MigrationPlan {
    /// Rows a run would insert. `0` means a run would write nothing.
    #[must_use]
    pub fn would_insert(&self) -> usize {
        self.sources.iter().map(|s| s.inserted).sum()
    }

    /// Would a run CREATE the database? The question the first run's irreversibility hangs on.
    #[must_use]
    pub fn would_create(&self) -> bool {
        matches!(self.outcome, PlannedOutcome::WouldCreate)
    }

    /// Total names read out of the files. Named like [`Migration::keys_read`] and for the same
    /// reason — see that method.
    #[must_use]
    pub fn keys_read(&self) -> usize {
        self.sources.iter().map(|s| s.read).sum()
    }
}

impl std::fmt::Display for MigrationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "settings database {} ({})", self.db.display(), self.outcome)?;
        if self.outcome == PlannedOutcome::WouldUpgradeSchema {
            write!(
                f,
                "\n  schema {} -> {}: the account becomes a ROW. Every key name would still \
                 answer exactly as it does today, and the credential file would be READ ONLY.\n  \
                 ⚠ ONE-WAY: after it, a binary older than this one refuses this store — which \
                 downstream is an EMPTY credential map and therefore every venue on paper. Roll \
                 the BINARY back only together with this store.",
                self.schema_before.unwrap_or(0),
                self.schema_now,
            )?;
        }
        if let Some(rows) = &self.rows
            && !rows.is_quiet()
        {
            // ⚠ Printed only when it has something to SAY. The counts on a `RowReport` are
            // indicative ("3 account row(s) written"), and a preview that stated them as fact would
            // be the second mood problem this type exists to avoid — see this type's own doc. Its
            // FINDINGS are mood-free: a refused key is refused, a name the classifier cannot place
            // cannot be placed, and an alias is an alias.
            write!(f, "\n  what the classifier finds in this store:\n  {rows}")?;
        }
        for s in &self.sources {
            write!(
                f,
                "\n  {} -> {}: {} key(s) read, {} would be inserted, {} already present",
                s.file.display(),
                s.table,
                s.read,
                s.inserted,
                s.already_present
            )?;
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) would be REFUSED and nothing would be overwritten — every other \
                 key above would land:",
                self.refused.len()
            )?;
            for a in &self.refused {
                write!(f, "\n    - {a}")?;
            }
        }
        if !self.doubly_claimed.is_empty() {
            write!(
                f,
                "\n  {} name(s) are in BOTH source files with an identical value and are counted \
                 under the node file's row above, so the credential file holds that many more \
                 `KEY=` lines than its row reports: {}",
                self.doubly_claimed.len(),
                self.doubly_claimed.join(", ")
            )?;
        }
        f.write_str("\n  NOTHING WAS WRITTEN — this is a plan, not a run")
    }
}

/// **What [`migrate`] would do, without doing any of it.** Reads; creates nothing; writes nothing.
///
/// Same two parameters as [`migrate`] and the same meaning — pass the SAME `is_node_key` predicate,
/// because the classification is the thing being previewed.
///
/// # Why a real dry run, and not one of the three that look cheaper
///
/// A migration's FIRST successful run is irreversible in practice: once the database exists,
/// `crate::store::backend_at` answers `Database` for every process on the box, `secrets.env` stops
/// being read, and the `is_node_key` classification is baked into two tables that
/// [`Ambiguity::WrongTable`] then refuses to re-decide. There is no repair verb. Three cheaper
/// previews were considered and all three are worse than the act they preview:
///
/// * **Open for write and roll back.** [`open_for_write`] creates the directory, the file and the
///   schema and leaves `user_version` at 0 until [`stamp_schema_version`] runs — so an aborted
///   transaction leaves precisely the one state this module has no resume path for, the state
///   [`DbErrorKind::SchemaVersion`]'s zero arm exists to explain. A preview that can strand a box is
///   not a preview.
/// * **Migrate a COPY in a temp directory.** It answers the right question and produces a SECOND
///   PLAINTEXT COPY of every live venue key, created under the umask — MEASURED as 0664 on the live
///   box (0054's *What must land*), i.e. group- and world-readable — in a directory this code does
///   not own and cannot promise to remove on a `SIGKILL`. The whole modes section of this module
///   exists to stop that happening once; doing it deliberately for a preview is worse.
/// * **Just run it, it is idempotent.** The second run is safe precisely BECAUSE the first already
///   happened. Idempotence says nothing about the act that creates the store, which is the act the
///   operator wants to see the shape of first.
///
/// This function is none of them: it is [`plan`], which opens the database READ-ONLY when one
/// exists and opens nothing at all when one does not, plus [`preview_rows`], which runs the REAL
/// classifier over an in-memory replica and opens nothing at all either.
///
/// # ⚠ The classifier RUNS here, and it did not
///
/// [`plan`] alone predicts the decisions made BEFORE a row is written. It cannot see a single one
/// of `crate::schema::write_rows`' own refusals, and the upgrade's failures live there — measured
/// end to end by a reviewer on a store holding both `ASTER_LIVE_API_KEY` and
/// `ASTER_MAINNET_API_KEY`, where this function printed *would be UPGRADED* and *every key name
/// would still answer exactly as it does today* and the apply that followed it failed on a unique
/// index. [`MigrationPlan::rows`] is that half, and [`preview_rows`] argues the replica.
///
/// # ⚠ What a dry run still cannot predict
///
/// It reports the DECISIONS, which are the whole of what is interesting, and it cannot promise the
/// WRITE. Four things belong to step 4 alone and are invisible here:
///
/// * **[`DbErrorKind::Io`] from the create helpers** — `create_dir_private` and
///   `create_file_private` run only on the write path, so a `settings/db` that cannot be created (a
///   read-only mount, a directory owned by another user, a full disk) surfaces on the apply and
///   never on the preview;
/// * **[`DbErrorKind::JournalMode`]** — `PRAGMA journal_mode = DELETE` is set by [`open_for_write`],
///   and a preview never asks the engine for it;
/// * **an INSERT or a COMMIT failing** — a full disk, a lock held by another writer, a transaction
///   that cannot land;
/// * **staleness.** A plan is a photograph. Both files and the database can change between the
///   preview and the apply, and the apply re-plans from scratch rather than trusting this one — see
///   [`migrate`], whose `created` comes from the OPEN and not from the plan's probe.
///
/// A refusal, by contrast, IS predicted: [`MigrateError::Ambiguous`] is decided entirely in [`plan`]
/// and returns from both entry points identically.
pub fn preview(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> Result<MigrationPlan, MigrateError> {
    let planned = plan(settings_dir, is_node_key)?;
    let must_upgrade = planned.version.is_some_and(|v| v != SCHEMA_VERSION);
    let outcome = if must_upgrade {
        // ⚠ The upgrade OUTRANKS every other conditional outcome, including *already holds every
        // key*: a store at an older schema is not complete in the sense the operator reads that
        // word, because every credential write to it is refused.
        PlannedOutcome::WouldUpgradeSchema
    } else if planned.pending.is_empty() {
        if planned.exists {
            if planned.refused.is_empty() {
                PlannedOutcome::AlreadyComplete
            } else {
                PlannedOutcome::NothingNewButRefused
            }
        } else {
            PlannedOutcome::NothingToMigrate
        }
    } else if planned.exists {
        PlannedOutcome::WouldAdd
    } else {
        PlannedOutcome::WouldCreate
    };
    let inserted_keys: Vec<String> = planned.pending.keys().map(|(_, n)| n.clone()).collect();
    // ⚠ Skipped on the ONE outcome where a run does nothing at all: with no database, no pending
    // row and no upgrade due, there is no fill to describe and an empty report would read as
    // "nothing found" rather than "nothing asked".
    let rows = if outcome == PlannedOutcome::NothingToMigrate {
        None
    } else {
        preview_rows(&planned, classify)
            .map_err(|e| DbError::sql(&planned.db, e))
            .map_err(MigrateError::from)?
    };
    Ok(MigrationPlan {
        db: planned.db,
        outcome,
        schema_before: planned.version,
        schema_now: SCHEMA_VERSION,
        sources: planned.sources,
        doubly_claimed: planned.doubly_claimed,
        refused: planned.refused,
        inserted_keys,
        rows,
    })
}

/// **PERFORM ruling 10's move** — take every `credential` row the classifier marks
/// `PendingMove::VenueSetting` and re-file it as a `setting` row, in ONE transaction.
///
/// `plan` answers, for one credential NAME, the settings key it should become — `None` for a row
/// that does not move. `vike_bridge_core::credentials` owns that derivation
/// (`classify_credential_name`'s `pending_move` plus `venue_setting_key`); it arrives as a closure
/// because this crate declares no `vike-*` dependency.
///
/// `collisions` is the check `vike_bridge_core::credentials::rendered_name_collisions` performs,
/// handed in already evaluated for the keys this move would write.
///
/// # ⚠ TWO refusals, and both write NOTHING
///
/// * **A COLLISION** — a rendered name a live credential row already holds. §6.2 puts this check on
///   the migration because SQLite cannot express it: the two tables are one namespace and
///   `credential_one_live_name` holds inside `credential` alone.
/// * **A DIVERGENCE** — two names collapsing onto one key while holding different values. The ten
///   moving names are nine rows only because dukascopy's pair was MEASURED equal in the live store;
///   a store where they differ cannot be expressed by one row, and picking either would hand one
///   account the other's JForex server. Nothing here guesses which.
///
/// Both are all-or-nothing rather than per-row, for the reason `crate::schema::reshape_into` gives
/// about its own shortfall: a half-applied move leaves a value in neither home for some reader, and
/// a venue silently drops to paper with nothing to say why.
///
/// # ⚠ `dry_run` writes nothing and answers the same report
///
/// So an operator can see the whole verdict — keys, names, and both refusals — before anything
/// moves. That is the shape `vike-cli config adopt --dry-run` already has, and for the same reason:
/// this touches the only copy of a box's venue keys.
///
/// # Errors
/// The engine, or a store at a schema that predates the `setting` table.
pub fn move_pending_rows(
    path: &Path,
    plan: &dyn Fn(&str) -> Option<String>,
    collisions: BTreeMap<String, String>,
    dry_run: bool,
) -> Result<MovedRows, DbError> {
    let mut report = MovedRows { collisions, ..MovedRows::default() };
    let live = read_table(path, Table::Credential)?.into_map();

    // WHICH rows move, and what each becomes. A key several names share is the one-to-many case.
    let mut by_key: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, value) in &live {
        if let Some(key) = plan(name) {
            by_key.entry(key).or_default().push((name.clone(), value.clone()));
        }
    }

    // ⚠ THE DIVERGENCE CHECK, before anything is written and before the collision verdict is acted
    // on: a caller that saw only the collisions would learn about this one row at a time.
    for (key, rows) in &by_key {
        let mut values: Vec<&str> = rows.iter().map(|(_, v)| v.as_str()).collect();
        values.sort_unstable();
        values.dedup();
        if values.len() > 1 {
            let mut names: Vec<String> = rows.iter().map(|(n, _)| n.clone()).collect();
            names.sort();
            report.divergent.insert(key.clone(), names);
        }
    }
    if report.refused() {
        return Ok(report);
    }

    for (key, rows) in &by_key {
        report.keys.push(key.clone());
        report.names.extend(rows.iter().map(|(n, _)| n.clone()));
    }
    report.keys.sort();
    report.names.sort();
    if dry_run || report.keys.is_empty() {
        return Ok(report);
    }

    let (mut conn, _created, _version) = open_for_write(path)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    for (key, rows) in &by_key {
        // The value is the one every name in the group agreed on — the divergence check above is
        // what licenses taking the first.
        let value = &rows[0].1;
        tx.execute(
            "INSERT INTO setting (section, key, value) VALUES ('config', ?1, ?2) \
             ON CONFLICT(section, key) DO UPDATE SET value = excluded.value",
            (key, value),
        )
        .map_err(|e| DbError::sql(path, e))?;
        for (name, _) in rows {
            tx.execute("DELETE FROM credential WHERE name = ?1", [name])
                .map_err(|e| DbError::sql(path, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// Folding the SETTINGS rows back into the credential map — §6.2's ordering (A)
// ---------------------------------------------------------------------------------------------

/// **A credential map with the settings rows folded in, and what disagreed.**
///
/// The third and last of the prerequisites `crate::schema`'s §12 note demands before ruling 10's
/// rows may move. Its two siblings are pure and live in `vike_bridge_core::credentials` — the name
/// renderer and the collision check; this one is the READ, and it lives here because this crate
/// owns the tables.
#[derive(Debug, Clone)]
pub struct FoldedSecrets {
    /// Every legacy credential name a reader looks up, whichever table now holds its value.
    pub map: SecretMap,
    /// ⚠ **Names BOTH tables answered for, sorted.** Empty is the normal state, in both directions:
    /// before the move nothing renders, and after a complete move nothing is left behind.
    ///
    /// A non-empty list means the move is HALF-DONE, and it is returned as DATA rather than logged
    /// because this crate carries no logging dependency — the same shape
    /// `crate::store::permission_warning` already uses for a finding its caller must surface.
    pub collisions: Vec<String>,
}

/// **What a move DID, or would do** — the report `crate::db::move_pending_rows` answers with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovedRows {
    /// The settings keys written, sorted. One key per `venue_setting`-bound credential row, and
    /// FEWER keys than rows wherever a renderer is one-to-many (dukascopy's pair).
    pub keys: Vec<String>,
    /// The credential names removed, sorted. Always at least as many as [`Self::keys`].
    pub names: Vec<String>,
    /// ⚠ **Rows that would have COLLAPSED onto one key while holding DIFFERENT values**, as
    /// `key -> the names that disagreed`. Non-empty means nothing was written.
    ///
    /// §11 counts the ten moving names as nine rows because `DUKASCOPY_DEMO1_SERVER` and
    /// `DUKASCOPY_DEMO2_SERVER` hold the SAME value — MEASURED in the live store on 2026-09-14,
    /// not assumed. If that ever stops being true, one row cannot express both and silently keeping
    /// either would hand one dukascopy account the other's JForex server.
    pub divergent: BTreeMap<String, Vec<String>>,
    /// ⚠ **Rendered names a LIVE credential row already holds**, from
    /// `vike_bridge_core::credentials::rendered_name_collisions`. Non-empty means nothing was
    /// written.
    pub collisions: BTreeMap<String, String>,
}

impl MovedRows {
    /// `true` when something stopped the move. Both refusals are all-or-nothing.
    #[must_use]
    pub fn refused(&self) -> bool {
        !self.divergent.is_empty() || !self.collisions.is_empty()
    }
}

/// **Read the credential table and fold in the legacy names the `setting` rows render.**
///
/// `render` turns one settings KEY into the legacy credential names it stands for —
/// `vike_bridge_core::credentials::venue_setting_names` composed with `parse_venue_setting_key` is
/// the production implementation. It arrives as a closure for the reason every other classifier in
/// this crate does: **this crate declares no `vike-*` dependency**, and the composition needs the
/// venue roster and the hand-mapped account table.
///
/// # ⚠ WHICH TABLE WINS A COLLISION, and why it is not the new one
///
/// `credential` wins. A collision means the move is half-done — the value sits in both homes and
/// the two may disagree — and preferring the SETTINGS row would change what a reader resolves at
/// the moment a half-finished migration exists. Preferring `credential` resolves to **exactly what
/// the box resolved before the move started**, which is the only choice that cannot alter live
/// behaviour while the tables are inconsistent. The collision is reported so the operator is not
/// left to discover it.
///
/// That is the read-time disposition. The MIGRATION's is stricter and belongs to it: it refuses to
/// create a collision at all (`vike_bridge_core::credentials::rendered_name_collisions`).
///
/// # ⚠ It is `Table::Credential` only
///
/// `node_key` has no settings twin and no renderer would produce its names — `docs/decisions/0051`
/// makes it a separate NAMESPACE, and folding a rendered name into it would be the shared-probe
/// defect that record was written to remove. The parameter is kept for symmetry with
/// [`read_table`] and a non-credential table simply renders nothing.
///
/// # Errors
/// The engine, a store at an unreadable schema, or a settings read that failed.
pub fn read_table_folded(
    path: &Path,
    table: Table,
    render: &dyn Fn(&str) -> Vec<String>,
) -> Result<FoldedSecrets, DbError> {
    let base = read_table(path, table)?;
    if table != Table::Credential {
        return Ok(FoldedSecrets { map: base, collisions: Vec::new() });
    }
    let mut map = base.into_map();
    let mut collisions = Vec::new();
    // A store too old to carry the table answers with nothing to fold, which is correct: no row
    // has moved on a box that has not been reshaped.
    let stored = match crate::settings::read_settings(path)? {
        crate::settings::SettingsSource::Rows { rows, .. } => rows,
        _ => return Ok(FoldedSecrets { map: SecretMap::from_map(map), collisions }),
    };
    for row in &stored.settings {
        for name in render(&row.key) {
            // ⚠ LAST-WINS is deliberately NOT used here — see the collision ⚠ above. The
            // credential row keeps the key and the disagreement is reported instead.
            match map.entry(name.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => collisions.push(name),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(row.value.clone());
                }
            }
        }
    }
    collisions.sort_unstable();
    collisions.dedup();
    Ok(FoldedSecrets { map: SecretMap::from_map(map), collisions })
}

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
mod db_tests {
    use super::*;

    /// A `rusqlite::Error` carrying one extended code, built the way [`preview_rows`] builds its
    /// own — the classifier's INPUT, so nothing here has to plant an on-disk state.
    fn failure(extended: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(extended), None)
    }

    /// **The whole finding, as a classification.** `SQLITE_READONLY_ROLLBACK` used to reach
    /// [`DbErrorKind::Sqlite`], where the operator got the engine's six words — *attempt to write
    /// a readonly database* — about a READ, on a store whose permissions are perfect, naming no
    /// repair.
    #[test]
    fn a_surviving_rollback_journal_is_its_own_named_kind() {
        let e = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(776));
        assert!(
            matches!(e.kind, DbErrorKind::ReadOnlyRollback),
            "SQLITE_READONLY_ROLLBACK must not fall through to the engine's own words: {:?}",
            e.kind
        );
        assert_eq!(
            rusqlite::ffi::SQLITE_READONLY_ROLLBACK,
            776,
            "the extended code this arm keys on moved under it"
        );
    }

    /// ⚠ **The other `SQLITE_READONLY_*` codes are NOT this arm**, and that is the half a match on
    /// the primary `rusqlite::ErrorCode::ReadOnly` would have got wrong. A `chmod 400` store, a
    /// read-only directory and a database moved out from under an open handle all raise
    /// `ErrorCode::ReadOnly`, and telling any of those operators *a writer died mid-write* would be
    /// this arm committing the defect it was added to fix.
    #[test]
    fn the_other_read_only_codes_are_not_a_dead_writer() {
        // SQLITE_READONLY (8), _RECOVERY (264), _CANTLOCK (520), _DBMOVED (1032).
        for code in [8, 264, 520, 1032] {
            let e = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(code));
            assert!(
                matches!(e.kind, DbErrorKind::Sqlite(_)),
                "extended code {code} must stay unclassified, not borrow the rollback story: {:?}",
                e.kind
            );
        }
    }

    /// **The message must name the REPAIR, and say the daemon cannot perform it.**
    ///
    /// Asserted as PROPERTIES rather than as a verbatim string: the wording is meant to be improved
    /// again, and a `assert_eq!` on the whole sentence turns every improvement into a test edit
    /// that says nothing about whether the sentence got better. What may not regress is what an
    /// operator can DO with it.
    #[test]
    fn the_rollback_message_names_the_repair_and_who_must_perform_it() {
        let msg = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(776)).to_string();
        for needle in [
            // the command, spelled so it can be pasted
            "vike-cli secrets",
            // ...from where, because the daemon's own namespace is the one place it does not work
            "OPERATOR SHELL",
            // ...and the thing an operator fears first, answered before anything else
            "NOTHING IS CORRUPT",
            // ...and that a restart is not the answer
            "CANNOT REPAIR THIS ITSELF",
            // ...and WHICH file is sitting there
            "-journal",
        ] {
            assert!(msg.contains(needle), "the repair is not reachable from the message: {msg}");
        }
        assert!(
            !msg.contains("could not be read"),
            "this arm replaces the opaque sentence rather than prefixing it — the outer \
             `SecretsError` already says the store could not be read once: {msg}"
        );
    }

    /// The busy arm is untouched by the widening — it is matched FIRST and on the primary code, so
    /// a future extended code in the busy family cannot be stolen by the rollback branch.
    #[test]
    fn the_busy_arm_still_wins_its_own_codes() {
        // SQLITE_BUSY (5), SQLITE_BUSY_SNAPSHOT (517), SQLITE_LOCKED (6).
        for code in [5, 517, 6] {
            let e = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(code));
            assert!(
                matches!(e.kind, DbErrorKind::StoreBusy),
                "extended code {code} must stay the named busy arm: {:?}",
                e.kind
            );
        }
    }
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// The journal path SQLite uses in DELETE mode: the database's own path plus `-journal`.
    #[cfg(unix)]
    fn journal_of(db: &Path) -> PathBuf {
        let mut name = db.as_os_str().to_os_string();
        name.push("-journal");
        PathBuf::from(name)
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn the_rollback_journal_is_0600_while_a_transaction_is_in_flight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");

        let (mut conn, created, _) = open_for_write(&db).expect("open");
        assert!(created);
        assert_eq!(mode_of(&db), 0o600, "the database itself must be 0600 — the precondition");

        let journal = journal_of(&db);
        assert!(!journal.exists(), "no journal before a transaction starts");

        // In flight: a transaction with a real row in it, NOT committed. This is the only window in
        // which the journal exists, so the `stat` has to happen here.
        let tx = conn.transaction().expect("begin");
        tx.execute(
            "INSERT INTO credential (name, value, field) VALUES (?1, ?2, 'API_KEY')",
            ("BINANCE_DEMO_API_KEY", "a-value-that-must-not-become-world-readable"),
        )
        .expect("insert");

        assert!(
            journal.exists(),
            "no rollback journal appeared — if SQLite stopped writing one this test measures \
             nothing and must be re-derived, not deleted"
        );
        // ⚠ THE MEASUREMENT. If this ever fails at 0644/0664, the fix is to PRE-CREATE the journal
        // path at 0600 before the transaction the way `create_file_private` pre-creates the database
        // — not to relax the assertion.
        let measured = mode_of(&journal);
        assert_eq!(
            measured, 0o600,
            "the rollback journal holds PAGES OF THE DATABASE — i.e. plaintext venue credentials — \
             and was created at {measured:o} rather than 0600"
        );

        tx.commit().expect("commit");
        stamp_schema_version(&db, &conn).expect("stamp");
        drop(conn);
        assert!(!journal.exists(), "and nothing is left at rest — DELETE mode's whole point");
    }

    /// **`open_for_write` leaves the version UNSTAMPED — only a committed caller stamps it.**
    ///
    /// ⚠ This is the mutation-provable half of the crash story, and it exists because its
    /// integration twin is not.
    /// `crates/vike-secrets/tests/database_migration.rs`'s
    /// `a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map` PLANTS the state
    /// a crash leaves and proves a reader is loud about it — a real and necessary property, and one
    /// that stays GREEN if the stamp moves back into [`open_for_write`], because the planted state
    /// is the same either way. An assertion that cannot fail for its stated reason is worse than no
    /// assertion, so the ORDER gets its own test, here, where the two functions are visible.
    ///
    /// Move `pragma_update(user_version)` back into [`open_for_write`] and this goes RED on its
    /// first assertion. That is the whole of its job.
    #[test]
    fn open_for_write_leaves_the_version_unstamped_until_the_caller_commits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");

        let (conn, created, _) = open_for_write(&db).expect("open");
        assert!(created, "a fresh path must report that this call created it");
        let version = |c: &Connection| -> i64 {
            c.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read user_version")
        };
        assert_eq!(
            version(&conn),
            0,
            "opening for write must NOT declare the database finished — a failure between here and \
             the caller's commit would leave a schema-stamped, ZERO-ROW store that \
             `check_schema_version` accepts and `Backend` then prefers forever, which is an empty \
             credential map and therefore the LIVE GATE"
        );
        // …and the schema IS there, so what the stamp withholds is the VERSION and nothing else.
        // (`field` joined this statement with schema 2 — it is `NOT NULL`, and a row without one
        // is exactly the misfiling `a_new_credential_name_without_a_classifier_is_refused…`
        // refuses.)
        conn.execute(
            "INSERT INTO credential (name, value, field) VALUES (?1, ?2, ?3)",
            ("BINANCE_DEMO_API_KEY", "a-value", "API_KEY"),
        )
        .expect("the tables must exist even though the version is unstamped");

        // The reader's verdict on that state, asked of the real function.
        let refused = check_schema_version(&db, &conn).expect_err("an unstamped store is refused");
        assert!(matches!(refused.kind, DbErrorKind::SchemaVersion { found: 0, .. }), "{refused}");

        stamp_schema_version(&db, &conn).expect("stamp");
        assert_eq!(version(&conn), SCHEMA_VERSION, "…and the stamp is what finishes it");
        check_schema_version(&db, &conn).expect("a stamped store reads");
    }

    /// **The version-0 refusal tells the operator HOW TO GET OUT, because nothing else will.**
    ///
    /// ⚠ The state is unrecoverable and that was written down only in a doc comment. A process
    /// killed between the migration's commit and [`stamp_schema_version`] leaves rows on disk at
    /// `user_version = 0`; every fallible reader then refuses LOUDLY, which is the designed
    /// behaviour AND a box whose credentials are unreadable until a human deletes a file by hand.
    /// There is no resume, no repair verb and no `--force`, so the one message anybody actually
    /// sees has to carry the recipe — `schema version 0, not 1` is a number and a dead end.
    ///
    /// The second half is the part a widened message would break: a database at some OTHER version
    /// is not this story (a future schema, or a file this project did not write), and telling its
    /// operator to delete it would be advice to destroy a store a newer binary can read.
    #[test]
    fn the_unfinished_database_says_it_cannot_be_resumed_and_names_the_way_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        let (conn, _, _) = open_for_write(&db).expect("open");

        let refused = check_schema_version(&db, &conn).expect_err("an unstamped store is refused");
        let said = refused.to_string();
        assert!(
            said.contains(&db.display().to_string()),
            "the refusal must name the file to delete, not just describe one: {said}"
        );
        for needle in ["never finished", "delete", "again"] {
            assert!(
                said.contains(needle),
                "the version-0 refusal no longer says `{needle}`. This is the ONE place an operator \
                 is told that the half-written database cannot be resumed and what to do instead; \
                 a doc comment beside code they are not reading is not that place: {said}"
            );
        }
        assert!(
            said.contains("untouched"),
            "…and that their credential files were not touched, which is what makes deleting the \
             database a safe instruction rather than a frightening one: {said}"
        );

        // A DIFFERENT version is a different story and must not carry the same recipe.
        let newer = DbError {
            path: db.clone(),
            kind: DbErrorKind::SchemaVersion {
                found: SCHEMA_VERSION + 1,
                expected: SCHEMA_VERSION,
            },
        };
        assert!(
            !newer.to_string().contains("delete"),
            "a database at a LATER schema version is not an interrupted migration — telling that \
             operator to delete it is telling them to destroy a store a newer binary reads: {newer}"
        );
    }

    /// The directory holding both is 0700, so even a journal that did NOT inherit 0600 would not be
    /// reachable by another user. Stated as a SECOND, independent floor rather than as a reason to
    /// skip the measurement above: a directory mode protects the path, not the file, and a backup
    /// job or an operator `cp -r` carries the file's own mode onward.
    #[cfg(unix)]
    #[test]
    fn the_directory_holding_the_journal_is_0700() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        let (conn, _, _) = open_for_write(&db).expect("open");
        drop(conn);
        assert_eq!(mode_of(db.parent().expect("parent")), 0o700);
    }

    /// **A write whose database VANISHED between the backend choice and the open creates nothing.**
    ///
    /// The race [`upsert_rows`]' invariant section is about, reconstructed rather than performed —
    /// the same technique, and for the same reason, as
    /// `crates/vike-secrets/tests/database_migration.rs`'s
    /// `a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map`: the window
    /// between `crate::store::backend_in`'s `is_file` and this open is a few instructions wide and
    /// cannot be widened portably from a test. What a test CAN do is hand [`upsert_rows`] the exact
    /// STATE that race produces — a database path with nothing at it — which is precisely the
    /// argument `crate::store::save_credentials_to_store` passes after its probe saw a file.
    ///
    /// It lives here rather than in `tests/` because [`upsert_rows`] is crate-private (nothing
    /// outside this crate may choose a store), so no integration test can reach the state at all.
    ///
    /// ⚠ **Restoring the old tail — `if created { stamp_schema_version(path, &conn)?; }` after the
    /// commit, with this early return removed — makes this test fail on its FIRST assertion**, and
    /// that is the whole of its value. What that code left behind was a schema-complete,
    /// version-stamped database holding only this write's two keys, from which
    /// `crate::store::backend_at` answers `Database` forever and every other credential on the box
    /// is retired in silence.
    #[test]
    fn a_write_whose_database_vanished_creates_nothing_and_fails_loudly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        assert!(!db.exists(), "the precondition: the probe saw a file and it is gone now");

        let refused = upsert_rows(
            &db,
            Table::Credential,
            &[
                ("BINANCE_DEMO_API_KEY".to_string(), "a-key".to_string()),
                ("BINANCE_DEMO_API_SECRET".to_string(), "a-secret".to_string()),
            ],
            Some(&test_classify),
        )
        .expect_err("a vanished database must be refused, not re-created");

        assert!(
            !db.exists(),
            "A DATABASE WAS CREATED BY A WRITE THAT HELD TWO KEYS. `backend_at` answers `Database` \
             from here on and the credential file beside it is never read again — the other \
             sixty-odd keys are gone, silently, and every venue drops to paper."
        );
        assert!(
            matches!(refused.kind, DbErrorKind::VanishedDatabase),
            "the refusal must name the vanished database: {refused}"
        );
        let said = refused.to_string();
        assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");

        // …and the rollback left no half-open artifact either: a `-journal` sidecar beside a
        // database that does not exist is a second way for a later reader to find something.
        let mut sidecar = db.as_os_str().to_os_string();
        sidecar.push("-journal");
        assert!(!PathBuf::from(sidecar).exists(), "a rollback journal survived the rollback");

        // The store this write was routed to is therefore still the FILES one, which is the
        // property the caller retries against.
        assert!(!database_present(&db), "`database_present` must still say no");
    }

    /// **An ORDINARY write — the database is there — is untouched by the refusal above.**
    ///
    /// The other half of the same edit, and the one that would catch a fix that refused too much: a
    /// `created` check placed where it also fired for an existing database would turn every write on
    /// every migrated box into a hard failure, and the test above would still pass.
    #[test]
    fn an_ordinary_write_on_a_database_that_exists_still_lands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");

        // Bring one into existence the ONLY way that is allowed to: a finished, stamped database.
        let (conn, created, _) = open_for_write(&db).expect("open");
        assert!(created);
        stamp_schema_version(&db, &conn).expect("stamp");
        drop(conn);

        // The FIRST write of a name this store has never held — the arm that needs a
        // classification, because schema 2's `field` is `NOT NULL`.
        upsert_rows(
            &db,
            Table::Credential,
            &[("OKX_DEMO_API_KEY".to_string(), "v1".to_string())],
            Some(&test_classify),
        )
        .expect("a write to a database that exists must land");
        // …and the UPDATE of a name it now holds, which needs NO classification at all: the row
        // already carries one. That is what keeps the venue's own rotation writer working.
        upsert_rows(
            &db,
            Table::Credential,
            &[("OKX_DEMO_API_KEY".to_string(), "v2".to_string())],
            None,
        )
        .expect("…and so must the update, with no classifier in sight");

        // ⚠ Read back WITHOUT a `map.get("LITERAL")`, deliberately. `crates/vike-ops/tests/
        // settings_registry.rs`'s `find_map_lookups` treats a literal inside a lookup call as a
        // RESOLVED READ of that environment variable — a bare literal in a test region is dropped
        // from its evidence, a lookup CALL SITE is not — so the tidy spelling demands a `SETTINGS`
        // row declaring `vike-secrets` reads `OKX_DEMO_API_KEY`, which it does not. The whole map
        // is a stronger assertion here anyway: it also pins that the update REPLACED rather than
        // inserted a second row.
        let rows = read_table(&db, Table::Credential).expect("read back").into_map();
        assert_eq!(rows.len(), 1, "the upsert must replace the row, not add one");
        let (name, value) = rows.into_iter().next().expect("one row");
        assert_eq!(name, "OKX_DEMO_API_KEY");
        assert_eq!(value, "v2", "the second write must win");
    }

    /// **[`migrate`] and [`preview`] reach their decision through ONE classifier, structurally.**
    ///
    /// The property is not "the two agree on the fixtures we wrote" — that is the behavioural half,
    /// and `crates/vike-secrets/tests/database_migration.rs`'s
    /// `the_dry_run_predicts_exactly_what_the_apply_does` holds it. A behavioural test stays green
    /// the day somebody adds a SECOND classifier that happens to agree on the cases those fixtures
    /// cover, and the whole hazard here is a dry run that describes a migration different from the
    /// one that follows it. So this asserts the shape: one definition, exactly two callers, and each
    /// entry point reaching it rather than deciding for itself.
    ///
    /// ⚠ **The needles are COMPOSED rather than spelled**, for the reason
    /// `crates/vike-ops/tests/credential_writer_gate.rs`'s `writer_names` gives about its own
    /// self-scan: this test reads THIS FILE, so a whole spelling here would be one more occurrence
    /// of the very thing being counted, and the count is the assertion.
    #[test]
    fn the_two_entry_points_share_one_classifier() {
        const SELF: &str = include_str!("db.rs");
        let call = concat!("plan(settings_dir, ", "is_node_key)?");
        let define = concat!("fn ", "plan(");

        assert_eq!(
            SELF.matches(define).count(),
            1,
            "there must be exactly ONE classifier in this module — a second is the drift this \
             module is written against"
        );
        assert_eq!(
            SELF.matches(call).count(),
            2,
            "exactly two callers of it, `migrate` and `preview`, and nothing else"
        );
        for entry in ["pub fn migrate(", "pub fn preview("] {
            // The FIRST occurrence is the definition; the array above puts a second copy of each
            // string in this file, below it.
            let at = SELF.find(entry).unwrap_or_else(|| panic!("{entry} must exist"));
            let rest = &SELF[at..];
            let end = rest.find("\n}\n").unwrap_or(rest.len());
            assert!(
                rest[..end].contains(call),
                "{entry} must reach its decision through the shared classifier rather than \
                 carrying one of its own"
            );
        }
    }

    /// A classification for the handful of names these unit tests write. It is not the production
    /// one and does not pretend to be — `vike_bridge_core::credentials::classify_credential_name`
    /// is, and this crate cannot see it. What every test below needs is only that a name resolves
    /// to SOME account deterministically.
    fn test_classify(name: &str) -> crate::schema::Classification {
        use crate::schema::{AccountKey, Classification, Placement};
        // ⚠ The prefix is COMPOSED from two tokens rather than spelled whole, for the reason
        // `vike_bridge_core::credentials`' `HAND_MAPPED_ACCOUNTS` gives: an env-prefixed literal in
        // a `src/` file is read by `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence
        // this file READS that variable, and a prefix is not a variable.
        for (head, tier, venue) in [("OKX", "DEMO", "okx"), ("BINANCE", "DEMO", "binance")] {
            if let Some(field) = name.strip_prefix(&format!("{head}_{tier}_")) {
                return Classification {
                    placement: Placement::Account(AccountKey {
                        venue: venue.to_string(),
                        tier: "demo".to_string(),
                        label: None,
                        discriminator: None,
                    }),
                    field: field.to_string(),
                    secret: true,
                    recognised: true,
                    pending_move: None,
                };
            }
        }
        Classification::unrecognised(name)
    }

    /// **MEASURED: `PRAGMA user_version` and DDL are BOTH rolled back with the transaction that
    /// set them** — which is what lets the schema-1 → schema-2 reshape be ONE atomic step.
    ///
    /// ⚠ This is a measurement of the ENGINE, pinned rather than assumed, and the design above it
    /// is unshippable if it ever stops holding. `crate::schema::reshape_into`'s doc carries the
    /// consequence: schema 2's `credential` still has `name` and `value` columns, so
    /// `SELECT name, value FROM credential` — the exact statement a schema-1 binary runs — is still
    /// valid SQL against the schema-2 shape. A reshape that could commit its tables and NOT its
    /// version stamp would leave a store an older binary accepts and answers from, with the two
    /// superseded rows folding onto their live twins' names. Atomicity is what removes that state.
    ///
    /// If this ever fails, the fix is a different reshape (a sentinel version committed first, and
    /// a message that does NOT tell the operator to delete a store holding keys the files do not) —
    /// **not** a relaxed assertion.
    #[test]
    fn the_schema_stamp_and_the_ddl_are_both_transactional() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        let (mut conn, _, _) = open_for_write(&db).expect("open");
        stamp_schema_version(&db, &conn).expect("stamp");

        let tx = conn.transaction().expect("begin");
        tx.pragma_update(None, "user_version", 99i64)
            .expect("set the version inside a transaction");
        tx.execute_batch("CREATE TABLE a_probe (a TEXT) STRICT;").expect("ddl inside it too");
        let inside: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read");
        assert_eq!(inside, 99, "the precondition: the write took effect inside the transaction");
        tx.rollback().expect("rollback");

        let after: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read");
        assert_eq!(
            after, SCHEMA_VERSION,
            "`PRAGMA user_version` SURVIVED a rollback. The reshape's atomicity rests on it not \
             doing that: a committed set of schema-2 tables under a schema-1 stamp is a store an \
             older binary reads and answers from."
        );
        let probe: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE name = 'a_probe'", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(probe, 0, "DDL survived a rollback, so the table rebuild is not atomic either");
    }

    /// **`PRAGMA foreign_keys` is ON, so schema 2's two `REFERENCES` clauses are a gate.**
    ///
    /// It is OFF by default and PER CONNECTION, so this is the difference between a schema that
    /// refuses a credential naming an account row that does not exist and one that merely
    /// describes refusing it. Asserted by attempting the dangling write, because asking the pragma
    /// what it is set to would pass against a connection that had set it and an engine that had
    /// ignored it.
    #[test]
    fn a_credential_naming_an_account_that_does_not_exist_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        let (conn, created, version) = open_for_write(&db).expect("open");
        assert!(created);
        assert_eq!(version, SCHEMA_VERSION, "a fresh store is born at the current schema");

        let refused = conn.execute(
            "INSERT INTO credential (account_id, field, value, name) VALUES (?1, ?2, ?3, ?4)",
            (9999i64, "API_KEY", "a-value", "OKX_DEMO_API_KEY"),
        );
        assert!(
            refused.is_err(),
            "a credential row naming account 9999 was ACCEPTED — `PRAGMA foreign_keys` did not \
             take, and schema 2's two REFERENCES clauses enforce nothing"
        );
    }

    /// **A NEW credential name with no classifier is REFUSED — never filed as the deployment's.**
    ///
    /// The tempting fix for schema 2's `field NOT NULL` is `field = name`, `account_id` NULL,
    /// `venue` NULL. That is not a fallback, it is a MISFILING: `(NULL, NULL)` is §5.1's
    /// infrastructure classification, so a venue credential written that way is silently detached
    /// from its account and from its venue, and nothing downstream can tell it apart from a
    /// `CLOUDFLARE_API_TOKEN`.
    #[test]
    fn a_new_credential_name_without_a_classifier_is_refused_rather_than_misfiled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        let (conn, _, _) = open_for_write(&db).expect("open");
        stamp_schema_version(&db, &conn).expect("stamp");
        drop(conn);

        let refused = upsert_rows(
            &db,
            Table::Credential,
            &[("OKX_DEMO_API_KEY".to_string(), "v1".to_string())],
            None,
        )
        .expect_err("a name this store has never held needs a classification");
        assert!(matches!(refused.kind, DbErrorKind::Unclassified { .. }), "{refused}");
        assert!(refused.to_string().contains("OKX_DEMO_API_KEY"), "it must name the KEY");
        assert!(!refused.to_string().contains("v1"), "…and never its value: {refused}");

        let rows = read_table(&db, Table::Credential).expect("read back").into_map();
        assert!(rows.is_empty(), "the refusal must have written nothing");
    }

    /// **A write to a store at an OLDER schema is refused by name, and says how to fix it.**
    ///
    /// The asymmetry this pins: [`READABLE_SCHEMA_VERSIONS`] keeps an unmigrated box READING, which
    /// is what makes the version bump deployable on its own. Writing is a different act — there is
    /// nowhere in schema 1's table to put the account classification a schema-2 row carries — so it
    /// refuses, rather than half-supporting a shape and leaving the operator to find out later.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_new_key_written_to_a_schema_1_store_is_refused_and_names_the_way_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("db").join("vike.db");
        plant_schema_1(&db, &[("BINANCE_DEMO_API_KEY".to_string(), "v1".to_string())], &[])
            .expect("plant");

        // The READ still works — that is the whole point of the dual-read.
        let rows = read_table(&db, Table::Credential).expect("a schema-1 store still reads");
        assert_eq!(rows.len(), 1);

        // …and a write of a name it already holds still works too, because no classification is
        // needed to replace a value.
        upsert_rows(
            &db,
            Table::Credential,
            &[("BINANCE_DEMO_API_KEY".to_string(), "v2".to_string())],
            Some(&test_classify),
        )
        .expect("replacing a known key needs no schema-2 column");

        let refused = upsert_rows(
            &db,
            Table::Credential,
            &[("OKX_DEMO_API_KEY".to_string(), "new".to_string())],
            Some(&test_classify),
        )
        .expect_err("a NEW name has nowhere to record its account in schema 1");
        assert!(
            matches!(refused.kind, DbErrorKind::WriteToOlderSchema { found: 1, .. }),
            "{refused}"
        );
        let said = refused.to_string();
        assert!(said.contains("OKX_DEMO_API_KEY"), "it must name the KEY: {said}");
        assert!(!said.contains("new"), "…and never its value: {said}");
        assert!(said.contains("migrate"), "…and name the verb that fixes it: {said}");
        assert!(
            said.contains("NOTHING WAS WRITTEN"),
            "…and say that the store is unchanged: {said}"
        );
    }
}

/// **Does `path` hold a SQLite database — judged by the file format's own header, not by its name?**
///
/// The SQLite file format begins with the 16-byte string [`SQLITE_MAGIC`], and that is the whole of
/// this probe: it opens the file, reads 16 bytes and stops. No row, no column and no credential
/// VALUE enters the process, so a caller that promises to open no store may still ask.
///
/// # ⚠ Not a second [`database_present`], and the difference is which question is being asked
///
/// [`database_present`] answers *is the project's database there* about a path THIS PROGRAM derived
/// ([`crate::db_path_in`]), so `is_file` is complete for it: nothing else can be at that path. This
/// answers *what did the OPERATOR just hand me*, about a path from a command line, where `is_file`
/// says nothing at all. They must never be swapped: `database_present` over an operator-named
/// `secrets.env` answers `true`, and a caller that then read it as a database would be reading a
/// text file through a SQL engine.
///
/// # What it is FOR
///
/// `vike-cli secrets --file PATH` names a store to inspect and reads it as `KEY=VALUE` text. Pointed
/// at a settings database that is not merely useless, it is quiet: a small database's pages are
/// largely NUL bytes, which ARE valid UTF-8, so `read_to_string` succeeds, [`crate::parse_dotenv`]
/// finds no assignment in the binary, and the listing prints `0 secret(s)` — *the store is empty*,
/// about the one artifact on the box holding every venue key. This is how that is refused instead.
///
/// Absent, unreadable or shorter than the header ⇒ `false`. A probe that cannot answer must not
/// claim a database: the caller's next step is to read the path as text, which is the behaviour
/// that was correct before this function existed.
#[must_use]
pub fn is_sqlite_file(path: &Path) -> bool {
    use std::io::Read as _;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; SQLITE_MAGIC.len()];
    f.read_exact(&mut head).is_ok() && head == *SQLITE_MAGIC
}

/// The SQLite file format's own 16-byte identification, from its published on-disk spec: every
/// database begins with it, including one this workspace never created.
///
/// Spelled here rather than at the call site because it is a fact about the FORMAT, which is what
/// this module owns — and because a credential file can never begin with it: the trailing NUL is a
/// byte no `KEY=VALUE` line contains.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

#[cfg(test)]
mod sqlite_header_tests {
    use super::*;

    #[test]
    fn a_text_credential_file_is_not_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.env");
        std::fs::write(&p, "BINANCE_LIVE_API_KEY=abc\n").unwrap();
        assert!(!is_sqlite_file(&p));
        // ...while the coarse probe cannot tell them apart, which is the whole reason both exist.
        assert!(database_present(&p));
    }

    #[test]
    fn an_absent_or_empty_path_is_not_a_database() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_sqlite_file(&dir.path().join("nothing-here")));
        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert!(!is_sqlite_file(&empty));
        // Shorter than the header: a truncated write must not read as a database either.
        let stub = dir.path().join("stub");
        std::fs::write(&stub, b"SQLite").unwrap();
        assert!(!is_sqlite_file(&stub));
    }

    #[test]
    fn a_real_database_is_recognised_by_its_own_header() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db").join(crate::DB_FILE);
        // Through the crate's own creator, so this asserts about the artifact `migrate` produces
        // rather than about a hand-planted 16 bytes.
        let conn = open_for_write(&db).unwrap().0;
        drop(conn);
        assert!(is_sqlite_file(&db));
    }

    /// ⚠ The failure this probe exists to prevent, driven end to end: a REAL database read as a
    /// credential file. The assertion is not that the parse errors — it is that it SUCCEEDS and
    /// yields nothing, which is the shape that reads as an empty store.
    #[test]
    fn a_database_read_as_text_is_silent_rather_than_loud() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db").join(crate::DB_FILE);
        drop(open_for_write(&db).unwrap().0);
        let bytes = std::fs::read(&db).unwrap();
        if let Ok(text) = String::from_utf8(bytes) {
            assert!(
                crate::parse_dotenv(&text).is_empty(),
                "a database parsed as KEY=VALUE yielded assignments; the refusal's premise moved"
            );
        }
        assert!(is_sqlite_file(&db), "and this is what stops it");
    }
}

/// **The move and the fold — ruling 10's last two prerequisites, driven over a real store.**
#[cfg(test)]
mod venue_setting_move {
    use super::*;

    /// A schema-current store holding the named credential rows and nothing else.
    fn store(rows: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("vike.db");
        let (conn, _created, _v) = open_for_write(&db).expect("open");
        conn.execute_batch(crate::schema::DDL).expect("ddl");
        for (name, value) in rows {
            conn.execute(
                "INSERT INTO credential (name, field, value) VALUES (?1, ?2, ?3)",
                (name, "F", value),
            )
            .expect("insert");
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION).expect("stamp");
        drop(conn);
        (dir, db)
    }

    // ⚠ The two legacy names are INVENTED, not the real dukascopy pair, and that is forced rather
    // than lazy: a whole `PREFIX_NAME` literal in a `src/` file is read by
    // `crates/vike-ops/tests/settings_registry.rs` sweep as evidence that THIS CRATE reads that
    // variable, and rows are keyed `(name, krate)` — a row under the bridge that owns the key does
    // not declare a read here. Nothing in this test depends on the names being real: what it drives
    // is TWO names collapsing onto ONE key and the fold handing both back.
    const D1: &str = "legacy-name-one";
    const D2: &str = "legacy-name-two";
    const KEY: &str = "venue.dukascopy.demo.server";
    /// A credential row that does NOT move — the control. ⚠ Deliberately NOT env-SHAPED: a whole
    /// `PREFIX_NAME` literal in a `src/` file is read by
    /// `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence that THIS CRATE reads that
    /// variable, and an invented one has no row to declare it. The two dukascopy names above are
    /// real store keys and carry rows; this one had to be invented, so it wears no prefix at all.
    const UNMOVED: &str = "a-row-that-does-not-move";

    /// What the production planner answers, spelled here as the two rows this test drives: the
    /// dukascopy pair onto ONE key, and an untouched credential onto `None`.
    ///
    /// ⚠ NOT named for the thing it stands in for. `the_two_entry_points_share_one_classifier`
    /// counts occurrences of that name in THIS FILE and requires exactly one, so a helper wearing
    /// it is a second match and reddens a gate about something else entirely.
    fn moves_to(name: &str) -> Option<String> {
        (name == D1 || name == D2).then(|| KEY.to_string())
    }

    /// The renderer's half, for the fold: one key back to both legacy names.
    fn render(key: &str) -> Vec<String> {
        if key == KEY { vec![D1.to_string(), D2.to_string()] } else { Vec::new() }
    }

    /// ⚠ **THE ONE-TO-MANY MOVE, END TO END.** Two credential rows holding the SAME value collapse
    /// onto ONE settings row — and the fold hands BOTH names back, so a loader that looks either up
    /// still finds it. That round trip is the whole claim of §6.2's ordering (A): the move has no
    /// reader-visible surface.
    #[test]
    fn two_rows_collapse_onto_one_key_and_the_fold_hands_both_names_back() {
        let (_d, db) = store(&[(D1, "jforex-demo"), (D2, "jforex-demo"), (UNMOVED, "untouched")]);
        let moved = move_pending_rows(&db, &moves_to, BTreeMap::new(), false).expect("move");
        assert!(!moved.refused(), "nothing should refuse: {moved:?}");
        assert_eq!(moved.keys, vec![KEY.to_string()], "nine rows, not ten");
        assert_eq!(moved.names.len(), 2, "…out of two credential rows: {moved:?}");

        // The credential table no longer holds them…
        let raw = read_table(&db, Table::Credential).expect("read").into_map();
        assert!(!raw.contains_key(D1), "moved out of `credential`");
        assert_eq!(raw.get(UNMOVED).map(String::as_str), Some("untouched"));

        // …and the FOLD is what makes that invisible to a reader.
        let folded = read_table_folded(&db, Table::Credential, &render).expect("fold");
        assert!(folded.collisions.is_empty(), "a completed move leaves none: {folded:?}");
        let map = folded.map.into_map();
        for name in [D1, D2] {
            assert_eq!(
                map.get(name).map(String::as_str),
                Some("jforex-demo"),
                "{name} is still found"
            );
        }
        assert_eq!(map.get(UNMOVED).map(String::as_str), Some("untouched"));
    }

    /// ⚠ **A DIVERGENCE REFUSES AND WRITES NOTHING.** The ten names are nine rows only because the
    /// dukascopy pair was MEASURED equal. Where they differ, one row cannot express both and
    /// picking either would hand one account the other's JForex server — so nothing is picked.
    #[test]
    fn two_rows_that_disagree_refuse_the_whole_move() {
        let (_d, db) = store(&[(D1, "jforex-demo-one"), (D2, "jforex-demo-two")]);
        let moved = move_pending_rows(&db, &moves_to, BTreeMap::new(), false).expect("move");
        assert!(moved.refused(), "a divergence must refuse: {moved:?}");
        assert_eq!(moved.divergent.len(), 1, "…and name the key: {moved:?}");
        assert!(moved.keys.is_empty() && moved.names.is_empty(), "NOTHING written: {moved:?}");
        // Proof rather than promise: both rows are still where they were.
        let raw = read_table(&db, Table::Credential).expect("read").into_map();
        assert_eq!(raw.len(), 2, "both credential rows survive: {raw:?}");
    }

    /// ⚠ **A COLLISION REFUSES TOO**, and it is handed in already evaluated — the check is
    /// `vike_bridge_core`'s, because composing a legacy name needs the venue roster this crate
    /// cannot depend on.
    #[test]
    fn a_collision_refuses_the_whole_move() {
        let (_d, db) = store(&[(D1, "jforex-demo")]);
        let collisions: BTreeMap<String, String> =
            [(D2.to_string(), KEY.to_string())].into_iter().collect();
        let moved = move_pending_rows(&db, &moves_to, collisions, false).expect("move");
        assert!(moved.refused() && moved.keys.is_empty(), "nothing written: {moved:?}");
        assert_eq!(read_table(&db, Table::Credential).expect("read").into_map().len(), 1);
    }

    /// A dry run answers the WHOLE verdict and writes nothing — the shape `config adopt` has, for
    /// the same reason: this touches the only copy of a box's venue keys.
    #[test]
    fn a_dry_run_reports_everything_and_writes_nothing() {
        let (_d, db) = store(&[(D1, "jforex-demo"), (D2, "jforex-demo")]);
        let moved = move_pending_rows(&db, &moves_to, BTreeMap::new(), true).expect("dry run");
        assert_eq!(moved.keys, vec![KEY.to_string()], "the verdict is complete");
        assert_eq!(moved.names.len(), 2);
        assert_eq!(read_table(&db, Table::Credential).expect("read").into_map().len(), 2);
    }

    /// ⚠ **THE HALF-DONE STATE IS REPORTED, AND `credential` WINS.** A settings row rendering a
    /// name a live credential row still holds resolves to what the box resolved BEFORE the move —
    /// the only choice that cannot alter live behaviour while the tables disagree.
    #[test]
    fn a_collision_at_read_time_keeps_the_credential_value_and_says_so() {
        let (_d, db) = store(&[(D1, "from-credential")]);
        let (conn, _c, _v) = open_for_write(&db).expect("open");
        conn.execute(
            "INSERT INTO setting (section, key, value) VALUES ('config', ?1, ?2)",
            (KEY, "from-settings"),
        )
        .expect("insert");
        drop(conn);

        let folded = read_table_folded(&db, Table::Credential, &render).expect("fold");
        assert_eq!(folded.collisions, vec![D1.to_string()], "reported by name");
        let map = folded.map.into_map();
        assert_eq!(
            map.get(D1).map(String::as_str),
            Some("from-credential"),
            "the pre-move value wins while the tables disagree"
        );
        // The name with no credential row folds in normally — the move is HALF done, not undone.
        assert_eq!(map.get(D2).map(String::as_str), Some("from-settings"));
    }
}
