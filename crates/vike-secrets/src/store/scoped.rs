//! The credential SCOPE a handle reads under (`CredentialScope`, the no-live-keys rule) and the
//! SCOPED read by declared names (`KeyScope`): what a process may hold, decided before the store
//! is opened.

use super::*;

// ---------------------------------------------------------------------------------------------
// The NO-LIVE-KEYS scope — a process that may never hold a live or real-money credential
// ---------------------------------------------------------------------------------------------

/// The ONE switch that narrows a process's [`CredentialStore`] to [`CredentialScope::NoLiveKeys`]:
/// `VIKE_NO_LIVE_KEYS`, whose only accepted values are `true` and `false`
/// ([`CredentialScope::parse`]).
///
/// ⚠ **This crate never reads it.** The ROOT reads it once, out of the one environment sweep it
/// owns — `vike_boot` for a booting root, and for one that runs no boot the map the root hands to
/// [`crate::open_credential_store`] — and the parsed [`CredentialScope`] reaches
/// [`CredentialStore::open`]. It replaced the retired `demo` credential-scope word, which only the
/// whole-map door read, so the scope bound one door family; the scope is now a property of the
/// handle, applied below every read it offers.
///
/// It is an environment variable deliberately: it is a property of ONE PROCESS, and the database is
/// shared by every process on the box.
pub const NO_LIVE_KEYS_ENV: &str = "VIKE_NO_LIVE_KEYS";

/// **Which credential rows a [`CredentialStore`] may hand to this process.** A property of the
/// handle, fixed when it is opened, and applied in the STATEMENT of every read the handle offers —
/// the whole map, the declared-name read, the presence read, the account key names and the node
/// keys — so there is no read of credential rows beside it that skips it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CredentialScope {
    /// Every row the store holds — the scope of a process nobody narrowed.
    #[default]
    Full,
    /// No LIVE or real-money credential: a row the scope withholds is never selected, so a request
    /// for it reads as ABSENT (the live gate's answer), and the read reports how many names were
    /// withheld. It withholds every credential of an account whose row's tier is `live` — the tier
    /// the operator wrote, never one read out of a name (decision 0156 item 6) — and every row of
    /// the two venue families whose demo is still real money, aster and polymarket.
    /// `crate::db`'s `DEMO_SCOPE_WITHHELD_SQL` is the predicate.
    NoLiveKeys,
}

impl CredentialScope {
    /// **Parse [`NO_LIVE_KEYS_ENV`]'s value, strictly.**
    ///
    /// | value | scope |
    /// |---|---|
    /// | unset (`None`) | [`CredentialScope::Full`] |
    /// | `true`, any letter case | [`CredentialScope::NoLiveKeys`] |
    /// | `false`, any letter case | [`CredentialScope::Full`] |
    /// | anything else — `1`, `yes`, a typo, an empty or blank string, a padded `" true"` | [`ScopeError`] |
    ///
    /// ⚠ A word outside the two is REFUSED rather than guessed at: a process told to hold no live
    /// key that misread the switch as "unset" would hold every one. The root turns the error into a
    /// startup refusal.
    ///
    /// # Errors
    /// [`ScopeError`] for every value but `true` and `false`.
    pub fn parse(value: Option<&str>) -> Result<CredentialScope, ScopeError> {
        match value {
            None => Ok(CredentialScope::Full),
            Some(v) if v.eq_ignore_ascii_case("true") => Ok(CredentialScope::NoLiveKeys),
            Some(v) if v.eq_ignore_ascii_case("false") => Ok(CredentialScope::Full),
            Some(v) => Err(ScopeError { value: v.to_string() }),
        }
    }
}

/// [`CredentialScope::parse`]'s refusal: the value is neither `true` nor `false`. Carries the value,
/// a mode word and never a credential, so it is safe to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeError {
    /// The value that was refused, verbatim.
    pub value: String,
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{NO_LIVE_KEYS_ENV}={:?} is refused: only `true` or `false` is accepted (unset is \
             `false`), so NO credential store was opened rather than a guess at what was meant",
            self.value
        )
    }
}

impl std::error::Error for ScopeError {}

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
/// it cannot see `vike_venues::credential_keys`' enumerators"* — is false since
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
/// `vike_secrets::credentials::StoreHealth` (an empty map from an ABSENT store is not the same
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
/// The [`Resolved`] findings are carried through unchanged — `source`, `warning`, the handle's
/// `credential_scope`, `withheld` and `health` — because a scoped caller is no less entitled to the
/// store's finding than a whole-table one, and `vike-cli secrets` and every composition root print
/// it from exactly these fields. [`ScopedSecrets::report`] is where they become log lines.
///
/// The narrowing is a genuinely narrower QUERY: `crate::db::read_table_scoped` binds the declared
/// names and selects no other row. A box with no database answers an empty scope.
#[derive(Clone, PartialEq, Eq)]
pub struct ScopedSecrets {
    scope: KeyScope,
    found: BTreeMap<String, String>,
    /// Which store answered — the same value [`Resolved::source`] carries.
    pub source: Source,
    /// The store's permission finding, unchanged from [`Resolved::warning`].
    pub warning: Option<PermissionWarning>,
    /// The handle's [`CredentialScope`] this read was made under.
    pub credential_scope: CredentialScope,
    /// How many DECLARED names the store holds a row for that [`CredentialScope::NoLiveKeys`]
    /// withheld; `0` under [`CredentialScope::Full`].
    pub withheld: usize,
    /// [`StoreHealth::Unreadable`] when this is the EMPTY answer for a store that would not open
    /// ([`ScopedSecrets::unreadable`]).
    pub health: StoreHealth,
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

    /// The EMPTY answer for a declared scope, from a store that answered with nothing: every
    /// declared name reads [`Lookup::AbsentFromStore`] (the live gate) and an undeclared one still
    /// reads [`Lookup::NotDeclared`]. Readable, unscoped, nothing withheld.
    #[must_use]
    pub fn empty(scope: &KeyScope, source: Source) -> Self {
        ScopedSecrets {
            scope: scope.clone(),
            found: BTreeMap::new(),
            source,
            warning: None,
            credential_scope: CredentialScope::Full,
            withheld: 0,
            health: StoreHealth::Readable,
        }
    }

    /// **The answer for a store that EXISTS and would not open**, for a caller that degrades rather
    /// than refuses: the [`Self::empty`] scope (every declared name the live gate's answer, an
    /// undeclared one still [`Lookup::NotDeclared`], so the two failures stay distinguishable), with
    /// [`StoreHealth::Unreadable`] carrying the error's text for [`Self::report`] to log.
    #[must_use]
    pub fn unreadable(
        scope: &KeyScope,
        credential_scope: CredentialScope,
        e: &SecretsError,
    ) -> Self {
        ScopedSecrets {
            credential_scope,
            health: StoreHealth::Unreadable(e.to_string()),
            ..ScopedSecrets::empty(scope, Source::None)
        }
    }

    /// **Log this read's findings** — the one place they become lines, shared with
    /// [`Resolved::report`] (see it for the lines and their levels).
    pub fn report(&self) {
        report_findings(self.warning.as_ref(), self.credential_scope, self.withheld, &self.health);
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

impl CredentialStore {
    /// **The DECLARED credentials and nothing else** — the scoped read, one statement per declared
    /// name, under this handle's [`CredentialScope`].
    ///
    /// | store | answer |
    /// |---|---|
    /// | the settings DATABASE, readable | the declared names it holds, minus any the scope withholds |
    /// | the settings DATABASE, unreadable | [`SecretsError`] — loud, never "no credentials" |
    /// | no location, or no database | an EMPTY scope, [`Source::None`] — the live gate |
    ///
    /// The same [`CredentialStore::backend`] decision every other read here makes, so a scoped
    /// reader and a whole-table reader in one process cannot disagree about which store is live.
    /// It answers `credential` rows only; venue settings are
    /// [`crate::venue_setting::VenueSettings`]' (decision 0095). Findings come back as data:
    /// [`ScopedSecrets::report`] logs them.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn read(&self, keys: &KeyScope) -> Result<ScopedSecrets, SecretsError> {
        match self.backend() {
            Backend::Database(db) => {
                let (secrets, withheld) = crate::db::read_table_scoped(
                    &db,
                    crate::db::Table::Credential,
                    keys,
                    self.scope(),
                )?;
                Ok(ScopedSecrets {
                    scope: keys.clone(),
                    found: secrets.into_map().into_iter().collect(),
                    source: Source::Database(db.clone()),
                    warning: permission_warning(&db),
                    credential_scope: self.scope(),
                    withheld,
                    health: StoreHealth::Readable,
                })
            }
            Backend::Absent => Ok(ScopedSecrets {
                credential_scope: self.scope(),
                ..ScopedSecrets::empty(keys, Source::None)
            }),
        }
    }

    /// **Which declared names the store holds a NON-BLANK value for — names only**, under this
    /// handle's scope (a withheld name is not present). The presence twin of
    /// [`CredentialStore::read`], on the same backend decision, so a presence question and a value
    /// read in one process cannot disagree about which store is live.
    ///
    /// | store | how it answers |
    /// |---|---|
    /// | the settings DATABASE | `crate::db::read_present_names_scoped`: a bound query whose selected column is a boolean, so no value becomes a Rust value here |
    /// | no location, or no database | an EMPTY set — the live gate, the same answer every declared name gets from an absent store |
    ///
    /// Blank is what every venue reader calls blank (`str::trim` then empty), so "present" here is
    /// what a reader would accept; that reader declares the one non-ASCII residual.
    ///
    /// # Errors
    /// [`SecretsError`] when a store that EXISTS will not open — never folded into an empty set,
    /// because an unreadable store reported as "nothing stored" sends the operator to store a key
    /// they already stored.
    pub fn present_names(&self, keys: &KeyScope) -> Result<BTreeSet<String>, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(crate::db::read_present_names_scoped(
                &db,
                crate::db::Table::Credential,
                keys,
                self.scope(),
            )?),
            Backend::Absent => Ok(BTreeSet::new()),
        }
    }
}
