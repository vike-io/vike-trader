//! WHICH store answers a run (`Backend`, per-RUN) and the front doors that open it: `resolve*`.

use super::*;

/// **IS THERE A CREDENTIAL STORE ON THIS BOX** — the one decision every reader and every writer in
/// this crate asks first, and the reason there is no ladder.
///
/// # The settings database is the ONLY credential store
///
/// `<project>/settings/db/vike.db` holds every credential and every node key
/// (`docs/decisions/0054-settings-move-into-one-database.md`,
/// `docs/decisions/0086-settings-live-only-in-the-database.md`). `vike-cli secrets init`
/// creates it on a fresh box.
///
/// # Per-RUN, never per-KEY
///
/// One `is_file` on one path, made ONCE, and after it every name has exactly one home for the life
/// of the process: the database, or nowhere. A key missing from the database is MISSING and hits the
/// live gate — there is no second lookup.
///
/// The probe is the DATABASE'S EXISTENCE and nothing else — not its contents, not its row count.
/// A database that exists and cannot be opened, or is not this schema, is a loud [`SecretsError`].
///
/// # What a box with NO database gets
///
/// * **Every READ answers an empty map** — [`Source::None`], the live gate: no credentials, so every
///   venue stays paper. Never a panic, never an environment variable read in place of a credential
///   (`docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md`).
/// * **Every WRITE is refused** naming [`CREATE_STORE_REMEDY`]: nothing in this crate creates the
///   database except `vike-cli secrets init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// `<project>/settings/db/vike.db` exists. It answers, wholly, for both key classes.
    Database(PathBuf),
    /// No settings database: there is NO credential store on this box. Reads answer empty (the live
    /// gate); writes refuse.
    Absent,
}

/// What a writer, and every refusal that stands in for one, tells an operator to do on a box with
/// no settings database — ONE spelling, so the daemon, the CLI and the GUI cannot word the only way
/// in five ways.
pub const CREATE_STORE_REMEDY: &str = "create it with `vike-cli secrets init` on a fresh box";

/// What a front door found: the credentials, WHICH store they came from, and the store's exposure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The credential map. Empty when there is no store — the live gate.
    pub secrets: SecretMap,
    /// Which store answered.
    pub source: Source,
    /// Set when the store is readable beyond its owner. The CALLER logs or prints it; see
    /// [`PermissionWarning`].
    pub warning: Option<PermissionWarning>,
}

/// Which store answers for this project — [`Backend`], and the ONE place the choice is made.
///
/// The entry point for a caller holding nothing but the override. `settings_dir` is
/// [`crate::SETTINGS_DIR_ENV`]'s value, carried as a PARAMETER like every other resolver's: this
/// crate reads no environment. ⚠ It reads the working directory (through
/// [`crate::workspace_db_path_from`]) when the override is absent; [`backend_in`] and [`backend_at`]
/// are the pure spellings.
#[must_use]
pub fn workspace_backend_from(settings_dir: Option<&str>) -> Backend {
    backend_at(&crate::store_locator::workspace_db_path_from(settings_dir))
}

/// [`workspace_backend_from`] for a caller that already holds the settings DIRECTORY as a path.
///
/// **[`resolve_store_in`] and [`save_credentials_to_store`] both call exactly this**, which is what
/// makes the reader and the writer structurally incapable of disagreeing about whether there is a
/// store: they take the same two arguments and ask the same function.
#[must_use]
pub fn backend_in(settings_dir: &Path) -> Backend {
    backend_at(&crate::store_locator::db_path_in(settings_dir))
}

/// The PURE root of the three — the database path arrives as a parameter.
///
/// | spelling | the caller holds | reads the process CWD? | the path resolver it wraps |
/// |---|---|---|---|
/// | [`workspace_backend_from`] | the OVERRIDE alone — `Option<&str>` | YES — the override wins inside | [`crate::workspace_db_path_from`] |
/// | [`backend_in`] | the settings DIRECTORY — `&Path` | no — pure | [`crate::db_path_in`] |
/// | [`backend_at`] | the DATABASE's own path — `&Path` | no — pure | — it IS the path |
///
/// A sibling added later is named after the path resolver it wraps. `_for` is not available (in
/// `crates/vike-secrets/src/store_locator.rs` it means *the override AND the cwd arrive as parameters*), and
/// neither is a bare `backend`, which in this workspace means THE VIKE-TRADEHUB DAEMON.
#[must_use]
pub fn backend_at(db: &Path) -> Backend {
    if crate::db::database_present(db) {
        Backend::Database(db.to_path_buf())
    } else {
        Backend::Absent
    }
}

/// Read one table of the settings database as a [`Resolved`].
///
/// Only ever called once [`backend_at`] has established the database is there, so every failure
/// below is "the store EXISTS and could not be read" — the LOUD arm, never an empty map.
pub(super) fn resolve_database(
    db: &Path,
    table: crate::db::Table,
) -> Result<Resolved, SecretsError> {
    let secrets = crate::db::read_table(db, table)?;
    Ok(Resolved {
        secrets,
        source: Source::Database(db.to_path_buf()),
        // A finding, never a refusal — see `permission_warning`.
        warning: permission_warning(db),
    })
}

/// **The answer for a box with NO settings database**: no credentials — the live gate.
///
/// Infallible on purpose: there is no store to fail to open.
pub(super) fn resolve_absent() -> Resolved {
    Resolved { secrets: SecretMap::default(), source: Source::None, warning: None }
}

/// [`resolve_store_in`] over the PROJECT's own settings directory, found by
/// [`crate::workspace_settings_dir_from`].
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value, which names the directory outright and
/// wins over the walk. It arrives as a PARAMETER: this crate reads no environment.
///
/// `vike_bridge_core::credentials::load_workspace_secrets_at` is the infallible wrapper over this.
pub fn resolve_project(settings_dir: Option<&str>) -> Result<Resolved, SecretsError> {
    resolve_store_in(
        &crate::store_locator::workspace_settings_dir_from(settings_dir),
        crate::db::Table::Credential,
    )
}

/// **Open the store that answers for `settings_dir` and `table`** — the one front door, for a caller
/// that already holds the settings DIRECTORY.
///
/// | on disk | result |
/// |---|---|
/// | the settings database, readable | the table's rows, [`Source::Database`] |
/// | the settings database, unreadable | [`SecretsError`] — loud, never "no credentials" |
/// | no settings database | an EMPTY map, [`Source::None`] — the live gate |
///
/// * [`Table::Credential`](crate::db::Table::Credential) ↔ the `credential` table
/// * [`Table::NodeKey`](crate::db::Table::NodeKey) ↔ the `node_key` table
///
/// ⚠ **It is deliberately NOT one of `crates/vike-ops/tests/settings_secrets/settings_registry/credential_store_scan.rs`'s
/// `CREDENTIAL_STORE_READERS`**: its settings directory is a mandatory `&Path` parameter, so it
/// cannot express the defect that ratchet hunts (a library walking for the store from a working
/// directory nothing can redirect).
///
/// ⚠ **It answers the credential TABLE and nothing else.** `venue_setting` rows are read through
/// [`crate::venue_setting::VenueSettings`] alone (decision 0095); a credential row still carrying a
/// setting's legacy name is read by nothing.
pub fn resolve_store_in(
    settings_dir: &Path,
    table: crate::db::Table,
) -> Result<Resolved, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => resolve_database(&db, table),
        Backend::Absent => Ok(resolve_absent()),
    }
}
