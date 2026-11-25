//! Load/save the [`AlertRuleSet`] as JSON — the same file conventions as
//! `vike_app_core::ui::window::workspace` (a never-brick load; a pretty-printed save), kept in its
//! OWN file rather than folded into `workspace.json` so the alerting feature is fully additive:
//! no file ⇒ no rules ⇒ the engine is inert, and every existing workspace round-trip is
//! byte-identical (untouched).
//!
//! **Env boundary (settings STEP 2).** This module reads NO environment variable and resolves no
//! path. Every entry point takes the file as a `&Path`: the calling BINARY owns both the env read
//! and the directory (`vike-tradehub`'s `alerts_path`). Even the basename is not stated here: it is
//! `vike_paths::state_path::ALERTS_FILE`, because a second reader (the saved-run retention) needs it and
//! this crate names no `vike-*` crate but its three floors.
//!
//! **A file of another version is REFUSED, never read with empty fields** (the owner's saved-file
//! ruling of 2026-11-17; decision 0117's amendment of 2026-11-18): [`load_path`] reads the
//! `version` first and answers a one-line hint naming the file and what to do for any version but
//! [`CURRENT_VERSION`], and for a file that does not parse. The caller runs no rule then (the OFF
//! state: a bad file never bricks startup) and nothing writes over the file.

use super::rule::{AlertRuleSet, CURRENT_VERSION};
use serde::Deserialize;

/// Read + parse the alerts file at a CALLER-SUPPLIED path — the only load seam, because the
/// binary owns the path resolution (the settings-registry rule: only binaries read the
/// environment). `vike-tradehub`'s headless mount resolves `$VIKE_ALERTS` /
/// `<project>/settings/state/alerts.json` and calls this.
///
/// `Ok(None)` if absent (no rules configured, the OFF state). `Err(hint)` when the file exists and
/// is not read: another version (an older build's words), not JSON, or a word this build does not
/// know. The caller logs the hint and runs no rule.
pub fn load_path(p: &std::path::Path) -> Result<Option<AlertRuleSet>, String> {
    let raw = match std::fs::read_to_string(p) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(not_read(p, &e)),
    };
    /// The one key read before the rest of the file.
    #[derive(Deserialize)]
    struct Stamp {
        version: Option<u32>,
    }
    let version = serde_json::from_str::<Stamp>(&raw).map_err(|e| not_read(p, &e))?.version;
    if version != Some(CURRENT_VERSION) {
        let said = version.map_or_else(|| "no version".to_string(), |v| format!("version {v}"));
        return Err(format!(
            "{} says {said}, so it is an alert rule file from another Vike Trader and is not read \
             (this build reads version {CURRENT_VERSION}, whose words are `comparison`, `fraction` \
             and `reconciliation_alert`): move it away and write the rules again. No alert rule \
             runs until then.",
            p.display()
        ));
    }
    serde_json::from_str(&raw).map(Some).map_err(|e| not_read(p, &e))
}

/// The hint for a rule file that exists and is not read for any reason but its version.
fn not_read(p: &std::path::Path, e: &dyn std::fmt::Display) -> String {
    format!(
        "{} could not be read ({e}): no alert rule runs until it is fixed or moved away.",
        p.display()
    )
}

/// Serialize + write the rule set to a CALLER-SUPPLIED path — the save twin of [`load_path`],
/// public for the same reason (the caller, not this library, decides where the file lives).
/// The file is replaced WHOLE (`vike_utility::atomic_file::write_atomic`: a temporary file renamed
/// over it), so a save cut short leaves the old rules as they were, never a truncated file.
pub fn save_path(set: &AlertRuleSet, p: &std::path::Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(set).map_err(std::io::Error::other)?;
    vike_utility::atomic_file::write_atomic(p, json.as_bytes())
}

#[path = "persist_tests.rs"]
#[cfg(test)]
mod persist_tests;
