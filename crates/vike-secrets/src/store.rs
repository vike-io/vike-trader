//! Opening the credential store: `<project>/settings/secrets.env`.
//!
//! [`resolve`] opens a path the caller names; [`resolve_project`] is the entry point for a caller
//! with no opinion, which asks [`crate::workspace_dotenv_path_from`] for the project's own file.
//! There is one file, so there is no precedence to implement here — only reading it, reporting
//! where the answer came from, and reporting a permission finding on it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// A credential map. Redacts in `Debug`; never implements `Display`.
///
/// `Debug` shows the KEY NAMES and the count but never a value — the same balance
/// `vike_bridge_core::credentials::Credentials` strikes with `api_key=***{last4}`. Key names are
/// already public knowledge (they are the `vike_ops::settings` registry's whole subject matter);
/// values are the credential.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretMap(BTreeMap<String, String>);

impl SecretMap {
    pub fn new(map: BTreeMap<String, String>) -> Self {
        SecretMap(map)
    }

    /// Build from the `HashMap<String, String>` shape the rest of the workspace speaks.
    pub fn from_map(map: HashMap<String, String>) -> Self {
        SecretMap(map.into_iter().collect())
    }

    /// Hand back the `HashMap<String, String>` every existing credential call site expects. Named
    /// so the call site reads as the deliberate end of redaction that it is.
    pub fn into_map(self) -> HashMap<String, String> {
        self.0.into_iter().collect()
    }

    /// The sorted key names — safe to print, and what `vike-cli secrets list` shows.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// **Fold ONE rendered `venue_setting` name in, unless the map already answers for it.**
    /// `true` means it COLLIDED and nothing was written.
    ///
    /// ⚠ `credential` wins, and that is the only safe disposition mid-migration: a name both
    /// tables answer for means the move is half-done and the two may disagree, so keeping the
    /// credential value resolves to exactly what this box resolved BEFORE the move started.
    /// Preferring the settings row would change what a live reader resolves at the moment a
    /// half-finished migration exists. [`fold_rendered_names`] reports the collision instead.
    ///
    /// `pub(crate)`: the fold is the STORE's, not a caller's — see [`resolve_store_in`].
    pub(crate) fn fold_in(&mut self, name: &str, value: &str) -> bool {
        match self.0.entry(name.to_string()) {
            std::collections::btree_map::Entry::Occupied(_) => true,
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(value.to_string());
                false
            }
        }
    }
}

impl std::fmt::Debug for SecretMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretMap({} entries: [", self.0.len())?;
        for (i, k) in self.0.keys().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{k}=***")?;
        }
        f.write_str("])")
    }
}

/// The one thing that can go wrong: the store EXISTS and could not be read.
///
/// An ABSENT store is not this — it is [`Source::None`] and an empty map, which is the live gate
/// (no credentials ⇒ every venue stays paper). "Not configured" and "cannot open" must never look
/// the same to an operator, which is the whole reason this type exists.
///
/// Carries the PATH and the OS reason, never file contents — so `Debug`/`Display` are safe to log
/// verbatim, which is the point: an operator has to be able to see "the store did not open" in a
/// daemon log without that log becoming a credential.
#[derive(Debug)]
pub struct SecretsError {
    /// The file that could not be read.
    pub path: PathBuf,
    /// Why the OS refused.
    pub source: std::io::Error,
}

impl std::fmt::Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credential store {} could not be read: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for SecretsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Where a resolved credential map came from — the provenance `vike-cli secrets` prints so an
/// operator is never guessing which file is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Parsed from this file.
    File(PathBuf),
    /// Read from a TABLE in the settings database at this path —
    /// `docs/decisions/0054-settings-move-into-one-database.md`.
    ///
    /// ⚠ **A fourth word in a consumer-visible domain.** `vike-cli secrets path --json` prints this
    /// provenance, so a consumer that matched on the two old spellings sees a new one the day a box
    /// migrates. It is added rather than folded into [`Source::File`] because folding would print a
    /// `.db` path in a sentence that says "file" and give an operator a path they can `cat`, which
    /// is precisely the thing constraint 2 of 0054 says must be replaced before it is removed.
    Database(PathBuf),
    /// Neither store exists. An empty map: the live gate (no credentials ⇒ stay paper).
    None,
}

/// Something about the credential file's PLACEMENT that its reader ought to know.
///
/// Returned as DATA rather than logged here: this crate has no dependencies at all (see the crate
/// doc) and must not grow a logging one for a warning string. `vike_bridge_core::credentials` logs
/// it through `tracing`; `vike-cli secrets` prints it on stderr.
///
/// **A finding is never a refusal.** Refusing to read a 0644 file would strand somebody mid-setup
/// with every venue on paper — strictly worse than the exposure it objects to. Reading one
/// SILENTLY is worse still, which is why this exists. The same reasoning covers
/// [`Finding::Symlink`] for a different reason: a symlinked store is a LEGITIMATE setup, and the
/// finding says where the file really is rather than objecting to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionWarning {
    /// The path that was asked about — the one the operator was shown, which for a symlink is not
    /// the file that is read.
    pub path: PathBuf,
    /// What was found there.
    pub finding: Finding,
}

/// The two things [`permission_warning`] can find. Separate variants because the ADVICE differs:
/// one is fixed with `chmod`, the other is not a defect at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// `st_mode & 0o777` grants something to group or other. Unix only.
    ExposedMode(u32),
    /// The path is a SYMLINK: the credentials actually read live elsewhere, and this path says
    /// nothing about that file's owner, its mode, or who may replace it in ITS directory.
    ///
    /// `target` is `read_link`'s answer (`None` only if the link became unreadable between the two
    /// calls); `target_mode` is the followed `st_mode & 0o777`, i.e. the mode of the file that
    /// `read_to_string` will actually open, and `None` when the link dangles.
    Symlink { target: Option<PathBuf>, target_mode: Option<u32> },
}

impl std::fmt::Display for PermissionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.path.display();
        match &self.finding {
            Finding::ExposedMode(mode) => write!(
                f,
                "{p} holds live venue credentials in PLAINTEXT and is readable beyond its owner \
                 (mode {mode:04o}); run `chmod 600 {p}`"
            ),
            Finding::Symlink { target, target_mode } => {
                write!(f, "{p} is a SYMLINK, so the live venue credentials actually read are ")?;
                match target {
                    Some(t) => write!(f, "at {}", t.display())?,
                    None => write!(f, "elsewhere")?,
                }
                match target_mode {
                    Some(m) => write!(f, " (mode {m:04o})")?,
                    None => write!(f, " (a DANGLING link — nothing is there)")?,
                }
                write!(
                    f,
                    "; check that file's owner and the permissions of the directory holding it — \
                     this path's own mode says nothing about either"
                )
            }
        }
    }
}

/// `Some` when the credential store at `path` is a symlink, or when it grants any permission to
/// group or other (`mode & 0o077`).
///
/// Read-write-execute across both classes, not just read: a group-WRITABLE credential file lets
/// somebody substitute the keys an order is signed with, which is worse than letting them read it.
///
/// ⚠ **The stat is `symlink_metadata`, not `metadata`, and that is the difference between seeing a
/// symlinked store and not.** `metadata` FOLLOWS the link, so an owner-only target reported clean
/// and the indirection itself was invisible — while the path the operator was shown told them
/// nothing about where their credentials live, who owns that file, or who can replace it in its
/// own directory (a 0600 file in a 0777 directory is anybody's to substitute). On the regular-file
/// case the two calls are identical, so nothing about an ordinary install changes.
///
/// ⚠ **A symlink's OWN mode is not reported, and must not be**: on Linux it is 0777 by
/// construction and the kernel ignores it, so folding it into the `mode & 0o077` test would warn
/// on every symlinked store forever and tell the operator to `chmod` something that is already
/// irrelevant. The mode carried on that variant is the TARGET's — what `read_to_string` opens.
///
/// **Unix only.** On Windows the mode arm is a `None`-returning no-op: the mode bits do not exist
/// there and the equivalent question is an ACL query, which needs a Win32 crate this workspace does
/// not carry. The SYMLINK arm is not unix-specific, but reporting it alone on Windows would be a
/// finding this crate cannot pair with the permission question that gives it meaning, so the
/// Windows no-op is left exactly as it was.
///
/// ⚠ **`pub` so a caller can ask the question WITHOUT opening the store.** This performs one or two
/// `stat` calls and a `readlink`; it never reads the file's CONTENTS, so it pulls no credential
/// value into the process. That distinction is the whole reason it is exported: [`resolve`] answers
/// the same question, but only as a side effect of `read_to_string` + `parse_dotenv`, so a command
/// whose documented contract is "opens nothing" — `vike-cli secrets path`, the command the README
/// and the ops runbook name FIRST — could not reach the finding, and therefore reported a
/// world-writable credential file in silence.
#[cfg(unix)]
pub fn permission_warning(path: &Path) -> Option<PermissionWarning> {
    use std::os::unix::fs::PermissionsExt;
    let link = std::fs::symlink_metadata(path).ok()?;
    if link.file_type().is_symlink() {
        return Some(PermissionWarning {
            path: path.to_path_buf(),
            finding: Finding::Symlink {
                target: std::fs::read_link(path).ok(),
                // FOLLOWED on purpose: the mode worth reporting is the one of the file that is
                // actually read, not the link's meaningless 0777.
                target_mode: std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o777),
            },
        });
    }
    let mode = link.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then(|| PermissionWarning {
        path: path.to_path_buf(),
        finding: Finding::ExposedMode(mode),
    })
}

#[cfg(not(unix))]
pub fn permission_warning(_path: &Path) -> Option<PermissionWarning> {
    None
}

/// The credential store this project used to have: `<project>/.env`.
///
/// A tombstone, kept as a named constant for the same reason `vike_config`'s
/// `REMOVED_PROJECT_FILE` is: it is the string [`legacy_store_warning`], its tests and the upgrade
/// note (`docs/ops/upgrading.md`) all have to agree on, and a name spelled once cannot drift from
/// the message that names it. Nothing reads the file.
pub const LEGACY_STORE_FILE: &str = ".env";

/// **A pre-one-store credential store left beside the project while the real store is ABSENT.**
///
/// The upgrade path was silent exactly here. [`resolve`] answers [`Source::None`], the map comes
/// back empty, every venue loader turns that into `None`, and every venue stays paper — which is the
/// CORRECT behaviour for a box with no credentials and an INDISTINGUISHABLE one for a box whose
/// credentials are sitting in the file that used to be read. The operator's symptom is "my orders
/// aren't reaching the venue", with nothing in any log.
///
/// Returned as DATA, like [`PermissionWarning`] and for the same reason: this crate has no
/// dependencies at all, a logging one included. `vike_bridge_core::credentials` logs it through
/// `tracing`; `vike-cli secrets` prints it on stderr.
///
/// ⚠ **A finding, never a refusal — a deliberate departure from
/// `vike_config::refuse_removed_project_file`, which REFUSES its leftover.** A `vike.toml` has no
/// other meaning, so refusing over one is safe. A `.env` does: it is a systemd `EnvironmentFile`,
/// and the CI box's live recorder ships `EnvironmentFile=-<project>/.env` holding `POLY_PROXY_ENABLED`
/// and no credential at all. Refusing would make a genuinely fresh, correct install unstartable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyStoreWarning {
    /// The leftover: `<project>/.env`. Its CONTENTS are never read — see [`legacy_store_warning`].
    pub legacy: PathBuf,
    /// The store that was looked for and is not there.
    pub store: PathBuf,
}

impl std::fmt::Display for LegacyStoreWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (legacy, store) = (self.legacy.display(), self.store.display());
        let dir = self.store.parent().unwrap_or(Path::new(".")).display();
        write!(
            f,
            "{legacy} is present but the credential store {store} is NOT — nothing has read a \
             project-root `{LEGACY_STORE_FILE}` since credentials moved into the settings \
             directory, so every venue stays paper. If that file holds venue credentials, copy \
             them into the store: `mkdir -p {dir} && cp {legacy} {store} && chmod 600 {store}`. \
             If it is a systemd EnvironmentFile of tunables it is still doing its job, and this \
             says only that the store is missing. Nothing has been moved or deleted for you."
        )
    }
}

/// `Some` when the pre-one-store [`LEGACY_STORE_FILE`] sits beside the project whose `store` this
/// is. **Ask only when the store is ABSENT** — [`resolve`] does, and so does `vike-cli secrets`.
///
/// The gate matters as much as the probe. A `.env` beside a store that LOADED is a systemd
/// `EnvironmentFile` doing its job (the CI box's recorder), and a warning that fires on a
/// correctly-configured box every time is one everybody learns to scroll past. An absent store is
/// precisely the silent case and nothing else.
///
/// `store` is `<project>/settings/secrets.env`, so the project is its GRANDparent — one
/// [`Path::parent`] more than `vike_config::refuse_removed_project_file`'s, which starts from the
/// settings directory. `None` when there is no such ancestor: there is then no project beside which
/// a leftover could be misleading anybody.
///
/// ⚠ **It never opens the file.** [`std::fs::metadata`] answers existence, and the contents are the
/// operator's credentials — no diagnostic needs them. Only `NotFound` counts as absent: any other
/// error means absence could not be ESTABLISHED, and a file we cannot see is exactly the one someone
/// would believe is in force (the same rule `refuse_removed_project_file` applies).
pub fn legacy_store_warning(store: &Path) -> Option<LegacyStoreWarning> {
    let project = store.parent()?.parent()?;
    let legacy = project.join(LEGACY_STORE_FILE);
    match std::fs::metadata(&legacy) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        _ => Some(LegacyStoreWarning { legacy, store: store.to_path_buf() }),
    }
}

/// **WHICH STORE ANSWERS THIS RUN** — the one decision `docs/decisions/0054`'s credential half
/// turns on, and the reason there is no ladder.
///
/// # Per-RUN, never per-KEY
///
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md`'s rule is *"a name has one home
/// decided statically; no ladder"*. Two shapes were available here and only one obeys it:
///
/// * **per-KEY fallback** (*look in the database; if this name is not there, look in the file*) —
///   forbidden. It is a precedence chain by construction, it makes "where is my Binance key" a
///   question with two answers, and on a half-filled database it reads half from each, which is the
///   one outcome the brief for this work singles out.
/// * **per-RUN choice** (*this project has a database, therefore the database answers WHOLLY;
///   otherwise the files answer wholly*) — this. One `is_file` on one path, made ONCE, and after it
///   every name has exactly one home for the life of the process. A key that is missing from the
///   answering store is MISSING, exactly as a key missing from `secrets.env` is missing today, and
///   it hits the live gate rather than a second lookup.
///
/// The probe is the DATABASE'S EXISTENCE and nothing else — not its contents, not its row count,
/// not whether it happens to carry the key somebody wants. A database that exists and cannot be
/// opened, or is not this schema, is a loud [`SecretsError`] and never a silent fall back to the
/// file: falling back there would be the ladder re-entering through the error path.
///
/// # What this must not do, and does not
///
/// * **A box with no database is bit-for-bit unchanged.** [`Backend::Files`] runs the same code it
///   ran before this type existed — same parser, same findings, same absent-arm live gate.
/// * **A box mid-migration never reads half from each.** The choice is made before any name is
///   looked up, so there is no per-name moment at which the other store could answer.
///
/// # The cost, stated rather than hidden
///
/// Once a database exists, an operator's edit to `secrets.env` is NOT READ. That is the point of
/// stage 2 and it is also a trap, because hand-editing that file is the documented workflow today —
/// so the file is REPORTED as shadowed rather than silently ignored: see [`Resolved::shadowed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// `<project>/settings/db/vike.db` exists. It answers, wholly, for both key classes.
    Database(PathBuf),
    /// No database. `secrets.env` and `node.env` answer, exactly as they did before 0054.
    Files,
}

/// The credential file that is still on disk and is NO LONGER READ, because the database answered.
///
/// A finding, never a refusal — same posture as [`PermissionWarning`] and returned as DATA for the
/// same reason (this crate carries no logging dependency). It exists because the shadowing is
/// invisible from the operator's side: every skill, runbook and CLI sentence in this tree today says
/// *edit `<project>/settings/secrets.env`*, and after the migration that edit changes nothing while
/// looking exactly like it worked.
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
            "{} is still on disk but is NO LONGER READ — the settings database {} answers now. \
             Nothing has moved or deleted it; an edit to that file changes nothing until it is \
             migrated in.",
            self.file.display(),
            self.db.display()
        )
    }
}

/// What [`resolve`] found: the credentials, WHICH store they came from, and any finding about that
/// store — its exposure, a leftover predecessor beside it, or a file the database now shadows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The credential map. Empty when the store does not exist — the live gate.
    pub secrets: SecretMap,
    /// Which store answered.
    pub source: Source,
    /// Set when the store is readable beyond its owner. The CALLER logs or prints it; see
    /// [`PermissionWarning`].
    pub warning: Option<PermissionWarning>,
    /// Set when the store is ABSENT and the pre-one-store [`LEGACY_STORE_FILE`] is sitting beside
    /// the project. The CALLER logs or prints it; see [`LegacyStoreWarning`].
    pub legacy: Option<LegacyStoreWarning>,
    /// Set when the DATABASE answered and the file it replaced is still on disk. The CALLER logs or
    /// prints it; see [`ShadowedStore`].
    pub shadowed: Option<ShadowedStore>,
    /// ⚠ **Names BOTH the `credential` table and the `venue_setting` rows answered for**, sorted.
    /// Empty is the normal state in both directions: before ruling 10's move nothing renders, and
    /// after a complete one nothing is left behind.
    ///
    /// A non-empty list means the move is HALF-DONE. The CREDENTIAL value is the one in force (see
    /// [`SecretMap::fold_in`]) and this is the fourth finding returned as DATA, for the same reason
    /// the three above it are: this crate carries no logging dependency.
    /// `vike_bridge_core::credentials::try_load_workspace_secrets_at` is where every composition
    /// root's read converges and is what surfaces it.
    pub collisions: Vec<String>,
}

/// **Open the credential store at `path`.**
///
/// | on disk | result |
/// |---|---|
/// | present and readable | the parsed map, [`Source::File`] |
/// | absent | an EMPTY map, [`Source::None`] — the live gate (no credentials ⇒ stay paper) |
/// | present and unreadable | [`SecretsError`] |
///
/// The absent arm is an ANSWER, not a failure: a checkout with no credentials is the normal state
/// of CI and of a fresh clone, and every venue loader turning that into `None` is the designed
/// behaviour. The unreadable arm is the opposite case and is loud, because a permissions bug that
/// silently degraded to "no credentials" would look exactly like a correct fresh install.
///
/// ⚠ The absent arm carries ONE extra finding: [`legacy_store_warning`], the pre-one-store
/// `<project>/.env`. An empty map is the right ANSWER for a box with no credentials and the wrong
/// one for a box whose credentials never moved, and from here the two are indistinguishable — so
/// the arm that produces the emptiness is where the difference has to be noticed. It is a finding,
/// not an error: the arm still returns the same empty map and the same [`Source::None`].
///
/// Nothing here ever writes, moves or deletes the file. It is the user's only copy of live venue
/// credentials.
pub fn resolve(path: &Path) -> Result<Resolved, SecretsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Resolved {
                secrets: SecretMap::default(),
                source: Source::None,
                warning: None,
                legacy: legacy_store_warning(path),
                shadowed: None,
                // This function was handed a PATH; it has no settings directory to look for
                // `venue_setting` rows beside. [`resolve_store_in`] is the layer that folds.
                collisions: Vec::new(),
            });
        }
        Err(source) => return Err(SecretsError { path: path.to_path_buf(), source }),
    };
    Ok(Resolved {
        secrets: SecretMap::from_map(crate::dotenv::parse_dotenv(&text)),
        source: Source::File(path.to_path_buf()),
        warning: permission_warning(path),
        // The store LOADED, so nothing was silent — and a `.env` beside a working store is a
        // systemd `EnvironmentFile`, not a leftover. See `legacy_store_warning`.
        legacy: None,
        // This function is the FILE arm by definition — it was handed a path and it read it. The
        // database never reaches here; `resolve_project` and `resolve_node_keys` choose above it.
        shadowed: None,
        // Same reason as the absent arm above: a path is not a settings directory.
        collisions: Vec::new(),
    })
}

/// Which store answers for this project — [`Backend`], and the ONE place the choice is made.
///
/// The entry point for a caller holding nothing but the override — and so the `workspace_*_from`
/// spelling, which is this crate's established name for exactly that shape.
/// [`backend_at`] carries the convention all three obey.
///
/// ⚠ **`_from` alone does NOT promise purity here, and reading it that way is what put the old
/// `backend_for` where it was.** In `vike_model::state_path` every `_from` resolver is
/// `(override, start)` and pure, so `_from` looks workspace-wide settled. It is not settled in THIS
/// crate: `crates/vike-secrets/src/dotenv.rs` carries both shapes at once, and the PREFIX decides
/// which. `project_secrets_path_from` and `project_settings_dir_from` are the `(override, start)`
/// pure pair that matches `state_path`; `workspace_dotenv_path_from`, `workspace_settings_dir_from`,
/// [`crate::workspace_node_path_from`] and [`crate::workspace_db_path_from`] take the override
/// ALONE and read `std::env::current_dir` inside. That second family is `backend_for`'s exact
/// polarity — one parameter, the cwd read behind the caller's back — which means the crate already
/// had a correct spelling for what that function did, five doors down, and the defect was never
/// that no name existed for the shape. It was that this one declined to use it.
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value, carried as a PARAMETER like every other
/// resolver's: this crate reads no environment. The database path comes from
/// [`crate::workspace_db_path_from`], i.e. the SAME walk the two files use, so a project cannot
/// resolve its database and its credential file to different projects — and this function is
/// NAMED after the one it calls, so the spelling and the derivation cannot drift apart.
///
/// Both database-aware resolvers call exactly this, so the GUI, the daemon and the CLI cannot answer
/// differently about which store is live — the same shape `vike_ops::reconcile_config::reconcile_gate`
/// uses for the reconcile default.
#[must_use]
pub fn workspace_backend_from(settings_dir: Option<&str>) -> Backend {
    backend_at(&crate::dotenv::workspace_db_path_from(settings_dir))
}

/// [`workspace_backend_from`] for a caller that already holds the settings DIRECTORY as a path —
/// the `_in` shape of the convention [`backend_at`] states.
///
/// The same decision over the same derivation —
/// `workspace_db_path_from(o) == db_path_in(&settings_dir_or_last_resort(o, cwd))` by construction —
/// so this is not a second probe, it is the same one reached without a `&str` round trip that a
/// non-UTF-8 project path would lose.
///
/// **[`resolve_store_in`] and [`save_credentials_to_store`] both call exactly this**, which is what
/// makes the reader and the writer structurally incapable of disagreeing about which store answers:
/// they take the same two arguments and ask the same function.
#[must_use]
pub fn backend_in(settings_dir: &Path) -> Backend {
    backend_at(&crate::dotenv::db_path_in(settings_dir))
}

/// The PURE root of the three — the database path arrives as a parameter, so every arm is
/// reachable from a test without a walk. [`workspace_backend_from`] and [`backend_in`] are both
/// this function over a path they derived first.
///
/// **The three spellings, and the convention anything added here later follows.** There is ONE
/// question — which store answers — asked from the three positions a caller can be standing in.
/// A name says WHAT THE CALLER ALREADY HOLDS, and whether the walk happens inside:
///
/// | spelling | the caller holds | reads the process CWD? | the path resolver it wraps |
/// |---|---|---|---|
/// | [`workspace_backend_from`] | the OVERRIDE alone — `Option<&str>` | YES — the override wins inside | [`crate::workspace_db_path_from`] |
/// | [`backend_in`] | the settings DIRECTORY — `&Path` | no — pure | [`crate::db_path_in`] |
/// | [`backend_at`] | the DATABASE's own path — `&Path` | no — pure | — it IS the path |
///
/// **The right-hand column is the whole convention, and it is not a parallel set of names to be
/// kept in step by hand.** Each row is literally the resolver beside it, wrapped:
/// `workspace_backend_from(o)` is `backend_at(&workspace_db_path_from(o))` and `backend_in(d)` is
/// `backend_at(&db_path_in(d))`. So these names are the call graph read aloud, and a sibling added
/// later is named after the path resolver it wraps — which also means it inherits that resolver's
/// answer to "does this walk", instead of asserting one.
///
/// ⚠ **Two spellings this family may NOT use, each for its own reason.**
///
/// `_for` is the first, and losing it is why the walking one moved. In
/// `crates/vike-secrets/src/dotenv.rs`'s `project_settings_dir_for` — and its
/// [`crate::node_path_for`] / [`crate::db_path_for`] siblings — `_for` means *both the override AND
/// the cwd arrive as parameters*, i.e. PURE, reading nothing. The retired `backend_for` spelled
/// that same suffix over the opposite polarity: one parameter, and the cwd read behind the caller's
/// back. Two functions one namespace apart answering "what does `_for` promise" in opposite
/// directions is the rot.
///
/// A bare `backend` is the second, and it would be a WORSE trade than the one it fixes. `backend`
/// is already a load-bearing noun in this workspace, and it means THE VIKE-TRADEHUB DAEMON:
/// `crates/vike-cli/src/lib.rs` routes a top-level verb `"backend"` into
/// `crates/vike-cli/src/cmd/node/mod.rs`'s `run`, so `vike-cli backend setup` stands a node up and
/// `backend connect` attaches a box to one. An unsuffixed `backend()` answering *which credential
/// STORE is live* would trade a suffix that is merely inconsistent for a STEM that is actively
/// wrong — and it would be shadowed by an ordinary `let backend = …` binding in this very file
/// ([`save_credentials_to_store`] has one).
///
/// ⚠ **That is a rule about QUALIFIERS, not about the stem, which is why [`Backend`] keeps it.**
/// This tree already runs two `backend` families side by side — this one, and
/// `crates/vike-app-core/src/backend_conn.rs`'s `BackendRecord` for *which daemon this box talks
/// to* — and what keeps them apart is that every member of both carries its crate's namespace plus
/// a qualifier of its own (`startup_backend_from`, `cli_observe_record`, `switch_backend` there;
/// `workspace_backend_from`, [`backend_in`], [`backend_at`] here). A type is read through its
/// namespace, `vike_secrets::Backend`, so it needs none. A bare free function is read as
/// `backend(…)`, and would be the one spelling in either family with no qualifier at all.
#[must_use]
pub fn backend_at(db: &Path) -> Backend {
    if crate::db::database_present(db) {
        Backend::Database(db.to_path_buf())
    } else {
        Backend::Files
    }
}

/// Read one table of the settings database as a [`Resolved`], reporting the file it shadows.
///
/// The database's own absent arm does not exist: this is only ever called once [`backend_at`] has
/// established the file is there, so every failure below is "the store EXISTS and could not be
/// read" — the LOUD arm, never an empty map. `crate::db::DbError`'s `From` impl is where that
/// mapping is argued.
fn resolve_database(
    db: &Path,
    table: crate::db::Table,
    shadows: &Path,
) -> Result<Resolved, SecretsError> {
    let secrets = crate::db::read_table(db, table)?;
    Ok(Resolved {
        secrets,
        source: Source::Database(db.to_path_buf()),
        // The same check the file store gets, on the artifact that now holds the credentials. A
        // finding, never a refusal — see `permission_warning`.
        warning: permission_warning(db),
        // `<project>/.env` is a pre-one-store leftover, and that question is about an ABSENT store.
        // A database that answered is not absent.
        legacy: None,
        shadowed: shadows
            .exists()
            .then(|| ShadowedStore { file: shadows.to_path_buf(), db: db.to_path_buf() }),
        // The fold is [`resolve_store_in`]'s, one layer up, where the settings DIRECTORY is in
        // hand — this function is reached for `node_key` too, and that namespace must never gain
        // a venue name (`docs/decisions/0051`).
        collisions: Vec::new(),
    })
}

/// [`resolve`] over the PROJECT's own store — `<project>/settings/secrets.env`, found by
/// [`crate::workspace_dotenv_path_from`].
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value, which names the directory outright and
/// wins over the walk. It arrives as a PARAMETER: this crate reads no environment, so the
/// composition root passes it down out of the one `std::env::vars()` sweep it already owns.
///
/// `vike_bridge_core::credentials::load_workspace_secrets_at` is the infallible wrapper over this.
///
/// ⚠ **Since `docs/decisions/0054`'s credential half, this asks [`workspace_backend_from`] FIRST.** With a
/// database present it reads the `credential` table and nothing else; with no database it reads the
/// file and nothing else. That choice is per-RUN and never per-KEY — [`Backend`] carries the whole
/// argument, including why the alternative is the ladder 0051 forbids.
pub fn resolve_project(settings_dir: Option<&str>) -> Result<Resolved, SecretsError> {
    resolve_store_in(
        &crate::dotenv::workspace_settings_dir_from(settings_dir),
        crate::db::Table::Credential,
    )
}

/// **Open the store that answers for `settings_dir` and `table`** — the one front door, for a caller
/// that already holds the settings DIRECTORY.
///
/// [`resolve_project`] is this function over the walk's answer with [`Table::Credential`], so every
/// composition root already reaches it. What it adds is a door for the callers that do NOT have a
/// `VIKE_SETTINGS_DIR` string to pass: a CLI verb whose own boot resolved the directory as a
/// `&Path`, and every writer, which must consult the same backend the reader will.
///
/// * [`Table::Credential`] ↔ `<settings_dir>/secrets.env`
/// * [`Table::NodeKey`] ↔ `<settings_dir>/node.env`
///
/// ⚠ **It is deliberately NOT one of `crates/vike-ops/tests/settings_registry.rs`'s
/// `CREDENTIAL_STORE_READERS`**, on that table's own stated criterion. The defect that ratchet
/// hunts is *a library walking for the store from a working directory nothing can redirect*, and a
/// function whose settings directory is a mandatory `&Path` parameter cannot express it — the same
/// reason [`resolve_node_keys`] is excluded there and says so.
///
/// ⚠ It carries NO legacy `secrets.env` fallback for [`Table::NodeKey`]. That fallback is
/// [`resolve_node_keys`]'s and belongs to the READ path for a box that has not migrated; a caller
/// asking "does the node store exist, and what is in it" before a WRITE must be told about the store
/// the write will land in and no other.
/// **THE FOLD — ruling 10's `venue_setting` rows, rendered back into the legacy credential names
/// every reader looks up.**
///
/// The renderer is [`crate::venue_setting_names`], which MOVED into this crate on 2026-09-22 for
/// exactly this: it used to live in `vike_bridge_core::credentials` (layer 30), so the fold could
/// only be applied by a caller that had already reached up there — and two of the three readers
/// had not. That module's doc carries both measured blindnesses.
///
/// `scope` narrows what is folded. `None` is the whole-map read; `Some` is the SCOPED one, and a
/// name the scope did not declare is skipped rather than added. The scope is the read's own
/// narrowing (owner ruling, 2026-09-16: *restrict what a process materialises*), and a fold that
/// ignored it would write every venue setting on the box into a map the process holds.
///
/// ⚠ **That is a MATERIALISATION property and nothing else — no caller can observe it, and that
/// was MEASURED rather than argued.** Two mutation runs on 2026-09-22 replaced `Some(scope)` with
/// `None` at the call site below and every behavioural test stayed green:
/// [`ScopedSecrets::narrow`] filters AFTER the fold so the returned map is identical, and the
/// database arm's base map comes from [`crate::read_table_scoped`], which BINDS the declared names
/// — so an unscoped fold has no undeclared credential row to collide with and the `collisions`
/// list is identical too. So the reason to pass the scope is the one the ruling gives (*a core
/// dump, a panic payload or a future logging bug reaches them all*), and the only thing that can
/// hold it is a structural check: `crates/vike-ops/tests/smoke_store_parity_gate.rs`'s
/// `the_scoped_fold_is_handed_its_scope`, measured red under that mutation.
/// `crates/vike-secrets/tests/venue_setting_fold.rs`'s
/// `the_scoped_fold_does_not_widen_past_its_scope` carries the same finding from the test side.
///
/// Returns the names BOTH tables answered for, sorted. See [`SecretMap::fold_in`] for why the
/// CREDENTIAL value wins one.
fn fold_rendered_names(
    rows: &crate::settings::StoredSettings,
    secrets: &mut SecretMap,
    scope: Option<&KeyScope>,
) -> Vec<String> {
    let mut collisions = Vec::new();
    // ⚠ **THE `venue_setting` TABLE, not `setting` rows filtered by key shape.** These values were
    // filed as `config.venue.<venue>…` until 2026-09-21, and that shape cannot work: `Config`
    // carries `#[serde(deny_unknown_fields)]` and has no `venue` field, so the whole `config`
    // section failed to deserialize and took the arming ceiling with it. Columns also mean no
    // decoding here — `crate::VenueSettingRow::value` is a plain string, where a `setting` row's
    // is a JSON scalar.
    for row in &rows.venue {
        for name in crate::venue_setting_names(&row.venue, row.tier.as_deref(), &row.field) {
            if scope.is_some_and(|s| !s.declares(&name)) {
                continue;
            }
            if secrets.fold_in(&name, &row.value) {
                collisions.push(name);
            }
        }
    }
    collisions.sort_unstable();
    collisions.dedup();
    collisions
}

/// [`fold_rendered_names`] over the rows beside `settings_dir`, for a caller that has just read a
/// credential table out of the store that answers there.
///
/// A box with no database, or one too old to carry the table, has nothing to fold — which is the
/// truthful reading, not a degradation: no row has moved on a box that has not been reshaped. A
/// database that EXISTS and will not open is LOUD, exactly as the credential read beside it is;
/// the alternative is a reader that silently resolves the pre-move answer for half its names.
fn fold_venue_settings_in(
    settings_dir: &Path,
    secrets: &mut SecretMap,
    scope: Option<&KeyScope>,
) -> Result<Vec<String>, SecretsError> {
    match crate::settings::read_settings_in(settings_dir)? {
        crate::settings::SettingsSource::Rows { rows, .. } => {
            Ok(fold_rendered_names(&rows, secrets, scope))
        }
        crate::settings::SettingsSource::NoDatabase { .. }
        | crate::settings::SettingsSource::TablesAbsent { .. } => Ok(Vec::new()),
    }
}

pub fn resolve_store_in(
    settings_dir: &Path,
    table: crate::db::Table,
) -> Result<Resolved, SecretsError> {
    let file = match table {
        crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
        crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
    };
    let mut resolved = match backend_in(settings_dir) {
        Backend::Database(db) => resolve_database(&db, table, &file)?,
        Backend::Files => resolve(&file)?,
    };
    // ⚠ THE FOLD, and it is here rather than in either arm above BY DESIGN — see
    // [`fold_rendered_names`]. `Backend::Files` means `database_present` answered `false`, so there
    // is no `venue_setting` table to read and this is a no-op by construction; running it on both
    // arms is what keeps the two backends from having different reader-visible behaviour, and
    // `crates/vike-secrets/tests/venue_setting_fold.rs` is what says so rather than assuming it.
    //
    // ⚠ `Table::Credential` ONLY. `node_key` is a different NAMESPACE (`docs/decisions/0051`), no
    // renderer produces its names, and folding one in would be the shared-probe defect that record
    // was written to remove.
    if table == crate::db::Table::Credential {
        resolved.collisions = fold_venue_settings_in(settings_dir, &mut resolved.secrets, None)?;
    }
    Ok(resolved)
}

// ---------------------------------------------------------------------------------------------
// The SCOPED read — a process holds only the keys it asked for
// ---------------------------------------------------------------------------------------------

/// **The credential names a process DECLARED it needs**, fixed before the store is opened.
///
/// # Why this exists — blast radius, not access control
///
/// **Owner ruling, 2026-09-16: restrict what a process materialises.** Every subcommand on a box
/// runs as one user with one filesystem view, anything that can open the store can read all of it,
/// and SQLite has no per-table grant — so this prevents no attacker who already has code execution.
/// What it prevents is a process that needs ONE name holding every venue secret the store carries,
/// where a core dump, a panic payload or a future logging bug reaches them all.
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` names that debt: the datahub's
/// isolation is carried by the PROJECT boundary, and the `backtest` case is *"carried by NEITHER of
/// those… Only the scoped read supplies it."*
///
/// # What it does NOT promise
///
/// It does not police where the names came from. This crate declares no `vike-*` dependency (see
/// the crate doc), so it cannot see `vike_model::credential_keys`' enumerators and cannot demand a
/// name be drawn from one. What it fixes is the SET: it is built once, before the read, and after
/// that every lookup is inside it or outside it — which is the property [`Lookup::NotDeclared`]
/// rests on.
///
/// Blank and whitespace-only names are dropped at construction rather than carried: a blank name
/// can match no row, and admitting one would let an empty `const` silently widen nothing while
/// looking like a declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyScope {
    names: BTreeSet<String>,
}

impl KeyScope {
    /// Declare a scope from a caller's name list — a `const` array, a venue crate's own
    /// `*_env_var_names()`, or any iterator of names.
    #[must_use]
    pub fn of<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        KeyScope {
            names: names
                .into_iter()
                .map(|n| n.as_ref().trim().to_string())
                .filter(|n| !n.is_empty())
                .collect(),
        }
    }

    /// Is `name` inside this scope? The question [`ScopedSecrets::get`] asks before it answers.
    #[must_use]
    pub fn declares(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// The declared names, sorted. Names only — a scope holds no value and never has.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The set the database reader binds as parameters.
    pub(crate) fn as_set(&self) -> &BTreeSet<String> {
        &self.names
    }
}

/// **A name asked for that this process never declared** — the state that must never look like an
/// absent credential.
///
/// Carries the name and the declared scope, both of which are key NAMES and therefore safe to
/// print, log and render (the same balance [`SecretMap`]'s `Debug` strikes). No value can reach
/// this type: it is constructed on the path where no row was ever selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndeclaredKey {
    /// The name the caller asked for.
    pub name: String,
    /// What the caller declared instead, sorted.
    pub declared: Vec<String>,
}

impl std::fmt::Display for UndeclaredKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} was never DECLARED by this process, so the credential store was not asked for it — \
             this is a scope defect, NOT an absent credential. Declared: [{}]",
            self.name,
            self.declared.join(", ")
        )
    }
}

impl std::error::Error for UndeclaredKey {}

/// **The three states a scoped lookup can be in — and the whole point is that there are THREE.**
///
/// # ⚠ In this workspace ABSENT CREDENTIALS ARE THE LIVE GATE
///
/// A venue whose keys cannot be found stays on PAPER, silently and by design; that is the correct
/// behaviour of an unconfigured install. So a scoped read introduces a new and dangerous shape — a
/// name the caller forgot to declare looks exactly like a name that is not in the store — and a
/// live venue would drop to paper with no error while the operator saw what a fresh install shows.
/// A silent trading outage wearing the costume of a correct default.
///
/// [`Self::NotDeclared`] is how the two are told apart STRUCTURALLY rather than by discipline. It is
/// the same three-state answer the tree already reaches for twice, for the identical reason:
/// `vike_bridge_core::credentials::StoreHealth` (an empty map from an ABSENT store is not the same
/// event as an empty map from an UNOPENABLE one) and [`crate::Accounts::Unanswerable`] (*no
/// accounts* is not *cannot answer about accounts*). Collapsing either would read downstream as
/// *this venue has no credentials*.
///
/// # There is deliberately no `Option`-yielding accessor
///
/// [`ScopedSecrets`] has no `get` that answers `Option<&str>`, and this enum has no method that
/// folds [`Self::NotDeclared`] into `None`. The ONE conversion is [`Self::declared`], which is a
/// `Result` — so a caller that wants an `Option` writes `?`, `expect` or a `match`, and the
/// undeclared arm is a thing it had to look at. That is the cost: every migrated call site handles
/// a third arm it did not handle before.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Lookup<'a> {
    /// The name was declared and the store holds it.
    Present(&'a str),
    /// The name was declared and the store does not hold it. **This is the live gate** — the same
    /// answer an unconfigured box gives, and the only one a caller may treat as "no credential".
    AbsentFromStore,
    /// The name was NOT declared, so the store was never asked. Not an answer about the store at
    /// all — a defect in the caller's own scope.
    NotDeclared(UndeclaredKey),
}

impl<'a> Lookup<'a> {
    /// `Ok(Some)` present · `Ok(None)` genuinely absent (the live gate) · `Err` never declared.
    ///
    /// The only conversion out of this enum, and it is a `Result` on purpose — see the type doc.
    ///
    /// # Errors
    /// [`UndeclaredKey`] when the caller asked for a name outside its own declared scope.
    pub fn declared(self) -> Result<Option<&'a str>, UndeclaredKey> {
        match self {
            Lookup::Present(v) => Ok(Some(v)),
            Lookup::AbsentFromStore => Ok(None),
            Lookup::NotDeclared(u) => Err(u),
        }
    }

    /// Whether this is the undeclared arm — for a caller that wants to branch without consuming.
    #[must_use]
    pub fn is_undeclared(&self) -> bool {
        matches!(self, Lookup::NotDeclared(_))
    }
}

/// **What a scoped read found: the declared names the store holds, and nothing else.**
///
/// The [`Resolved`] fields are carried through unchanged — `source`, `warning`, `legacy`,
/// `shadowed`, `collisions` — because a scoped caller is no less entitled to the store's findings
/// than a whole-table one, and `vike-cli secrets` and every composition root print them from
/// exactly these fields.
///
/// # ⚠ The FILE arm is a FILTER, not a narrower query, and saying so is the point
///
/// On a box with no settings database [`resolve`] must `read_to_string` and `parse_dotenv` the
/// whole file before anything can be selected from it, so every value is transiently in this
/// process's memory no matter what was declared. The narrowing there is over what is RETAINED —
/// what a core dump taken a second later, or a later logging bug, can reach — not over what is
/// read. The DATABASE arm is a genuinely narrower query: [`crate::read_table_scoped`] binds the
/// declared names and selects no other row.
#[derive(Clone, PartialEq, Eq)]
pub struct ScopedSecrets {
    scope: KeyScope,
    found: BTreeMap<String, String>,
    /// Which store answered — the same value [`Resolved::source`] carries.
    pub source: Source,
    /// The store's permission finding, unchanged from [`Resolved::warning`].
    pub warning: Option<PermissionWarning>,
    /// The pre-one-store leftover finding, unchanged from [`Resolved::legacy`].
    pub legacy: Option<LegacyStoreWarning>,
    /// The shadowed credential file, unchanged from [`Resolved::shadowed`].
    pub shadowed: Option<ShadowedStore>,
    /// The half-done-move finding, unchanged from [`Resolved::collisions`].
    ///
    /// ⚠ It is computed INSIDE the scope, so it names only collisions on DECLARED names. A process
    /// that asked for five proxy keys is told about those five and about nothing else — the same
    /// narrowing that governs what it materialises governs what it is told.
    pub collisions: Vec<String>,
}

impl ScopedSecrets {
    /// **The one accessor, and it answers three states** — see [`Lookup`].
    ///
    /// A name outside [`Self::scope`] is [`Lookup::NotDeclared`] whether or not the store holds it:
    /// the store was not asked, so there is nothing to report about it.
    pub fn get(&self, name: &str) -> Lookup<'_> {
        if !self.scope.declares(name) {
            return Lookup::NotDeclared(UndeclaredKey {
                name: name.to_string(),
                declared: self.scope.names().map(str::to_string).collect(),
            });
        }
        match self.found.get(name) {
            Some(v) => Lookup::Present(v.as_str()),
            None => Lookup::AbsentFromStore,
        }
    }

    /// What this process declared.
    #[must_use]
    pub fn scope(&self) -> &KeyScope {
        &self.scope
    }

    /// The declared names the store actually holds, sorted. Names only.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.found.keys().map(String::as_str)
    }

    /// How many declared names the store holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.found.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.found.is_empty()
    }

    /// **Hand the declared-and-present pairs to a consumer that speaks `HashMap`** — the bridge for
    /// a caller whose downstream reader was written against the whole map.
    ///
    /// ⚠ It is the END of the three-state distinction, and a caller reaching for it is choosing
    /// that: a `.get` on the returned map answers `None` for an undeclared name exactly as it does
    /// for an absent one. Reach for it only where the declared set is itself the point — the two
    /// polymarket allow-lists, whose scope IS a refusal (`POLY_PRIVATE_KEY` must not leak onto the
    /// proxy path), and a fixed-name reader in another crate whose own constant supplied the scope.
    /// Everywhere else use [`Self::get`].
    #[must_use]
    pub fn into_map(self) -> HashMap<String, String> {
        self.found.into_iter().collect()
    }

    /// The EMPTY answer for a declared scope — what an unreadable store degrades to, so that every
    /// declared name reads [`Lookup::AbsentFromStore`] (the live gate) and an undeclared one still
    /// reads [`Lookup::NotDeclared`].
    #[must_use]
    pub fn empty(scope: &KeyScope, source: Source) -> Self {
        ScopedSecrets {
            scope: scope.clone(),
            found: BTreeMap::new(),
            source,
            warning: None,
            legacy: None,
            shadowed: None,
            collisions: Vec::new(),
        }
    }

    /// Narrow a whole-store [`Resolved`] to `scope`, keeping every finding.
    ///
    /// ⚠ **The fold has already happened when this is called** — see [`resolve_store_scoped_in`].
    /// There used to be a `fold_in` here, a per-name door `vike_bridge_core::credentials` used to
    /// push rendered names through from above, because the renderer lived up there and this type's
    /// fields are private. The renderer moved down, so the fold happens inside the store and the
    /// door is gone rather than left open as a second way in.
    fn narrow(resolved: Resolved, scope: &KeyScope) -> Self {
        let Resolved { secrets, source, warning, legacy, shadowed, collisions } = resolved;
        let mut found = BTreeMap::new();
        let mut all = secrets.into_map();
        for name in scope.names() {
            if let Some(v) = all.remove(name) {
                found.insert(name.to_string(), v);
            }
        }
        ScopedSecrets { scope: scope.clone(), found, source, warning, legacy, shadowed, collisions }
    }
}

impl std::fmt::Debug for ScopedSecrets {
    /// Key names and counts, never a value — the same contract [`SecretMap`]'s `Debug` holds, and
    /// it has to be spelled again here because this type carries its own map.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedSecrets")
            .field("declared", &self.scope.len())
            .field("present", &self.found.keys().collect::<Vec<_>>())
            .field("source", &self.source)
            .finish()
    }
}

/// **[`resolve`] narrowed to `scope`** — the FILE arm of the scoped read.
///
/// Identical to [`resolve`] in every observable way except what it retains: the same absent /
/// present / unreadable trichotomy, the same [`Source`], the same three findings. See
/// [`ScopedSecrets`]' ⚠ section for why this arm is a filter rather than a narrower read.
///
/// # Errors
/// [`SecretsError`] when the file exists and cannot be read — the loud arm, unchanged.
pub fn resolve_scoped(path: &Path, scope: &KeyScope) -> Result<ScopedSecrets, SecretsError> {
    Ok(ScopedSecrets::narrow(resolve(path)?, scope))
}

/// **[`resolve_store_in`] narrowed to `scope`** — the scoped front door, for a caller that already
/// holds the settings DIRECTORY.
///
/// The SAME [`backend_in`] decision on the same directory, so a scoped reader and a whole-table
/// reader in one process cannot disagree about which store is live. The database arm is
/// [`crate::read_table_scoped`], which binds the declared names and selects no other row; the file
/// arm is [`resolve`], narrowed here — the same composition [`resolve_scoped`] is, spelled out so
/// the FOLD below sits between the read and the narrowing rather than after both.
///
/// ⚠ **It folds ruling 10's `venue_setting` rows, and it does so INSIDE the scope.** Without that
/// this front door is blind to every name that has moved — which is exactly what
/// `crates/bridges/polymarket/src/egress.rs`'s `dotenv_proxy_vars` measured: the three
/// `POLY_PROXY_*` rows in the database were read by NOTHING, and the built-in defaults stayed in
/// force with no error anywhere. See [`fold_rendered_names`] for why fold-then-filter is the
/// order and not the reverse.
///
/// ⚠ Like [`resolve_store_in`], deliberately NOT one of
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_READERS`: its settings
/// directory is a mandatory `&Path` parameter, so it cannot express the defect that ratchet hunts
/// (a library walking for the store from a working directory nothing can redirect).
/// [`resolve_project_scoped`], which DOES walk, is keyed there.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn resolve_store_scoped_in(
    settings_dir: &Path,
    table: crate::db::Table,
    scope: &KeyScope,
) -> Result<ScopedSecrets, SecretsError> {
    let file = match table {
        crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
        crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
    };
    // ⚠ **This arm does NOT delegate to [`resolve_store_in`]**, and that is why the fold has to be
    // spelled twice: the database half asks `crate::read_table_scoped`, which BINDS the declared
    // names and selects no other row, so there is no whole-table read to narrow. What both
    // spellings share is [`fold_venue_settings_in`], so the two cannot disagree about what a row
    // renders — only about how much of the table was read in the first place.
    let mut resolved = match backend_in(settings_dir) {
        Backend::Database(db) => Resolved {
            secrets: crate::db::read_table_scoped(&db, table, scope)?,
            source: Source::Database(db.clone()),
            warning: permission_warning(&db),
            legacy: None,
            shadowed: file.exists().then(|| ShadowedStore { file: file.clone(), db: db.clone() }),
            collisions: Vec::new(),
        },
        Backend::Files => resolve(&file)?,
    };
    // ⚠ FOLD, THEN NARROW — never the other way round, and never unscoped. `scope` is passed into
    // the fold itself, so a name this process did not declare is never written into the map at all
    // rather than written and then filtered out. [`fold_rendered_names`] carries the owner ruling.
    if table == crate::db::Table::Credential {
        resolved.collisions =
            fold_venue_settings_in(settings_dir, &mut resolved.secrets, Some(scope))?;
    }
    Ok(ScopedSecrets::narrow(resolved, scope))
}

/// **[`resolve_project`] narrowed to `scope`** — the scoped read over the project walk, for a
/// caller holding the `VIKE_SETTINGS_DIR` override and nothing else.
///
/// Same derivation as [`resolve_project`], same `Backend` decision, same findings; what differs is
/// that the process ends up holding the declared names and no others.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn resolve_project_scoped(
    settings_dir: Option<&str>,
    scope: &KeyScope,
) -> Result<ScopedSecrets, SecretsError> {
    resolve_store_scoped_in(
        &crate::dotenv::workspace_settings_dir_from(settings_dir),
        crate::db::Table::Credential,
        scope,
    )
}

/// **Which accounts the store that ANSWERS for `settings_dir` holds** — the account-table twin of
/// [`resolve_store_in`], for a caller that already holds the settings DIRECTORY.
///
/// It asks the same [`backend_in`] the credential reader and the credential writer both ask, so the
/// three cannot disagree about which store is live. There is no `table` parameter: an account
/// belongs to the credential plane by definition, and `crate::db::Table::NodeKey`'s namespace holds
/// a pair of deployment keys that belong to no account at all.
///
/// # ⚠ The `Backend::Files` answer, and why it is not an empty list
///
/// A box with no settings database has no `account` table on it, and its credentials are perfectly
/// present under their legacy key names. Answering `Known(vec![])` there would tell every caller
/// *this store has no accounts* about a store that has sixteen of them — the shape of the failure
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §1 is about, and one that would
/// read downstream as *this venue has no credentials*. So the answer is
/// [`crate::db::NoAccountTable::FileStore`], carrying the credential file that IS answering:
/// `vike_model::account_keys::accounts_in_store` is the reader for that store, and it is the one
/// every caller uses today.
///
/// That arm is reached whether or not the credential file exists. An ABSENT file is the live gate
/// (no credentials ⇒ every venue stays paper) and is not this function's question; what it reports
/// is that the DATABASE is not what answers here, which is true either way.
///
/// # It is FALLIBLE, and has no infallible twin on purpose
///
/// `vike_bridge_core::credentials::load_workspace_secrets_at` may swallow its error into an empty
/// map because an empty credential map degrades SAFELY — it is the live gate. An empty account list
/// carries no such guarantee: it is an assertion about the store, not a refusal to arm. So a store
/// that exists and will not open comes back as [`SecretsError`] and the caller decides, exactly as
/// `vike_bridge_core::credentials::try_load_workspace_secrets_at` does for the map.
pub fn resolve_accounts_in(settings_dir: &Path) -> Result<crate::db::Accounts, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => Ok(crate::db::read_accounts(&db)?),
        Backend::Files => {
            Ok(crate::db::Accounts::Unanswerable(crate::db::NoAccountTable::FileStore {
                file: crate::dotenv::secrets_path_in(settings_dir),
            }))
        }
    }
}

/// [`resolve_accounts_in`] over the project walk — the account twin of [`resolve_project`], for a
/// caller that holds the `VIKE_SETTINGS_DIR` override as a `&str` and nothing else.
///
/// Same derivation as every other front door here: `settings_dir` is the override or `None`, and
/// the walk answers when it is `None`.
pub fn resolve_accounts(settings_dir: Option<&str>) -> Result<crate::db::Accounts, SecretsError> {
    resolve_accounts_in(&crate::dotenv::workspace_settings_dir_from(settings_dir))
}

/// **The credential key NAMES each account row owns, routed to the store that answers** — the
/// companion [`resolve_accounts_in`] needs before a human can ACT on its rows.
///
/// [`crate::db::AccountKeys`] carries the argument: two rows of `(dukascopy, demo, NULL)` render
/// identically except for an opaque `id`, and the thing that tells them apart —
/// `DUKASCOPY_DEMO1_*` versus `DUKASCOPY_DEMO2_*` — lives in the `credential` table rather than in
/// the `account` one. Values are never selected; see that type.
///
/// # `None` means EXACTLY what [`Accounts::Unanswerable`] means, and a caller may not merge them
///
/// `None` is *this store has no `account` table to key* — a [`Backend::Files`] box, where the
/// accounts live in the key names and `vike_model::account_keys::accounts_in_store` is the reader
/// that applies. `Some(map)` is *the table is there*, and a row absent from the map is an account
/// with no live credential row naming it, which is a real state and not a missing answer.
///
/// [`backend_in`] on the same `settings_dir`, so this and [`resolve_accounts_in`] cannot disagree
/// about which store they are describing.
///
/// [`Accounts::Unanswerable`]: crate::db::Accounts::Unanswerable
pub fn resolve_account_keys_in(
    settings_dir: &Path,
) -> Result<Option<std::collections::BTreeMap<i64, crate::db::AccountKeys>>, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => Ok(Some(crate::db::read_account_keys(&db)?)),
        Backend::Files => Ok(None),
    }
}

/// [`resolve_account_keys_in`] over the project walk — the twin [`resolve_accounts`] is to
/// [`resolve_accounts_in`], for a caller that holds the `VIKE_SETTINGS_DIR` override and nothing
/// else.
///
/// It exists because the two readers are used TOGETHER and neither is useful alone for the case
/// they were built for: `vike_mount`'s dukascopy arm has to turn an `account` row into a broker, and
/// the row's own cells cannot do it (both dukascopy rows are `(dukascopy, demo, NULL)`) — the
/// discriminator is the owner PREFIX of its credential names, which only this reader carries. A
/// caller reaching one front door over the walk and the other over a hand-built path could open two
/// different stores; same derivation, same override, so these two cannot.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn resolve_account_keys(
    settings_dir: Option<&str>,
) -> Result<Option<std::collections::BTreeMap<i64, crate::db::AccountKeys>>, SecretsError> {
    resolve_account_keys_in(&crate::dotenv::workspace_settings_dir_from(settings_dir))
}

/// **The UPSERT, routed to the store that actually answers** — the write twin of
/// [`resolve_store_in`], and the reason a migrated box can still change a key.
///
/// # The defect this exists to close
///
/// Every credential WRITE in this workspace used to name a FILE. After
/// `docs/decisions/0054`'s credential half that file is shadowed: the write succeeds, the file
/// genuinely changes, the change journal records it, the caller reports success — and no reader ever
/// opens that file again. The sharpest instance is `crates/bridges/ctrader/src/token_store.rs`'s
/// `persist`, which stores a grant THE VENUE rotated: a shadowed write means the token is lost at
/// restart and that session cannot re-authenticate.
///
/// # One decision, shared with the reader
///
/// [`backend_in`], on the same `settings_dir` the reader is given. There is no second probe and no
/// second path derivation, so a writer and a reader in one process — or in two — cannot disagree
/// about which store is live.
///
/// # The UPSERT rule carries over unchanged
///
/// **Replace exactly the named keys; leave everything else alone.**
///
/// * the FILE branch is [`save_credentials`] VERBATIM — not reimplemented, not wrapped, not
///   "improved" — so every byte-preservation property that transform is tested for (comments, blank
///   lines, order, duplicate lines, the destination mode, a symlinked store, the atomic rename) is
///   the same property here;
/// * the DATABASE branch is `INSERT … ON CONFLICT(name) DO UPDATE SET value` over the named rows in
///   ONE transaction — every other row untouched, and a pair impossible to half-write. It creates no
///   database: it is reached only when one already exists.
///
/// Nothing in either branch rewrites a store WHOLESALE, and there is no flag that does.
///
/// # The multiline refusal happens FIRST, for both branches
///
/// A value spanning more than one line cannot be represented in the file's grammar. SQLite would
/// take it happily, which is exactly why it is refused here rather than in the file branch alone: a
/// key that round-trips on a migrated box and is rejected on an unmigrated one is a divergence
/// between two stores that must answer identically.
///
/// Returns the [`Backend`] that was written, so a caller can say WHERE the key landed.
/// # `classify` — required for a NEW credential name, meaningless for a node key
///
/// Since the settings database's schema 2, a `credential` row says which ACCOUNT it belongs to
/// (`docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4), and that classification is
/// derived from the key NAME by machinery this crate cannot see — so it arrives as a closure, the
/// same seam and for the same layering reason [`crate::migrate`] takes `is_node_key` through.
/// `vike_bridge_core::credentials::classify_credential_name` is the production implementation.
///
/// **It is only consulted for a name this store has never held.** Replacing a known key's value
/// needs no classification, because the row already carries one — which is what keeps the venue's
/// own rotation writer (`crates/bridges/ctrader/src/token_store.rs`'s `persist`) working with
/// nothing to supply. `None` is therefore correct for every [`crate::Table::NodeKey`] caller:
/// 0051's pair belongs to no account and its table is `(name, value)` in every schema. A NEW
/// credential name with `None` is refused by name rather than filed as a deployment-level
/// credential belonging to nothing — see `crate::DbErrorKind::Unclassified`.
///
/// The FILE branch ignores it entirely: `secrets.env` has no schema to classify into.
pub fn save_credentials_to_store(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
) -> std::io::Result<Backend> {
    crate::env_write::refuse_multiline(updates)?;
    let backend = backend_in(settings_dir);
    match &backend {
        Backend::Database(db) => crate::db::upsert_rows(db, table, updates, classify)
            // `DbError`'s `Display` carries the path and the reason and never a row value — the
            // same bridge `From<DbError> for SecretsError` argues for the read path.
            .map_err(|e| std::io::Error::other(e.to_string()))?,
        Backend::Files => {
            let file = match table {
                crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
                crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
            };
            crate::env_write::save_credentials(&file, updates)?;
        }
    }
    Ok(backend)
}

/// **Write ONE account row's `venue_account_id`, routed to the store that actually answers** — the
/// account-row twin of [`save_credentials_to_store`], and the write twin of [`resolve_accounts_in`].
///
/// # The Backend decision is the SAME one, asked the same way
///
/// [`backend_in`], on the same `settings_dir` the reader is given — so the verb that LISTS the
/// account rows and the verb that writes one cannot disagree about which store they are talking
/// about. There is no second probe and no second path derivation.
///
/// # ⚠ A `Backend::Files` box is REFUSED, not silently no-op'd
///
/// A file store has no `account` table: on that box the accounts live in the credential key NAMES
/// and `vike_model::account_keys::accounts_in_store` is the reader that applies. There is nothing
/// here to write and nowhere to write it, so the answer is
/// [`crate::DbErrorKind::NoDatabase`] — loud, naming the file that IS answering and the migration
/// that would move it. **It is emphatically not a per-key fallback**: [`Backend`]'s whole rule is
/// that the choice is per RUN, and a writer that quietly stored the book somewhere else on an
/// unmigrated box would be inventing exactly the ladder
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids.
///
/// It creates no database either, on that path or any other: [`crate::migrate`] is still the only
/// function in this crate that may bring one into existence, and `set_venue_account_id`'s own
/// `created` arm is the belt behind this probe for the case where the file is removed in between.
///
/// # What this is NOT
///
/// It is not §11 step 3's FOLD of the ten stored book keys.
///
/// ⚠ It read *"and it is not the venue handshake's write path"* until 2026-09-15, and that half is
/// now false: the handshake fold reaches the store through this same router, passing
/// [`crate::BookSource::Handshake`] where the operator door passes [`crate::BookSource::Operator`].
/// One router, one writer, told which claim it is recording —
/// `crates/vike-ops/tests/credential_writer_gate.rs` is why a second function was not the answer.
/// `crate::db::set_venue_account_id`'s own doc separates the three sources; the short version is
/// that the OPERATOR door is for a book that is in NO store and derivable from nothing — which
/// today is dukascopy's two demo accounts and nothing else.
///
/// Nothing here writes, moves or deletes either credential FILE, and no `credential` row is read or
/// touched.
///
/// # `venue_account_id: None` is a CLEAR
///
/// It puts the column back to `NULL` — *not yet known* — and it is the repair a pair of rows
/// written the wrong way round needs, because correcting either one alone is refused by ruling 11's
/// index in both directions. `crate::db::set_venue_account_id`'s CLEAR section is the argument. It
/// reaches the same store through the same [`backend_in`] and refuses a [`Backend::Files`] box
/// identically: clearing a column that does not exist is not a thing to succeed quietly at.
pub fn set_venue_account_id_in(
    settings_dir: &Path,
    id: i64,
    venue_account_id: Option<&str>,
    replace: bool,
    source: crate::db::BookSource<'_>,
) -> Result<crate::db::BookWrite, crate::db::DbError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => {
            crate::db::set_venue_account_id(&db, id, venue_account_id, replace, source)
        }
        Backend::Files => Err(crate::db::DbError {
            path: crate::dotenv::db_path_in(settings_dir),
            kind: crate::db::DbErrorKind::NoDatabase {
                file: crate::dotenv::secrets_path_in(settings_dir),
            },
        }),
    }
}

/// **The account LIFECYCLE, routed to the store that actually answers** — create / rename /
/// (de)activate / remove, the sibling of [`set_venue_account_id_in`] and the write twin of
/// [`resolve_accounts_in`].
///
/// # The Backend decision is the SAME one, asked the same way
///
/// [`backend_in`], on the same `settings_dir` the reader is given — so the verb that LISTS the
/// account rows and the verb that edits one cannot disagree about which store they are talking
/// about. There is no second probe and no second path derivation.
///
/// # ⚠ A `Backend::Files` box is REFUSED, not silently no-op'd
///
/// A file store has no `account` table: on that box the accounts live in the credential key NAMES
/// and `vike_model::account_keys::accounts_in_store` is the reader that applies. There is nothing
/// here to write and nowhere to write it, so the answer is [`crate::DbErrorKind::NoDatabase`] —
/// loud, naming the file that IS answering and the migration that would move it. It is emphatically
/// not a per-key fallback: [`Backend`]'s whole rule is that the choice is per RUN.
///
/// # ⚠ It CREATES NO DATABASE, and that is reason 1 of `docs/decisions/0036` at its sharpest
///
/// The mere EXISTENCE of `<project>/settings/db/vike.db` is the whole of [`Backend`]'s per-run
/// choice, so an account verb that created one would make every credential in `secrets.env` unread
/// on that box in the same act — the LIVE GATE, silently, from a command about filing.
/// `vike-cli secrets migrate` stays the one creator; the probe above and
/// [`crate::db::edit_account`]'s own `created` rollback are the two layers that hold it.
///
/// Nothing here writes, moves or deletes either credential FILE, and no `credential` row is written
/// or its value read — [`crate::db::edit_account`]'s *what it never does* section is the contract.
///
/// # Errors
/// [`crate::DbError`] for every refusal [`crate::db::edit_account`] states, and for a
/// `Backend::Files` box.
pub fn edit_account_in(
    settings_dir: &Path,
    edit: crate::db::AccountEdit<'_>,
) -> Result<crate::db::AccountWrite, crate::db::DbError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => crate::db::edit_account(&db, edit),
        Backend::Files => Err(crate::db::DbError {
            path: crate::dotenv::db_path_in(settings_dir),
            kind: crate::db::DbErrorKind::NoDatabase {
                file: crate::dotenv::secrets_path_in(settings_dir),
            },
        }),
    }
}

/// Where a node key was actually found, so a caller can WARN about the legacy home without
/// re-deriving the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKeySource {
    /// The `node_key` table of `<project>/settings/db/vike.db` — the home since
    /// `docs/decisions/0054`. When this is the answer, NO file was consulted at all.
    Database,
    /// `<project>/settings/node.env` — the home. Nothing to say.
    NodeFile,
    /// `<project>/settings/secrets.env` — the LEGACY home, still read, warned about.
    LegacyCredentialStore,
    /// Neither file carries a node key. The ordinary unconfigured state; silent.
    Absent,
}

/// The NODE-key store: `<project>/settings/node.env`, falling back to the credential store for keys
/// that have not moved yet.
///
/// ⚠ **This is the ONE fallback in the two-store design, it is a MIGRATION and it is temporary.**
/// The rule the split obeys is that a name has one home decided statically — no ladder. This arm
/// exists because the tradehub node keys were written into `secrets.env` by every `node setup` run
/// before 2026-09-08 and a live daemon is holding a pair there right now; deleting the read outright
/// would take a running node's authentication away on the next deploy. It returns
/// [`NodeKeySource::LegacyCredentialStore`] so the caller can say so, once, by name.
///
/// ⚠ It is DELIBERATELY not a merge. Whichever file answers FIRST answers wholly: a pair split
/// across the two files is a half-migrated box, and merging would hide that while producing a
/// mismatched pair — an opaque `bad mac` at the node, which is the exact symptom
/// `crates/vike-cli/tests/node_cli.rs` records as the expensive one. `node.env` existing with a
/// non-empty node key is the whole test.
///
/// ⚠ **`is_node_key` is what "wholly" is scoped OVER, and passing a predicate WIDER than the pair
/// you are about to read is a defect, not a convenience.** This function's answer is *which file*,
/// and the caller then reads its own names out of that file; so a probe matching a name the caller
/// does not use lets ANOTHER service's migration decide this one's. Measured: with the four-name
/// `vike_model::credential_keys::is_platform_key`, a `node.env` holding only the DATAHUB pair —
/// what `vike-cli datahub setup` writes — answered [`NodeKeySource::NodeFile`] for a TRADEHUB
/// caller, whose working pair in `secrets.env` was then dropped, producing a silent `bad mac` (no
/// migration notice fires, because the source was not the legacy one). Pass the FAMILY predicate:
/// `vike_model::credential_keys::is_tradehub_node_key` / `is_datahub_node_key`, whose disjointness
/// and exhaustiveness over the table are pinned by that module's
/// `the_two_service_families_partition_the_platform_table`.
///
/// Nothing here writes, moves or deletes either file.
///
/// ⚠ **The database does NOT stack a third level on that fallback — it REPLACES both.**
/// `docs/decisions/0054` fixes the order explicitly: *"retire 0051's fallback first, or in the same
/// PR that adds the database read, so the depth never exceeds one"*, because a node key resolvable
/// from the database, from `node.env` AND from legacy `secrets.env` is two levels deep and makes
/// 0051's own retirement condition unsatisfiable. So the branches are disjoint, not nested:
///
/// * **[`Backend::Database`]** — the `node_key` table answers WHOLLY. No file is opened, so the
///   legacy arm below is not merely unreached, it is unreachable, and the answer is
///   [`NodeKeySource::Database`].
/// * **[`Backend::Files`]** — byte-identical to the behaviour before 0054, legacy fallback and all.
///
/// What discharges 0051 rather than deferring it is the MIGRATION, not this function: `crate::db`'s
/// `migrate` classifies a node key found in the credential store into the `node_key` table, so a box
/// that never moved its pair by hand arrives in the one-home state by migrating.
pub fn resolve_node_keys(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
) -> Result<(Resolved, NodeKeySource), SecretsError> {
    if let Backend::Database(db) = workspace_backend_from(settings_dir) {
        let file = crate::dotenv::workspace_node_path_from(settings_dir);
        let resolved = resolve_database(&db, crate::db::Table::NodeKey, &file)?;
        // Deliberately NOT probed with `is_node_key`: the table IS the namespace, so "which store
        // carries this family" has already been answered by the schema. Probing would re-introduce
        // the choice the two tables exist to remove.
        return Ok((resolved, NodeKeySource::Database));
    }
    let node = resolve(&crate::dotenv::workspace_node_path_from(settings_dir))?;
    let carries_one = node.secrets.keys().any(&is_node_key);
    if carries_one {
        return Ok((node, NodeKeySource::NodeFile));
    }
    let legacy = resolve_project(settings_dir)?;
    let source = if legacy.secrets.keys().any(&is_node_key) {
        NodeKeySource::LegacyCredentialStore
    } else {
        NodeKeySource::Absent
    };
    Ok((legacy, source))
}

/// The sentence a caller prints when [`resolve_node_keys`] answered
/// [`NodeKeySource::LegacyCredentialStore`] — one place, so five binaries cannot word the same
/// migration five ways.
#[must_use]
pub fn legacy_node_key_notice(settings_dir_display: &str) -> String {
    format!(
        "node keys are still in {settings_dir_display}/{} — the file that also holds every venue \
         key. Move the `VIKE_*_OBSERVE_KEY` / `VIKE_*_CONTROL_KEY` lines to \
         {settings_dir_display}/{}, which holds node keys and nothing else; they are read from \
         there first. This fallback is a migration and will be removed.",
        crate::dotenv::SECRETS_FILE,
        crate::dotenv::NODE_FILE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "vike-secrets-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> SecretMap {
        let mut m = BTreeMap::new();
        m.insert("BINANCE_LIVE_API_KEY".to_string(), "key-abcd".to_string());
        m.insert("BINANCE_LIVE_API_SECRET".to_string(), "sup3r-s3cr3t".to_string());
        SecretMap::new(m)
    }

    fn put(path: &Path, value: &str) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(path, format!("# a comment\nBINANCE_LIVE_API_KEY={value}\n")).unwrap();
    }

    fn value(r: &Resolved) -> String {
        r.secrets.clone().into_map().get("BINANCE_LIVE_API_KEY").cloned().unwrap_or_default()
    }

    #[test]
    fn debug_shows_key_names_but_never_a_value() {
        let shown = format!("{:?}", sample());
        assert!(shown.contains("BINANCE_LIVE_API_KEY=***"));
        assert!(shown.contains("2 entries"));
        assert!(!shown.contains("sup3r-s3cr3t"));
        assert!(!shown.contains("key-abcd"));
    }

    #[test]
    fn debug_of_an_empty_map_is_harmless() {
        assert_eq!(format!("{:?}", SecretMap::default()), "SecretMap(0 entries: [])");
    }

    /// The store is read, byte for byte, and never rewritten.
    #[test]
    fn an_existing_store_is_parsed_and_left_alone() {
        let d = tmpdir("read");
        let store = d.join("settings").join("secrets.env");
        put(&store, "from-the-project");

        let r = resolve(&store).unwrap();
        assert_eq!(r.source, Source::File(store.clone()));
        assert_eq!(value(&r), "from-the-project");
        assert!(std::fs::read_to_string(&store).unwrap().contains("=from-the-project"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An ABSENT store is an empty map, not an error — the live gate.
    #[test]
    fn no_store_at_all_is_an_empty_map_not_an_error() {
        let d = tmpdir("absent");
        let r = resolve(&d.join("settings").join("secrets.env")).unwrap();
        assert!(r.secrets.is_empty());
        assert_eq!(r.source, Source::None);
        assert_eq!(r.warning, None);
        assert_eq!(r.legacy, None, "no leftover beside it either");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A leftover `<project>/.env` with NO store is a finding — the silent case.**
    ///
    /// The empty map is unchanged and [`Source::None`] is unchanged: this adds a diagnostic beside
    /// the live gate, it does not alter it. What it separates is the two boxes that produce the
    /// identical empty map — one with no credentials (correct) and one whose credentials never moved
    /// (every venue silently on paper).
    #[test]
    fn a_leftover_dotenv_beside_the_project_is_a_finding_when_the_store_is_absent() {
        let d = tmpdir("legacy-absent");
        let store = d.join("settings").join("secrets.env");
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::write(d.join(LEGACY_STORE_FILE), "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();

        let r = resolve(&store).unwrap();
        // The live gate is untouched.
        assert!(r.secrets.is_empty());
        assert_eq!(r.source, Source::None);
        // …and the finding names both files.
        let w = r.legacy.expect("a leftover store must not be silent");
        assert_eq!(w.legacy, d.join(LEGACY_STORE_FILE));
        assert_eq!(w.store, store);
        let msg = w.to_string();
        assert!(msg.contains("stays paper"), "{msg}");
        assert!(msg.contains("chmod 600"), "the finding must say how to fix it: {msg}");
        assert!(msg.contains("EnvironmentFile"), "…and when it is NOT a problem: {msg}");
        assert!(!msg.contains("never-printed"), "the probe must never read the CONTENTS: {msg}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A `.env` beside a store that LOADED is silent.** It is a systemd `EnvironmentFile` — the CI box's
    /// live recorder ships one — and nothing was silent about credentials, because the store answered.
    #[test]
    fn a_dotenv_beside_a_present_store_is_not_a_finding() {
        let d = tmpdir("legacy-present");
        let store = d.join("settings").join("secrets.env");
        put(&store, "from-the-store");
        std::fs::write(d.join(LEGACY_STORE_FILE), "POLY_PROXY_ENABLED=false\n").unwrap();

        let r = resolve(&store).unwrap();
        assert_eq!(value(&r), "from-the-store");
        assert_eq!(r.legacy, None, "a store that loaded is not a silent transition");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The probe is `metadata`, so only `NotFound` is absence — a DIRECTORY named `.env` still
    /// reports, and a path with no grandparent has no project to look beside.
    #[test]
    fn only_not_found_counts_as_absent_and_a_rootless_path_is_skipped() {
        let d = tmpdir("legacy-dir");
        let store = d.join("settings").join("secrets.env");
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::create_dir_all(d.join(LEGACY_STORE_FILE)).unwrap();
        assert!(legacy_store_warning(&store).is_some(), "a directory is not established absence");

        // ...but a DIRECTORY makes `metadata` return `Ok`, so the case above exercises the `_` arm
        // and never the guard itself. The guard — "only NotFound counts as absent" — needs an
        // error that is NOT NotFound, and this is it: probing THROUGH a regular file yields
        // ENOTDIR. Without this line, replacing the guard with `true` (absence always established)
        // passes, and a store nobody can stat reports as cleanly absent — the exact conflation the
        // doc above forbids.
        #[cfg(unix)]
        {
            let file = d.join("proj");
            put(&file, "not a directory");
            let under_a_file = file.join("settings").join(crate::SECRETS_FILE);
            assert!(
                legacy_store_warning(&under_a_file).is_some(),
                "ENOTDIR is not established absence — only NotFound is"
            );
        }

        // `secrets.env` alone: parent is "", and "" has no parent — no project, no probe.
        assert_eq!(legacy_store_warning(Path::new(crate::SECRETS_FILE)), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A store that EXISTS and cannot be opened is an ERROR, never a silent empty map.**
    ///
    /// The two must not look the same: an empty map means "this box is not configured", and a
    /// permissions bug quietly wearing that answer would look exactly like a correct fresh install
    /// while every venue dropped to paper for a completely different reason. A DIRECTORY where the
    /// file should be is the portable stand-in for an unreadable file (a `chmod 000` proves nothing
    /// when the test runs as root, which CI does).
    #[test]
    fn an_unreadable_store_errors_instead_of_reporting_no_credentials() {
        let d = tmpdir("unreadable");
        let store = d.join("secrets.env");
        std::fs::create_dir_all(&store).unwrap();

        let e = resolve(&store).expect_err("an unopenable store must not read as `not configured`");
        assert_eq!(e.path, store);
        assert!(e.to_string().contains("could not be read"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The project entry point resolves through the settings-directory override, with no
    /// environment read anywhere in this crate.
    #[test]
    fn the_project_entry_point_honours_an_explicit_settings_dir() {
        let d = tmpdir("project");
        let settings = d.join("settings");
        put(&settings.join("secrets.env"), "deployed");

        let r = resolve_project(settings.to_str()).unwrap();
        assert_eq!(r.source, Source::File(settings.join("secrets.env")));
        assert_eq!(value(&r), "deployed");

        // …and a blank override falls through to the walk rather than resolving to `""`.
        assert_eq!(resolve_project(Some("  ")).unwrap(), resolve_project(None).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Two projects, two stores, one process — no global state is involved in pointing at either.
    #[test]
    fn two_stores_coexist_in_one_process() {
        let a = tmpdir("coexist-a");
        let b = tmpdir("coexist-b");
        put(&a.join("secrets.env"), "first");
        put(&b.join("secrets.env"), "second");

        assert_eq!(value(&resolve_project(a.to_str()).unwrap()), "first");
        assert_eq!(value(&resolve_project(b.to_str()).unwrap()), "second");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// **The permission warning fires on a too-open store and not on 0600.**
    ///
    /// `0o640` (group-readable) and `0o604` (other-readable) are the two shapes a `cp` or a shared
    /// deploy actually produces; `0o620` proves the check is not read-only — a group-WRITABLE
    /// credential file lets somebody substitute the keys an order is signed with.
    #[cfg(unix)]
    #[test]
    fn a_group_or_world_accessible_store_warns_but_still_loads() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("perm-warn");
        let store = d.join("secrets.env");
        // A value nothing else in this test spells, so the "never prints a value" assert is real.
        put(&store, "s3cr3t-key-material");

        for mode in [0o644u32, 0o640, 0o604, 0o620, 0o666] {
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).unwrap();
            let r = resolve(&store).unwrap();
            // It LOADS — a finding is never a refusal.
            assert_eq!(value(&r), "s3cr3t-key-material", "mode {mode:04o} must still load");
            let Some(w) = r.warning else { panic!("mode {mode:04o} must warn") };
            assert_eq!(w.path, store);
            assert_eq!(w.finding, Finding::ExposedMode(mode));
            let msg = w.to_string();
            assert!(msg.contains("chmod 600"), "the warning must say how to fix it: {msg}");
            assert!(
                !msg.contains("s3cr3t-key-material"),
                "the warning must never print a credential VALUE"
            );
        }

        // …and 0600 is silent.
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
        let r = resolve(&store).unwrap();
        assert_eq!(r.warning, None, "an owner-only store must not warn");
        assert_eq!(value(&r), "s3cr3t-key-material");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A SYMLINKED store is reported — and the check is the one that can see it.**
    ///
    /// The mode question was asked through `std::fs::metadata`, which FOLLOWS the link, so the
    /// answer described a file at a path the operator was never shown: an owner-only target
    /// reported clean while the path itself said nothing about where the credentials actually
    /// live, who owns that directory, or who can replace the file in it. `symlink_metadata` is
    /// what makes the indirection visible.
    ///
    /// The target here is 0600 on purpose — that is exactly the case the follow-the-link check
    /// calls clean and returns `None` for.
    ///
    /// ⚠ Still a FINDING, never a refusal: one shared credential file symlinked into several
    /// project checkouts is a legitimate setup (it is what the retired `~/.vike/secrets.env` slot
    /// existed for), so the store must keep loading. Refusing it would strand that operator with
    /// every venue on paper.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_store_is_reported_even_when_the_file_it_points_at_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("perm-symlink");
        let real = d.join("shared-secrets.env");
        put(&real, "s3cr3t-key-material");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.join("secrets.env");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let w = permission_warning(&link)
            .expect("a symlinked credential store must be reported, whatever its target's mode");
        assert_eq!(w.path, link);
        // The LINK-vs-TARGET contract, which nothing asserted: the reported mode is the FOLLOWED
        // target's 0o600 — not the link's own kernel-ignored 0o777, and not a raw `st_mode` with
        // its file-type bits still on (0o100600). A sweep replaced the `& 0o777` mask with `|` and
        // with `^` and the whole vike-secrets suite stayed green, because every assertion here was
        // about the MESSAGE and none about the number in it. (This is not an exposure inversion:
        // the `Symlink` finding fires whatever the mode, so nothing was ever silenced.)
        assert_eq!(
            w.finding,
            Finding::Symlink { target: Some(real.clone()), target_mode: Some(0o600) },
            "the reported mode is the target's, masked to the permission bits"
        );
        let msg = w.to_string();
        assert!(
            msg.to_lowercase().contains("symlink"),
            "the finding must say the path is a symlink: {msg}"
        );
        assert!(
            msg.contains(&real.display().to_string()),
            "the finding must name where the credentials actually are: {msg}"
        );
        assert!(
            !msg.contains("s3cr3t-key-material"),
            "the finding must never print a credential VALUE: {msg}"
        );

        // A finding is never a refusal — the store still loads through the link.
        let r = resolve(&link).unwrap();
        assert_eq!(value(&r), "s3cr3t-key-material", "a symlinked store must still load");
        assert!(r.warning.is_some(), "…and `resolve` must surface the same finding");
        let _ = std::fs::remove_dir_all(&d);
    }
}

// ⚠ `FoldOutcome` lived here and is GONE, deliberately. It was `ScopedSecrets::fold_in`'s answer —
// the per-name door `vike_bridge_core::credentials` pushed rendered names through from above, back
// when the renderer lived in that crate and the fold could not run inside the store. The renderer
// moved down on 2026-09-22, `resolve_store_scoped_in` folds natively, and a door with nothing
// behind it is a second way to fold the same rows. Deleting it is what makes the fold singular;
// see `crates/vike-ops/tests/smoke_store_parity_gate.rs`, which is what keeps it that way.
