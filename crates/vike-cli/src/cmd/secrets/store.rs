//! Which store a `secrets` verb reads: the settings-directory resolution every verb shares, opened
//! as the one `vike_secrets::CredentialStore` handle under the process's declared scope.
//!
//! The `--file PATH` flag is gone: the settings database is the only store, and `grammar`'s
//! `FILE_FLAG_REMOVED` is what a script that still passes it is told.

use std::path::Path;

use super::settings_dir_of;

/// The store this verb should report on: whichever store answers for the project — the settings
/// database, or none (every read answers empty) — as ONE handle, opened over [`settings_dir_of`]'s
/// directory under `scope`, the scope the boot parsed. No I/O: each read asks the backend afresh.
pub(super) fn credential_store(
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
    scope: vike_secrets::CredentialScope,
) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings_dir_of(
            settings_dir,
            settings_dir_override,
        ))),
        scope,
    )
}
