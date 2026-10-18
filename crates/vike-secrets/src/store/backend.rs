//! WHICH store answers a run (`Backend`, per-RUN), and the ONE handle every read goes through:
//! `CredentialStore`, opened over a `StoreLocation` under a `CredentialScope`.

use super::*;

/// **IS THERE A CREDENTIAL STORE ON THIS BOX** — the one decision every reader and every writer in
/// this crate asks first, and the reason there is no ladder.
///
/// # The settings database is the ONLY credential store
///
/// `<project>/settings/db/vike.db` holds every credential and every hub key
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
/// * **Every READ answers an empty map** — [`Source::None`], the live gate: no credentials, so no
///   `demo` or `live` account runs. Never a panic, never an environment variable read in place of a credential
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

/// **What a whole-map read found**: the credentials, WHICH store they came from, and every finding
/// about it — as DATA. [`Resolved::report`] is the ONE place those findings become log lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The credential map. Empty when there is no store — the live gate.
    pub secrets: SecretMap,
    /// Which store answered.
    pub source: Source,
    /// Set when the store is readable beyond its owner. [`Resolved::report`] logs it; a caller that
    /// prints it itself (`vike-cli secrets`) reads it here. See [`PermissionWarning`].
    pub warning: Option<PermissionWarning>,
    /// The handle's [`CredentialScope`] this read was made under.
    pub scope: CredentialScope,
    /// How many DISTINCT names a narrowed scope withheld; `0` under
    /// [`CredentialScope::Full`], where no count is taken.
    pub withheld: usize,
    /// Whether the store answered at all. [`StoreHealth::Unreadable`] only on the EMPTY answer
    /// [`Resolved::unreadable`] builds for a store that exists and would not open — so a caller that
    /// RENDERS A COUNT knows the zero is not a measurement.
    pub health: StoreHealth,
}

impl Resolved {
    /// The answer for a box with NO store (no location, or no database): an empty map,
    /// [`Source::None`] — the live gate. Infallible on purpose: there is no store to fail to open.
    pub(super) fn absent(scope: CredentialScope) -> Resolved {
        Resolved {
            secrets: SecretMap::default(),
            source: Source::None,
            warning: None,
            scope,
            withheld: 0,
            health: StoreHealth::Readable,
        }
    }

    /// **The answer for a store that EXISTS and would not open**, for a caller that degrades rather
    /// than refuses: an EMPTY map (the live gate: no `demo` or `live` account runs) carrying
    /// [`StoreHealth::Unreadable`] with the error's text — a path and an OS reason, never a value —
    /// so [`Resolved::report`] logs it LOUDLY and a count rendered from the map reads "unknown".
    #[must_use]
    pub fn unreadable(scope: CredentialScope, e: &SecretsError) -> Resolved {
        Resolved { health: StoreHealth::Unreadable(e.to_string()), ..Resolved::absent(scope) }
    }

    /// **Log this read's findings — the ONE place a store finding becomes a log line.** The READS
    /// never log: a caller that runs before a subscriber exists (`vike_boot`) or prints the finding
    /// itself must not have a line chosen for it, so a root calls this once it can log.
    ///
    /// | finding | line |
    /// |---|---|
    /// | the store would not open ([`StoreHealth::Unreadable`]) | `error!`, naming the store and the reason; nothing else is reported |
    /// | the store is exposed or a symlink ([`PermissionWarning`]) | `warn!`, ONCE per distinct message per process ([`warn_once`]) |
    /// | [`CredentialScope::NoLiveKeys`] | `warn!`, how many names the store withheld |
    /// | [`CredentialScope::NoVenueKeys`] | `info!`, how many names the store withheld |
    pub fn report(&self) {
        report_findings(self.warning.as_ref(), self.scope, self.withheld, &self.health);
    }
}

/// The body of [`Resolved::report`] and [`ScopedSecrets::report`]: ONE spelling of every finding's
/// line, so the two read shapes cannot word a finding two ways.
pub(super) fn report_findings(
    warning: Option<&PermissionWarning>,
    scope: CredentialScope,
    withheld: usize,
    health: &StoreHealth,
) {
    if let StoreHealth::Unreadable(e) = health {
        // `SecretsError` carries no secret material (see its doc) — safe to log verbatim.
        match scope {
            CredentialScope::Full => tracing::error!(
                "credential store could not be opened, continuing with NO credentials \
                 (every venue stays paper): {e}"
            ),
            CredentialScope::NoLiveKeys => tracing::error!(
                "credential store could not be opened under the demo-only scope, continuing with \
                 NO credentials: {e}"
            ),
            CredentialScope::NoVenueKeys => tracing::error!(
                "credential store could not be opened under the no-venue-keys scope, continuing \
                 with no hub keys and no deployment rows: {e}"
            ),
        }
        return;
    }
    if let Some(w) = warning {
        // `PermissionWarning`: paths and an octal mode; on `Finding::Symlink` the second path is
        // the link's TARGET, still not a value.
        warn_once(&w.to_string());
    }
    if scope == CredentialScope::NoLiveKeys {
        tracing::warn!(
            "DEMO-ONLY credential scope ({NO_LIVE_KEYS_ENV}=true): the store withheld {withheld} \
             name(s) of live-tier accounts and of aster/polymarket; a request for any of them \
             reads as absent"
        );
    }
    if scope == CredentialScope::NoVenueKeys {
        tracing::info!(
            "NO-VENUE-KEYS credential scope (chosen by this binary's boot, not by the \
             environment): the store withheld {withheld} name(s) of account and venue \
             credentials; this process holds no venue key value"
        );
    }
}

/// [`backend_at`] for a caller that already holds the settings DIRECTORY as a path.
///
/// **Every [`CredentialStore`] read and every writer in this crate asks exactly this**, which is
/// what makes the reader and the writer structurally incapable of disagreeing about whether there
/// is a store: they take the same directory and ask the same function.
#[must_use]
pub fn backend_in(settings_dir: &Path) -> Backend {
    backend_at(&crate::store_locator::db_path_in(settings_dir))
}

/// The PURE root of the two — the database path arrives as a parameter.
///
/// | spelling | the caller holds | the path resolver it wraps |
/// |---|---|---|
/// | [`backend_in`] | the settings DIRECTORY — `&Path` | [`crate::db_path_in`] |
/// | [`backend_at`] | the DATABASE's own path — `&Path` | — it IS the path |
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

/// **WHERE a [`CredentialStore`] lives: the settings DIRECTORY** (`<project>/settings`), already
/// resolved. The database is `db/vike.db` inside it ([`crate::db_path_in`]).
///
/// Resolving it is the ROOT's job, done once: `vike_boot::boot`'s step 2 (`Booted::store`), or
/// [`crate::open_credential_store`] for a root that runs no boot. A test or a verb that names a
/// directory outright (a temporary directory, an operator's `--from`) builds one with
/// [`StoreLocation::new`]. This type resolves nothing and performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreLocation(PathBuf);

impl StoreLocation {
    /// The store in `settings_dir`, a directory the caller has already resolved. No I/O: whether a
    /// database is there is [`CredentialStore::backend`]'s question, asked per read.
    #[must_use]
    pub fn new(settings_dir: impl Into<PathBuf>) -> StoreLocation {
        StoreLocation(settings_dir.into())
    }

    /// The settings directory — what a writer in this crate takes, and where the change journal
    /// sits beside the store (`<settings>/state/changes`).
    #[must_use]
    pub fn settings_dir(&self) -> &Path {
        &self.0
    }
}

/// **THE credential store, as one handle** — a location and a scope, and every READ the workspace
/// performs on it. `Clone` and cheap; opening it performs NO I/O, and every read asks the backend
/// afresh (one `is_file`), so a handle built at startup sees a store created later.
///
/// # One handle, one scope, every read under it
///
/// The [`CredentialScope`] is a property of the HANDLE, fixed at [`CredentialStore::open`], and is
/// applied in the statement of every read it offers — [`Self::read_all`], [`Self::read`],
/// [`Self::present_names`], [`Self::account_keys`] and [`Self::hub_keys`]. There is no public read
/// of credential rows beside these, so no caller can read around the scope by picking a different
/// door: the doors that once could are gone.
///
/// # What each read answers
///
/// | on disk | result |
/// |---|---|
/// | the settings database, readable | the rows, [`Source::Database`], findings as data |
/// | the settings database, unreadable | [`SecretsError`] — loud, never "no credentials" |
/// | no location (`open(None, _)`), or no database | EMPTY, [`Source::None`] — the live gate, never an error |
///
/// A read never logs; its answer carries the findings and the caller reports them
/// ([`Resolved::report`], [`ScopedSecrets::report`]) once it can log. A caller that degrades on an
/// unreadable store builds the empty answer with [`Resolved::unreadable`] /
/// [`ScopedSecrets::unreadable`], which `report` then logs at `error`.
///
/// # Writes are not here
///
/// The writers take the settings DIRECTORY ([`StoreLocation::settings_dir`]) and a journal, and
/// record every change they make: `crate::save_credentials_to_store` and its siblings. A write does
/// not need the scope (a scope only narrows what is READ).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStore {
    location: Option<StoreLocation>,
    scope: CredentialScope,
}

impl CredentialStore {
    /// **Open the handle.** No I/O. `None` is the box with no store at all: every read answers
    /// empty (the live gate), never an error.
    #[must_use]
    pub fn open(location: Option<StoreLocation>, scope: CredentialScope) -> CredentialStore {
        CredentialStore { location, scope }
    }

    /// Where the store lives, or `None` when the handle was opened over no location.
    #[must_use]
    pub fn location(&self) -> Option<&StoreLocation> {
        self.location.as_ref()
    }

    /// The scope every read of this handle is made under.
    #[must_use]
    pub fn scope(&self) -> CredentialScope {
        self.scope
    }

    /// **Is there a store?** [`backend_in`] over the location — the same probe every writer asks —
    /// and [`Backend::Absent`] for a handle with no location.
    #[must_use]
    pub fn backend(&self) -> Backend {
        match &self.location {
            Some(location) => backend_in(location.settings_dir()),
            None => Backend::Absent,
        }
    }

    /// **The whole `credential` table, under this handle's scope** — for the callers that need the
    /// SET: a live mount composes its key names from a grid, each venue's own names and an unbounded
    /// account label, so no list it could declare would be complete; `vike-cli secrets list` and
    /// `config show` have the store itself as their subject. A caller that knows its names takes
    /// [`Self::read`] instead and holds nothing else.
    ///
    /// Under [`CredentialScope::NoLiveKeys`] a withheld row is never SELECTED (its value never
    /// becomes a Rust value here) and [`Resolved::withheld`] says how many names were.
    ///
    /// ⚠ **It answers the credential TABLE and nothing else.** `venue_setting` rows are read through
    /// [`crate::venue_setting::VenueSettings`] alone (decision 0095); a credential row still carrying
    /// a setting's legacy name is read by nothing.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn read_all(&self) -> Result<Resolved, SecretsError> {
        match self.backend() {
            Backend::Database(db) => {
                let (secrets, withheld) =
                    crate::db::read_table(&db, crate::db::Table::Credential, self.scope)?;
                Ok(Resolved {
                    secrets,
                    source: Source::Database(db.clone()),
                    // A finding, never a refusal — see `permission_warning`.
                    warning: permission_warning(&db),
                    scope: self.scope,
                    withheld,
                    health: StoreHealth::Readable,
                })
            }
            Backend::Absent => Ok(Resolved::absent(self.scope)),
        }
    }

    /// **Every account's and every venue's credentials, by FIELD** — the live `credential` rows
    /// filed against an account (`account_id` + `field`) or a venue (`venue_id` + `field`), under
    /// this handle's scope. A composition root reads it ONCE, beside [`Self::accounts`] and
    /// [`Self::account_keys`], and hands the three down as one
    /// [`crate::account_directory::AccountDirectory`]; the mount builds each account's
    /// [`crate::account_credentials::AccountCredentials`] out of it (decision 0156 item 6). A
    /// DEPLOYMENT row (no account, no venue) is not here: it is read by name, out of
    /// [`Self::read_all`].
    ///
    /// No store answers an EMPTY book (the live gate), never an error.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn credential_book(
        &self,
    ) -> Result<crate::account_credentials::CredentialBook, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(crate::db::read_credential_book(&db, self.scope)?),
            Backend::Absent => Ok(crate::account_credentials::CredentialBook::new()),
        }
    }

    /// **ONE account's credentials and ONE venue's application credentials, by FIELD** — the live
    /// rows whose `account_id` is `account` (`None`: no account) or whose venue is `venue`, under
    /// this handle's scope, and NO other row: the statement names them, so every other account's
    /// rows (a live account's among them) are never SELECTED. It is [`Self::credential_book`] for a
    /// root that has already chosen the account from the key-less reads ([`Self::accounts`],
    /// [`Self::account_keys`]) and must hold nothing else (the gatewayhub's credentialed data
    /// feeds). No store answers an EMPTY book (the live gate), never an error.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn credential_book_for(
        &self,
        account: Option<i64>,
        venue: &str,
    ) -> Result<crate::account_credentials::CredentialBook, SecretsError> {
        match self.backend() {
            Backend::Database(db) => {
                Ok(crate::db::read_credential_book_for(&db, account, venue, self.scope)?)
            }
            Backend::Absent => Ok(crate::account_credentials::CredentialBook::new()),
        }
    }

    /// **ONE field of the ONE active account of `(venue, label, tier)`**, under this handle's scope —
    /// for a root that needs one account's one value and must hold nothing else (the datahub's OANDA
    /// history token). Only that row's value is selected. `None` when no single active row of that
    /// address exists, or the field is absent or blank; no store answers `None` (the live gate).
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn account_field(
        &self,
        venue: &str,
        label: &vike_model::accounts::account_keys::AccountLabel,
        tier: vike_model::AccountTier,
        field: &str,
    ) -> Result<Option<vike_model::secret::Secret<String>>, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(crate::db::read_account_field(
                &db,
                venue,
                label.text(),
                tier,
                field,
                self.scope,
            )?
            .map(vike_model::secret::Secret::new)),
            Backend::Absent => Ok(None),
        }
    }

    /// [`Self::account_field`]'s PRESENCE twin: whether that one field holds a non-blank value,
    /// computed by SQLite — the value never becomes a Rust value in this process. No store answers
    /// `false`.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn account_field_present(
        &self,
        venue: &str,
        label: &vike_model::accounts::account_keys::AccountLabel,
        tier: vike_model::AccountTier,
        field: &str,
    ) -> Result<bool, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(crate::db::read_account_field_present(
                &db,
                venue,
                label.text(),
                tier,
                field,
                self.scope,
            )?),
            Backend::Absent => Ok(false),
        }
    }

    /// **The NAMES of the live DEPLOYMENT rows** — filed against neither an account nor a venue (the
    /// pager token, a data provider's key, an attribution code written by name), under this handle's
    /// scope. Names only. What a by-NAME writer may rotate, told apart from an account's or a venue's
    /// row by the row's own columns — never by the shape of its name (decision 0156 item 6). No store
    /// answers an empty set.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn deployment_names(&self) -> Result<std::collections::BTreeSet<String>, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(crate::db::read_deployment_names(&db, self.scope)?),
            Backend::Absent => Ok(std::collections::BTreeSet::new()),
        }
    }

    /// **A watcher of this store's commits** ([`crate::StoreWatch`], decision 0169): `None` when there is no
    /// store (nothing to watch). The one thing it reads is the database header's change counter, never
    /// a row, so it is outside this handle's [`CredentialScope`] by construction.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn watch(&self) -> Result<Option<crate::db::StoreWatch>, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(Some(crate::db::StoreWatch::begin(&db)?)),
            Backend::Absent => Ok(None),
        }
    }
}
