//! Resolving the write-ahead journal sink: the directory from the caller's row, the tuning from the
//! resolved profile.

use super::schema::RunProfile;

/// **The write-ahead command journal**, from the ONE source of its directory and the two sources of
/// its tuning (decision 0111, amended 2026-10-11).
///
/// - `journal_dir` is the `config.journal_dir` settings row, as the BINARY resolved it. It is the
///   journal's switch and its only directory: `None` — no row — is journaling OFF, so a standard
///   mount stays zero-overhead and byte-identical. A run profile cannot turn the journal on, point
///   it elsewhere or turn it off.
/// - `snapshot_every` is the `config.journal_snapshot_every` row (`None`, or `0`, keeps
///   [`crate::JournalConfig::at`]'s default cadence).
/// - `profile` is the resolved run profile; its `[sinks.journal]` table, when present, lays the
///   tuning keys it NAMES over the two above ([`super::ProfileJournal::tune`]). A key it leaves off
///   keeps the row's value or the default.
///
/// It reads no environment and no settings of its own: the binary resolves the rows and hands them
/// in. `crates/vike-tradehub/src/profile_rows.rs`'s `journal_config_for` is the worked example, and
/// `crates/vike-datahub/src/datahub_cli/mounts.rs`'s `mount_journal_materializer` reads the same
/// `config.journal_dir` row to follow what that daemon writes — the two agree by construction.
#[must_use]
pub fn journal_config_from(
    profile: Option<&RunProfile>,
    journal_dir: Option<std::path::PathBuf>,
    snapshot_every: Option<u64>,
) -> Option<crate::JournalConfig> {
    let mut cfg = crate::JournalConfig::at(journal_dir?);
    if let Some(v) = snapshot_every.filter(|&v| v >= 1) {
        cfg.snapshot_every = v;
    }
    if let Some(tuning) = profile.and_then(|p| p.sinks.journal.as_ref()) {
        tuning.tune(&mut cfg);
    }
    Some(cfg)
}
