//! Which store a `secrets` verb reads: the settings-directory resolution every verb shares.
//!
//! The `--file PATH` flag is gone: the settings database is the only store, and `grammar`'s
//! `FILE_FLAG_REMOVED` is what a script that still passes it is told.

use std::path::Path;

use super::settings_dir_of;

/// Open the store this verb should report on: whichever store answers for the project — the
/// settings database, or none (an empty map).
pub(super) fn resolve_store(
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<vike_secrets::Resolved, vike_secrets::SecretsError> {
    vike_secrets::resolve_store_in(
        &settings_dir_of(settings_dir, settings_dir_override),
        vike_secrets::Table::Credential,
    )
}
