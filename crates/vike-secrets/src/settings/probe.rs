//! Schema probes: `table_exists` and `has_column`, asked of the live store rather than assumed from
//! the DDL.
//!
//! A probe reads a store's SHAPE, never its data, so it converts nothing
//! (`docs/decisions/0117-there-are-no-migrations.md`): it is how a reader answers truthfully about a
//! store older than the shipped DDL.

use super::*;

/// Does this database carry `name` as a TABLE?
///
/// The settings tables are deliberately NOT gated on [`crate::SCHEMA_VERSION`] — see
/// [`crate::schema::DDL`]'s note, where a bump is measured as the live gate for two already-migrated
/// boxes — so *is it there* is asked of `sqlite_master` rather than of a number. `name` is a
/// `&'static str` from this module's own call sites, never operator input.
pub(super) fn table_exists(
    db: &Path,
    conn: &rusqlite::Connection,
    name: &str,
) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(db, e))?;
    Ok(found > 0)
}

/// **Does `table` carry `column`?** — asked of the live schema, never assumed from
/// [`crate::schema::DDL`].
///
/// ⚠ **The DDL cannot answer this and that is the whole reason the function exists.** Every table
/// there is `CREATE TABLE IF NOT EXISTS`, which does exactly nothing to a table that is already
/// present — so a column added to an existing table's DDL reaches a store BORN after the change and
/// no store born before it. A reader that selected an assumed column would fail on such a store
/// with a SQL error where the honest answer is `None` (`account.max_exposure` and `venue.title` are
/// the two columns read this way).
///
/// `pub(crate)` and in the bare `rusqlite::Result` currency rather than [`DbError`]:
/// [`crate::db::ensure_venue_rows`] needs this exact check and sits inside
/// [`crate::db::fill_into`]'s transaction, which is `rusqlite::Result` throughout — `DbError` needs
/// a `path` that call has no cheap way to attach at that depth, and the wrap belongs once at the
/// outer boundary.
pub(crate) fn has_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: String = r.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}
