//! **`symbol_set`: a user's named symbol lists, one row per name.**
//!
//! The desktop's Data Manager "DataSets" — a name, the symbols, and the provider, interval and benchmark the set
//! defaults to — saved through the data daemon's `SetSymbolSet` / `DeleteSymbolSet` verbs and read by its
//! `SymbolSets` verb (`crates/vike-datahub/src/server/symbol_sets.rs`). The owner's ruling of 2026-11-12: they live in
//! the one `vike.db` on the server, and the desktop's old `storage/datasets.json` is read by nothing. The table
//! starts EMPTY; nothing imports that file (`docs/decisions/0117-there-are-no-migrations.md`).
//!
//! # What this module may write
//!
//! * **One UPSERT or one DELETE of ONE row, by name, and nothing else.** No other table, no other row.
//! * **A store that does not exist is refused, never created** ([`StoreErrorKind::NoSettingsDatabase`]).
//! * **No version stamp and no change-journal record**, `crate::strategy_code`'s argument: the table is in
//!   [`crate::DDL`], the seal counts `setting` alone, and a symbol list is not a credential, an account or a
//!   setting.

use std::path::Path;

use rusqlite::{TransactionBehavior, params};

use crate::db::{StoreError, StoreErrorKind, database_present, open_for_read, open_for_write};
use crate::store_locator::db_path_in;

/// One saved list, as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSetRow {
    /// The list's name, its one address.
    pub name: String,
    /// The symbols, in the user's order.
    pub symbols: Vec<String>,
    /// The data provider the list defaults to (`Auto`, `binance`, ...).
    pub provider: String,
    /// The bar interval the list defaults to.
    pub interval: String,
    /// The benchmark symbol, or empty.
    pub benchmark: String,
    /// `true` for a list a user made, `false` for a built-in one a user saved over.
    pub user: bool,
}

/// Run one write transaction against an EXISTING store.
fn write_one(
    settings_dir: &Path,
    statement: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<usize>,
) -> Result<usize, StoreError> {
    let db = db_path_in(settings_dir);
    let refused = |kind| StoreError { path: db.clone(), kind };
    let Some(mut conn) = open_for_write(&db)? else {
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    };
    let write = || -> rusqlite::Result<usize> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(crate::schema::DDL)?;
        let changed = statement(&tx)?;
        tx.commit()?;
        Ok(changed)
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

/// **Save one list**, replacing the row of the same name or adding it.
///
/// # Errors
/// [`StoreErrorKind::NoSettingsDatabase`] (none is created), a busy store, or one at another schema. Nothing was written.
pub fn write_symbol_set_row(settings_dir: &Path, set: &SymbolSetRow) -> Result<(), StoreError> {
    let symbols = serde_json::to_string(&set.symbols)
        .map_err(|e| StoreError::io(&db_path_in(settings_dir), std::io::Error::other(e)))?;
    write_one(settings_dir, |tx| {
        tx.execute(
            "INSERT INTO symbol_set (name, symbols, provider, interval, benchmark, user) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (name) DO UPDATE SET symbols = excluded.symbols, \
             provider = excluded.provider, interval = excluded.interval, \
             benchmark = excluded.benchmark, user = excluded.user",
            params![set.name, symbols, set.provider, set.interval, set.benchmark, set.user],
        )
    })
    .map(drop)
}

/// **Remove the list called `name`.** `Ok(false)` when no row had that name.
///
/// # Errors
/// As [`write_symbol_set_row`].
pub fn delete_symbol_set_row(settings_dir: &Path, name: &str) -> Result<bool, StoreError> {
    write_one(settings_dir, |tx| tx.execute("DELETE FROM symbol_set WHERE name = ?1", [name]))
        .map(|n| n > 0)
}

/// **Every saved list**, in the order they were first saved. Empty for a box with no store and for a store no writer
/// has carried this table into.
///
/// # Errors
/// A store that exists and cannot be read, or a row whose symbols do not parse.
pub fn read_symbol_set_rows(settings_dir: &Path) -> Result<Vec<SymbolSetRow>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(Vec::new());
    }
    let conn = open_for_read(&db)?;
    type Raw = (String, String, String, String, String, bool);
    let query = || -> rusqlite::Result<Vec<Raw>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'symbol_set')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT name, symbols, provider, interval, benchmark, user FROM symbol_set ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect()
    };
    query()
        .map_err(|e| StoreError::sql(&db, e))?
        .into_iter()
        .map(|(name, symbols, provider, interval, benchmark, user)| {
            let symbols = serde_json::from_str(&symbols).map_err(|e| {
                StoreError::io(
                    &db,
                    std::io::Error::other(format!("the `{name}` symbol list does not parse: {e}")),
                )
            })?;
            Ok(SymbolSetRow { name, symbols, provider, interval, benchmark, user })
        })
        .collect()
}
