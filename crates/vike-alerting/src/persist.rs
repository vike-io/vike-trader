//! Load/save the [`AlertRuleSet`] as JSON — the same file conventions as
//! `vike_app_core::workspace::persist` (a never-brick load; a pretty-printed save), kept in its
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
/// `<project>/settings/state/alerts.json` in its `main.rs` and calls this.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{AlertRule, AlertRuleSet, Compare, RuleTrigger};

    #[test]
    fn disk_round_trip_and_missing_and_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("alerts.json");

        // missing file → None (the OFF state), never an error.
        assert!(load_path(&p).is_none());

        let set = AlertRuleSet {
            rules: vec![
                AlertRule::new(
                    "p",
                    RuleTrigger::Price {
                        venue: "binance".into(),
                        symbol: "BTCUSDT".into(),
                        op: Compare::Above,
                        level: 100_000.0,
                    },
                ),
                AlertRule::new("r", RuleTrigger::OrderRejected),
            ],
            ..Default::default()
        };
        save_path(&set, &p).unwrap();
        let back = load_path(&p).expect("saved file loads");
        assert_eq!(back, set, "disk round-trip is lossless");

        // corrupt file → None (never bricks), same never-brick contract as the workspace loader.
        std::fs::write(&p, "{ not valid json ]").unwrap();
        assert!(load_path(&p).is_none());
    }

    /// This module resolves NOTHING: the basename is all it states, and both entry points take a
    /// caller-supplied `&Path`. The name is a cross-crate contract — `vike-tradehub`'s
    /// `alerts_path` joins it onto the state directory — so a change on either side that did not
    /// happen on the other would leave the daemon reading a file nobody writes.
    #[test]
    fn the_basename_is_all_this_module_states() {
        assert_eq!(ALERTS_FILE, "alerts.json");
    }
}
