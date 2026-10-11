//! The ENV half's pure resolver: each registry row against the stores, redacted at the source.

use std::collections::{HashMap, HashSet};

use vike_config::redact::{REDACTED, SET, UNSET, is_secret};
use vike_ops::settings::{Naming, Setting, all_settings};

// ---------------------------------------------------------------------------------------------
// The pure resolver — the ENV half
// ---------------------------------------------------------------------------------------------

/// Which store the effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The documented default from the `SETTINGS` row — nothing configured it.
    Default,
    /// The credential store — the settings DATABASE, `<project>/settings/db/vike.db`
    /// (`docs/decisions/0054-settings-move-into-one-database.md`).
    ///
    Database,
    /// The real process environment.
    Env,
}

impl Source {
    /// The stable wire/table spelling. Pinned by test — later phases may move stores, but a tool
    /// parsing `--json` must keep reading the same words for the same stores.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            // The SAME word `vike-cli secrets list --json`'s `kind` prints for the same store, so
            // an operator comparing the two surfaces is reading one fact.
            Source::Database => "database",
            Source::Env => "env",
        }
    }

    /// The word for a value that came from **the credential store** — which store that is being
    /// `vike_secrets::backend_in`'s per-RUN answer, taken here as a PARAMETER.
    ///
    /// ⚠ **Nothing in this file probes.** The choice is made once, in [`execute`], and travels
    /// down as data. A per-KEY fallback — look in the database, then in the file — is the ladder
    /// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids and the shape
    /// `vike_secrets::Backend`'s own doc argues against; a resolver that could ask twice would be
    /// able to answer twice.
    fn of_store(backend: &vike_secrets::Backend) -> Self {
        match backend {
            // With no database the credential map is EMPTY, so no value ever reaches this arm
            // carrying a store word; it is spelled for totality, never shown.
            vike_secrets::Backend::Database(_) | vike_secrets::Backend::Absent => Source::Database,
        }
    }

    /// Did this value come from the credential store at all?
    fn is_store(self) -> bool {
        matches!(self, Source::Database)
    }
}

/// What a row's READER consults — the honest qualifier on [`Source`], derived from
/// [`vike_ops::settings::Setting::naming`] because that is the one column that distinguishes a
/// direct `env::var` from a lookup on a map somebody else supplied. See the module doc for why
/// `Setting::layer` cannot answer this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reads {
    /// A direct `env::var`/`env::var_os` — the PROCESS environment.
    ProcessEnv,
    /// `vars.get(..)` on a caller-supplied map. Which map is the CALLER's choice: the venue
    /// loaders are handed the credential map, `reconcile_config` and the store-root readers the
    /// process-env sweep. The registry does not record which, so neither does this.
    CallerMap,
    /// A computed/parameterised name (`Naming::Dynamic`) — allowlisted, not resolved.
    Unknown,
}

impl Reads {
    pub(super) fn of(naming: Naming) -> Self {
        match naming {
            // ⚠ When ONE variable is named at BOTH kinds of site, `naming` records the DIRECT read
            // (`vike_ops::settings`' design note, tie-break 2) — so this says "at least one reader
            // calls env::var", never "no reader consults a map".
            Naming::Literal | Naming::Konst(_) => Reads::ProcessEnv,
            Naming::MapLookup => Reads::CallerMap,
            Naming::Dynamic => Reads::Unknown,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Reads::ProcessEnv => "env",
            Reads::CallerMap => "caller-map",
            Reads::Unknown => "unknown",
        }
    }
}

/// One resolved setting, ready to print.
///
/// INVARIANT: when `secret` is true, neither `value` nor `default` contains any byte of the
/// underlying stores — redaction happens here, in the resolver, so no printer can leak it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) name: &'static str,
    pub(crate) krate: &'static str,
    pub(crate) source: Source,
    pub(crate) reads: Reads,
    pub(crate) secret: bool,
    /// The effective value — ALREADY redacted when `secret`.
    pub(crate) value: String,
    /// The row's documented default — ALREADY redacted when `secret`.
    pub(crate) default: String,
}

impl Resolved {
    /// The row's value came ONLY from the credential store, but its reader calls `env::var` — so
    /// the process will not see it unless it is exported.
    ///
    /// REPORTED rather than corrected: a reader CAN have a store fallback (`poly_reconcile_enabled`
    /// is the documented one), and the registry does not record which do. "This may not reach its
    /// reader, here is why" is knowable; "this value is not in effect" is not.
    pub(crate) fn store_may_not_reach_reader(&self) -> bool {
        // Either shape of the store — the diagnosis is about the value not being EXPORTED, which
        // is equally true of a row the settings database answered.
        self.source.is_store() && self.reads == Reads::ProcessEnv
    }
}

/// Resolve ONE registry row against two caller-supplied maps. Pure: no process env, no filesystem,
/// no globals — which is what makes the precedence and redaction rules testable without the
/// process-global `set_var` races that `SRC_TEST_MODULE_OVERRIDES` exists to work around.
///
/// Precedence: `env` > the credential store > `row.default`. PRESENCE wins, not non-emptiness (see
/// the module doc): a key mapped to `""` is a real override and is reported as one.
///
/// `stored` is the map the credential loader returned and `backend` is WHICH STORE produced it —
/// [`Source::of_store`]'s parameter. The two travel together because a map alone cannot say where
/// it came from.
pub(crate) fn resolve(
    row: &Setting,
    env: &HashMap<String, String>,
    stored: &HashMap<String, String>,
    backend: &vike_secrets::Backend,
) -> Resolved {
    let (raw, source) = match env.get(row.name) {
        Some(v) => (v.as_str(), Source::Env),
        None => match stored.get(row.name) {
            Some(v) => (v.as_str(), Source::of_store(backend)),
            None => (row.default, Source::Default),
        },
    };

    let secret = is_secret(row.name);
    let (value, default) = if secret {
        let shown = if source != Source::Default && !raw.is_empty() { SET } else { UNSET };
        // A credential row's default is `""` today; anything else is redacted rather than printed.
        let default = if row.default.is_empty() { "" } else { REDACTED };
        (shown.to_string(), default.to_string())
    } else {
        (raw.to_string(), row.default.to_string())
    };

    Resolved {
        name: row.name,
        krate: row.krate,
        source,
        reads: Reads::of(row.naming),
        secret,
        value,
        default,
    }
}

/// Resolve the whole registry, apply the view filters, and sort.
///
/// Sorted by `(name, krate)` rather than left in `SETTINGS` array order on purpose: this output is
/// the later phases' regression baseline, and array order is an editing artifact that would churn
/// the diff every time a row is inserted. `SETTINGS` is keyed on `(name, krate)`, so one variable
/// read by several crates with different defaults yields several adjacent rows — that is the table
/// being honest, not a duplicate.
pub(super) fn resolve_all(
    env: &HashMap<String, String>,
    stored: &HashMap<String, String>,
    backend: &vike_secrets::Backend,
    filter: Option<&str>,
    changed_only: bool,
) -> Vec<Resolved> {
    let needle = filter.map(str::to_ascii_lowercase);
    let mut rows: Vec<Resolved> = all_settings()
        .filter(|row| match needle.as_deref() {
            None => true,
            Some(n) => {
                row.name.to_ascii_lowercase().contains(n)
                    || row.krate.to_ascii_lowercase().contains(n)
            }
        })
        .map(|row| resolve(row, env, stored, backend))
        .filter(|r| !changed_only || r.source != Source::Default)
        .collect();
    rows.sort_by(|a, b| a.name.cmp(b.name).then(a.krate.cmp(b.krate)));
    rows
}

/// The credential-store keys that match **no** `SETTINGS` row — the env table's complement, and the
/// answer to "I put it in the store and nothing anywhere shows it".
///
/// Both halves of this command are driven by catalogs, so an unmatched store key produces no row and
/// is INDISTINGUISHABLE from a key that was never set. That is the defect this type exists to close.
///
/// ⚠ **Split by name SHAPE.** Three options were weighed and only this one holds both properties:
///
/// * *name everything* — makes the command an enumerator of the credential store, exactly what the
///   module doc's redaction rule forbids for a surface meant to be pasted into an issue.
/// * *count everything* — safe, and useless: a real store holds bespoke per-venue credentials
///   (`FXCM_{TIER}_USER`, `DUKASCOPY_DEMO1_LOGIN`, per-account ids) beside its `{VENUE}_{TIER}_API_*`
///   keys, so on any box with live credentials the count is large, constant and buries the one key
///   that matters. ⚠ It used to be larger still, and for a worse reason: the registry declared
///   almost none of the `{VENUE}_{TIER}_API_*` grid at all, because those keys are COMPUTED and
///   appeared as no literal anywhere. That family is enumerable data now
///   (`vike_model::credential_keys`) and fully declared, so a store key of that shape MATCHES a row
///   and is reported in the table above with its true source instead of vanishing into this count.
/// * *split* — a credential-shaped name ([`is_secret`]) is COUNTED, never named; everything else is
///   NAMED. The counted half is where the expected noise lives, and its message says so; the named
///   half is high-signal, because a store key that is neither a known setting nor credential-shaped
///   is almost always a typo or a setting that has been removed.
///
/// The named half discloses strictly less than this command already prints: a non-secret registry
/// row's full VALUE is in the table above it. The counted half keeps the module doc's rule intact.
///
/// ⚠ A key whose typo lands on a credential SHAPE (`VIKE_NODE_CONTROL_KEY` — the case that prompted
/// this) is therefore counted, not named. That is the honest trade: its name shape is
/// indistinguishable from a real node key's, and `vike-cli secrets list` is the surface that names
/// store keys.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UnknownKeys {
    /// Unmatched keys safe to print by name, sorted for a stable diff.
    pub(crate) named: Vec<String>,
    /// Unmatched keys whose NAME is credential-shaped — disclosed by COUNT only.
    pub(crate) credential_shaped: usize,
}

impl UnknownKeys {
    pub(super) fn is_empty(&self) -> bool {
        self.named.is_empty() && self.credential_shaped == 0
    }
}

/// Build [`UnknownKeys`] from the store map. Pure — no process env, no filesystem — and it reads
/// only the store's KEYS: a value never enters this path at all, so no redaction step can be
/// forgotten here.
///
/// `filter` is the same `--filter` needle the tables use, matched case-insensitively on the key.
pub(crate) fn unknown_store_keys(
    stored: &HashMap<String, String>,
    filter: Option<&str>,
) -> UnknownKeys {
    let declared: HashSet<&str> = all_settings().map(|s| s.name).collect();
    let needle = filter.map(str::to_ascii_lowercase);
    let mut out = UnknownKeys::default();
    for key in stored.keys() {
        if declared.contains(key.as_str()) {
            continue;
        }
        if needle.as_deref().is_some_and(|n| !key.to_ascii_lowercase().contains(n)) {
            continue;
        }
        if is_secret(key) {
            out.credential_shaped += 1;
        } else {
            out.named.push(key.clone());
        }
    }
    out.named.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// The FILES half — MOVED to `vike_config::show` (`FileRow`/`resolve_file_row`/`file_rows`,
// imported at the top; its behaviour tests moved with it). The tradehub node's
// `Request::SettingsShow` arm serves the SAME rows, and a redaction rule with two copies is the
// one duplication a disclosure surface cannot afford — the printers below are all that stayed.
// ---------------------------------------------------------------------------------------------
