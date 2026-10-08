//! WHICH store answers a run (`Backend`, per-RUN) and the front doors that open it: `resolve*`.

use super::*;

/// **IS THERE A CREDENTIAL STORE ON THIS BOX** — the one decision every reader and every writer in
/// this crate asks first, and the reason there is no ladder.
///
/// # The settings database is the ONLY credential store
///
/// `<project>/settings/db/vike.db` holds every credential and every node key
/// (`docs/decisions/0054-settings-move-into-one-database.md`,
/// `docs/decisions/0086-settings-live-only-in-the-database.md`). The FILE store that answered a box
/// with no database — `secrets.env` and `node.env`, parsed on every read and rewritten by a
/// byte-preserving upsert on every write — is GONE, on the owner's order of 2026-10-07 (*"we don't
/// use any files anymore, we use sqlite: remove the file plane"*). Its precondition was
/// `vike-cli secrets migrate --init`, which creates the store directly in the database
/// (`docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`, *What would reopen
/// this*). This enum's `Backend::Files` arm was that store's per-run answer and went with it.
///
/// # Per-RUN, never per-KEY
///
/// One `is_file` on one path, made ONCE, and after it every name has exactly one home for the life
/// of the process: the database, or nowhere. A key missing from the database is MISSING and hits the
/// live gate — there is no second lookup, in a file or anywhere else.
///
/// The probe is the DATABASE'S EXISTENCE and nothing else — not its contents, not its row count.
/// A database that exists and cannot be opened, or is not this schema, is a loud [`SecretsError`].
///
/// # What a box with NO database gets
///
/// * **Every READ answers an empty map** — [`Source::None`], the live gate: no credentials, so every
///   venue stays paper. Never a panic, never an environment variable read in place of a credential
///   (`docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md`).
/// * **A credential FILE left on disk is SAID OUT LOUD**, never read: see
///   [`UnreadCredentialFile`]. A box whose keys still sit in a `secrets.env` would otherwise drop
///   every venue to paper in complete silence.
/// * **Every WRITE is refused** naming [`CREATE_STORE_REMEDY`]: nothing in this crate creates the
///   database except `vike-cli secrets migrate`.
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
pub const CREATE_STORE_REMEDY: &str = "create it with `vike-cli secrets migrate --init` on a fresh \
     box, or carry an existing secrets.env/node.env into it with `vike-cli secrets migrate` \
     (`--dry-run` first; it only READS those files and never edits, moves or deletes them)";

/// The credential file that is still on disk and is NOT READ, because the database answered.
///
/// A finding, never a refusal — same posture as [`PermissionWarning`] and returned as DATA for the
/// same reason (this crate carries no logging dependency). Its twin for a box with NO database is
/// [`UnreadCredentialFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowedStore {
    /// The file that is present and unread.
    pub file: PathBuf,
    /// The database that answered instead.
    pub db: PathBuf,
}

impl std::fmt::Display for ShadowedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is still on disk but is NO LONGER READ — the settings database {} is the only \
             credential store. Nothing has moved or deleted it; an edit to that file changes \
             nothing until it is migrated in (`vike-cli secrets set` writes the database).",
            self.file.display(),
            self.db.display()
        )
    }
}

/// What a front door found: the credentials, WHICH store they came from, and any finding about that
/// store — its exposure, a leftover predecessor, a file the database shadows, or a credential file
/// on a box that has no store at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The credential map. Empty when there is no store — the live gate.
    pub secrets: SecretMap,
    /// Which store answered.
    pub source: Source,
    /// Set when the store is readable beyond its owner. The CALLER logs or prints it; see
    /// [`PermissionWarning`].
    pub warning: Option<PermissionWarning>,
    /// Set when there is no store and the pre-one-store [`LEGACY_STORE_FILE`] is sitting beside the
    /// project. The CALLER logs or prints it; see [`LegacyStoreWarning`].
    pub legacy: Option<LegacyStoreWarning>,
    /// Set when the DATABASE answered and the file it replaced is still on disk. The CALLER logs or
    /// prints it; see [`ShadowedStore`].
    pub shadowed: Option<ShadowedStore>,
    /// Set when there is NO database and a credential file is still on disk — unread. The CALLER
    /// logs it LOUDLY (it is the one state in which a configured box silently runs on paper); see
    /// [`UnreadCredentialFile`].
    pub unread: Option<UnreadCredentialFile>,
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
    backend_at(&crate::dotenv::workspace_db_path_from(settings_dir))
}

/// [`workspace_backend_from`] for a caller that already holds the settings DIRECTORY as a path.
///
/// **[`resolve_store_in`] and [`save_credentials_to_store`] both call exactly this**, which is what
/// makes the reader and the writer structurally incapable of disagreeing about whether there is a
/// store: they take the same two arguments and ask the same function.
#[must_use]
pub fn backend_in(settings_dir: &Path) -> Backend {
    backend_at(&crate::dotenv::db_path_in(settings_dir))
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
/// `crates/vike-secrets/src/dotenv.rs` it means *the override AND the cwd arrive as parameters*), and
/// neither is a bare `backend`, which in this workspace means THE VIKE-TRADEHUB DAEMON.
#[must_use]
pub fn backend_at(db: &Path) -> Backend {
    if crate::db::database_present(db) {
        Backend::Database(db.to_path_buf())
    } else {
        Backend::Absent
    }
}

/// The credential FILE name a table used to be served from — `secrets.env` for
/// [`crate::db::Table::Credential`], `node.env` for [`crate::db::Table::NodeKey`] — inside
/// `settings_dir`. Used ONLY to REPORT a file that is no longer read; nothing opens it as a store.
pub(super) fn store_file_in(settings_dir: &Path, table: crate::db::Table) -> PathBuf {
    match table {
        crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
        crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
    }
}

/// Read one table of the settings database as a [`Resolved`], reporting the file it shadows.
///
/// Only ever called once [`backend_at`] has established the database is there, so every failure
/// below is "the store EXISTS and could not be read" — the LOUD arm, never an empty map.
pub(super) fn resolve_database(
    db: &Path,
    table: crate::db::Table,
    shadows: &Path,
) -> Result<Resolved, SecretsError> {
    let secrets = crate::db::read_table(db, table)?;
    Ok(Resolved {
        secrets,
        source: Source::Database(db.to_path_buf()),
        // A finding, never a refusal — see `permission_warning`.
        warning: permission_warning(db),
        legacy: None,
        shadowed: shadows
            .exists()
            .then(|| ShadowedStore { file: shadows.to_path_buf(), db: db.to_path_buf() }),
        unread: None,
    })
}

/// **The answer for a box with NO settings database**: no credentials (the live gate), plus every
/// finding that tells an operator WHY.
///
/// Infallible on purpose. There is no store to fail to open; a credential FILE that cannot be read
/// is still reported (as [`UnreadCredentialFile`] with the OS reason), because what matters is that
/// it is not a store any more, and the map stays empty either way.
///
/// The pre-one-store `<project>/.env` finding is asked only for the CREDENTIAL table and only when
/// no `secrets.env` is there: a `secrets.env` on disk is already the louder finding, and
/// [`legacy_store_warning`]'s own doc says why a `.env` beside anything else is noise.
pub(super) fn resolve_absent(settings_dir: &Path, table: crate::db::Table) -> Resolved {
    let file = store_file_in(settings_dir, table);
    let unread = unread_credential_file(&file, &crate::dotenv::db_path_in(settings_dir));
    let legacy = match table {
        crate::db::Table::Credential if unread.is_none() => legacy_store_warning(&file),
        _ => None,
    };
    Resolved {
        secrets: SecretMap::default(),
        source: Source::None,
        warning: None,
        legacy,
        shadowed: None,
        unread,
    }
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
        &crate::dotenv::workspace_settings_dir_from(settings_dir),
        crate::db::Table::Credential,
    )
}

/// **Open the store that answers for `settings_dir` and `table`** — the one front door, for a caller
/// that already holds the settings DIRECTORY.
///
/// | on disk | result |
/// |---|---|
/// | the settings database, readable | the table's rows, [`Source::Database`] (+ [`ShadowedStore`] if a file is beside it) |
/// | the settings database, unreadable | [`SecretsError`] — loud, never "no credentials" |
/// | no settings database | an EMPTY map, [`Source::None`] — the live gate (+ [`UnreadCredentialFile`] if a file is on disk) |
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
/// setting's legacy name is refused at boot (`vike_config::refuse_stranded_venue_settings`).
pub fn resolve_store_in(
    settings_dir: &Path,
    table: crate::db::Table,
) -> Result<Resolved, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => resolve_database(&db, table, &store_file_in(settings_dir, table)),
        Backend::Absent => Ok(resolve_absent(settings_dir, table)),
    }
}
