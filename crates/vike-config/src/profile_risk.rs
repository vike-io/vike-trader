//! **A run profile's `[risk]` table as rows** —
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 2, and the one
//! place this crate knows anything about a RUN PROFILE.
//!
//! # Why this is the phase the record calls *"the one that pays for itself on its own"*
//!
//! These are LIVE PRE-TRADE CEILINGS. `crates/vike-exec/src/risk.rs`'s `RiskGate` denies on them,
//! for every order the core admits, whatever its origin — and **no operator can read their values
//! from any panel**. [`crate::ceilings::PRE_TRADE_CEILINGS`] names the two that matter and says out
//! loud that `vike-cli config show` cannot print their numbers, because resolving them means
//! parsing a `vike_core::RunProfile` and `vike-cli` links neither `vike-core` nor `vike-exec` (the
//! `light-consumers` CI lane exists to hold it out of that closure). A ceiling nobody can read is
//! worse than a ceiling nobody set.
//!
//! And it is the half that actually judges an order, which is a claim worth checking rather than
//! repeating: MEASURED in `crates/vike-mount/src/policy.rs`'s `MountPolicy::from`, whose exhaustive
//! destructure binds `max_notional_per_order: _` — so the `policy.max_notional_per_order` ceiling
//! reaches no `vike_model::RiskLimits` at all, while this table's does.
//!
//! # ⚠ What this module does NOT do, and the sentence to read before extending it
//!
//! **Nothing on the mount path reads a mirrored row.** These rows are a DISCLOSURE copy, and no
//! `vike_model::ProfileRisk` is built from one — `vike-tradehub` builds it from the ACTIVE `run`
//! row on the profile plane and from nothing else since decision 0111
//! (`crates/vike-tradehub/src/profile_rows.rs`'s `rows_to_run_profile`; this said a FILE was the
//! only builder until the profile plane started binding) — so a `profile_risk` row cannot arm a
//! venue, cannot raise a ceiling, and cannot satisfy
//! `vike_mount::require_live_risk_budget` — the refusal that stops a box starting live without
//! `max_notional_per_order` and `max_total_exposure`. `crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs`
//! pins the files that may read them, so a future phase that gives the rows a READER has to say so
//! there.
//!
//! That asymmetry is deliberate and it is the whole safety argument for landing this phase now: a
//! ceiling readable from a table must not become a ceiling WRITABLE from anywhere the file was not.
//!
//! ⚠ **This paragraph went on to rest that on WHERE the writer ran, and that half is obsolete.**
//! It said `vike-cli config mirror --profile` was the one writer, running in an operator shell
//! OUTSIDE both daemons' mount namespaces, *"MEASURED: no `.service` under `deploy/` grants
//! `settings/db`"*. Two things changed. `deploy/vike-tradehub.service` grants `settings/db` since
//! the owner's ruling of 2026-09-18, so the kernel no longer keeps the trading daemon off this
//! store. And the mirror is RETIRED: `config mirror --profile` now stores the run profile's whole
//! body on the profile plane (`vike_secrets::profile_store`), where an ACTIVE `run` row DOES bind,
//! and nothing in production writes a `profile_risk` row at all
//! (`crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs`'s `WRITERS`). The residuals
//! below were written while the mirror still ran; read them as the history of the retired table.
//!
//! **NOTE — protection of a ceiling against a write by the daemon now rests on CODE, not on the
//! filesystem or the unit.** Since `docs/decisions/0086-settings-live-only-in-the-database.md`
//! point 6 the daemon writes `policy.*` rows over its control channel, and what
//! guards that write is listed, each piece cited, in `crates/vike-secrets/src/settings.rs`'s module
//! doc. A run profile's `[risk]` ceilings are outside that verb's reach: its writers on the
//! profile plane are pinned to `vike-cli` by `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs`.
//!
//! # The key list is the TYPE's, and the check is the type's own parser
//!
//! Phase 1 keeps the settings files' validate-on-load by materialising rows back through the
//! existing `PolicyPatch`/`ConfigPatch`/… types, which are in THIS crate. `[risk]`'s type is
//! `vike_model::ProfileRisk`, below this crate since
//! `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`, so the property is bought the
//! same way: a `[risk]` value is judged by deserializing it into `ProfileRisk` itself
//! ([`check_risk_key`]), and the key list is `vike_model::ProfileRisk::keys()` — serde's own field
//! list. So an unknown `[risk]` key is refused BY NAME here exactly as `deny_unknown_fields`
//! refuses it at the boot, a wrong-typed value is refused by the parser that refuses it there, and
//! the mirror can be neither stricter nor looser than the boot about one key. (Before 0114 this was
//! a hand-written roster held to the struct by a test that read the struct's SOURCE as text.)
//!
//! What stays hand-written is ONE column of prose, [`RISK_KEY_BOUNDS`] — the clause `vike-cli
//! config show` prints beside a value — and a typed test holds its key list equal to `keys()`.
//!
//! ⚠ **The check is per KEY and not the profile's whole validation, and the difference is stated
//! rather than implied.** `vike_core::RunProfile::validate` also refuses a `mode = "live"` profile
//! that sets the venue-owned instrument fields, and refuses a `max_leverage < 1.0`; this module
//! performs neither, because both belong to `RunProfile::validate` and `ProfileRisk::apply_to`,
//! which run at the boot that ACTS on the file. A mirror is not a second gate in front of the
//! daemon — `vike-cli config check` is that, for the files it can load. What the check buys is that
//! a row cannot be written for a key that does not exist, or with a value its field refuses, which
//! are the defects a database introduces and a file does not.
//!
//! # `profile` is a NAME, not a path
//!
//! 0057's Question 3 — *profile SELECTION: environment or row* — is **open**, and this module must
//! not answer it sideways. A `profile_risk` row is keyed by the profile's FILE NAME, so the table
//! says *these are the ceilings of the profile that goes by this name* and never *this is the risk
//! budget*. Which profile is live is the profile plane's ACTIVE `run` row (decision 0111 retired the
//! `VIKE_RUN_PROFILE` / `--profile` file rungs). Nothing here selects one and nothing here writes
//! an active row.
//!
//! # ⚠ DECLARED RESIDUALS — what this phase does NOT do, written down rather than discovered
//!
//! * **The mirror is MANUAL and a row can go STALE, and nothing detects it.** The rows are a copy
//!   of a file that changes without them, so an operator who edits the profile and does not re-run
//!   `config mirror --profile` reads a number the daemon is not using. Every surface that prints a
//!   row says so in as many words, and that is a WORDING mitigation rather than a mechanism. What
//!   a mechanism would need is a digest or an mtime stored beside the rows, i.e. schema surface;
//!   it is deliberately not built here, because the honest version also needs a reader that can
//!   find the file again, and the file's PATH is exactly what this table refuses to store (see the
//!   section above). A mirrored profile whose file has since been DELETED keeps its rows for the
//!   same reason.
//! * **`[sinks]`, `[guards]` and the profile's identity fields are NOT mirrored.** 0057 has them
//!   moving *"with the body"*, and the body is Phase 3's subject — which is blocked on Question 3.
//!   So `run-live.toml` is not migrated; ONE table of it is copied for reading. Nothing in this
//!   tree should be read as saying otherwise.
//! * **`vike_config::CONSUMPTION` gains no run-profile row**, although 0057 says each moved key
//!   *"joins the consumption gate on arrival"*. That gate derives its key set from the typed model
//!   of the four settings files, so a `[risk]` key is invisible to it in both directions and
//!   admitting one means changing the derivation — Phase 3 work, on a wider key set. What holds
//!   the `[risk]` keys meanwhile is stronger than a needle table for the part it covers and
//!   narrower for the rest: `vike_model::ProfileRisk`'s `to_risk_limits` is built field-by-field
//!   with no `..Default`, so no `[risk]` key can be declared-and-dropped at the config edge, and
//!   [`crate::ceilings::PRE_TRADE_CEILINGS`] names the enforcement site for the three that are
//!   operator-written size ceilings. Whether every other `RiskLimits` field is then read by a gate
//!   lane is NOT something this phase established.
//! * **`VIKE_RUN_PROFILE`'s `Layer::Library` row was left untouched here.** 0057 banked removing it
//!   as a bonus of *"replacing the path selector"*; that selector went with decision 0111, which
//!   took the row out of `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
//!   `LIBRARY_PIN` and made the variable a `crate::REMOVED_ENV` refusal.

use std::path::Path;

use vike_model::ProfileRisk;
use vike_secrets::{ProfileRiskRow, StoredProfileRisk};

use crate::error::ConfigError;
use crate::load::parse_toml_str;

/// The TOML table a run profile's pre-trade ceilings live in. One literal, used by the renderer and
/// by the gate that reads the real type.
pub const RISK_TABLE: &str = "risk";

/// The scalar SHAPE a `[risk]` key's value has — what `vike-cli config show` prints, and the word a
/// refusal names. Worked out from the real parser by [`risk_key_kind`], never declared.
///
/// Three words rather than the Rust spellings (`Option<f64>` / `Option<usize>` / `i64` / `bool`)
/// because the OPTIONALITY is not a property a row can carry: an absent key is an absent ROW, and a
/// `#[serde(default)]` non-`Option` field is absent from a file in exactly the same way. What a row
/// must agree with is the scalar kind, and that is what this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskKeyKind {
    /// `f64` — a TOML float **or a TOML integer**: the real parser takes `max_leverage = 3` for an
    /// `Option<f64>` (`crates/vike-config/tests/profile_risk_rows.rs`'s
    /// `the_real_parsers_cross_shape_answers_are_pinned`).
    Float,
    /// `usize` / `i64` — a TOML integer, and ONLY an integer.
    Integer,
    /// `bool`, and only `true`/`false`.
    Boolean,
}

impl RiskKeyKind {
    /// The word `vike-cli config show` prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RiskKeyKind::Float => "float",
            RiskKeyKind::Integer => "integer",
            RiskKeyKind::Boolean => "boolean",
        }
    }
}

/// **What each `[risk]` key bounds, in one clause** — the `BOUNDS` column of `vike-cli config
/// show`, in `vike_model::ProfileRisk::keys()`'s order.
///
/// The one hand-written list left of the `[risk]` vocabulary, and it holds PROSE only: names and
/// shapes come from the type. `the_bounds_table_is_the_types_own_key_list_in_order` holds its key
/// list equal to `ProfileRisk::keys()`, so a new field reddens until it has a sentence.
///
/// It deliberately carries NO `refuses_live_mount_when_absent` flag, although three of these keys
/// are also [`crate::ceilings::Ceiling`] rows that do: that fact has an authority already
/// ([`crate::ceilings::PRE_TRADE_CEILINGS`], gated against the source of the one function that
/// performs the refusal) and a second copy here would be free to disagree with it.
/// [`ceiling_for`] is the join.
pub const RISK_KEY_BOUNDS: &[(&str, &str)] = &[
    (
        "tick_size",
        "the price grid a quantizer rounds to. VENUE-OWNED: a live mount fetches its own \
         instrument grid and REFUSES a profile that sets this.",
    ),
    ("lot_size", "the quantity grid. VENUE-OWNED, same refusal as `tick_size`."),
    ("min_notional", "the per-order notional FLOOR. VENUE-OWNED, same refusal as `tick_size`."),
    (
        "min_qty",
        "the per-order quantity floor, checked on the lot-rounded qty. VENUE-OWNED, same refusal \
         as `tick_size`.",
    ),
    ("max_notional_per_order", "the per-order notional CEILING on every order this engine admits."),
    (
        "max_total_exposure",
        "ONE SYMBOL's projected open notional at ONE VENUE — not the account, despite the name.",
    ),
    ("max_orders_per_window", "the throttle's order count per window."),
    ("window_ms", "the throttle window in ms; active only while `max_orders_per_window` is set."),
    (
        "max_leverage",
        "THE leverage knob (>= 1.0), converted to an initial-margin fraction at the config edge to \
         arm the buying-power check.",
    ),
    (
        "block_reduce_only_overshoot",
        "whether a reduce-only order larger than the position is refused rather than clamped.",
    ),
    ("required_free_bp_pct", "the free-buying-power haircut on equity, in [0.0, 1.0)."),
];

/// The `BOUNDS` clause for `name`, or `None` when no `[risk]` key goes by it.
#[must_use]
pub fn risk_key_bounds(name: &str) -> Option<&'static str> {
    RISK_KEY_BOUNDS.iter().find(|(key, _)| *key == name).map(|(_, what)| *what)
}

/// Why [`check_risk_key`] refused a `[risk]` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskKeyRefusal {
    /// No `vike_model::ProfileRisk` field goes by this name (`deny_unknown_fields` refuses it at
    /// the boot too).
    Unknown,
    /// The field exists and its parser refuses this value; the parser's own message.
    Refused(String),
}

/// **Judge one `[risk]` key and value by `vike_model::ProfileRisk`'s own deserialize** — the parse
/// the boot performs, on a one-key table, so the refusal names the key that caused it.
///
/// # Errors
///
/// [`RiskKeyRefusal::Unknown`] for a name outside `ProfileRisk::keys()`;
/// [`RiskKeyRefusal::Refused`] for a value the field's parser refuses (a float for an integer
/// field, an integer for a bool, any table or array).
pub fn check_risk_key(key: &str, value: &toml::Value) -> Result<(), RiskKeyRefusal> {
    if !ProfileRisk::keys().contains(&key) {
        return Err(RiskKeyRefusal::Unknown);
    }
    let mut one = toml::Table::new();
    one.insert(key.to_string(), value.clone());
    toml::Value::Table(one)
        .try_into::<ProfileRisk>()
        .map(|_| ())
        .map_err(|e| RiskKeyRefusal::Refused(e.to_string()))
}

/// **The scalar shape of a `[risk]` key, asked of the real parser**: `None` for a name outside the
/// type, or for a field none of the three probes fits (a shape `RiskKeyKind` has no word for —
/// `every_key_classifies_through_the_real_parser` makes that a STOP rather than a default).
///
/// The probes run in this order on purpose: `true` (only a `bool` takes it), then `1.5` (only an
/// `f64` takes it; an integer field refuses it), then `1` (an integer field, the `f64`s having
/// answered already).
#[must_use]
pub fn risk_key_kind(name: &str) -> Option<RiskKeyKind> {
    let takes = |v: toml::Value| check_risk_key(name, &v).is_ok();
    if !ProfileRisk::keys().contains(&name) {
        None
    } else if takes(toml::Value::Boolean(true)) {
        Some(RiskKeyKind::Boolean)
    } else if takes(toml::Value::Float(1.5)) {
        Some(RiskKeyKind::Float)
    } else if takes(toml::Value::Integer(1)) {
        Some(RiskKeyKind::Integer)
    } else {
        None
    }
}

/// The [`crate::ceilings::Ceiling`] row for a `[risk]` key, when the ceilings table carries one.
///
/// The JOIN rather than a duplicated flag — see [`RISK_KEY_BOUNDS`]. Most `[risk]` keys have no
/// row: the ceilings table covers the ones an operator writes as a SIZE ceiling, and the
/// venue-owned grid fields are not that.
#[must_use]
pub fn ceiling_for(name: &str) -> Option<&'static crate::ceilings::Ceiling> {
    crate::ceilings::ceilings_named(name)
        .find(|c| c.home == crate::ceilings::CeilingHome::RunProfileRisk)
}

/// **Read a run profile and render its `[risk]` table as rows.**
///
/// The profile is parsed as raw TOML and ONLY its `[risk]` table is looked at — see this module's
/// doc for why the whole-profile validation is not performed here and where it is. A profile with
/// no `[risk]` table renders zero rows, which is the honest mirror of a file that sets no ceiling
/// (and which `vike_model::ProfileRisk::default` is the runtime answer for).
///
/// Refuses, naming the key, whatever [`check_risk_key`] refuses: a key outside
/// `vike_model::ProfileRisk::keys()`, and a value its field's parser refuses — a wrong scalar, a
/// nested table or an array (no `[risk]` key is either, and a silently-mangled value would be worse
/// than a refusal — the rule [`crate::mirror`]'s `flatten` states for the settings files).
pub fn risk_rows_from_profile(profile: &Path) -> Result<StoredProfileRisk, ConfigError> {
    let text = std::fs::read_to_string(profile)
        .map_err(|source| ConfigError::Read { file: profile.to_path_buf(), source })?;
    let doc: toml::Table = parse_toml_str(profile, &text)?;

    let mut rows = Vec::new();
    match doc.get(RISK_TABLE) {
        None => {}
        Some(toml::Value::Table(risk)) => {
            for (key, value) in risk {
                let message = match check_risk_key(key, value) {
                    Ok(()) => None,
                    Err(RiskKeyRefusal::Unknown) => Some(format!(
                        "unknown `[risk]` key `{key}` — `vike_model::ProfileRisk` is \
                         `deny_unknown_fields`, so this profile would fail at startup too. \
                         Nothing has been written."
                    )),
                    Err(RiskKeyRefusal::Refused(why)) => Some(format!(
                        "`[risk] {key}` must be a {}; the profile's parser refuses this value \
                         too ({why}). Nothing has been written.",
                        risk_key_kind(key).map_or("scalar", RiskKeyKind::as_str)
                    )),
                };
                if let Some(message) = message {
                    return Err(ConfigError::Parse {
                        file: profile.to_path_buf(),
                        key: Some(key.clone()),
                        message,
                    });
                }
                rows.push(ProfileRiskRow { key: key.clone(), value: value.to_string() });
            }
        }
        Some(_) => {
            return Err(ConfigError::Parse {
                file: profile.to_path_buf(),
                key: Some(RISK_TABLE.to_string()),
                message: "`risk` must be a TOML TABLE — this profile would fail at startup too. \
                          Nothing has been written."
                    .to_string(),
            });
        }
    }
    rows.sort();

    Ok(StoredProfileRisk { profile: profile_name_of(profile), rows })
}

/// **The NAME a profile is stored under** — its file name, never its path.
///
/// A path is a fact about one box's disk; the file name is what an operator recognises (and what a
/// deployed unit's retired `VIKE_RUN_PROFILE` ended with). See this module's doc for why the key
/// must not become an active-profile claim.
///
/// A path with no file-name component (`.`, `..`, a bare root) renders as the whole path rather
/// than as an empty string — a row keyed on `""` would collide with every other such profile, and
/// the operator needs to see whatever they typed.
#[must_use]
pub fn profile_name_of(profile: &Path) -> String {
    profile
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| profile.display().to_string())
}

/// A mirrored row whose key is no `vike_model::ProfileRisk` field — the one defect a database
/// introduces that a file does not, and the read half of [`check_risk_key`]'s refusal.
///
/// A row can only reach this state by a route no verb in this tree offers (a hand `INSERT`, a
/// restored backup, a migration written elsewhere), which is exactly why the READ side has to be
/// able to name it: the value is in no `[risk]` schema, so it configures nothing, and an operator
/// looking at a disclosure table must be told that rather than shown a ceiling that is not one.
#[must_use]
pub fn unknown_rows(profile: &StoredProfileRisk) -> Vec<&ProfileRiskRow> {
    profile.rows.iter().filter(|r| !ProfileRisk::keys().contains(&r.key.as_str())).collect()
}

/// The `[risk]` keys this profile's rows do NOT carry, in `ProfileRisk::keys()` order — what an
/// operator reads as *unset*, which for two of them means a live mount refuses to start.
#[must_use]
pub fn missing_keys(profile: &StoredProfileRisk) -> Vec<&'static str> {
    ProfileRisk::keys()
        .iter()
        .copied()
        .filter(|k| !profile.rows.iter().any(|r| r.key == *k))
        .collect()
}

#[path = "profile_risk_tests.rs"]
#[cfg(test)]
mod profile_risk_tests;
