//! `retire_row`: the database half of the credential DELETE — a SOFT retire of ONE live name, never
//! a physical row delete (`docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`).

use super::*;

/// The `superseded_at` marker a DELETED credential row carries.
///
/// The column records THAT a row is not the live one, and why — the same convention the schema-2
/// fill's alias marker (`crate::schema`'s `ALIAS_MARK`) follows, so the three kinds of non-live row
/// (a rollback copy, an alias spelling, a deletion) can be told apart by eye in a `SELECT`. When it
/// happened is the change journal's (`credential_retire`), not this column's.
///
/// ⚠ **The readers depend on this exact spelling**: `crate::db::read`'s `live_predicate` hides
/// every row of a NAME that has a retired row and no live one, and it spells this literal inside a
/// `&'static str`. `the_read_predicate_hides_the_mark_the_delete_writes` holds the two equal.
pub(crate) const RETIRED_MARK: &str = "retired-by-the-operator";

/// The row a reader answers for a name, as the delete needs it: `(id, account_id, venue_id, field,
/// is_live)`.
type ReadRow = (i64, Option<i64>, Option<i64>, String, bool);

/// **The DELETE, database half** — retire the ONE row the readers answer for `name`, so the name
/// reads as ABSENT from then on (absent credentials are the live gate: that account reads as not
/// configured, which is paper).
///
/// A soft retire: `superseded_at` is set to [`RETIRED_MARK`] and the row — name, account, field,
/// VALUE — stays in the table as the evidence of the act and the one way back. Nothing here runs a
/// `DELETE` statement, and nothing else is touched: no other row, no account row, no hub key.
///
/// The partial unique indexes (`credential_one_live_name`, `credential_one_live_value`, both
/// `WHERE superseded_at IS NULL`) stop constraining the retired row, so the same name — or another
/// spelling of the same field — can be ADDED again later, and the re-added row is the one every
/// reader answers with.
///
/// # What it refuses — each before anything is written
///
/// * [`Table::HubKey`], or a name that IS a hub key ([`DbErrorKind::HubKeyNotRetired`]): node
///   keys are minted, and a deleted one locks out every client of that service (decision 0051);
/// * a name with no row the readers would answer for — absent, or already retired
///   ([`DbErrorKind::NotHeld`]);
/// * a name whose credential ANOTHER spelling still answers for at the same account field
///   ([`DbErrorKind::RetireLeavesASpelling`]) — retiring one would leave the account armed with the
///   same value under the other name;
/// * a database that is not [`SCHEMA_VERSION`], and one that VANISHED between the backend choice
///   and this open ([`DbErrorKind::VanishedDatabase`], with the file this open created unlinked):
///   the same two invariants [`upsert_rows`] holds, for its reasons. [`create_store`] stays the only
///   creator.
///
/// One IMMEDIATE transaction — the decision and the write are atomic against another writer, the
/// shape [`edit_account`] takes for its read-then-write and for its reason.
pub(crate) fn retire_row(path: &Path, table: Table, name: &str) -> Result<(), DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    // Decided from the TABLE before anything is opened: the caller said hub key.
    if table == Table::HubKey {
        return Err(refuse(DbErrorKind::HubKeyNotRetired { key: name.to_string() }));
    }
    let (mut conn, created) = open_for_write(path)?;
    // ⚠ The race arm `upsert_rows` documents: the file vanished after the backend probe saw it.
    // Close the engine BEFORE unlinking — an open handle keeps the file alive on Windows.
    if created {
        drop(conn);
        let _ = std::fs::remove_file(path);
        return Err(refuse(DbErrorKind::VanishedDatabase));
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;
    // THE WRITE FUNNEL, run by every writer before its first statement (`ensure_venue_rows`'s doc).
    ensure_venue_rows(&tx).map_err(|e| DbError::sql(path, e))?;

    // …and decided from the NAME: a hub key named against the credential table is still a node
    // key, and the refusal is the same.
    let in_node_table = tx
        .prepare("SELECT 1 FROM hub_key WHERE name = ?1")
        .and_then(|mut stmt| stmt.exists([name]))
        .map_err(|e| DbError::sql(path, e))?;
    if in_node_table {
        return Err(refuse(DbErrorKind::HubKeyNotRetired { key: name.to_string() }));
    }

    // The row the readers answer for this name: the SAME predicate and the same last-wins order
    // (`ORDER BY id`, newest last) every reader folds with, so the delete retires exactly the value
    // a reader would have handed out — never a different row of the same name.
    let live = live_predicate(Table::Credential);
    let target: Option<ReadRow> = tx
        .query_row(
            &format!(
                "SELECT id, account_id, venue_id, field, superseded_at IS NULL FROM credential \
                 WHERE name = ?1 AND ({live}) ORDER BY id DESC LIMIT 1"
            ),
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    let Some((id, account_id, venue_id, field, is_live)) = target else {
        return Err(refuse(DbErrorKind::NotHeld { key: name.to_string(), table }));
    };

    // Another SPELLING the readers still answer for at the same placement and field — a tier
    // alias filed beside this LIVE row. Asked only when the target IS the live row: an alias row
    // is retired on its own (the live spelling it shadows stays, and is visibly still there), so
    // the operator deletes the aliases first and the live name last, one name per call, and no
    // order of deletes can dead-end. `IS` rather than `=`, so the NULL halves of a venue or
    // infrastructure placement compare equal (SQLite's NULL-safe equality).
    let others: Vec<String> = if !is_live {
        Vec::new()
    } else {
        let mut stmt = tx
            .prepare(&format!(
                "SELECT DISTINCT name FROM credential \
                 WHERE name <> ?1 AND field = ?2 AND account_id IS ?3 AND venue_id IS ?4 \
                 AND ({live}) ORDER BY name"
            ))
            .map_err(|e| DbError::sql(path, e))?;
        let rows = stmt
            .query_map((name, &field, account_id, venue_id), |r| r.get::<_, String>(0))
            .map_err(|e| DbError::sql(path, e))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| DbError::sql(path, e))?);
        }
        out
    };
    if !others.is_empty() {
        return Err(refuse(DbErrorKind::RetireLeavesASpelling { key: name.to_string(), others }));
    }

    // THE act: one row, by id, marked — and nothing else in the store changes.
    tx.execute("UPDATE credential SET superseded_at = ?2 WHERE id = ?1", (id, RETIRED_MARK))
        .map_err(|e| DbError::sql(path, e))?;
    tx.commit().map_err(|e| DbError::sql(path, e))
}
