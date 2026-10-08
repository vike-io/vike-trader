//! `test-support` constructors: plant a schema-1 store, create an empty current-schema store.

use super::open::{create_dir_private, create_file_private, stamp_schema_version};
use super::*;

/// **Plant a finished schema-1 store** — the state both live boxes are in, for a test that has to
/// reshape one.
///
/// Behind `test-support`, and it is the ONE function in this crate that creates a database
/// [`migrate`] did not. It is not a second migration: it writes exactly the rows it is handed into
/// exactly the shape schema 1 had, and it exists because the reshape's whole claim is about a
/// database this code will never write again.
///
/// # Errors
/// The engine, or the filesystem.
#[cfg(feature = "test-support")]
pub fn plant_schema_1(
    path: &Path,
    credentials: &[(String, String)],
    node_keys: &[(String, String)],
) -> Result<(), DbError> {
    if let Some(dir) = path.parent() {
        create_dir_private(dir).map_err(|e| DbError::io(path, e))?;
    }
    create_file_private(path).map_err(|e| DbError::io(path, e))?;
    let mut conn = Connection::open(path).map_err(|e| DbError::sql(path, e))?;
    conn.execute_batch(SCHEMA_1).map_err(|e| DbError::sql(path, e))?;
    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    for (table, rows) in [(Table::Credential, credentials), (Table::NodeKey, node_keys)] {
        let sql = format!("INSERT INTO {} (name, value) VALUES (?1, ?2)", table.sql_name());
        for (name, value) in rows {
            tx.execute(&sql, (name, value)).map_err(|e| DbError::sql(path, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    conn.pragma_update(None, "user_version", 1i64).map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// **Plant a finished, CURRENT-schema, otherwise-empty store** — the primitive
/// `crate::profile_store`'s writers refuse to be (`refuse_absent_store` is the FIRST thing every one
/// of them runs, deliberately: see that function's own doc for why a profile write may never be the
/// thing that brings a credential store into existence). A test that wants to seed profile rows
/// (`store_profile`/`set_active`) into a store with no credentials at all reaches one here in a
/// single call. ⚠ It is no longer the ONLY path: [`migrate`] creates the same empty store when the
/// caller passes [`WhenNothingToCarry::CreateEmptyStore`] (`vike-cli secrets migrate --init`), and
/// a test of the PRODUCTION fresh-install path should take that one. This stays for the profile
/// tests that only need a store to exist.
///
/// Behind `test-support`, like [`plant_schema_1`] beside it: this crate's OWN production code
/// creates an empty store only through [`migrate`]'s explicit arm, and this function does exactly
/// that and nothing else —
/// [`open_for_write`]'s base DDL, then the SAME stamp `migrate` applies on a create
/// (`stamp_schema_version`'s call site is the authority for the ordering: inside the transaction,
/// before commit, never left for `open_for_write` to do on its own — see that function's doc for why
/// the stamp is deferred at all).
///
/// # Errors
/// The engine, or the filesystem. [`DbErrorKind`] via [`DbError::sql`]/[`DbError::io`].
#[cfg(feature = "test-support")]
pub fn create_empty_store_for_test(path: &Path) -> Result<(), DbError> {
    let (mut conn, created, _version) = open_for_write(path)?;
    if created {
        let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
        stamp_schema_version(path, &tx)?;
        tx.commit().map_err(|e| DbError::sql(path, e))?;
    }
    Ok(())
}
