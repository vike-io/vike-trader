//! `test-support` constructor: create an empty current-schema store.

use super::open::stamp_schema_version;
use super::*;

/// **Plant a finished, CURRENT-schema, otherwise-empty store** — the primitive
/// `crate::profile_store`'s writers refuse to be (`refuse_absent_store` is the FIRST thing every one
/// of them runs, deliberately: see that function's own doc for why a profile write may never be the
/// thing that brings a credential store into existence). A test that wants to seed profile rows
/// (`store_profile`/`set_active`) into a store with no credentials at all reaches one here in a
/// single call. ⚠ It is not the ONLY path: [`create_store`] creates the same empty store
/// (`vike-cli secrets init`), and a test of the PRODUCTION fresh-install path should take that one.
/// This stays for the profile tests that only need a store to exist.
///
/// Behind `test-support`: this crate's OWN production code creates an empty store only through
/// [`create_store`], and this function does exactly that and nothing else —
/// [`open_for_write`]'s base DDL, then the SAME stamp `create_store` applies on a create
/// (`stamp_schema_version`'s call site is the authority for the ordering: inside the transaction,
/// before commit, never left for `open_for_write` to do on its own — see that function's doc for why
/// the stamp is deferred at all).
///
/// # Errors
/// The engine, or the filesystem. [`DbErrorKind`] via [`DbError::sql`]/[`DbError::io`].
#[cfg(feature = "test-support")]
pub fn create_empty_store_for_test(path: &Path) -> Result<(), DbError> {
    let (mut conn, created) = open_for_write(path)?;
    if created {
        let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
        stamp_schema_version(path, &tx)?;
        tx.commit().map_err(|e| DbError::sql(path, e))?;
    }
    Ok(())
}
