//! `templates.json` — the strategy registry's rosters, parameter surfaces and live-mount verdicts.
//! - `templates.json` — `vike_strategy::registry`'s `PORTABLE_STRATEGIES` / `SIMULATOR_ONLY` /
//!   `SCRIPT_ONLY` rosters, each portable name's `PARAM_KEYS` row (rendered as a tagged object,
//!   because `ParamKeys::NotEnumerated` carries a REASON a plain key list could not) and its
//!   `LIVE_CAPABLE` verdict (`None` = mountable on the live core today; `Some(reason)` = resolves
//!   but would not trade). That registry's own tests hold the three portable tables exhaustive
//!   against each other; the renderer inherits that and PANICS on a missing row rather than
//!   guessing, the same rule as [`venue_record`](crate::venue_record)'s wiring lookup.

use serde_json::{Value, json};
use vike_strategy::{
    LIVE_CAPABLE, PARAM_KEYS, PORTABLE_STRATEGIES, ParamKeys, ParamType, SCRIPT_ONLY,
    SIMULATOR_ONLY,
};

/// Kebab-case name of a [`ParamType`] — the accessor class a strategy's `from_params` reads a key
/// through (`vike_strategy::PARAM_KEYS`'s doc).
const fn param_type_name(kind: &ParamType) -> &'static str {
    match kind {
        ParamType::Number => "number",
        ParamType::Integer => "integer",
        ParamType::Str => "string",
        ParamType::Bool => "bool",
        ParamType::Table => "table",
        ParamType::StrOrInteger => "string-or-integer",
    }
}

/// A strategy's [`ParamKeys`] row as a tagged object: `declared` carries the key list,
/// `not-enumerated` carries the registry's stated reason. Tagged rather than a bare array for the
/// same reason as `venues::fee_value` — the second variant has a payload a
/// list cannot hold.
fn param_keys_value(keys: &ParamKeys) -> Value {
    match keys {
        ParamKeys::Declared(list) => json!({
            "kind": "declared",
            "keys": list
                .iter()
                .map(|(name, kind)| json!({ "name": name, "kind": param_type_name(kind) }))
                .collect::<Vec<_>>(),
        }),
        ParamKeys::NotEnumerated(reason) => json!({ "kind": "not-enumerated", "reason": reason }),
    }
}

/// A `LIVE_CAPABLE` row: `verdict` is `true` exactly when the registry says the strategy is
/// mountable on the live core today, and `reason` is the registry's stated blocker otherwise —
/// `null` on a `true` verdict, never an empty string, so a consumer cannot mistake one for the
/// other.
fn live_capable_value(blocker: Option<&str>) -> Value {
    match blocker {
        None => json!({ "verdict": true, "reason": Value::Null }),
        Some(reason) => json!({ "verdict": false, "reason": reason }),
    }
}

/// One portable strategy's complete `templates.json` record.
///
/// # Panics
/// On a `PORTABLE_STRATEGIES` name with no `PARAM_KEYS` or `LIVE_CAPABLE` row — deliberately, the
/// same rule as [`venue_record`](crate::venue_record): the strategy registry's own tests hold
/// those tables exhaustive, so this arm is unreachable from a green tree and exporting a guess
/// would be the silent-wrong answer the tables exist to remove.
#[must_use]
pub fn template_record(id: &str) -> Value {
    let (_, keys) = PARAM_KEYS
        .iter()
        .find(|(name, _)| *name == id)
        .unwrap_or_else(|| panic!("portable strategy {id} has no PARAM_KEYS row"));
    let (_, verdict) = LIVE_CAPABLE
        .iter()
        .find(|(name, _)| *name == id)
        .unwrap_or_else(|| panic!("portable strategy {id} has no LIVE_CAPABLE row"));
    json!({
        "id": id,
        "params": param_keys_value(keys),
        "live_capable": live_capable_value(verdict.blocker()),
    })
}

/// A `(name, reason)` roster row — the shape `SIMULATOR_ONLY` and `SCRIPT_ONLY` share.
fn reasoned_row(row: &(&str, &str)) -> Value {
    let (id, reason) = row;
    json!({ "id": id, "reason": reason })
}

/// The whole `templates.json` document: `portable` (one [`template_record`] per
/// `PORTABLE_STRATEGIES` entry, in roster order), `simulator_only` (one `{ id, reason }` per
/// `SIMULATOR_ONLY` entry — the names a backtest profile may still resolve that a live mount
/// cannot) and `script_only` (the same shape over `SCRIPT_ONLY` — the inline-`src` script arm that
/// lives in `vike-backtest` and sits on no roster), each in its table's order and each carrying the
/// registry's own reason.
#[must_use]
pub fn templates_value() -> Value {
    json!({
        "portable": PORTABLE_STRATEGIES.iter().copied().map(template_record).collect::<Vec<_>>(),
        "simulator_only": SIMULATOR_ONLY.iter().map(reasoned_row).collect::<Vec<_>>(),
        "script_only": SCRIPT_ONLY.iter().map(reasoned_row).collect::<Vec<_>>(),
    })
}
