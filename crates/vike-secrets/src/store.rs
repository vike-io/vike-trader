//! Opening the credential store: the settings database, or `<project>/settings/secrets.env` on a
//! project that has none.
//!
//! [`resolve`] opens the file at a path the caller names; [`resolve_project`] is the entry point
//! for a caller with no opinion, which asks [`crate::workspace_settings_dir_from`] for the
//! project's settings directory and lets [`Backend`] choose the store. There is one store per run,
//! so there is no precedence to implement here — only reading it, reporting where the answer came
//! from, and reporting a permission finding on a file store.
//!
//! ⚠ **What a credential map carries is the credential TABLE, and nothing else.** Ruling 10's
//! `venue_setting` rows were FOLDED into it under their legacy credential names (`IBKR_DEMO_PORT`,
//! `POLY_RATE_GATE`, …) by both front doors here, so the readers that still looked those names up
//! kept finding moved rows. That fold (`fold_rendered_names`, applied through
//! `fold_venue_settings_in`) carried them until decision 0095's Task 7, which moved every
//! reader onto [`crate::venue_setting::VenueSettings`] and retired it. A credential row still
//! carrying a setting's legacy name now refuses startup (`vike_config::refuse_stranded_venue_settings`)
//! rather than being read by nothing, and `vike-cli secrets move-venue-config` is what moves it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use vike_model::change_journal::{Actor, Change, ChangeJournal, ChangeJournalError, Outcome, Proc};

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
    // ⚠ A fourth finding, `collisions` — the names a `venue_setting` row and a credential row BOTH
    // answered for while the store folded the rows into this map — lived here until decision
    // 0095's Task 7 retired the fold. No row reaches this map now, so nothing can collide in it.
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
    })
}

/// Which store answers for this project — [`Backend`], and the ONE place the choice is made.
///
/// The entry point for a caller holding nothing but the override — and so the `workspace_*_from`
/// spelling, which is this crate's established name for exactly that shape.
/// [`backend_at`] carries the convention all three obey.
///
/// ⚠ **`_from` alone does NOT promise purity here, and reading it that way is what put the old
/// `backend_for` where it was.** In `vike_model::paths::state_path` every `_from` resolver is
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
/// differently about which store is live — the same shape `vike_tradehub::reconcile_config::reconcile_gate`
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
/// `crates/vike-app-core/src/backend/backend_conn.rs`'s `BackendRecord` for *which daemon this box talks
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
    })
}

/// [`resolve_store_in`] over the PROJECT's own settings directory, found by
/// [`crate::workspace_settings_dir_from`] — the database there, or `<project>/settings/secrets.env`
/// when it has none.
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
/// [`resolve_project`] is this function over the walk's answer with
/// [`Table::Credential`](crate::db::Table::Credential), so every
/// composition root already reaches it. What it adds is a door for the callers that do NOT have a
/// `VIKE_SETTINGS_DIR` string to pass: a CLI verb whose own boot resolved the directory as a
/// `&Path`, and every writer, which must consult the same backend the reader will.
///
/// * [`Table::Credential`](crate::db::Table::Credential) ↔ the `credential` table, or
///   `<settings_dir>/secrets.env` with no database
/// * [`Table::NodeKey`](crate::db::Table::NodeKey) ↔ the `node_key` table, or
///   `<settings_dir>/node.env` with no database
///
/// ⚠ **It is deliberately NOT one of `crates/vike-ops/tests/settings_registry.rs`'s
/// `CREDENTIAL_STORE_READERS`**, on that table's own stated criterion. The defect that ratchet
/// hunts is *a library walking for the store from a working directory nothing can redirect*, and a
/// function whose settings directory is a mandatory `&Path` parameter cannot express it — the same
/// reason [`resolve_node_keys`] is excluded there and says so.
///
/// ⚠ It carries NO legacy `secrets.env` fallback for
/// [`Table::NodeKey`](crate::db::Table::NodeKey). That fallback is
/// [`resolve_node_keys`]'s and belongs to the READ path for a box that has not migrated; a caller
/// asking "does the node store exist, and what is in it" before a WRITE must be told about the store
/// the write will land in and no other.
///
/// ⚠ **It answers the credential TABLE and nothing else.** Until decision 0095's Task 7 it also
/// FOLDED ruling 10's `venue_setting` rows into the credential map under their legacy names
/// (`IBKR_DEMO_PORT`, `POLY_RATE_GATE`, …), because every venue reader still looked those names up.
/// The readers take the rows as [`crate::venue_setting::VenueSettings`] now, so the fold is retired;
/// a credential row still carrying a setting's legacy name is refused at boot
/// (`vike_config::refuse_stranded_venue_settings`) rather than read by nothing.
pub fn resolve_store_in(
    settings_dir: &Path,
    table: crate::db::Table,
) -> Result<Resolved, SecretsError> {
    let file = match table {
        crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
        crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
    };
    // No fold: `venue_setting` rows are read through `crate::venue_setting::VenueSettings` alone
    // (decision 0095). A credential row under a legacy name is refused at boot
    // (`vike_config::refuse_stranded_venue_settings`).
    match backend_in(settings_dir) {
        Backend::Database(db) => resolve_database(&db, table, &file),
        Backend::Files => resolve(&file),
    }
}

// ---------------------------------------------------------------------------------------------
// The DEMO-ONLY scope — a process that may never hold a live or real-money credential
// ---------------------------------------------------------------------------------------------

/// **Is `name` WITHHELD under the demo-only scope?** A name naming a LIVE or MAINNET tier
/// (`_LIVE_`/`_MAINNET_` anywhere, or as the last segment), or one of the two by-name families whose
/// DEMO spelling is still real money or a real account: `ASTER_` (no testnet credentials are
/// configured, so even a read signs against the real account) and `POLY_` (polymarket moves live
/// funds). Case-insensitive, which can only widen what is withheld.
///
/// ⚠ It judges the NAME, which is the one thing every requester shares however it spells a tier:
/// a smoke asking through `Environment::Live`, a bridge's own `Env::Live` or `Network::Mainnet`, or
/// a bare `"LIVE"` string all compose a name carrying one of these tokens, and under the scope that
/// name is simply not in the map. A name that carries none of them (an app registration such as
/// `CTRADER_CLIENT_ID`, an attribution code) is not withheld.
///
/// The SQL twin is `crates/vike-secrets/src/db.rs`'s `DEMO_SCOPE_WITHHELD_SQL`;
/// `crates/vike-secrets/tests/demo_only_scope.rs` holds the two equal.
///
/// ⚠ The two families are matched on the name's first `_`-separated segment rather than with a
/// string literal spelling the prefix: `vike_ops::scan`'s map-lookup sweep reads any library string
/// literal that starts with a venue prefix as an environment-variable LOOKUP, and these are not
/// lookups. `split_once('_')` requires the underscore, so this is exactly `starts_with` of the
/// family plus `_`, the SQL twin's `GLOB 'ASTER_*'`.
#[must_use]
pub fn withheld_by_demo_scope(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.contains("_LIVE_")
        || n.ends_with("_LIVE")
        || n.contains("_MAINNET_")
        || n.ends_with("_MAINNET")
        || n.split_once('_').is_some_and(|(family, _)| family == "ASTER" || family == "POLY")
}

/// **The credential store under the DEMO-ONLY scope**: [`resolve_store_in`] for the `credential`
/// table, minus every name [`withheld_by_demo_scope`] matches, plus how many distinct names were
/// withheld. The findings ride through unchanged.
///
/// | store | how the scope applies |
/// |---|---|
/// | the settings DATABASE | [`crate::read_credentials_demo_only`]: the exclusion is in the `WHERE`, so a withheld row's value is never selected |
/// | a credential FILE | [`resolve`], then the withheld names are dropped before this returns — the file arm's usual caveat ([`ScopedSecrets`]' ⚠ section): every value is transiently parsed |
///
/// Never a fallback: a store that will not open is the same loud [`SecretsError`] as everywhere
/// else, and a withheld name is ABSENT from the map — the live gate's answer — rather than replaced.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn resolve_store_demo_only_in(settings_dir: &Path) -> Result<(Resolved, usize), SecretsError> {
    let file = crate::dotenv::secrets_path_in(settings_dir);
    match backend_in(settings_dir) {
        Backend::Database(db) => {
            let (secrets, withheld) = crate::db::read_credentials_demo_only(&db)?;
            let resolved = Resolved {
                secrets,
                source: Source::Database(db.clone()),
                warning: permission_warning(&db),
                legacy: None,
                shadowed: file
                    .exists()
                    .then(|| ShadowedStore { file: file.clone(), db: db.clone() }),
            };
            Ok((resolved, withheld))
        }
        Backend::Files => {
            let mut resolved = resolve(&file)?;
            let all = std::mem::take(&mut resolved.secrets).into_map();
            let before = all.len();
            let kept: HashMap<String, String> =
                all.into_iter().filter(|(name, _)| !withheld_by_demo_scope(name)).collect();
            let withheld = before - kept.len();
            resolved.secrets = SecretMap::from_map(kept);
            Ok((resolved, withheld))
        }
    }
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
/// It does not police where the names came from, and that is a CHOICE rather than a limit. ⚠ The
/// reason this used to give — *"this crate declares no `vike-*` dependency (see the crate doc), so
/// it cannot see `vike_model::credential_keys`' enumerators"* — is false since
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20): those enumerators ARE reachable. The scope still takes any name, because a caller
/// declaring the names it needs is not the same act as the store deciding which names may exist:
/// the bespoke FX shapes, a venue-scoped setting and a key a venue itself rotated are all legal
/// store contents, and a scope that refused an un-enumerated name would turn a read into a second
/// opinion about the credential grammar. What it fixes is the SET: it is built once, before the read, and after
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
/// `shadowed` — because a scoped caller is no less entitled to the store's findings
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
    // ⚠ `collisions`, the half-done-move finding computed inside the scope, lived here until
    // decision 0095's Task 7 retired the store's fold — see the matching note on [`Resolved`].
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
        }
    }

    /// Narrow a whole-store [`Resolved`] to `scope`, keeping every finding.
    ///
    /// ⚠ There used to be a `fold_in` here, a per-name door `vike_bridge_core::credentials` pushed
    /// rendered `venue_setting` names through from above while the renderer lived up there; it went
    /// when the store took the fold over, and the store's fold itself went with decision 0095's
    /// Task 7. A scoped map carries credential rows and nothing else.
    fn narrow(resolved: Resolved, scope: &KeyScope) -> Self {
        let Resolved { secrets, source, warning, legacy, shadowed } = resolved;
        let mut found = BTreeMap::new();
        let mut all = secrets.into_map();
        for name in scope.names() {
            if let Some(v) = all.remove(name) {
                found.insert(name.to_string(), v);
            }
        }
        ScopedSecrets { scope: scope.clone(), found, source, warning, legacy, shadowed }
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
/// arm is [`resolve`], narrowed here — the same composition [`resolve_scoped`] is.
///
/// ⚠ **It answers credential rows only.** Until decision 0095's Task 7 it folded ruling 10's
/// `venue_setting` rows into the scoped map too, inside the scope, because
/// `crates/bridges/polymarket/src/egress.rs`'s now-deleted `dotenv_proxy_vars` read the proxy family
/// through this door and was blind to every moved row without it. That reader went with decision
/// 0095 (the bridge takes its egress from a root's declaration), and the fold went with Task 7:
/// every venue setting is read through [`crate::venue_setting::VenueSettings`] alone.
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
    // ⚠ **This arm does NOT delegate to [`resolve_store_in`]**: the database half asks
    // `crate::read_table_scoped`, which BINDS the declared names and selects no other row, so there
    // is no whole-table read to narrow.
    let resolved = match backend_in(settings_dir) {
        Backend::Database(db) => Resolved {
            secrets: crate::db::read_table_scoped(&db, table, scope)?,
            source: Source::Database(db.clone()),
            warning: permission_warning(&db),
            legacy: None,
            shadowed: file.exists().then(|| ShadowedStore { file: file.clone(), db: db.clone() }),
        },
        Backend::Files => resolve(&file)?,
    };
    Ok(ScopedSecrets::narrow(resolved, scope))
}

/// **Which declared names the store that answers for `settings_dir` holds a NON-BLANK value for —
/// names only.** The presence twin of [`resolve_store_scoped_in`], on the same [`backend_in`]
/// decision, so a presence question and a value read in one process cannot disagree about which
/// store is live.
///
/// | store | how it answers |
/// |---|---|
/// | the settings DATABASE | [`crate::read_present_names_scoped`]: a bound query whose selected column is a boolean, so no value becomes a Rust value here |
/// | a credential FILE | [`resolve`], then the names whose value is non-blank; the parsed map is dropped before this returns — the scoped read's own declared caveat ([`ScopedSecrets`]' ⚠ section), unchanged |
/// | no store | an EMPTY set — the live gate, the same answer every declared name gets from an absent store |
///
/// Blank is what every venue reader calls blank (`str::trim` then empty), so "present" here is
/// what a reader would accept; [`crate::read_present_names_scoped`] declares the one non-ASCII
/// residual of the database arm.
///
/// ⚠ Like [`resolve_store_scoped_in`], deliberately NOT one of
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_READERS`: its settings
/// directory is a mandatory `&Path` parameter.
///
/// # Errors
/// [`SecretsError`] when a store that EXISTS will not open — never folded into an empty set,
/// because an unreadable store reported as "nothing stored" sends the operator to store a key they
/// already stored.
pub fn present_names_scoped_in(
    settings_dir: &Path,
    table: crate::db::Table,
    scope: &KeyScope,
) -> Result<BTreeSet<String>, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => Ok(crate::db::read_present_names_scoped(&db, table, scope)?),
        Backend::Files => {
            let file = match table {
                crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
                crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
            };
            let scoped = resolve_scoped(&file, scope)?;
            Ok(scoped
                .found
                .iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(name, _)| name.clone())
                .collect())
        }
    }
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
/// `vike_model::accounts::account_keys::accounts_in_store` is the reader for that store, and it is the one
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
/// accounts live in the key names and `vike_model::accounts::account_keys::accounts_in_store` is the reader
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
/// they were built for: the dukascopy mount has to turn an `account` row into a broker, and
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

/// **Every row of the `venue` table in the database beside `settings_dir`** — [`crate::db::read_venues`]
/// over [`crate::dotenv::db_path_in`], the venue-table twin of [`resolve_accounts_in`] for a caller
/// that already holds the settings DIRECTORY. Read-only: an absent database, or one that predates
/// the table, answers an empty list, and the read creates nothing.
///
/// ⚠ **It lives HERE and not beside `read_venues`, and the reason is a gate.**
/// `crates/vike-secrets/src/db.rs` is the module that defines `migrate`, and
/// `crates/vike-ops/tests/credential_source_roster_gate.rs`'s
/// `the_migration_reads_exactly_the_sources_the_roster_names` treats every path-resolver call in
/// that module as a path the migration reads or writes. A READER that resolved a path there would
/// read as a source of the migration. This file already resolves the path for every other `_in`
/// reader, so the one call belongs beside them.
///
/// # Errors
/// [`crate::DbError`] when a database that exists will not read.
pub fn read_venues_in(settings_dir: &Path) -> Result<Vec<crate::db::VenueRow>, crate::db::DbError> {
    crate::db::read_venues(&crate::dotenv::db_path_in(settings_dir))
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
/// * the FILE branch is [`crate::save_credentials`] VERBATIM — not reimplemented, not wrapped, not
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
/// derived from the key NAME by machinery this crate cannot see — `vike-bridge-core` declares
/// `layer = 25` where tier 15's rule is *nothing above rank 10*, and it declares `vike-secrets`
/// itself, so the edge is a cycle as well as a band violation — and it arrives as a closure for
/// that reason. `vike_bridge_core::credentials::classify_credential_name` is the production
/// implementation. ⚠ This sentence read *"the same seam and for the same layering reason
/// [`crate::migrate`] takes `is_node_key` through"*, and the second half is false:
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20) took THAT seam's layer bound away — `vike_model::credential_keys::is_platform_key`
/// is reachable from here — and it was ruled on 2026-09-26 to stay on different grounds, written
/// out in [`crate::migrate`]'s own doc. The shape is shared; the argument is not.
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

/// **What [`Change::credential_write`] and [`Change::account_lifecycle`] need to describe WHO is
/// writing, and how — the two things that were never derivable from the write itself.**
///
/// A finding, in the same sense [`PermissionWarning`] and [`LegacyStoreWarning`] are: this crate
/// carries no logging dependency (see the crate doc), so a journal-append failure is returned as
/// [`JournalAppendError`] rather than logged, exactly as `vike_model::change_journal::
/// ChangeJournalError`'s own doc argues for itself. The CALLER — `vike-connections`, which already
/// links `tracing` for its own console lines — logs it.
#[derive(Debug, Clone)]
pub struct CredentialJournal<'a> {
    /// WHO is writing.
    pub actor: Actor,
    /// The venue the keys belong to, or `"multi"` for a save spanning several.
    pub venue: &'a str,
    /// The credential tier (`"SIM"`/`"DEMO"`/`"LIVE"`, or
    /// `vike_model::change_journal::TIER_UNTIERED`).
    pub tier: &'a str,
    /// The writing process's identity — see `vike_model::change_journal::Proc`. A PARAMETER, never
    /// derived with `Proc::current()` in here: this function runs inside every binary that links
    /// this crate, so `env!("CARGO_PKG_VERSION")` read HERE would name `vike-secrets`'s own version
    /// rather than the caller's, which is exactly the confusion `Proc`'s own doc — *"which binary
    /// wrote this"* — exists to answer honestly.
    pub proc: Proc,
    /// The instant to stamp the record with. A PARAMETER, because `vike_model::change_journal`
    /// reads no clock — the instant travels from the composition root all the way down.
    pub now_ms: i64,
}

/// Why a journal append did not happen. See [`CredentialJournal`] for why this is returned rather
/// than logged.
#[derive(Debug)]
pub struct JournalAppendError {
    /// The journal directory the append was attempted against.
    pub dir: PathBuf,
    /// The underlying failure.
    pub source: ChangeJournalError,
}

impl std::fmt::Display for JournalAppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "change NOT recorded to the change journal at {} ({}) — the store write ITSELF already \
             committed",
            self.dir.display(),
            self.source
        )
    }
}

impl std::error::Error for JournalAppendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The journal beside `settings_dir` — `<settings_dir>/state/changes`, the same derivation
/// `vike_model::paths::state_path::project_state_dir(_from)` uses (`project_settings_dir(start)?.join(
/// STATE_SUBDIR)`), so a caller handing this function the SAME settings directory it read the store
/// from can never resolve a ledger describing a different project than the one the write landed in.
pub(crate) fn journal_beside(settings_dir: &Path, process: Proc) -> ChangeJournal {
    ChangeJournal::in_state_dir(
        &settings_dir.join(vike_model::paths::state_path::STATE_SUBDIR),
        process,
    )
}

/// **[`save_credentials_to_store`], plus its durable [`Change::credential_write`] record —
/// together, so the two cannot drift apart at a call site.**
///
/// This is the collapse of what used to be a two-call pattern at `vike-connections`'
/// (`save_credentials_journalled`, now deleted): a caller resolved the store's path AND the ledger
/// separately, then had to remember to call both. Here there is one call, and the record is built
/// from what this function ITSELF just wrote — `updates`' key NAMES and nothing else, per
/// [`Change::credential_write`]'s own "note what this signature does NOT take".
///
/// # Ordering, and the two failure paths
///
/// The store write happens FIRST (via [`save_credentials_to_store`]) and its error returns
/// unchanged — a write that did not land must not leave a record saying it did. A JOURNAL failure,
/// by contrast, cannot fail the call: the credential IS on disk, and sending the caller down an
/// error path for a write that succeeded would be worse than a silently-missed ledger line. It
/// comes back as `Some(JournalAppendError)` in the `Ok` tuple for the caller to log — see
/// [`CredentialJournal`] for why this crate cannot log it itself.
pub fn save_credentials_to_store_journalled(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
    journal: CredentialJournal<'_>,
) -> std::io::Result<(Backend, Option<JournalAppendError>)> {
    let backend = save_credentials_to_store(settings_dir, table, updates, classify)?;

    // ⚠ `.0` ONLY — the projection that makes a value unrepresentable downstream even before
    // `Change::credential_write`'s own signature refuses to hold one.
    let keys: Vec<&str> = updates.iter().map(|(key, _value)| key.as_str()).collect();
    // The FILE NAME this record has always carried, `secrets.env`, whichever backend actually
    // answered — see `Change::credential_write`'s `store` cell: it names the historical credential
    // surface, not the live one, and changing that is a decision for a later record, not this move.
    let change = Change::credential_write(
        Outcome::Applied,
        journal.actor,
        crate::dotenv::SECRETS_FILE,
        journal.venue,
        journal.tier,
        &keys,
    );
    let cj = journal_beside(settings_dir, journal.proc);
    let journal_error = cj
        .append(journal.now_ms, &change)
        .err()
        .map(|source| JournalAppendError { dir: cj.dir().to_path_buf(), source });
    Ok((backend, journal_error))
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
/// and `vike_model::accounts::account_keys::accounts_in_store` is the reader that applies. There is nothing
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
/// and `vike_model::accounts::account_keys::accounts_in_store` is the reader that applies. There is nothing
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

/// **WHO is writing a store edit that carries no credential value — an account-lifecycle edit, or a
/// venue setting — and when.** The twin of [`CredentialJournal`], for the same reason:
/// [`edit_account_in_journalled`] and [`crate::settings::set_venue_setting_in_journalled`] cannot
/// derive either cell from the edit itself.
#[derive(Debug, Clone)]
pub struct AccountJournal {
    /// WHO is writing.
    pub actor: Actor,
    /// The writing process's identity. See [`CredentialJournal::proc`] for why this is a parameter.
    pub proc: Proc,
    /// The instant to stamp the record with.
    pub now_ms: i64,
}

/// **[`edit_account_in`], plus its durable [`Change::account_lifecycle`] record — together**, the
/// account-plane twin of [`save_credentials_to_store_journalled`] and the collapse of what used to
/// be `vike-connections`' `edit_account_journalled`.
///
/// # What it never carries
///
/// **No credential value, on any path** — [`edit_account_in`] never selects a `value` column, and
/// `Change::account_lifecycle` takes no value parameter, which is the enforcement rather than a
/// convention. The key NAMES that DO reach the record are recorded for the reason
/// [`Change::credential_write`]'s doc gives: they are not secret, and *"what did that account own
/// when it was deactivated"* is unanswerable without them.
///
/// # Ordering, and the two failure paths
///
/// The row write happens FIRST and its error returns unchanged — an edit that did not land must not
/// leave a record saying it did. Nothing to record when nothing CHANGED: a ledger line for a no-op
/// reads as an edit that did not happen. A JOURNAL failure, by contrast, cannot fail the call: the
/// row IS written, and it comes back as `Some(JournalAppendError)` for the caller to log.
///
/// # Errors
/// [`crate::DbError`] for every refusal [`edit_account_in`] states.
pub fn edit_account_in_journalled(
    settings_dir: &Path,
    edit: crate::db::AccountEdit<'_>,
    journal: AccountJournal,
) -> Result<(crate::db::AccountWrite, Option<JournalAppendError>), crate::db::DbError> {
    let done = edit_account_in(settings_dir, edit)?;
    if !done.changed {
        return Ok((done, None));
    }
    let journal_error = {
        let Some(row) = done.after.as_ref().or(done.before.as_ref()) else {
            return Ok((done, None));
        };
        let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
        let change = Change::account_lifecycle(
            Outcome::Applied,
            journal.actor,
            crate::dotenv::DB_FILE,
            done.verb,
            row.id,
            &row.venue,
            &row.tier,
            done.before.as_ref().and_then(|b| b.label.as_deref()),
            done.after.as_ref().and_then(|a| a.label.as_deref()),
            done.after.as_ref().is_some_and(|a| a.active),
            &keys,
        );
        let cj = journal_beside(settings_dir, journal.proc);
        cj.append(journal.now_ms, &change)
            .err()
            .map(|source| JournalAppendError { dir: cj.dir().to_path_buf(), source })
    };
    Ok((done, journal_error))
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

#[path = "store_tests.rs"]
#[cfg(test)]
mod store_tests;

// ⚠ `FoldOutcome` lived here and is GONE, deliberately. It was `ScopedSecrets::fold_in`'s answer —
// the per-name door `vike_bridge_core::credentials` pushed rendered names through from above, back
// when the renderer lived in that crate and the fold could not run inside the store. The renderer
// moved down on 2026-09-22 and the store folded natively; decision 0095's Task 7 then retired the
// fold altogether, so no `venue_setting` row reaches any credential map.
// `crates/vike-ops/tests/smoke_store_parity_gate.rs` is what keeps a second fold from appearing.
