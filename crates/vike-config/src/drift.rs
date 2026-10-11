//! **Which dotted keys two resolutions of the settings rows disagree about.**
//!
//! There is no second source to disagree with (`docs/decisions/0086` deleted the file-vs-store
//! `compare_sources`, `Drift` and `boot_warnings`). [`differing_keys`] is the **write path's own
//! guard**
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
/// Derived from [`crate::provenance::setting_keys`]' leaf walk so the vocabulary is `config show`'s.
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

    out
}

/// How a key nothing set is RENDERED in a drift report.
///
/// It is a word rather than an empty cell because the two sides of this comparison are two whole
/// resolutions, and *this side does not set the key* is the commonest and most consequential
/// disagreement there is — a blank would read as a rendering accident.
const UNSET: &str = "(unset)";
