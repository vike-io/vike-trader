//! Load/save the [`AlertRuleSet`] as JSON — the same file conventions as
//! `vike_app_core::ui::workspace::persist` (a never-brick load; a pretty-printed save), kept in its
//! OWN file rather than folded into `workspace.json` so the alerting feature is fully additive:
//! no file ⇒ no rules ⇒ the engine is inert, and every existing workspace round-trip is
//! byte-identical (untouched).
//!
//! **Env boundary (settings STEP 2).** This module reads NO environment variable and resolves no
//! path. Every entry point takes the file as a `&Path`: the calling BINARY owns both the env read
//! and the directory (`vike-tradehub`'s `alerts_path`), and [`ALERTS_FILE`] is the single thing
//! stated here, so the caller composing that directory does not re-spell the basename.
//!
//! Forward-compat lives in the serde model ([`crate::rule`]): each field is
//! `#[serde(default)]`, so an older `alerts.json` still loads. A corrupt/unparseable file logs a
//! warning and is IGNORED (returns `None`) — a bad file must never brick startup, exactly like the
//! workspace loader.

use super::rule::AlertRuleSet;

/// The alerts file's basename, stated once so the caller resolving the state directory
/// (`vike-tradehub`'s `alerts_path`) names it rather than re-spelling it.
pub const ALERTS_FILE: &str = "alerts.json";

/// Read + parse the alerts file at a CALLER-SUPPLIED path — the only load seam, because the
/// binary owns the path resolution (the settings-registry rule: only binaries read the
/// environment). `vike-tradehub`'s headless mount resolves `$VIKE_ALERTS` /
/// `<project>/settings/state/alerts.json` and calls this.
///
/// `None` if absent or unparseable (never bricks — a missing/corrupt file simply means "no rules
/// configured", the OFF state), never an error.
pub fn load_path(p: &std::path::Path) -> Option<AlertRuleSet> {
    let raw = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str::<AlertRuleSet>(&raw) {
        Ok(set) => Some(set),
        Err(e) => {
            tracing::warn!("alerts file unparseable, ignoring (no rules): {e}");
            None
        }
    }
}

/// Serialize + write the rule set to a CALLER-SUPPLIED path — the save twin of [`load_path`],
/// public for the same reason (the caller, not this library, decides where the file lives).
pub fn save_path(set: &AlertRuleSet, p: &std::path::Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(set).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}

#[path = "persist_tests.rs"]
#[cfg(test)]
mod persist_tests;
