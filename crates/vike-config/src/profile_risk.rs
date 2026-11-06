//! **A run profile's `[risk]` vocabulary** — the key list, each key's scalar shape and bounds
//! sentence, and the join onto the ceilings table. The one place this crate knows anything about a
//! RUN PROFILE.
//!
//! # Why this exists
//!
//! These are LIVE PRE-TRADE CEILINGS. `crates/vike-exec/src/risk.rs`'s `RiskGate` denies on them,
//! for every order the core admits, whatever its origin. [`crate::ceilings::PRE_TRADE_CEILINGS`]
//! names the two that matter; `vike-cli` links neither `vike-core` nor `vike-exec` (the
//! `light-consumers` CI lane holds it out of that closure), so it cannot parse a
//! `vike_config::RunProfile`, and it asks this module what a `[risk]` key is instead.
//!
//! The values are the ACTIVE `run` row's on the profile plane
//! (`crates/vike-tradehub/src/profile_rows.rs`'s `rows_to_run_profile`; decision 0111): no file and
//! no variable builds a `vike_model::ProfileRisk`, and the one writer is
//! `vike-cli config bootstrap-run`.
//!
//! # The key list is the TYPE's, and the check is the type's own parser
//!
//! `[risk]`'s type is `vike_model::ProfileRisk`, below this crate since
//! `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`. A `[risk]` value is judged by
//! deserializing it into `ProfileRisk` itself ([`check_risk_key`]), and the key list is
//! `vike_model::ProfileRisk::keys()` — serde's own field list. So an unknown `[risk]` key is refused
//! BY NAME here exactly as `deny_unknown_fields` refuses it at the boot, and a wrong-typed value is
//! refused by the parser that refuses it there.
//!
//! What stays hand-written is ONE column of prose, [`RISK_KEY_BOUNDS`] — the clause `vike-cli
//! config show` prints beside a value — and a typed test holds its key list equal to `keys()`.
//!
//! ⚠ **The check is per KEY and not the profile's whole validation.**
//! `vike_config::RunProfile::validate` also refuses a `max_leverage < 1.0`, and
//! `ProfileRisk::apply_to` refuses the venue-owned instrument fields at the mount of every
//! `demo`/`live` account (decision 0156: a profile states no tier, so nothing refuses them at
//! load); this module performs neither, because both run at the boot that ACTS on the profile.
//!
//! # `profile_risk` rows are a RETIRED disclosure copy
//!
//! Nothing in this tree writes a `profile_risk` row and nothing reads one. [`unknown_rows`]
//! and [`missing_keys`] are the read half `vike-cli config show` still uses to describe any that a
//! restored backup carries.

use vike_model::ProfileRisk;
use vike_secrets::{ProfileRiskRow, StoredProfileRisk};

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
    /// `Option<f64>` (`crates/vike-config/src/profile_risk_tests.rs`'s
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
