//! The live smokes' way to the credential store (feature `test-support` only).
//!
//! A live smoke is a harness: it runs under the CI lane's `VIKE_NO_LIVE_KEYS=true` and under a
//! developer's own shell. Both switches are read HERE, once, so no smoke re-spells the parse and no
//! smoke can read the store around the scope. Production never calls this: a binary opens its store
//! through [`crate::open_credential_store`].
//!
//! ⚠ A settings directory the operator NAMES (`VIKE_SETTINGS_DIR`) must hold the database: a named
//! directory with none is a wrong path (a typo, or a shell that rewrote it), and read as an absent
//! store it would make every smoke self-skip for credentials nobody looked for. Unset, the walk's
//! missing store is still the self-skip: no credentials on this box.

use std::collections::HashMap;

use super::{
    CredentialScope, CredentialStore, NO_LIVE_KEYS_ENV, StoreBackend, StoreLocation, backend_in,
};
use crate::store_locator::{db_path_in, nonblank, workspace_settings_dir_from};

/// The store a smoke reads: `VIKE_SETTINGS_DIR` (or the workspace walk) under the scope
/// `VIKE_NO_LIVE_KEYS` names.
///
/// # Panics
/// With [`smoke_store_from`]'s refusal: `VIKE_NO_LIVE_KEYS` set to anything but `true` or `false` (a
/// smoke that cannot tell whether the live fence is up must not run), or `VIKE_SETTINGS_DIR` naming
/// a directory that holds no settings database.
pub fn smoke_store() -> CredentialStore {
    let no_live_keys = std::env::var(NO_LIVE_KEYS_ENV).ok();
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    smoke_store_from(no_live_keys.as_deref(), settings_dir.as_deref())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
}

/// [`smoke_store`] over the two variables' values (the test's seam). It only stats the database:
/// nothing here creates, opens or touches it.
///
/// # Errors
/// The words [`smoke_store`] panics with: `CredentialScopeError`'s, or the named settings
/// directory and the database path that is not there.
pub(super) fn smoke_store_from(
    no_live_keys: Option<&str>,
    settings_dir: Option<&str>,
) -> Result<CredentialStore, String> {
    let scope = CredentialScope::parse(no_live_keys).map_err(|e| e.to_string())?;
    let dir = workspace_settings_dir_from(settings_dir);
    if nonblank(settings_dir).is_some() && backend_in(&dir) == StoreBackend::Absent {
        return Err(format!(
            "VIKE_SETTINGS_DIR names {}, which holds no settings database ({} is not there): a \
             smoke refuses rather than skip as if the store held no credentials. Point \
             VIKE_SETTINGS_DIR at the settings directory holding db/vike.db, or unset it to read \
             the project of the working directory",
            dir.display(),
            db_path_in(&dir).display()
        ));
    }
    Ok(CredentialStore::open(Some(StoreLocation::new(dir)), scope))
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
        store.credential_registry(),
    )
}
