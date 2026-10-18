//! The live smokes' way to the credential store (feature `test-support` only).
//!
//! A live smoke is a harness: it runs under the CI lane's `VIKE_NO_LIVE_KEYS=true` and under a
//! developer's own shell. Both switches are read HERE, once, so no smoke re-spells the parse and no
//! smoke can read the store around the scope. Production never calls this: a binary opens its store
//! through [`crate::open_credential_store`].

use std::collections::HashMap;

use super::{CredentialScope, CredentialStore, NO_LIVE_KEYS_ENV, StoreLocation};
use crate::store_locator::workspace_settings_dir_from;

/// The store a smoke reads: `VIKE_SETTINGS_DIR` (or the workspace walk) under the scope
/// `VIKE_NO_LIVE_KEYS` names.
///
/// # Panics
/// When `VIKE_NO_LIVE_KEYS` is set to anything but `true` or `false`: a smoke that cannot tell
/// whether the live fence is up must not run (`ScopeError`'s message says what to set).
pub fn smoke_store() -> CredentialStore {
    let scope = match CredentialScope::parse(std::env::var(NO_LIVE_KEYS_ENV).ok().as_deref()) {
        Ok(scope) => scope,
        Err(e) => panic!("{e}"),
    };
    let dir = workspace_settings_dir_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    CredentialStore::open(Some(StoreLocation::new(dir)), scope)
}

/// The smoke's credential map: what [`smoke_store`] reads, EMPTY when the store cannot be read (a
/// smoke then skips for absent credentials, as it always did).
pub fn smoke_vars() -> HashMap<String, String> {
    let store = smoke_store();
    store.read_all().map(|r| r.secrets.into_map()).unwrap_or_default()
}

/// **The account directory a smoke resolves its account from** — the `account` rows, their key
/// names and every account's credentials, read through [`smoke_store`] under its scope. A smoke asks
/// it for the account it trades (`AccountDirectory::credentials_for`), never a key name (decision
/// 0156 item 6). A store that cannot be read is carried as the directory's own error, so the smoke
/// skips for absent credentials, as it always did.
pub fn smoke_directory() -> crate::account_directory::AccountDirectory {
    let store = smoke_store();
    crate::account_directory::AccountDirectory::read(
        store.accounts(),
        store.account_keys(),
        store.credential_book(),
    )
}
