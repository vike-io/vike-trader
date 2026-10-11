//! The engine seam: reading and restoring a `sqlite_sequence` mark, and the naive rebuild whose
//! mark carry is the one decision under test.

use std::collections::BTreeSet;

use crate::support::Fixture;
use crate::support::sql::{columns, foreign_key_violations};
use rusqlite::{Connection, OptionalExtension};
use vike_secrets::{AccountEdit, DDL};

// -------------------------------------------------------------------------------------------
// The engine seam — the table this crate did not know existed
// -------------------------------------------------------------------------------------------

/// This table's `sqlite_sequence` high-water mark, or `None` when it has no row (or when no
/// `AUTOINCREMENT` table has ever existed in this database, in which case the table itself is
/// absent and a bare query would be a `no such table` error rather than an answer).
pub(super) fn mark(conn: &Connection, table: &str) -> Option<i64> {
    let present: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
            [],
            |r| r.get(0),
        )
        .expect("sqlite_master is always readable");
    if present == 0 {
        return None;
    }
    conn.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| r.get(0))
        .optional()
        .expect("reading a mark")
}

/// Put a mark back — the half a rebuild has to perform for itself.
pub(super) fn set_mark(conn: &Connection, table: &str, seq: i64) {
    conn.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table]).expect("clear the mark");
    conn.execute("INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)", (table, seq))
        .expect("restore the mark");
}

pub(super) fn max_id(conn: &Connection, table: &str) -> Option<i64> {
    conn.query_row(&format!("SELECT max(id) FROM {table}"), [], |r| r.get::<_, Option<i64>>(0))
        .expect("max id")
}

/// **Every table [`DDL`] gives a `venue_id INTEGER REFERENCES venue(id)` column** — `account`,
/// `credential`, `venue_setting` — checked for a row naming `venue_id`. Used to
/// turn *"nothing in this store points at it"* from a comment into an assertion: a roster reorder
/// that moved a REFERENCED venue to the top would otherwise make the DELETE below refuse (a
/// foreign-key question) rather than silently measure the wrong thing, but only if something
/// actually checks for it first.
pub(super) fn nothing_references_venue(conn: &Connection, venue_id: i64) -> bool {
    ["account", "credential", "venue_setting"].iter().all(|table| {
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE venue_id = ?1"),
                [venue_id],
                |r| r.get(0),
            )
            .expect("counting venue_id references");
        n == 0
    })
}

pub(super) fn table_exists(conn: &Connection, table: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .expect("sqlite_master is always readable");
    n > 0
}

pub(super) fn index_exists(conn: &Connection, index: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [index],
            |r| r.get(0),
        )
        .expect("sqlite_master is always readable");
    n > 0
}

/// Whether [`rebuild_preserving_ids`] carries the old mark across, which is the ONE difference
/// between a correct rebuild and §4.1's hazard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkCarry {
    /// The correct procedure.
    Carried,
    /// §4.1's hazard, spelled out: the rows are replayed with their ids and nothing else is done.
    Dropped,
}

/// **Rebuild `table` in place, preserving every surviving row's id** — the NAIVE shape: rename
/// the old table aside, re-run the shipped [`DDL`] to create the new one, copy the rows across,
/// drop the old one.
///
/// ⚠ **This is NOT the shape stage 4 used, and this doc used to claim it was.** The production
/// rebuild stage 4 shipped (`rebuild_table_from_ddl`, deleted since with every other in-place
/// upgrade step) built the NEW table BESIDE the old one under a scratch name instead, and renamed
/// `scratch -> {table}` at the end — the OPPOSITE direction — because Task 7 measured the
/// rename-aside shape corrupt one production rebuild (see the pragma note below) and inverted the
/// problem away rather than patching it. A future production rebuild should take that shape. This function keeps the naive, rename-aside shape deliberately: it is what makes
/// the pragma pair below load-bearing rather than tidy, and it is what this file's §4.1
/// measurement is about.
///
/// # ⚠ The pragma pair below — BOTH load-bearing, and MEASURED rather than argued
///
/// This attribution was written down three times in three different directions by careful reading,
/// each time confidently. It was settled on 2026-09-23 by running it: each pragma mutated out on a
/// throwaway branch against this file's own tests, plus the 2x2 matrix asked of the engine
/// directly. What follows is what the engine answered (SQLite 3.53.2, the `bundled`
/// `libsqlite3-sys` build this workspace links). Do not "correct" it from a doc page — re-measure.
///
/// **The starting state is not what it looks like.** [`open`] is a bare `Connection::open`, and a
/// bare connection HERE comes up with `foreign_keys = 1`, not `0`: the bundled build is compiled
/// with `SQLITE_DEFAULT_FOREIGN_KEYS` (it is listed in `PRAGMA compile_options`), which inverts
/// SQLite's own documented default. So neither statement below is a restatement of the default,
/// and an argument that starts "foreign keys are off anyway" starts from a false premise.
///
/// * `foreign_keys = OFF` — **required, and not because it suppresses the rewrite.** Two things
///   rest on it. (1) `DROP TABLE {scratch}` below performs an implicit `DELETE FROM`, which fires
///   every child row's foreign key: with this statement deleted, both of this function's callers
///   die at that line with `drop the old table: … FOREIGN KEY constraint failed` (extended code
///   787) — measured. (2) It is what makes the NEXT pragma effective at all; see the matrix.
/// * `legacy_alter_table = ON` — **this is the pragma that suppresses SQLite's rewrite** of every
///   OTHER table's `REFERENCES` clause to follow a rename, and it suppresses it only while
///   `foreign_keys` is OFF. With this statement deleted and `foreign_keys = OFF` kept, a
///   `REFERENCES account(id)` clause is rewritten to name the scratch table instead and survives
///   the scratch table's own DROP pointing at nothing: both callers redden on this function's
///   closing check, ``the rebuild of `venue` left dangling references`` with 3 of them and
///   ``the rebuild of `account` left dangling references`` with 5 — measured.
///
/// The matrix behind those two sentences, on the rename-ASIDE shape, `REFERENCES` rewritten?
///
/// | `foreign_keys` | `legacy_alter_table` | rewritten | `pragma_foreign_key_check` |
/// |---|---|---|---|
/// | OFF | OFF | **yes** | 1 |
/// | OFF | ON  | no      | 0 |
/// | ON  | OFF | **yes** | the DROP fails first |
/// | ON  | ON  | **yes** | the DROP fails first |
///
/// The last row is the one that makes `legacy_alter_table` a conditional cure rather than a cure:
/// with `foreign_keys` ON it does nothing at all.
///
/// # ⚠ A rename-ASIDE outside a transaction is a DIFFERENT HAZARD from a rename-INTO-PLACE inside
/// one — do not carry this pragma pair across the two
///
/// That is the whole reason this note is long. The beside-not-aside rebuild (stage 4's
/// `rebuild_table_from_ddl`, deleted since) renamed the SCRATCH into place, inside the caller's
/// transaction, on a connection whose `foreign_keys` was pinned ON at open. Nothing references the
/// scratch name, so there is no clause to rewrite — `legacy_alter_table` genuinely IS inert there,
/// measured: the child's `REFERENCES` clause comes out byte-identical under both settings. And such a
/// rebuild cannot reach for `foreign_keys = OFF` either, because `PRAGMA foreign_keys` is a
/// documented NO-OP inside a transaction — measured too: issued inside one it returns `Ok` and the
/// engine still answers `1`. What carries that shape is `defer_foreign_keys` plus building beside
/// rather than aside.
///
/// **The transplant is not hypothetical, and the matrix above explains it.** Task 7's first
/// rebuild attempt copied THIS pair into THAT shape: inside a transaction `foreign_keys = OFF` did
/// nothing, so the rename ran at the matrix's last row, where `legacy_alter_table = ON` also does
/// nothing — and its own `pragma_foreign_key_check` refused it, naming two dangling `credential`
/// rows. The lesson recorded from that failure was "`foreign_keys` is the pragma that suppresses",
/// which is this doc's third wrong direction. What actually happened is that a pair correct for one
/// shape was moved into a shape neither pragma can serve.
pub(super) fn rebuild_preserving_ids(conn: &Connection, table: &str, carry: MarkCarry) {
    let scratch = format!("{table}_rebuild_scratch");
    let before = mark(conn, table);

    conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")
        .expect("a rebuild suspends the constraints it is about to break");
    conn.execute_batch(&format!("ALTER TABLE {table} RENAME TO {scratch};"))
        .expect("rename the old table aside");
    conn.execute_batch(DDL).expect("re-create it from the shipped schema");

    // The columns the OLD table actually had, intersected with the new one's — a rebuild carries
    // what it can and never names a column one side does not have.
    let old: BTreeSet<String> = columns(conn, &scratch).into_iter().collect();
    let shared: Vec<String> =
        columns(conn, table).into_iter().filter(|c| old.contains(c)).collect();
    assert!(shared.iter().any(|c| c == "id"), "a rebuild of `{table}` must carry its ids");
    let list = shared.join(", ");
    conn.execute_batch(&format!("INSERT INTO {table} ({list}) SELECT {list} FROM {scratch};"))
        .expect("replay the surviving rows with their ids");

    // ⚠ THE STATEMENT §4.1 IS ABOUT. Dropping the scratch table deletes ITS `sqlite_sequence` row —
    // the one the rename moved the mark onto — so what the rebuilt table is left holding is the
    // mark the replay above created, i.e. `max(id)` of what survived.
    conn.execute_batch(&format!("DROP TABLE {scratch};")).expect("drop the old table");

    // ⚠ AND A SECOND `DDL` PASS, which is not belt-and-braces. A rename carries the old table's
    // NAMED INDEXES with it, so `account_one_account_per_book` was still attached to the scratch
    // table when the batch above ran — and `CREATE UNIQUE INDEX IF NOT EXISTS` saw that name
    // already taken and did nothing. The drop then took the index with the scratch table, leaving a
    // rebuilt table with no unique index on it and every statement still green.
    // ⚠ A beside-not-aside rebuild (stage 4's, deleted since) does NOT meet this trap: it builds the
    // new table under the scratch name (the opposite direction from this function), so the indexes
    // stay attached to the table being DROPPED — their names come free again, and a closing `DDL`
    // pass re-creates them on the table that now carries the real name.
    conn.execute_batch(DDL).expect("re-create the indexes the scratch table was holding");

    if carry == MarkCarry::Carried
        && let Some(seq) = before
    {
        set_mark(conn, table, seq);
    }

    let violations = foreign_key_violations(conn);
    assert_eq!(violations, 0, "the rebuild of `{table}` left dangling references");
    conn.execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")
        .expect("restore the constraints");
}

// -------------------------------------------------------------------------------------------
// Fixtures
// -------------------------------------------------------------------------------------------

/// Create an account through the REAL verb and hand back its id.
///
/// ⚠ **This is how *"remove the top account"* is spelled given `AccountEdit::Remove`'s refusal.**
/// `edit_account` refuses to delete a row that still owns live `credential` rows
/// (`DbErrorKind::AccountHasCredentials`, named by key), so every account this gate creates is
/// created by the LIFECYCLE verb and given no credential at all — `account_key_names` then answers
/// empty and the removal is permitted. The alternative (writing credentials and superseding them
/// first) would need whole credential key spellings this fixture has no other use for.
pub(super) fn create(fx: &Fixture, venue: &str, tier: &str, label: &str) -> i64 {
    fx.edit(AccountEdit::Create { venue, tier, label: Some(label) })
        .expect("create")
        .after
        .expect("a create leaves a row")
        .id
}
