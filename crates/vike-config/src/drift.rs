//! **Which dotted keys two resolutions of the settings rows disagree about.**
//!
//! ⚠ **This module used to compare TWO SOURCES — the four settings files against the database's
//! rows — for `vike-cli config compare`, `config adopt`'s precondition, `config check`'s drift
//! finding, and a boot-time stale-draft warning.** `docs/decisions/0086` deletes all four: there is
//! no second source left to disagree with (*"there are no settings files any more, as source,
//! fallback, export or way back"*), so `compare_sources`, `Drift`, `boot_warnings` and the file-vs-
//! store precondition are GONE, not guarded.
//!
//! What survives is [`differing_keys`] alone, repurposed as the **write path's own guard**
//! (`docs/decisions/0086`'s verdict 2): a one-row write resolves the store TWICE — once as it
//! stands, once with the candidate row applied — and this function is what proves the candidate
//! changed only the key it named. `crate::write::validate_row_write` is its one caller.

use crate::load::Settings;

/// One key two resolutions of the settings rows disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftedKey {
    /// The dotted key as an operator names it — `policy.max_notional_per_order`. Spelled from
    /// [`crate::provenance::setting_keys`]' leaf walk, so the message speaks `config show`'s
    /// vocabulary rather than a second one.
    pub key: String,
    /// What the BEFORE resolution held, rendered.
    pub before: String,
    /// What the AFTER resolution holds, rendered.
    pub after: String,
}

/// **The dotted keys whose resolved values differ between two [`Settings`].**
///
/// Derived from [`crate::provenance::setting_keys`]' leaf walk so the vocabulary is `config show`'s
/// — plus the two axes that walk structurally cannot see, each named explicitly below.
pub(crate) fn differing_keys(before: &Settings, after: &Settings) -> Vec<DriftedKey> {
    let mut out = Vec::new();
    for spec in crate::provenance::setting_keys() {
        let a = crate::provenance::effective_value(before, &spec);
        let b = crate::provenance::effective_value(after, &spec);
        if a != b {
            out.push(DriftedKey {
                key: spec.key.clone(),
                before: a.unwrap_or_else(|| UNSET.to_string()),
                after: b.unwrap_or_else(|| UNSET.to_string()),
            });
        }
    }

    // ⚠ **`is_declared` is `#[serde(skip)]`, so the leaf walk above cannot see it** — and it is the
    // one axis whose disagreement silently unmounts a box. `VenuePolicy::default()` is every roster
    // venue at `Paper`, so a candidate that carries `[venues]` with everything at `paper` and one
    // that carries nothing produce the identical MAP; only this flag separates them, and only it
    // decides whether `vike_mount::venue_arming_migration`'s paste-ready banner fires.
    if before.policy.venues.is_declared() != after.policy.venues.is_declared() {
        out.push(DriftedKey {
            key: "policy.venues (stated at all)".to_string(),
            before: before.policy.venues.is_declared().to_string(),
            after: after.policy.venues.is_declared().to_string(),
        });
    }

    // ...and the per-ACCOUNT ceilings, which ride `policy.accounts` as one leaf and are compared
    // here by their resolved map so a disagreeing LABEL is named rather than folded into one row.
    let (ba, aa) =
        (before.policy.venues.accounts_by_venue(), after.policy.venues.accounts_by_venue());
    if ba != aa {
        out.push(DriftedKey {
            key: "policy.accounts".to_string(),
            before: format!("{ba:?}"),
            after: format!("{aa:?}"),
        });
    }

    out
}

/// How a key nothing set is RENDERED in a drift report.
///
/// It is a word rather than an empty cell because the two sides of this comparison are two whole
/// resolutions, and *this side does not set the key* is the commonest and most consequential
/// disagreement there is — a blank would read as a rendering accident.
const UNSET: &str = "(unset)";
