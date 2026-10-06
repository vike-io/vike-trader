//! WHICH store answers a run (`Backend`, per-RUN) and the front doors that open it: `resolve*`.

use super::*;

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
pub(super) fn resolve_database(
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
/// ⚠ **It is deliberately NOT one of `crates/vike-ops/tests/settings/settings_registry/credential_store_scan.rs`'s
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
