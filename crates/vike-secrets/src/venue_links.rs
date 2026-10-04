//! **The venue links' move onto `venue_id`** — the operator's deliberate door onto it
//! ([`apply_venue_links`], which `vike-cli config migrate-store` calls), and, behind
//! `test-support`, what a test needs in order to plant a store on the shape the release before the
//! move wrote and to read back what a carry moved.
//!
//! The move itself is `crate::schema::migrate_venue_links_onto_venue_id`, a pass of the write
//! funnel (`crate::db::ensure_venue_id_columns`): it rebuilds `account`, `venue_setting` and
//! `venue_arming` onto a `NOT NULL` `venue_id` at the first write a store meets.
//!
//! # Why the move has a door of its own
//!
//! Riding the funnel, the carry happens at whichever write a store meets first — days after a
//! release, perhaps, and at a moment nobody chose: a daemon's control-channel write, or a venue
//! rotating a credential the store must persist. When the funnel refuses the store (a row naming a
//! venue the roster lacks, a key two rows would share, a dangling reference), it refuses THAT write,
//! and for a rotated credential that write is the grant's only persistence. On a box whose daemon
//! cannot write `settings/`, an operator's process is the only one that can carry the store at all.
//! So [`apply_venue_links`] carries it on purpose, says that it did, says when there was nothing to
//! carry without opening the store for writing, and meets a refusal while the operator is at the
//! keyboard. (⚠ Until the venue-links plan's final fix wave `vike-cli config migrate-store` reported
//! a store "current" whenever decision 0095 had nothing to do, while all three tables could still
//! admit a NULL `venue_id`.)
//!
//! # ⚠ The second release's CONTRACTION goes through the same door
//!
//! The plan's second release drops the text `venue` from `account`, `credential` and
//! `venue_setting` (`crate::schema`'s `DROPPED_COLUMNS`, delivered by the same funnel). That is a
//! one-way step with a refusal of its own, so the reasons above apply to it unchanged: riding the
//! funnel alone, a store the first release carried would be contracted at whatever write came next
//! — possibly a rotated credential's only persistence — and while it read "already carried" here a
//! store the drop refuses would have been reported as finished. So the probe counts a store as
//! having something to do while EITHER is owed: a linked table whose `venue_id` is missing or
//! nullable, or a dropped text `venue` still held (`crate::schema::text_venue_tables`), and
//! [`VenueLinks::Carried`] says which of the two this run did. (⚠ Until the second release's first
//! fix round it asked only the first, and a first-release store answered "already carried".)

use std::path::Path;

use crate::db::{DbError, DbErrorKind};

/// What [`apply_venue_links`] found, and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueLinks {
    /// No settings database on this box.
    NoDatabase,
    /// A database at a schema older than the `account` table (`crate::ACCOUNT_TABLE_SCHEMA`) —
    /// READABLE by design (`crate::READABLE_SCHEMA_VERSIONS`), and with no linked table to carry.
    /// Nothing was opened for writing. ⚠ The write funnel cannot run on such a store at all: its
    /// `DDL` batch fails to prepare against a schema-1 `credential`
    /// (`crate::db::ensure_venue_id_columns`' doc names that defect), so this is answered before
    /// any write is tried. `vike-cli secrets migrate` brings the store to the current schema.
    OlderSchema {
        /// The schema the database carries, as `PRAGMA user_version` reported it.
        found: i64,
    },
    /// Nothing is owed: every linked table this store holds reads its venue by a `NOT NULL`
    /// `venue_id`, and no table the plan's second release contracts still holds its text `venue`.
    /// Found by the READ-ONLY probe, which opens nothing for writing — or, when another writer
    /// carried the store between that probe and [`apply_venue_links`]' own write transaction, by
    /// the same probe asked again inside that transaction, which is then dropped with nothing
    /// committed. (⚠ Until the venue-links plan's final fix wave this said "nothing was opened for
    /// writing", which that second path does not keep.)
    ///
    /// ⚠ It describes the store as this call found it, not what the calling PROCESS did: when
    /// decision 0095's migration was pending, its transaction
    /// (`crate::live_means_mainnet::apply_live_means_mainnet`, which a booting root runs before any
    /// verb) ran the write funnel — which carries and contracts the store — before this call asked.
    AlreadyCarried,
    /// The store was carried and/or contracted now, in ONE transaction of the write funnel every
    /// writer runs, and that transaction committed. Both lists are what the probe found OWED on
    /// that transaction before the funnel ran, and the probe found nothing owed after it.
    Carried {
        /// The linked tables whose `venue_id` was missing or admitted NULL — carried onto the
        /// number. Empty for a store the first release had already carried.
        onto_venue_id: Vec<&'static str>,
        /// The tables whose text `venue` column was dropped. Empty for a store already contracted
        /// (which a store the first release never carried cannot be).
        text_dropped_from: Vec<&'static str>,
    },
}

/// What [`pending`] found owed: `(the linked tables to carry onto venue_id, the tables still holding
/// a dropped text venue)`.
type Owed = (Vec<&'static str>, Vec<&'static str>);

/// **What this store still owes the venue-links plan**, READ-ONLY: the linked tables to carry onto
/// `venue_id` ([`uncarried_tables`]) and the tables still holding the text `venue` the second release
/// drops (`crate::schema::text_venue_tables`, the drop pass's own spelling). Nothing is owed when
/// both are empty.
fn pending(conn: &rusqlite::Connection) -> rusqlite::Result<Owed> {
    Ok((uncarried_tables(conn)?, crate::schema::text_venue_tables(conn)?))
}

/// Each linked table this store HOLDS whose venue link is not yet a `NOT NULL` `venue_id` — the
/// column missing (a store older than stage 2) or admitting NULL (the shape the release before the
/// move wrote). READ-ONLY, and asked of `pragma_table_info`, so it is the table the engine holds
/// rather than the statement that created it.
///
/// The tables are the pass's own (`crate::schema::VENUE_LINK_TABLES`), so this probe and the pass
/// cannot disagree about what a carry is. A table the store does not hold has nothing to carry: the
/// funnel's `DDL` batch creates it on the shipped shape whenever a writer first needs it.
///
/// The nullability is `crate::schema::column_is_nullable`'s, the one spelling the passes ask too
/// (this probe spelled its own `pragma_table_info` query until review).
fn uncarried_tables(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<&'static str>> {
    let mut out = Vec::new();
    for table in crate::schema::VENUE_LINK_TABLES {
        let held: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )?;
        if held == 0 {
            continue;
        }
        if !crate::settings::has_column(conn, table, "venue_id")?
            || crate::schema::column_is_nullable(conn, table, "venue_id")?
        {
            out.push(table);
        }
    }
    Ok(out)
}

/// **Carry the store's venue links onto `venue_id`, and drop the text column the second release
/// drops, ON PURPOSE** — the moves the next write would otherwise make at a moment nobody chose
/// (see this module's doc).
///
/// Shaped like `crate::live_means_mainnet::apply_live_means_mainnet`: it probes READ-ONLY first, so
/// a store with nothing owed is never opened for writing — a binary whose sandbox cannot write
/// `settings/db` can still ask — and when something is owed it runs ONE transaction of the write
/// funnel (`crate::db::ensure_venue_id_columns`), the same path every writer runs, so the carry, the
/// contraction and their refusals are exactly the ones a write would meet. Nothing is journalled:
/// both change the store's SHAPE and no setting's value.
///
/// ⚠ **[`VenueLinks::Carried`] is MEASURED, not assumed.** The probe runs again on the transaction
/// before the funnel — another writer may have carried the store between the read-only probe and
/// this transaction, and then the answer is [`VenueLinks::AlreadyCarried`] with nothing committed —
/// and once more after it, before the commit: a pass that declined a table, or anything else that
/// left a linked table uncarried or a dropped text column in place, refuses rather than commits a
/// store the verb would then call finished. (Until review it reported `Carried` for whatever the
/// funnel did.)
///
/// # Errors
/// A store that cannot be read, or — when something is owed — cannot be written; a database that
/// vanished between the probe and the write ([`DbErrorKind::VanishedDatabase`], and the database
/// this call had begun to create is removed again); [`DbErrorKind::RepairRefused`] when the funnel
/// refused the store by name (trap 7, trap 5, a dangling reference, or the drop pass's refusal of a
/// text venue that has no number), with the refusal's own text, exactly as every writer reports it;
/// and an engine-kind error naming the tables when the funnel ran and left something owed. Nothing is
/// committed on any of them.
pub fn apply_venue_links(settings_dir: &Path) -> Result<VenueLinks, DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    if !crate::db::database_present(&db) {
        return Ok(VenueLinks::NoDatabase);
    }
    {
        let (conn, version) = crate::db::open_for_read(&db)?;
        if version < crate::db::ACCOUNT_TABLE_SCHEMA {
            return Ok(VenueLinks::OlderSchema { found: version });
        }
        let (uncarried, text_held) = pending(&conn).map_err(|e| DbError::sql(&db, e))?;
        if uncarried.is_empty() && text_held.is_empty() {
            return Ok(VenueLinks::AlreadyCarried);
        }
    }
    let (mut conn, created, _version) = crate::db::open_for_write(&db)?;
    // ⚠ `created` is true only if the file was REMOVED between the probe above and this open, and
    // then the only safe act is to put back what was found: no database. A store this call created
    // would make `crate::store::Backend` answer `Database` for every process on the box and retire
    // every key in the credential file beside it — `crate::db`'s `upsert_rows` argues it at length.
    // Close the engine BEFORE unlinking: an open handle keeps the file alive on Windows.
    if created {
        drop(conn);
        let _ = std::fs::remove_file(&db);
        return Err(DbError { path: db, kind: DbErrorKind::VanishedDatabase });
    }
    // IMMEDIATE, not the default DEFERRED: the funnel reads the store's shape before it writes, the
    // read-then-write shape `crate::db::BUSY_TIMEOUT`'s own doc names as the case a busy timeout
    // cannot cover. `apply_live_means_mainnet` argues the same.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(&db, e))?;
    // The probe again, now that this transaction holds the store: another writer may have carried
    // it since the read-only look. Dropping the transaction commits nothing.
    let (onto_venue_id, text_dropped_from) = pending(&tx).map_err(|e| DbError::sql(&db, e))?;
    if onto_venue_id.is_empty() && text_dropped_from.is_empty() {
        return Ok(VenueLinks::AlreadyCarried);
    }
    // A NAMED refusal comes back `DbErrorKind::RepairRefused` through `DbError::sql`, as at every
    // other door; the dropped transaction rolls back everything the funnel did before it refused.
    crate::db::ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(&db, e))?;
    // …and once more before the commit, so `Carried` is what the store now holds.
    let (uncarried, text_held) = pending(&tx).map_err(|e| DbError::sql(&db, e))?;
    if !uncarried.is_empty() || !text_held.is_empty() {
        let e = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the venue links' carry ran and left [{}] with a `venue_id` that is missing or \
                 admits NULL and [{}] still holding the text `venue` it drops; nothing was \
                 committed",
                uncarried.join(", "),
                text_held.join(", ")
            )),
        );
        return Err(DbError::sql(&db, e));
    }
    tx.commit().map_err(|e| DbError::sql(&db, e))?;
    Ok(VenueLinks::Carried { onto_venue_id, text_dropped_from })
}

/// The shipped `DDL` of the venue-links plan's first release, frozen: the shape every aging helper
/// here derives from, because the second release removed the text columns those shapes carry.
///
/// ⚠ **History, so it does not move.** [`pre_venue_link_ddl`] used to derive its batch from the
/// SHIPPED `crate::schema::DDL`, and so did every aging helper built on it (the pre-step-7,
/// pre-rename and pre-stage-2 shapes in `crates/vike-secrets/tests/support/mod.rs` and the tests
/// that age further on top). Once the second release took the text `venue` out of `account`,
/// `credential` and `venue_setting`, the shipped batch no longer held the lines those helpers swap
/// back, and an old shape cannot be derived from a batch that has forgotten it. So the batch they
/// derive from is this one: copied verbatim from `crate::schema::DDL` as it stood before the
/// contraction, which was byte-for-byte v0.1.41's `DDL` (MEASURED against that tag, 2026-10-03).
/// Never edit it to follow the shipped batch; a test that needs today's shape reads `DDL`.
#[cfg(feature = "test-support")]
pub const RELEASE_1_DDL: &str = "\
CREATE TABLE IF NOT EXISTS node_key (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL UNIQUE,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS venue (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL UNIQUE,
    title TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS account (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    venue            TEXT    NOT NULL,
    venue_id         INTEGER NOT NULL REFERENCES venue(id),
    tier             TEXT    NOT NULL,
    armed            INTEGER NOT NULL DEFAULT 0,
    label            TEXT,
    venue_account_id TEXT,
    parent_id        INTEGER REFERENCES account(id),
    active           INTEGER NOT NULL DEFAULT 1,
    last_verified_at TEXT,
    notes            TEXT,
    UNIQUE (venue, tier, label),
    UNIQUE (venue_id, tier, label),
    CHECK (tier IN ('paper', 'demo', 'live')),
    CHECK (armed IN (0, 1)),
    CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS account_one_account_per_book
    ON account (venue_id, venue_account_id)
    WHERE active = 1 AND venue_account_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS credential (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER REFERENCES account(id),
    venue         TEXT,
    venue_id      INTEGER REFERENCES venue(id),
    field         TEXT    NOT NULL,
    value         TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    secret        INTEGER NOT NULL DEFAULT 1,
    superseded_at TEXT,
    notes         TEXT,
    CHECK (account_id IS NULL OR venue IS NULL),
    CHECK (secret IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_value
    ON credential (account_id, field) WHERE superseded_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_name
    ON credential (name) WHERE superseded_at IS NULL;

CREATE TABLE IF NOT EXISTS venue_setting (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    venue    TEXT NOT NULL,
    venue_id INTEGER NOT NULL REFERENCES venue(id),
    tier     TEXT NOT NULL,
    field    TEXT NOT NULL,
    value    TEXT NOT NULL,
    notes    TEXT,
    UNIQUE (venue, tier, field),
    UNIQUE (venue_id, tier, field),
    CHECK (tier IN ('any', 'paper', 'demo', 'live'))
) STRICT;

CREATE TABLE IF NOT EXISTS setting (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    section TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (section, key),
    CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
) STRICT;

CREATE TABLE IF NOT EXISTS venue_arming (
    id       INTEGER PRIMARY KEY,
    venue    TEXT NOT NULL,
    venue_id INTEGER NOT NULL REFERENCES venue(id),
    label    TEXT,
    mode     TEXT NOT NULL,
    max_exposure REAL,
    CHECK (mode IN ('paper', 'demo', 'live')),
    CHECK (max_exposure IS NULL OR max_exposure > 0.0)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_venue
    ON venue_arming (venue_id) WHERE label IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_account
    ON venue_arming (venue_id, label) WHERE label IS NOT NULL;

CREATE TABLE IF NOT EXISTS settings_adoption (
    id              INTEGER PRIMARY KEY,
    adopted_at      TEXT    NOT NULL,
    tool_version    TEXT    NOT NULL,
    files_present   TEXT    NOT NULL,
    venues_declared INTEGER NOT NULL,
    setting_rows    INTEGER NOT NULL,
    arming_rows     INTEGER NOT NULL,
    notes           TEXT,
    CHECK (id = 1),
    CHECK (venues_declared IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS profile_risk (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    profile TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (profile, key)
) STRICT;
";

/// **[`RELEASE_1_DDL`] with the venue-links flip reverted, and `venue.title` with it**: `venue_id`
/// nullable in `account`, `venue_setting` and `venue_arming`; no `venue_id` uniqueness; the book
/// index and both arming indexes keyed on the text `venue`; and the `venue` table without the
/// `title` column the same release added (#2370). Byte-for-byte v0.1.40's `DDL`, the batch the
/// release before the flip shipped — MEASURED on 2026-10-03 by applying these swaps to the shipped
/// literal and comparing it with that tag's, string for string. (⚠ It derived from the SHIPPED
/// `crate::schema::DDL` until the plan's second release removed the text columns from it; its base
/// is the frozen first-release batch since, and its output is unchanged.)
///
/// ⚠ It is a BATCH, not a copy of any store: a store's tables carry the shape of whichever release
/// created them plus every `ALTER` since, so the live boxes' stores match it in what the flip reads
/// (`venue_id` nullable, the text-keyed indexes, no `title`) and may differ in physical column
/// order.
///
/// ⚠ **The `title` swap is what makes a carry test meet `crate::db`'s `ensure_venue_rows` title
/// `ALTER`** inside the carry transaction, as every real release-before store does. Until review it
/// was missing, so this called itself byte-for-byte the release-before batch while every store it
/// planted already had a column v0.1.40 never wrote.
///
/// ⚠ **Derived rather than transcribed**: a test that plants its own guess proves the migration
/// against a table nobody ever shipped. Each replacement must match EXACTLY ONCE, so a respelling of
/// the frozen batch fails here instead of leaving a fixture that quietly stopped aging anything.
///
/// ⚠ **The one copy of this derivation.** It lived in `crates/vike-secrets/tests/support/mod.rs`
/// until a test in ANOTHER crate needed a store on this shape (`vike-boot`'s boot through a repair
/// refusal, and `vike-cli`'s `config migrate-store`), and a test directory is visible to no other
/// crate. It moved here behind `test-support` rather than being copied.
///
/// # Panics
/// When the frozen batch no longer spells a reverted line exactly once — a fixture, not a
/// production path.
#[cfg(feature = "test-support")]
#[must_use]
pub fn pre_venue_link_ddl() -> String {
    let swaps: [(&str, &str); 8] = [
        (
            "    id    INTEGER PRIMARY KEY AUTOINCREMENT,\n    name  TEXT NOT NULL UNIQUE,\n    \
             title TEXT\n",
            "    id   INTEGER PRIMARY KEY AUTOINCREMENT,\n    name TEXT NOT NULL UNIQUE\n",
        ),
        (
            "    venue_id         INTEGER NOT NULL REFERENCES venue(id),\n",
            "    venue_id         INTEGER REFERENCES venue(id),\n",
        ),
        ("    UNIQUE (venue_id, tier, label),\n", ""),
        (
            "    ON account (venue_id, venue_account_id)\n",
            "    ON account (venue, venue_account_id)\n",
        ),
        (
            "    venue_id INTEGER NOT NULL REFERENCES venue(id),\n    tier     TEXT NOT NULL,\n",
            "    venue_id INTEGER REFERENCES venue(id),\n    tier     TEXT NOT NULL,\n",
        ),
        ("    UNIQUE (venue_id, tier, field),\n", ""),
        (
            "    venue_id INTEGER NOT NULL REFERENCES venue(id),\n    label    TEXT,\n",
            "    venue_id INTEGER REFERENCES venue(id),\n    label    TEXT,\n",
        ),
        (
            "    ON venue_arming (venue_id) WHERE label IS NULL;\n",
            "    ON venue_arming (venue) WHERE label IS NULL;\n",
        ),
    ];
    let mut ddl = RELEASE_1_DDL.to_string();
    for (new, old) in swaps {
        assert_eq!(ddl.matches(new).count(), 1, "the frozen batch must spell {new:?} exactly once");
        ddl = ddl.replacen(new, old, 1);
    }
    let account_index = "    ON venue_arming (venue_id, label) WHERE label IS NOT NULL;\n";
    assert_eq!(ddl.matches(account_index).count(), 1);
    ddl.replacen(account_index, "    ON venue_arming (venue, label) WHERE label IS NOT NULL;\n", 1)
}

/// **Plant a settings store on the shape the release before the venue flip wrote** —
/// [`pre_venue_link_ddl`], the whole roster in `venue` (ids in `vike_model::VENUES` order),
/// the caller's `rows` (raw SQL, run with foreign keys ON), decision 0095's marker, and the schema
/// stamp — at `<settings_dir>/db/vike.db`, which must not exist yet.
///
/// ⚠ **It comes back MARKED for decision 0095**, because every store a release carrying 0095 has
/// written is: the funnel writes the marker at the first write whether or not anything was
/// rewritten. A test that needs 0095 PENDING calls
/// [`crate::live_means_mainnet::unmark_live_means_mainnet`] after this.
///
/// For a test in a crate that cannot name `rusqlite` — which is every crate above this one.
///
/// # Panics
/// On any store error — a fixture, not a production path.
#[cfg(feature = "test-support")]
pub fn plant_pre_venue_link_store(settings_dir: &Path, rows: &str) {
    plant_store_on(settings_dir, &pre_venue_link_ddl(), rows);
}

/// **Plant a settings store on the shape the plan's FIRST release carried a store onto** —
/// [`RELEASE_1_DDL`] (links by `venue_id`, `NOT NULL`, with the text `venue` still beside them), and
/// otherwise exactly what [`plant_pre_venue_link_store`] plants: the whole roster, the caller's
/// `rows`, decision 0095's marker and the schema stamp. It is the shape both live boxes hold until
/// the second release's first write, and the one `vike-cli config migrate-store` CONTRACTS.
///
/// # Panics
/// On any store error — a fixture, not a production path.
#[cfg(feature = "test-support")]
pub fn plant_release_1_store(settings_dir: &Path, rows: &str) {
    plant_store_on(settings_dir, RELEASE_1_DDL, rows);
}

/// The one opener behind both planters: create `<settings_dir>/db/vike.db` on `ddl`, file the roster
/// (ids in `vike_model::VENUES` order), run `rows` with foreign keys ON, mark decision 0095
/// and stamp the schema. ⚠ `crates/vike-ops/tests/credential_source_roster_gate.rs` pins this
/// function as the module's one store-opening site; the planters above it open nothing themselves.
#[cfg(feature = "test-support")]
fn plant_store_on(settings_dir: &Path, ddl: &str, rows: &str) {
    let db = crate::dotenv::db_path_in(settings_dir);
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).expect("the database's directory");
    }
    let conn = rusqlite::Connection::open(&db).expect("create the settings database");
    conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA foreign_keys = ON;")
        .expect("the two pragmas every open of this store sets");
    conn.execute_batch(ddl).expect("the shape being planted");
    {
        let mut roster =
            conn.prepare("INSERT INTO venue (name) VALUES (?1)").expect("the roster's insert");
        for venue in vike_model::VENUES {
            roster.execute([*venue]).expect("one roster venue");
        }
    }
    conn.execute_batch(rows).expect("the caller's rows");
    crate::live_means_mainnet::plant_marker(&conn).expect("decision 0095's marker");
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).expect("the schema stamp");
}

/// **What a carry may and may not move, read raw** — for a test that has to compare a store before
/// and after one, from a crate that cannot name `rusqlite`.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSnapshot {
    /// Each of `account`, `venue_setting` and `venue_arming` whose `venue_id` the ENGINE still lets
    /// hold a NULL — asked of `pragma_table_info`, so it is the table the engine holds and not the
    /// statement that created it.
    pub nullable: Vec<String>,
    /// `(table, id, venue_id)` for every row of those three tables, in that table order and then by
    /// id. `venue_id` reads `None` for SQL NULL, and for a table that has no such column.
    pub rows: Vec<(String, i64, Option<i64>)>,
    /// Every `sqlite_sequence` mark, `(table, seq)`, by table name — empty for a store that arms no
    /// table with `AUTOINCREMENT`.
    pub marks: Vec<(String, i64)>,
    /// Each of `account`, `credential`, `venue_setting` and `venue_arming` that still carries a text
    /// `venue` column, in that order — all four before the plan's second release contracts a store,
    /// `venue_arming` alone after it (that table keeps its text until Plan B deletes it).
    pub text_venue: Vec<String>,
    /// `(id, account_id, venue_id)` for every `credential` row, by id — the filing a contraction
    /// REBUILDS that table under, and which it may not move: an account-scoped row keeps its
    /// account, a venue-scoped one its venue's number, an infrastructure row neither. `venue_id`
    /// reads `None` for SQL NULL and for a table that has no such column. Kept apart from
    /// [`LinkSnapshot::rows`], which carries no `account_id`. (⚠ Until the venue-links plan's final
    /// fix wave this snapshot left `credential` out, so a test driving the contraction through
    /// `vike-cli config migrate-store` planted credential rows and never compared them.)
    pub credentials: Vec<(i64, Option<i64>, Option<i64>)>,
}

/// Read a [`LinkSnapshot`] of the store beside `settings_dir`, READ-ONLY.
///
/// The three tables are spelled here rather than taken from the pass's own list, so a test built on
/// this measures the same tables whatever that list says.
///
/// # Panics
/// On any store error — a fixture, not a production path.
#[cfg(feature = "test-support")]
#[must_use]
pub fn link_snapshot(settings_dir: &Path) -> LinkSnapshot {
    use rusqlite::OptionalExtension as _;
    let db = crate::dotenv::db_path_in(settings_dir);
    let (conn, _version) = crate::db::open_for_read(&db).expect("open the settings database");
    let mut nullable = Vec::new();
    let mut rows = Vec::new();
    for table in ["account", "venue_setting", "venue_arming"] {
        let notnull: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = 'venue_id'"
                ),
                [],
                |r| r.get(0),
            )
            .optional()
            .expect("the column's nullability");
        if notnull == Some(0) {
            nullable.push(table.to_string());
        }
        let column = if notnull.is_some() { "venue_id" } else { "NULL" };
        let mut stmt = conn
            .prepare(&format!("SELECT id, {column} FROM {table} ORDER BY id"))
            .expect("the rows' select");
        let found = stmt
            .query_map([], |r| Ok((table.to_string(), r.get::<_, i64>(0)?, r.get(1)?)))
            .expect("the rows");
        for row in found {
            rows.push(row.expect("one row"));
        }
    }
    let armed: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
            [],
            |r| r.get(0),
        )
        .expect("the sequence table's presence");
    let mut marks = Vec::new();
    if armed > 0 {
        let mut stmt =
            conn.prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name").expect("the marks");
        let found = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("the marks");
        for mark in found {
            marks.push(mark.expect("one mark"));
        }
    }
    let mut text_venue = Vec::new();
    for table in ["account", "credential", "venue_setting", "venue_arming"] {
        if crate::settings::has_column(&conn, table, "venue").expect("the table's columns") {
            text_venue.push(table.to_string());
        }
    }
    let numbered =
        crate::settings::has_column(&conn, "credential", "venue_id").expect("the columns");
    let column = if numbered { "venue_id" } else { "NULL" };
    let mut stmt = conn
        .prepare(&format!("SELECT id, account_id, {column} FROM credential ORDER BY id"))
        .expect("the credential rows' select");
    let found = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))
        .expect("the credential rows");
    let mut credentials = Vec::new();
    for row in found {
        credentials.push(row.expect("one credential row"));
    }
    LinkSnapshot { nullable, rows, marks, text_venue, credentials }
}
