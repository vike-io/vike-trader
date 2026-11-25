//! **`strategy_code`: a strategy's number in an order client id, one row per `(mount_id,
//! strategy_name)`, never reused.**
//!
//! A v2 order client id carries the strategy that placed it as four upper-case base36 characters:
//! `base36(id)` of this table's row, `0001` to `ZZZZ`. `0000` means "no strategy" and is never a row
//! (the table's `CHECK (id BETWEEN 1 AND 1679615)`). The table is [`crate::DDL`]'s; this module is
//! its one writer and its one reader. The code travels as the row's `i64`, the way `Account::id`
//! travels for the account code: rendering the four characters is the id format's business, not the
//! store's.
//!
//! # What this module may write
//!
//! * **One `INSERT` of one `strategy_code` row, and nothing else.** No `UPDATE`, no `DELETE`, no
//!   other table. A code once handed out is never re-pointed and never freed; `AUTOINCREMENT` keeps
//!   the high-water mark, so even a row an operator deletes by hand never comes back as another
//!   strategy's code.
//! * **A store that does not exist is refused, never created** ([`StoreErrorKind::NoSettingsDatabase`]):
//!   the file's existence is the live gate (`crate::store::StoreBackend`).
//! * **No version stamp and no change-journal record.** `CREATE TABLE IF NOT EXISTS` reaches an
//!   existing store on its first call, and [`crate::SCHEMA_VERSION`] stays (the argument is
//!   `crate::link_sessions`' module doc). The row is a label the id format reads, not a credential, an
//!   account or a setting: the seal counts `setting` alone, and nothing here arms or routes anything.
//!
//! # Insert-or-get, and why it is not `INSERT OR IGNORE`
//!
//! On an `AUTOINCREMENT` table SQLite moves the `sqlite_sequence` mark for an IGNORED insert too
//! (measured: `crates/vike-secrets/src/db/venues.rs`'s `ensure_venue_rows`), so an `OR IGNORE` on
//! every mount would spend a code per restart against a cap of 1,679,615. The writer asks first and
//! inserts only a key the table does not hold, inside ONE `IMMEDIATE` transaction: two processes
//! asking at once are serialised by the store's lock, so they never share a code, and a key asked
//! again (a re-mount, a restart) spends nothing.
//!
//! It runs the [`crate::DDL`] batch first, as every writer does, and not the funnel's `venue` top-up:
//! a `strategy_code` row names no venue.

use std::path::Path;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::db::{StoreError, StoreErrorKind, database_present, open_for_read, open_for_write};
use crate::store_locator::db_path_in;
use vike_model::orders::client_id::OrderClientIdStrategyCode;

/// One row, as a decode reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyCodeRow {
    /// The mount the code was handed to (`vike_core::strategy_state::mount_id_with`'s id).
    pub mount_id: String,
    /// The strategy's name on that mount.
    pub strategy_name: String,
}

/// **The code of `strategy_name` on `mount_id`: the row the table holds, or a new one.**
///
/// The same key answers the same code for the life of the store; another key never gets it. The
/// answer is in `1..=`[`OrderClientIdStrategyCode::MAX_ID`] (`ZZZZ`, 36^4 − 1; the table's `CHECK` spells the same number), never `0`.
///
/// # Errors
/// * [`StoreErrorKind::NoSettingsDatabase`]: no store at `settings_dir`; none is created.
/// * [`StoreErrorKind::StrategyCodesExhausted`]: the key is new and every code has been handed out.
///   Nothing was written.
/// * The store is busy, at another schema, or will not open. Nothing was written.
pub fn strategy_code_for(
    settings_dir: &Path,
    mount_id: &str,
    strategy_name: &str,
) -> Result<i64, StoreError> {
    let db = db_path_in(settings_dir);
    let refused = |kind| StoreError { path: db.clone(), kind };
    let Some(mut conn) = open_for_write(&db)? else {
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    };
    let mut write = || -> rusqlite::Result<Option<i64>> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(crate::schema::DDL)?;
        let held: Option<i64> = tx
            .query_row(
                "SELECT id FROM strategy_code WHERE mount_id = ?1 AND strategy_name = ?2",
                params![mount_id, strategy_name],
                |r| r.get(0),
            )
            .optional()?;
        if held.is_some() {
            return Ok(held);
        }
        // The mark, not `max(id)`: a row removed by hand keeps its code spent.
        let mark: i64 = tx
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'strategy_code'", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        if mark >= OrderClientIdStrategyCode::MAX_ID {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO strategy_code (mount_id, strategy_name, assigned_at) \
             VALUES (?1, ?2, datetime('now'))",
            params![mount_id, strategy_name],
        )?;
        let code = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(code))
    };
    write().map_err(|e| StoreError::sql(&db, e))?.ok_or_else(|| {
        refused(StoreErrorKind::StrategyCodesExhausted {
            mount_id: mount_id.to_string(),
            strategy_name: strategy_name.to_string(),
        })
    })
}

/// **Which strategy `code` names**, for a decode. `None` for `0` ("no strategy"), a code no row
/// holds, a store with no such table yet, and a box with no store.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn strategy_name_for(
    settings_dir: &Path,
    code: i64,
) -> Result<Option<StrategyCodeRow>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(None);
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Option<StrategyCodeRow>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'strategy_code')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(None);
        }
        conn.query_row(
            "SELECT mount_id, strategy_name FROM strategy_code WHERE id = ?1",
            [code],
            |r| Ok(StrategyCodeRow { mount_id: r.get(0)?, strategy_name: r.get(1)? }),
        )
        .optional()
    };
    query().map_err(|e| StoreError::sql(&db, e))
}
