//! The demo-only scope and the SCOPED read: a process holds only the names it declared.

use super::*;

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
/// The SQL twin is `crates/vike-secrets/src/db/read.rs`'s `DEMO_SCOPE_WITHHELD_SQL`;
/// `crates/vike-secrets/tests/reads/demo_only_scope.rs` holds the two equal.
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
/// `crates/vike-ops/tests/settings/settings_registry/credential_store_scan.rs`'s `CREDENTIAL_STORE_READERS`: its settings
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
/// `crates/vike-ops/tests/settings/settings_registry/credential_store_scan.rs`'s `CREDENTIAL_STORE_READERS`: its settings
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
