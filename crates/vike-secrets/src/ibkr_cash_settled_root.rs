//! **`ibkr_cash_settled_root`: the futures roots on which IBKR admits a dated future, one row per root.**
//!
//! IBKR's contract details carry no settlement field, so which roots settle in CASH is the operator's knowledge,
//! kept here (the owner's answer to question 7 of 2026-11-13; decision 0234's amendment of that day). The gatewayhub
//! reads the rows with the rest of its settings snapshot and hands them to the bridge's future admission
//! (`vike_ibkr::instrument::any_symbol::forex_future::future_facts`); `vike-cli config ibkr-cash-settled-roots` lists,
//! adds and removes them.
//!
//! # The seed: create-new, never over anything
//!
//! [`seed_ibkr_cash_settled_roots`] creates the table holding [`RELEASE_SEED`] (the eight CME equity-index roots the
//! bridge admitted before the table existed) when the table does not exist, in ONE transaction, and does nothing at all
//! when it does: a root the operator removed is never put back, and an EMPTY table is the operator's verdict (every
//! future refused), not a table to fill. Its callers are IBKR's own: the gatewayhub's first IBKR future admission on a
//! store without the table (`crates/vike-gatewayhub/src/trade/adapters/ibkr_roots.rs`'s `roots_for_future`), and
//! `vike-cli config ibkr-cash-settled-roots seed` for an operator. No server start runs it (the owner, 2026-11-16: one
//! venue's step never sits in the shared start), so a box that trades no IBKR future holds no table. That is also why
//! the DDL stays out of [`crate::DDL`]: every writer runs that batch, which would create the table EMPTY on every box.
//!
//! # What this module may write
//!
//! * **The table, created by the seed alone, with its rows; then ONE `INSERT` or ONE `DELETE` of ONE row.** No other
//!   table. [`add_ibkr_cash_settled_root`] and [`remove_ibkr_cash_settled_root`] answer `None` on a store without the
//!   table and create nothing.
//! * **A store that does not exist is refused, never created** ([`StoreErrorKind::NoSettingsDatabase`]).
//! * **No version stamp and no change-journal record**, `crate::symbol_set`'s argument: a futures root is public
//!   vocabulary, not a credential, an account or a setting row.

use std::path::Path;

use rusqlite::TransactionBehavior;

use crate::db::{StoreError, StoreErrorKind, database_present, open_for_read, open_for_write};
use crate::store_locator::db_path_in;

/// The roots an install seeds: the CME equity-index futures, full and micro, that settle in CASH. A root that settles by
/// DELIVERY (CL, GC, ...) is left out on purpose: IB closes such a position days before the last trading day, at prices
/// the engine would carry as a position the venue no longer holds. Read by [`seed_ibkr_cash_settled_roots`] alone.
pub const RELEASE_SEED: [&str; 8] = ["ES", "MES", "NQ", "MNQ", "RTY", "M2K", "YM", "MYM"];

/// The table. A root is upper-case letters and digits, as the bridge reads one out of a contract name.
const DDL: &str = "
CREATE TABLE IF NOT EXISTS ibkr_cash_settled_root (
    root TEXT NOT NULL PRIMARY KEY,
    CHECK (root <> '' AND root NOT GLOB '*[^A-Z0-9]*')
) STRICT;
";

/// Whether the table exists on `conn`.
fn table_present(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' \
         AND name = 'ibkr_cash_settled_root')",
        [],
        |r| r.get(0),
    )
}

/// Run one write transaction against an EXISTING store (`crate::symbol_set`'s shape).
fn write_one<T>(
    settings_dir: &Path,
    statement: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T>,
) -> Result<T, StoreError> {
    let db = db_path_in(settings_dir);
    let refused = |kind| StoreError { path: db.clone(), kind };
    let Some(mut conn) = open_for_write(&db)? else {
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    };
    let write = || -> rusqlite::Result<T> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = statement(&tx)?;
        tx.commit()?;
        Ok(out)
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

/// **The install seed**: create the table holding [`RELEASE_SEED`] when it does not exist. `true` when this call
/// created it, `false` when it was there already and was left exactly as it was.
///
/// # Errors
/// [`StoreErrorKind::NoSettingsDatabase`] (none is created), a busy store, or one at another schema. Nothing was
/// written.
pub fn seed_ibkr_cash_settled_roots(settings_dir: &Path) -> Result<bool, StoreError> {
    write_one(settings_dir, |tx| {
        if table_present(tx)? {
            return Ok(false);
        }
        tx.execute_batch(DDL)?;
        for root in RELEASE_SEED {
            tx.execute("INSERT INTO ibkr_cash_settled_root (root) VALUES (?1)", [root])?;
        }
        Ok(true)
    })
}

/// **Add one root** (upper-case letters and digits; the table's `CHECK` refuses anything else). `None` when the table
/// does not exist (nothing written), `Some(false)` when the root was already there.
///
/// # Errors
/// As [`seed_ibkr_cash_settled_roots`], and a root the table's `CHECK` refuses.
pub fn add_ibkr_cash_settled_root(
    settings_dir: &Path,
    root: &str,
) -> Result<Option<bool>, StoreError> {
    write_one(settings_dir, |tx| {
        if !table_present(tx)? {
            return Ok(None);
        }
        // `ON CONFLICT ... DO NOTHING`, never `INSERT OR IGNORE`: that would also swallow the `CHECK`.
        let added = tx.execute(
            "INSERT INTO ibkr_cash_settled_root (root) VALUES (?1) ON CONFLICT (root) DO NOTHING",
            [root],
        )?;
        Ok(Some(added > 0))
    })
}

/// **Remove one root.** `None` when the table does not exist (nothing written), `Some(false)` when no row held it.
///
/// # Errors
/// As [`seed_ibkr_cash_settled_roots`].
pub fn remove_ibkr_cash_settled_root(
    settings_dir: &Path,
    root: &str,
) -> Result<Option<bool>, StoreError> {
    write_one(settings_dir, |tx| {
        if !table_present(tx)? {
            return Ok(None);
        }
        let removed = tx.execute("DELETE FROM ibkr_cash_settled_root WHERE root = ?1", [root])?;
        Ok(Some(removed > 0))
    })
}

/// **Every root, in the order it was added** (the seed's first). `None` for a box with no store and for a store the seed
/// has not reached.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn read_ibkr_cash_settled_roots(
    settings_dir: &Path,
) -> Result<Option<Vec<String>>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(None);
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Option<Vec<String>>> {
        if !table_present(&conn)? {
            return Ok(None);
        }
        let mut stmt = conn.prepare("SELECT root FROM ibkr_cash_settled_root ORDER BY rowid")?;
        let roots = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(Some(roots))
    };
    query().map_err(|e| StoreError::sql(&db, e))
}
