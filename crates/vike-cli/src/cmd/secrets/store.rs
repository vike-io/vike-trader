//! Which store a `secrets` verb reads: the settings-directory resolution every verb shares.
//!
//! ⚠ **Until 2026-10-07 this file also carried the `--file PATH` flag's whole resolution** — a
//! credential FILE named on the command line, read as text, its own shadowing directory, and the
//! refusal of a `--file` that named a settings database. The credential FILE store was removed that
//! day (the settings database is the only store), and the flag went with it: `grammar`'s
//! `FILE_FLAG_REMOVED` is what a script that still passes it is told.

use std::path::{Path, PathBuf};

use super::settings_dir_of;

/// The retired credential FILE this invocation's findings name — `<settings>/secrets.env` inside
/// the settings directory the DISPATCHER resolved, else the walk from the working directory, under
/// the SAME `$VIKE_SETTINGS_DIR` value that dispatcher's boot was handed. Not a store: the settings
/// database beside it is the only one, and `list`/`path` use this path only to say the file is
/// shadowed or NOT READ.
///
/// PURE — no I/O. BOTH `settings_dir` and `settings_dir_override` come from `crate::run`'s single
/// environment sweep, not from a read of this library file's own.
///
/// ⚠ **The last arm takes the override**, and an override-BLIND spelling is wrong there:
/// `vike_boot::boot` used to resolve the directory as `spec.cwd.and_then(..)`, so with no readable
/// working directory it yielded `None` while still returning the override — and a blind resolver then
/// fell through to the RELATIVE last resort while every daemon on the box read the named directory.
/// The upstream cause is fixed (`vike_boot::boot` calls `vike_secrets::project_settings_dir_for`),
/// so this arm is unreachable from `crate::run`; it is KEPT as the belt, and
/// `the_store_honours_the_override_when_no_settings_dir_was_resolved` tests it directly.
pub(super) fn store_path(
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> PathBuf {
    vike_secrets::secrets_path_in(&settings_dir_of(settings_dir, settings_dir_override))
}

/// Open the store this verb should report on: whichever store answers for the project — the
/// settings database, or none (an empty map with the absent arm's findings).
pub(super) fn resolve_store(
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<vike_secrets::Resolved, vike_secrets::SecretsError> {
    vike_secrets::resolve_store_in(
        &settings_dir_of(settings_dir, settings_dir_override),
        vike_secrets::Table::Credential,
    )
}
