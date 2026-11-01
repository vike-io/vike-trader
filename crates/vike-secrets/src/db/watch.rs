//! A read-only handle that answers ONE cheap question: *did another connection commit since I last
//! asked?* (decision 0169 item 3: the trigger by which a running tradehub notices that an account's
//! `active` flag moved).
//!
//! It is `PRAGMA data_version`, which SQLite answers from the database header: no table is read and
//! no row is returned, and the number differs between two asks on the SAME connection exactly when
//! ANOTHER connection (another process included) committed in between, so a commit is never seen
//! half written. It works under this store's `journal_mode = DELETE` (a rollback-journal database
//! also bumps the header's change counter on every commit), which is why a file watcher is not
//! needed: that would see a burst of create/delete events per write.
//!
//! The connection is opened the way every read here is ([`open_for_read`]: read-only, schema
//! checked, busy timeout pinned), so the watcher can neither create nor change the store. It holds
//! the connection for its whole life, because `data_version` is per connection.

use super::*;

/// See the module doc. One per watching thread (a `rusqlite` connection is `Send`, not `Sync`).
#[derive(Debug)]
pub struct StoreWatch {
    conn: Connection,
    path: PathBuf,
    seen: i64,
}

impl StoreWatch {
    /// Open a read-only connection to the database at `path` and take the baseline: the first
    /// [`Self::changed`] after this answers about commits made from now on.
    ///
    /// # Errors
    /// [`DbError`] when the database will not open or carries another schema version.
    pub fn begin(path: &Path) -> Result<StoreWatch, DbError> {
        let conn = open_for_read(path)?;
        let seen = data_version(&conn).map_err(|e| DbError::sql(path, e))?;
        Ok(StoreWatch { conn, path: path.to_path_buf(), seen })
    }

    /// Whether another connection committed since the last call (or since [`Self::begin`]). A read
    /// error leaves the baseline where it was, so the next call still reports a change that
    /// happened during the failure.
    ///
    /// # Errors
    /// [`DbError`] when the engine will not answer (a busy timeout, a vanished file).
    pub fn changed(&mut self) -> Result<bool, DbError> {
        let now = data_version(&self.conn).map_err(|e| DbError::sql(&self.path, e))?;
        let moved = now != self.seen;
        self.seen = now;
        Ok(moved)
    }
}

/// The header's change counter as this connection sees it.
fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
}
