//! The reshape: `reshape_into` carries a schema-1 `credential` table into schema 2, IN PLACE.

use std::collections::BTreeMap;

use rusqlite::Transaction;

use super::*;

/// The schema-1 `credential` table, re-created by [`reshape_into`] under a scratch name so the
/// rebuild is a copy rather than an in-place `ALTER`. SQLite cannot add or drop a PRIMARY KEY with
/// `ALTER TABLE`, and schema 1's `name TEXT PRIMARY KEY` has to become schema 2's surrogate `id`.
const RESHAPE_SCRATCH: &str = "credential_schema1";

// ---------------------------------------------------------------------------------------------
// The reshape
// ---------------------------------------------------------------------------------------------

/// **Carry a schema-1 `credential` table into schema 2, IN PLACE, inside the caller's transaction.**
///
/// # ⚠ Why this is one transaction and not a sequence of steps
///
/// MEASURED in `db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`: `PRAGMA
/// user_version` and DDL are BOTH rolled back with the transaction that set them. So the whole
/// reshape — the new table, the copy, the drop, the rename and the stamp — commits or does not, and
/// there is no state in between for a reader to find. That matters more here than it did for schema
/// 1's create, and in the opposite direction: schema 2's `credential` still has `name` and `value`
/// columns, so `SELECT name, value FROM credential` — the exact statement a schema-1 binary runs —
/// **is still valid SQL against the schema-2 shape**. A half-applied reshape stamped 1 over
/// schema-2 tables would therefore be ACCEPTED by an older binary and answered from silently.
/// Atomicity is what removes that state rather than documents it.
///
/// It follows that a reshape which fails leaves a working schema-1 store, re-runnable, with the
/// credential files beside it untouched — the same disposition `crate::db::migrate` takes about a
/// database it could not finish creating.
///
/// # The rebuild is a COPY, because `ALTER TABLE` cannot do it
///
/// Schema 1's `credential` is keyed `name TEXT PRIMARY KEY`; schema 2's is keyed on a surrogate
/// `id` with `name` merely unique among LIVE rows (§4.1 — a superseded row carries the same name as
/// the value that replaced it, which is what makes it a rollback copy). SQLite's `ALTER TABLE` can
/// add a column but cannot add or drop a PRIMARY KEY, so the old table is RENAMED aside, the new
/// one created from [`DDL`], the rows classified across, and the old one dropped.
///
/// ⚠ **No `VACUUM` afterwards.** It writes a full temp copy of the database — i.e. plaintext venue
/// credentials — into `SQLITE_TMPDIR` under the umask, which is the exact hazard `crate::db`'s
/// modes section exists to prevent and which its `preview` already refused once as an
/// implementation shortcut. The freed pages are reused by the next write; that is cheaper than a
/// second plaintext copy.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`].
pub fn reshape_into(
    tx: &Transaction<'_>,
    comments: &FileComments,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let rows: BTreeMap<String, String> = {
        let mut stmt = tx.prepare("SELECT name, value FROM credential ORDER BY name")?;
        let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = BTreeMap::new();
        for row in it {
            let (n, v) = row?;
            out.insert(n, v);
        }
        out
    };

    tx.execute_batch(&format!("ALTER TABLE credential RENAME TO {RESHAPE_SCRATCH};"))?;
    // The `DDL` batch, and then the `venue` ROSTER, before a single row is minted. ⚠ The roster is
    // load-bearing since the venue-links flip: `account.venue_id` is `NOT NULL`, and the account
    // rows `write_rows` mints take their number from the `venue` table in the same statement — so
    // an EMPTY roster (what the batch alone creates on a schema-1 store) refused the whole upgrade
    // with `NOT NULL constraint failed: account.venue_id`, where it used to leave a NULL for
    // `crate::db::ensure_venue_id_columns`' backfill. `crate::db::ensure_venue_rows` is that
    // function's own first statement, so it is the one spelling of both halves.
    crate::db::ensure_venue_rows(tx)?;
    let report = write_rows(tx, &rows, comments, classify)?;

    // ⚠ **EVERY ROW MUST HAVE CARRIED, and a shortfall fails the RUN rather than one key.**
    //
    // This is the one place a per-key refusal is not survivable, and the difference from
    // `crate::db::migrate`'s is the SOURCE. There, a refused key is still in `secrets.env`, the run
    // carries its neighbours, and the operator re-runs after fixing one line. Here the source is
    // the table about to be DROPPED: a refusal that merely skipped a row would commit a schema-2
    // store missing a credential that exists nowhere else in the database, stamped at the current
    // version so every reader accepts it. A venue would silently drop to paper with nothing to say
    // why.
    //
    // The guard is a COUNT rather than an inspection of `refused`, deliberately: it catches any
    // future path that drops a row for a reason nobody has thought of yet, not just the three
    // refusals that exist today. The refused NAMES ride the message, because a count alone is not
    // something an operator can act on. Returning `Err` here rolls the whole transaction back — the
    // rename, the new tables and the stamp with it — so what is left on disk is the working
    // schema-1 store this call started from.
    //
    // ⚠ **ALIAS rows are added to the count**, and leaving them out turned the one collision this
    // store can produce into a whole-run failure. A `{VENUE}_MAINNET_*` key beside its
    // `{VENUE}_LIVE_*` twin is CARRIED — it is written, both names answer, and the report says so
    // — but it is written `superseded_at IS NOT NULL`, so it is not a LIVE row and `live_rows`
    // does not count it. The guard is about rows that VANISHED, and an alias did not.
    if report.live_rows + report.alias_rows != rows.len() {
        // ⚠ **The whole REFUSAL, not just its key.** This listed `SchemaRefusal::key` alone, which
        // is enough for the three refusals whose repair is obvious from the name and is NOT enough
        // for `CollidingLiveValues`, whose whole content is that this key collides with ANOTHER
        // one: an operator told only *the rows it could not carry: ASTER_MAINNET_API_KEY* has to
        // guess which other line in their file it disagrees with. Every variant's `Display` names a
        // key and never a value, so printing the refusal costs nothing the key did not.
        let missing: Vec<String> = report.refused.iter().map(ToString::to_string).collect();
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the schema upgrade read {} credential row(s) and could classify only {} — \
                 NOTHING WAS WRITTEN and this store is still at its old schema, which still \
                 reads. The rows it could not carry: {}. Every one of them exists only in this \
                 database, so dropping the old table would have destroyed them.",
                rows.len(),
                report.live_rows + report.alias_rows,
                if missing.is_empty() {
                    "(none named)".to_string()
                } else {
                    format!("\n    - {}", missing.join("\n    - "))
                },
            )),
        ));
    }

    tx.execute_batch(&format!("DROP TABLE {RESHAPE_SCRATCH};"))?;
    // The FK columns schema 2 introduces are checked before the caller commits, so a reshape that
    // produced a dangling `account_id` is an error rather than a store nobody notices is broken.
    // `PRAGMA foreign_keys` only enforces NEW statements; this asks about the whole database.
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    if violations > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the reshaped credential store has {violations} dangling account reference(s); \
                 nothing was committed"
            )),
        ));
    }
    Ok(report)
}
