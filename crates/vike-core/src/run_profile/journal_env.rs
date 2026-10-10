//! Resolving the write-ahead journal sink from a resolved profile or the caller's directory rung.

use super::schema::RunProfile;

/// Pure resolver behind [`journal_config_from`]: pick the write-ahead journal sink from an optional
/// resolved profile and an optional quick-path dir.
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

/// The write-ahead command journal for a binary that resolved NO run profile, from the caller's
/// DIRECTORY rung — a directory (a single-knob opt-in with the default cadence,
/// [`crate::JournalConfig::at`]) and an optional snapshot-cadence override. No directory → `None`:
/// journaling is OFF by default, so a standard mount stays zero-overhead and byte-identical.
///
/// It reads no environment and no settings of its own: the BINARY resolves both values (the
/// `config.journal_dir` and `config.journal_snapshot_every` rows, decision 0111) and hands them in.
/// `crates/vike-tradehub/src/profile_rows.rs`'s `JournalRung` is the worked example.
///
/// ⚠ **A resolved run profile is the caller's to apply FIRST, and it decides alone**: its
/// `[sinks].journal` is the answer even when it names none (`choose_journal`'s rule), which is
/// `crates/vike-tradehub/src/profile_rows.rs`'s `journal_config_for`. This function is the rung
/// below it.
#[must_use]
pub fn journal_config_from(
    journal_dir: Option<std::path::PathBuf>,
    snapshot_every: Option<u64>,
) -> Option<crate::JournalConfig> {
    choose_journal(None, journal_dir, snapshot_every)
}
