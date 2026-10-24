//! Resolving the write-ahead journal sink: the directory from the caller's row, the tuning from the
//! resolved profile.

use vike_config::RunProfile;
use vike_config::run_profile::ProfileJournal;

/// **The write-ahead command journal**, from the ONE source of its directory and the two sources of
/// its tuning (decision 0111, amended 2026-10-11).
///
/// - `journal_dir` is the `config.journal_dir` settings row, as the BINARY resolved it. It is the
///   journal's switch and its only directory: `None` — no row — is journaling OFF, so a standard
///   mount stays zero-overhead and byte-identical. A run profile cannot turn the journal on, point
///   it elsewhere or turn it off.
/// - `snapshot_every` is the `config.journal_snapshot_every` row (`None`, or `0`, keeps
///   [`vike_core::JournalConfig::at`]'s default cadence).
/// - `profile` is the resolved run profile; its `[sinks.journal]` table, when present, lays the
///   tuning keys it NAMES over the two above ([`tune_journal`]). A key it leaves off
///   keeps the row's value or the default.
///
/// It reads no environment and no settings of its own: the binary resolves the rows and hands them
/// in. `crates/vike-tradehub/src/profile_rows.rs`'s `journal_config_for` is the worked example, and
/// `crates/vike-datahub/src/datahub_cli/lanes.rs`'s `start_journal_materializer` reads the same
/// `config.journal_dir` row to follow what that daemon writes — the two agree by construction.
#[must_use]
pub fn journal_config_from(
    profile: Option<&RunProfile>,
    journal_dir: Option<std::path::PathBuf>,
    snapshot_every: Option<u64>,
) -> Option<vike_core::JournalConfig> {
    let mut cfg = vike_core::JournalConfig::at(journal_dir?);
    if let Some(v) = snapshot_every.filter(|&v| v >= 1) {
        cfg.snapshot_every = v;
    }
    if let Some(tuning) = profile.and_then(|p| p.sinks.journal.as_ref()) {
        tune_journal(tuning, &mut cfg);
    }
    Some(cfg)
}

/// Lay the keys a profile's `[sinks.journal]` NAMES over `cfg` — a key left off leaves `cfg`'s value
/// standing (compile-checked mapping onto the real [`vike_core::JournalConfig`]).
pub fn tune_journal(tuning: &ProfileJournal, cfg: &mut vike_core::JournalConfig) {
    if let Some(v) = tuning.segment_bytes {
        cfg.file.segment_bytes = v;
    }
    if let Some(v) = tuning.flush_every {
        cfg.file.flush_every = v;
    }
    if let Some(v) = tuning.snapshot_every {
        cfg.snapshot_every = v;
    }
}
