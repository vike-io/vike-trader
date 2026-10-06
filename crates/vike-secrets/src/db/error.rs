//! The error vocabulary: `Table`, `DbError`, `DbErrorKind` (every refusal) and their conversions.

use super::open::BUSY_TIMEOUT;
use super::*;

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
    pub(super) fn other(self) -> Table {
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
    /// **The store's REPAIR refused it BY NAME** — `ensure_venue_id_columns`, the funnel that
    /// carries an existing store onto this binary's shape before any write lands, met rows only a
    /// human can repair, and the whole transaction was rolled back with it: trap 5 (rows a
    /// `venue_setting` key would share once carried), trap 7 (rows naming a venue the roster does
    /// not hold) or a rebuild that left a dangling reference (`crate::schema`'s
    /// `rebuild_table_from_ddl` carries all three). The engine error it arrived in is kept as the
    /// `source()`, and its message names the rows and the SQLite-client repair, because no
    /// `vike-cli` verb can make one — every one of them runs this same funnel.
    ///
    /// ⚠ **Built by [`DbError::sql`], so EVERY writer reaches it** — a credential write, an account
    /// edit, a venue setting, a settings row, a mirror, `vike-cli config migrate-store`, and
    /// decision 0095's boot path alike — and ONLY for those three refusals: they travel inside the
    /// engine error as `crate::schema::NamedRefusal`, and `sql` asks every error whether it
    /// carries one. Any OTHER failure inside the funnel — a full disk, a read-only store, a corrupt
    /// page, a constraint the copy itself met — stays [`DbErrorKind::Sqlite`], on the boot path
    /// too. ⚠ Until the final fix wave of the venue-links plan it was the reverse on both counts:
    /// the boot path alone classified the funnel's failures, and it classified ALL of them, a full
    /// disk included, while every other writer reported a named refusal as a store that *could
    /// not be read*.
    ///
    /// What it tells a booting root apart from: [`crate::live_means_mainnet::apply_live_means_mainnet`]
    /// runs decision 0095's ceiling rewrite FIRST and then this funnel, in one transaction, so a
    /// boot can fail for two unrelated reasons — 0095's own step (a store this process cannot
    /// write, say), whose repair IS `vike-cli config migrate-store` run as the owner, and this
    /// refusal, which that verb meets the same way. `vike_boot`'s ceiling step passes this one
    /// through without the 0095 framing.
    RepairRefused(rusqlite::Error),
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
    /// `vike_dukascopy`'s `resolve_account` matches a policy address against
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
    /// [`AccountEdit::SetTier`] was given a word that is no [`crate::schema::ACCOUNT_TIERS`]
    /// member — including the retired `sim` spelling.
    ///
    /// ⚠ **It does not echo the word**, the same rule [`DbErrorKind::AccountLabelMalformed`] obeys
    /// and for that variant's reason verbatim: on the surfaces that reach an account verb the flag
    /// beside this one carries a credential VALUE, so a refusal that quoted what it was handed
    /// would be the one channel here that prints an operator's secret back at them. There are only
    /// three legal words and the message names all three, which is what the operator actually
    /// needs. An operator-facing caller validates first and its own refusal may echo, because there
    /// the word came off the command line the human is still looking at.
    AccountTierUnknown,
    /// [`AccountEdit::SetTier`] was asked to move a row whose credential keys SPELL a different
    /// tier.
    ///
    /// ⚠ **The move would create a SECOND account row the next time one of those keys is
    /// written** — [`DbErrorKind::AccountKeysPinTheLabel`]'s mechanism, one column along.
    /// `crate::schema::AccountResolver` finds an account from the key NAME, nothing here rewrites a
    /// credential name, and nothing in this workspace may.
    ///
    /// ⚠ **The comparison runs in ONE vocabulary and has to.** `account.tier` is an
    /// [`crate::schema::ACCOUNT_TIERS`] spelling (`paper` | `demo` | `live`, lowercase) while a key
    /// name carries a `vike_model::credential_keys::CREDENTIAL_TIERS` token (`SIM` | `DEMO` |
    /// `LIVE`, uppercase, with the legacy `MAINNET` normalizing onto `LIVE`). Compared as they
    /// stand the two never match, so the guard would be an assertion that cannot fire for its
    /// stated reason. [`crate::schema::account_tier_named`] is the bridge, and it is what makes
    /// `paper` work at all: the token is `SIM`, so a rule that uppercased the account tier and
    /// hunted for `_PAPER_` would find nothing and silently permit every move on and off `paper`.
    ///
    /// ⚠ **DECLARED RESIDUAL: a key whose name spells NO tier this workspace can classify pins
    /// nothing.** Aster's `TESTNET` token is outside `CREDENTIAL_TIERS` and outside
    /// `crate::venue_setting::HAND_MAPPED_ACCOUNTS`, so an aster row moves freely however its keys
    /// are spelled. Refusing on evidence that does not exist is the alternative, and it would
    /// refuse a legitimate move for every unclassifiable family at once. The hand-mapped families
    /// (alpaca's `SANDBOX`, dukascopy's `DEMO1`/`DEMO2`, the `POLY_` trio) ARE classified and DO
    /// pin, because that table answers with an `ACCOUNT_TIERS` word directly.
    AccountKeysPinTheTier {
        /// The row.
        id: i64,
        /// The tier its keys spell — an [`crate::schema::ACCOUNT_TIERS`] word, never a key token.
        tier: String,
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
            // ⚠ It does not say that nothing was written: the refusal it carries says so, once.
            DbErrorKind::RepairRefused(e) => write!(
                f,
                "settings database {p} could not be carried onto the shape this binary writes, so \
                 the write was refused: {e}"
            ),
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
                 nothing to seal here: this box resolves every setting from zero rows — no \
                 ceiling, no dead-man, and every venue capped `paper` with nothing recording that \
                 an arming was ever stated — until the first `vike-cli config set` lands a row. \
                 NOTHING WAS WRITTEN."
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
                 A-Z and 0-9 only, at most {MAX_LABEL_LEN} characters, and never \
                 `{RESERVED_DEFAULT_LABEL}` — that is the account an UNLABELLED key already \
                 addresses, so a row carrying it would be a second answer to one question. It is \
                 case-SENSITIVE and deliberately not repaired: a label this store uppercased on \
                 your behalf is a label that then does not match what you wrote in the \
                 `policy.accounts.<venue>.<LABEL>` key. The value is deliberately not echoed \
                 here — the flag beside this one carries a credential."
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
                 WRITTEN — a label is how a `policy.accounts.<venue>.<LABEL>` key addresses an \
                 account, so two rows may not answer to one. Pick another label, or edit account \
                 {holder}."
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
            DbErrorKind::AccountTierUnknown => write!(
                f,
                "that is not a tier of {p}: an account tier is one of {}. NOTHING WAS WRITTEN, and \
                 what you typed is deliberately not quoted back here — the flag beside this one \
                 carries a credential VALUE, and a refusal that echoed its argument would be the \
                 one channel on this surface that prints a secret. ⚠ The retired `sim` spelling is \
                 refused too: it became `paper` and a migrated store's own CHECK no longer admits \
                 it.",
                crate::schema::ACCOUNT_TIERS.join(" | ")
            ),
            DbErrorKind::AccountKeysPinTheTier { id, tier, keys } => write!(
                f,
                "account {id} in {p} owns credential keys that SPELL the `{tier}` tier: {}. \
                 NOTHING WAS WRITTEN. Moving the row would not rename the keys — nothing in this \
                 workspace rewrites a credential store wholesale — and the classifier would then \
                 read `{tier}` out of those names again and CREATE A SECOND ACCOUNT the next time \
                 one of them is written. Key NAMES only are shown; no value was read. A row with \
                 no credentials moves freely, which is the case this verb exists for: an account \
                 added at the wrong --tier, before any key was filed against it. To move one that \
                 HAS keys, the keys are what move — write them under the new tier's names and \
                 retire the old ones.",
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
            DbErrorKind::Sqlite(e) | DbErrorKind::RepairRefused(e) => Some(e),
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
    ///
    /// **A refusal the store's REPAIR names.** The write funnel's three named refusals (trap 5,
    /// trap 7, a dangling reference) travel inside the engine error as
    /// `crate::schema::NamedRefusal`, and every caller of the funnel maps its error through here,
    /// so each of them answers [`DbErrorKind::RepairRefused`] for exactly those three — and a bare
    /// engine failure inside the same funnel stays [`DbErrorKind::Sqlite`], whichever door it came
    /// through. Until the venue-links plan's final fix wave every writer but one reported a refused
    /// WRITE as a store that *could not be read*, which points an operator at the file's
    /// permissions.
    pub(crate) fn sql(path: &Path, e: rusqlite::Error) -> Self {
        let named = |kind| DbError { path: path.to_path_buf(), kind };
        if crate::schema::NamedRefusal::is(&e) {
            return named(DbErrorKind::RepairRefused(e));
        }
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
