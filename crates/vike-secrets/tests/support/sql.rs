//! Raw-SQL helpers for the tests that look past the public API at the database itself — and the
//! `AgedStore` fixture that plants a store at an OLDER shape for them to look at.
//!
//! Every function here was written two to four times, one copy per test file that needed to ask the
//! engine what shape a table is in: the `CREATE TABLE` text (`table_sql`), the column list
//! (`columns`), the named indexes (`indexes`), the whole-database referential check
//! (`foreign_key_violations`) and whether a table still carries the text `venue`
//! (`has_text_venue`). They ask the ENGINE rather than reading the DDL batch, which is the point of
//! each: the batch and the table are the same question only when a rebuild actually ran.
//!
//! ⚠ **Each takes a `&Connection`, not a fixture**, because the files that need them hold three
//! different fixture types (`support::Fixture`, `AgedStore` below, and their own). A connection is
//! what they all have in common, and `Fixture::conn()` / `AgedStore::conn()` are the one-line
//! doors to it.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// Open the database at `db`, expecting it to be there.
pub fn open(db: &Path) -> Connection {
    Connection::open(db).expect("open")
}

/// **Create `db` (and its directory) and lay `ddl` down on it** — the preamble in front of every
/// aged-store plant: the same two pragmas a real open sets (`journal_mode = DELETE`, the rollback
/// journal this crate pins, and `foreign_keys = ON`, which SQLite leaves off per connection), then
/// the batch. The connection comes back OPEN so the caller can plant rows on it and stamp
/// [`stamp_version`] before dropping it.
pub fn plant_ddl(db: &Path, ddl: &str) -> Connection {
    std::fs::create_dir_all(db.parent().expect("db parent")).expect("db dir");
    let conn = Connection::open(db).expect("create");
    conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA foreign_keys = ON;").expect("pragmas");
    conn.execute_batch(ddl).expect("the aged shape");
    conn
}

/// Stamp `PRAGMA user_version` — the schema version a planted store claims to be at.
pub fn stamp_version(conn: &Connection, version: i64) {
    conn.pragma_update(None, "user_version", version).expect("stamp");
}

/// The `PRAGMA user_version` the store carries.
pub fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("pragma")
}

/// The `CREATE TABLE` text the engine is holding for `table`. Panics if there is no such table —
/// every caller asks about one it just built.
pub fn table_sql(conn: &Connection, table: &str) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|e| panic!("`{table}` must exist: {e}"))
}

/// The statement the engine holds for ANY schema object called `name` — a table, but also an
/// index, whose `CREATE INDEX` text is the only place its key columns and predicate can be read.
pub fn object_sql(conn: &Connection, name: &str) -> String {
    conn.query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [name], |r| r.get(0))
        .unwrap_or_else(|e| panic!("`{name}`: {e}"))
}

/// `table`'s column names in declaration order, as the ENGINE answers rather than as the batch
/// reads.
pub fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})")).expect("prepare");
    let rows = stmt.query_map([], |r| r.get::<_, String>(1)).expect("query");
    rows.map(Result::unwrap).collect()
}

/// Whether `table` carries a column called `column` — asked of the engine, so a respelling of the
/// batch cannot fool it.
pub fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get::<_, i64>(0),
    )
    .expect("probe the table's columns")
        > 0
}

/// Whether `table` still carries the text `venue` column: `true` on an aged store, `false` once a
/// write has carried it onto the shipped shape, where `venue_id` stands alone (the venue-links
/// plan's second release took the text out of `account`, `credential` and `venue_setting`).
pub fn has_text_venue(conn: &Connection, table: &str) -> bool {
    has_column(conn, table, "venue")
}

/// Whether the engine declares `table.column` `NOT NULL` — the property a rebuild changes and a
/// statement's text only SUGGESTS. Panics if there is no such column.
pub fn is_not_null(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        &format!("SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n == 1)
    .unwrap_or_else(|e| panic!("`{table}.{column}`: {e}"))
}

/// The NAMED indexes the engine holds for `table`, sorted — the ones a statement declared. The
/// auto-index a table-level `UNIQUE` builds (`sqlite_autoindex_…`) is left out; ask
/// [`indexes_with_auto`] for it.
pub fn indexes(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("prepare");
    let rows = stmt.query_map([table], |r| r.get::<_, String>(0)).expect("query");
    rows.map(Result::unwrap).collect()
}

/// EVERY index the engine holds for `table` that carries a name, sorted — the auto-index a
/// table-level `UNIQUE` builds is named too (`sqlite_autoindex_…`), and a test that asks whether a
/// rebuild kept a `UNIQUE` has to see it.
pub fn indexes_with_auto(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 ORDER BY name",
        )
        .expect("prepare");
    let rows = stmt.query_map([table], |r| r.get::<_, String>(0)).expect("query");
    rows.map(Result::unwrap).collect()
}

/// The engine's own whole-database referential check: how many rows name a parent that is not
/// there.
pub fn foreign_key_violations(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
        .expect("fk check")
}

/// **A settings directory holding a store at an OLDER shape** — the fixture behind every test that
/// proves an in-place upgrade: a tempdir, `settings/`, and a database laid down by [`plant_ddl`]
/// from the DDL batch the test hands it (usually one `support::ddl` derived).
///
/// It deliberately knows nothing about ROWS: what a test plants on the aged shape is the premise of
/// that test, so [`AgedStore::plant`] hands the open connection back and the test inserts its own.
/// What it carries is the part every one of them wrote identically — the directory, the paths, the
/// connection door, and the engine-level questions this module's functions answer.
pub struct AgedStore {
    _dir: tempfile::TempDir,
    settings: PathBuf,
    db: PathBuf,
}

impl AgedStore {
    /// Lay `ddl` down in a fresh settings directory. The returned connection is open: plant rows,
    /// [`stamp_version`], then drop it before the store is handed to the code under test.
    pub fn plant(ddl: &str) -> (AgedStore, Connection) {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let db = vike_secrets::db_path_in(&settings);
        let conn = plant_ddl(&db, ddl);
        (AgedStore { _dir: dir, settings, db }, conn)
    }

    pub fn dir(&self) -> &Path {
        &self.settings
    }

    pub fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    pub fn db(&self) -> &Path {
        &self.db
    }

    pub fn conn(&self) -> Connection {
        open(&self.db)
    }

    /// [`table_sql`] over a fresh connection.
    pub fn table_sql(&self, table: &str) -> String {
        table_sql(&self.conn(), table)
    }

    /// [`columns`] over a fresh connection.
    pub fn columns(&self, table: &str) -> Vec<String> {
        columns(&self.conn(), table)
    }

    /// [`indexes`] over a fresh connection.
    pub fn indexes(&self, table: &str) -> Vec<String> {
        indexes(&self.conn(), table)
    }

    /// [`foreign_key_violations`] over a fresh connection.
    pub fn foreign_key_violations(&self) -> i64 {
        foreign_key_violations(&self.conn())
    }
}
