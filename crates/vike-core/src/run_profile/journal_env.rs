//! Resolving the write-ahead journal sink from a resolved profile or a caller-supplied map.

use std::collections::HashMap;

use super::schema::RunProfile;

/// Pure resolver behind [`journal_config_from`] (map-free, so it is unit-tested directly): pick
/// the write-ahead journal sink from an optional resolved profile and an optional quick-path dir.
///
/// A resolved `profile` is AUTHORITATIVE — its `[sinks].journal` decides, even when that means "no
/// journal" (a profile with no journal sink returns `None`); the directory is not consulted when a
/// profile is present, so a profile can never be silently overridden. Absent a profile,
/// `journal_dir` (with an optional `snapshot_every` override) builds a default-cadence
/// [`crate::JournalConfig`] via [`crate::JournalConfig::at`]. Absent both → `None`.
pub(crate) fn choose_journal(
    profile: Option<&RunProfile>,
    journal_dir: Option<std::path::PathBuf>,
    snapshot_every: Option<u64>,
) -> Option<crate::JournalConfig> {
    if let Some(p) = profile {
        return p.sinks.journal_config();
    }
    let mut cfg = crate::JournalConfig::at(journal_dir?);
    if let Some(v) = snapshot_every.filter(|&v| v >= 1) {
        cfg.snapshot_every = v;
    }
    Some(cfg)
}

/// The write-ahead command journal for a binary that resolved NO run profile, from a
/// CALLER-SUPPLIED map — `VIKE_JOURNAL_DIR` (a single-knob opt-in with the default cadence,
/// [`crate::JournalConfig::at`]) and `VIKE_JOURNAL_SNAPSHOT_EVERY` (an optional snapshot-cadence
/// override). Neither present → `None`: journaling is OFF by default, so a standard mount stays
/// zero-overhead and byte-identical.
///
/// It reads no process environment of its own: the BINARY does the I/O and decides precedence by
/// what it puts in the map. `crates/vike-tradehub/src/tradehub_cli/flags.rs`'s `journal_vars` is
/// the worked example — it starts from the process env and inserts the `config.journal_dir`
/// setting only where the variable is absent.
///
/// ⚠ **A resolved run profile is the caller's to apply FIRST, and it decides alone**: its
/// `[sinks].journal` is the answer even when it names none (`choose_journal`'s rule), which is
/// `crates/vike-tradehub/src/profile_rows.rs`'s `journal_config_for`. This function is the rung
/// below it. It used to short-circuit on `VIKE_RUN_PROFILE` and re-open that FILE; the run profile
/// is a settings row now (decision 0111) and no binary reads a profile file, so the variable is
/// refused at startup (`vike_config::REMOVED_ENV`) instead.
#[must_use]
pub fn journal_config_from(vars: &HashMap<String, String>) -> Option<crate::JournalConfig> {
    let dir = vars.get("VIKE_JOURNAL_DIR")?;
    let snapshot_every =
        vars.get("VIKE_JOURNAL_SNAPSHOT_EVERY").and_then(|s| s.parse::<u64>().ok());
    choose_journal(None, Some(std::path::PathBuf::from(dir)), snapshot_every)
}
