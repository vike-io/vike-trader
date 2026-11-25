//! **`operator_instrument_list`: the operator's OWN credentialed instrument lists, one row per venue.**
//!
//! `vike-backend catalog refresh <venue>` (`crates/vike-tradehub/src/catalog_cli.rs`) writes a row on the box that holds
//! the venue's keys; that command's `list` and the desktop's symbol picker
//! (`crates/vike-app-core/src/data/catalog_refresh/engine.rs`) read them (`docs/decisions/0066` decision 9). A row is
//! one venue's list as columns: the `fetched` date, the qualifier that makes the list honest, and the instruments as
//! one JSON array of `vike_model::Instrument`. What a list must carry (a stamp, a qualifier, an eligible venue) is
//! `vike_catalog`'s rule, checked by its readers; this module stores rows.
//!
//! The table replaced `<project>/settings/state/venue-catalog-local.json` (the owner's ruling of 2026-11-12: symbol
//! lists live in the one `vike.db`). It starts EMPTY: nothing imports that file and nothing reads it any more
//! (`docs/decisions/0117-there-are-no-migrations.md`).
//!
//! # What this module may write
//!
//! * **One UPSERT of ONE venue's row, and nothing else.** A write replaces that venue's row whole and touches no other
//!   venue's, so two writers can no longer erase each other's lists, which the whole-file rewrite could. It reads
//!   `venue` for the number: the row links by `venue_id`, as every link in this store does.
//! * **A store that does not exist is refused, never created** ([`StoreErrorKind::NoSettingsDatabase`]): the file's
//!   existence is the live gate (`crate::store::StoreBackend`).
//! * **No version stamp and no change-journal record**, `crate::strategy_code`'s argument: the table is in
//!   [`crate::DDL`] (`CREATE TABLE IF NOT EXISTS`, so it reaches an existing store at its first write), the seal counts
//!   `setting` alone, and an instrument list is not a credential, an account or a setting.

use std::path::Path;

use rusqlite::{TransactionBehavior, params};
use vike_model::Instrument;

use crate::db::{
    StoreError, StoreErrorKind, database_present, ensure_venue_rows, open_for_read, open_for_write,
};
use crate::store_locator::db_path_in;

/// One venue's list, as the table holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct OperatorInstrumentList {
    /// The roster slug (`alpaca`).
    pub venue: String,
    /// When the list was fetched, `YYYY-MM-DD`.
    pub fetched: String,
    /// The account-shaped fact that makes the list honest (the tier it was fetched at).
    pub qualifier: String,
    /// The list. May be empty: a measured zero.
    pub instruments: Vec<Instrument>,
}

/// **Write ONE venue's list**, replacing that venue's row and no other.
///
/// # Errors
/// * [`StoreErrorKind::NoSettingsDatabase`]: no store at `settings_dir`; none is created.
/// * A venue the store has no `venue` row for (`venue_id` is `NOT NULL`), a busy store, or a store at another
///   schema. Nothing was written.
pub fn write_operator_instrument_list(
    settings_dir: &Path,
    list: &OperatorInstrumentList,
) -> Result<(), StoreError> {
    let db = db_path_in(settings_dir);
    let refused = |kind| StoreError { path: db.clone(), kind };
    let instruments = serde_json::to_string(&list.instruments)
        .map_err(|e| StoreError::io(&db, std::io::Error::other(e)))?;
    let Some(mut conn) = open_for_write(&db)? else {
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    };
    let mut write = || -> rusqlite::Result<()> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_venue_rows(&tx)?;
        tx.execute(
            "INSERT INTO operator_instrument_list (venue_id, fetched, qualifier, instruments) \
             VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4) \
             ON CONFLICT (venue_id) DO UPDATE SET fetched = excluded.fetched, \
             qualifier = excluded.qualifier, instruments = excluded.instruments",
            params![list.venue, list.fetched, list.qualifier, instruments],
        )?;
        tx.commit()
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

/// **Every venue's list**, in the order they were first written. Empty for a box with no store and for a store no
/// writer has carried this table into.
///
/// # Errors
/// A store that exists and cannot be read, or a row whose instruments do not parse: never an empty answer in
/// their place, since a writer that took an unreadable store for an empty one would overwrite lists it never read.
pub fn read_operator_instrument_lists(
    settings_dir: &Path,
) -> Result<Vec<OperatorInstrumentList>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(Vec::new());
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Vec<(String, String, String, String)>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' \
             AND name = 'operator_instrument_list')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT v.name, l.fetched, l.qualifier, l.instruments \
             FROM operator_instrument_list l JOIN venue v ON v.id = l.venue_id ORDER BY l.id",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()
    };
    query()
        .map_err(|e| StoreError::sql(&db, e))?
        .into_iter()
        .map(|(venue, fetched, qualifier, json)| {
            let instruments = serde_json::from_str(&json).map_err(|e| {
                StoreError::io(
                    &db,
                    std::io::Error::other(format!(
                        "`{venue}`'s instrument list does not parse: {e}"
                    )),
                )
            })?;
            Ok(OperatorInstrumentList { venue, fetched, qualifier, instruments })
        })
        .collect()
}
