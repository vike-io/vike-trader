//! **`operator_instrument`: the instruments the operator added BY HAND, one row per instrument.**
//!
//! A venue's kept list (`<project>/instruments/<venue>.json`) is what the venue lists. An instrument it does not
//! list (a Dukascopy US stock CFD the account's list leaves out, an IG pair: IG publishes no list at all) is the
//! operator's own row here, added and removed one instrument at a time through the datahub's
//! `Request::OperatorInstruments` (`crates/vike-datahub/src/server/operator_instruments.rs`), which also folds the rows
//! into the venue's kept list, so every reader of the kept lists reads them too.
//!
//! Why a table of its own and not more rows of `operator_instrument_list`: that table holds ONE row per venue,
//! written WHOLE by `vike-backend catalog refresh` (`crate::operator_instrument_list`), so a row added by hand there
//! would be erased by the next refresh. Here no refresh writes anything, by construction.
//!
//! # What this module may write
//!
//! * **One UPSERT or one DELETE of ONE row, and nothing else.** A row is addressed by its venue and the venue's
//!   own name for the instrument (`raw_symbol`); it links by `venue_id`, as every link in this store does.
//! * **A store that does not exist is refused, never created** ([`StoreErrorKind::NoSettingsDatabase`]).
//! * **No version stamp and no change-journal record**, `crate::strategy_code`'s argument: the table is in
//!   [`crate::DDL`] (`CREATE TABLE IF NOT EXISTS`, so it reaches an existing store at its first write), the seal
//!   counts `setting` alone, and an instrument is not a credential, an account or a setting.

use std::path::Path;

use rusqlite::{TransactionBehavior, params};
use vike_model::Instrument;

use crate::db::{
    StoreError, StoreErrorKind, database_present, ensure_venue_rows, open_for_read, open_for_write,
};
use crate::store_locator::db_path_in;

/// One instrument the operator added, as the table holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct OperatorInstrument {
    /// The word the operator typed (`EURUSD`), upper case: what a remove may name besides the venue's own name.
    pub asked: String,
    /// When it was added, `YYYY-MM-DD`.
    pub added: String,
    /// The row itself; its `venue` and `raw_symbol` are the row's address.
    pub instrument: Instrument,
}

/// Run one write transaction against an EXISTING store (`crate::symbol_set`'s shape).
fn write_one(
    settings_dir: &Path,
    statement: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<usize>,
) -> Result<usize, StoreError> {
    let db = db_path_in(settings_dir);
    let refused = |kind| StoreError { path: db.clone(), kind };
    if !database_present(&db) {
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    }
    let (mut conn, created) = open_for_write(&db)?;
    if created {
        // The file vanished between the probe and the open: leave no EMPTY database behind
        // (`crate::strategy_code`'s repair).
        drop(conn);
        let _ = std::fs::remove_file(&db);
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    }
    let write = || -> rusqlite::Result<usize> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // The DDL (this table reaches an existing store here) and the venue rows a row links to.
        ensure_venue_rows(&tx)?;
        let changed = statement(&tx)?;
        tx.commit()?;
        Ok(changed)
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

/// **Add one instrument**, replacing the row of the same venue and `raw_symbol`.
///
/// # Errors
/// [`StoreErrorKind::NoSettingsDatabase`] (none is created), a venue the store has no `venue` row for, a busy
/// store, or one at another schema. Nothing was written.
pub fn write_operator_instrument(
    settings_dir: &Path,
    row: &OperatorInstrument,
) -> Result<(), StoreError> {
    let instrument = serde_json::to_string(&row.instrument)
        .map_err(|e| StoreError::io(&db_path_in(settings_dir), std::io::Error::other(e)))?;
    let i = &row.instrument;
    write_one(settings_dir, |tx| {
        tx.execute(
            "INSERT INTO operator_instrument (venue_id, raw_symbol, asked, added, instrument) \
             VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4, ?5) \
             ON CONFLICT (venue_id, raw_symbol) DO UPDATE SET asked = excluded.asked, \
             added = excluded.added, instrument = excluded.instrument",
            params![i.venue, i.raw_symbol, row.asked, row.added, instrument],
        )
    })
    .map(drop)
}

/// **Remove the one row of `venue` named `name`**: the venue's own name (`raw_symbol`) or the word the operator
/// typed (`asked`). `Ok(false)` when no row has either.
///
/// # Errors
/// As [`write_operator_instrument`].
pub fn delete_operator_instrument(
    settings_dir: &Path,
    venue: &str,
    name: &str,
) -> Result<bool, StoreError> {
    write_one(settings_dir, |tx| {
        tx.execute(
            "DELETE FROM operator_instrument WHERE venue_id = (SELECT id FROM venue WHERE name = ?1) \
             AND (raw_symbol = ?2 OR asked = ?2)",
            params![venue, name],
        )
    })
    .map(|n| n > 0)
}

/// **Every instrument the operator added**, every venue, in the order they were first added. Empty for a box with
/// no store and for a store no writer has carried this table into.
///
/// # Errors
/// A store that exists and cannot be read, or a row that does not parse: never an empty answer in their place, since
/// the kept lists' fold would then drop rows it never read.
pub fn read_operator_instruments(
    settings_dir: &Path,
) -> Result<Vec<OperatorInstrument>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db) {
        return Ok(Vec::new());
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Vec<(String, String, String, String)>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' \
             AND name = 'operator_instrument')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT v.name, o.asked, o.added, o.instrument \
             FROM operator_instrument o JOIN venue v ON v.id = o.venue_id ORDER BY o.id",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()
    };
    query()
        .map_err(|e| StoreError::sql(&db, e))?
        .into_iter()
        .map(|(venue, asked, added, json)| {
            let instrument = serde_json::from_str(&json).map_err(|e| {
                StoreError::io(
                    &db,
                    std::io::Error::other(format!(
                        "`{venue}`'s instrument `{asked}` added by hand does not parse: {e}"
                    )),
                )
            })?;
            Ok(OperatorInstrument { asked, added, instrument })
        })
        .collect()
}
