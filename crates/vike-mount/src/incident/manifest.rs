//! The incident bundle's manifest DTOs and their pure assembly ([`build_manifest`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use super::fnv1a64_hex;
use super::{redact_env, utc_stamp_rfc3339};

// ---- manifest DTOs (Deserialize so tests round-trip the artifact) ----

/// The incident window, echoed into the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub since_ms: i64,
    pub cutoff_ms: i64,
    pub cutoff_utc: String,
    pub now_ms: i64,
    pub now_utc: String,
}

/// Package / git / build provenance of the binary that produced the bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBuildInfo {
    pub pkg_name: String,
    pub pkg_version: String,
    /// `"debug"` / `"release"` (`cfg!(debug_assertions)`).
    pub build_profile: String,
    /// `git rev-parse HEAD`; `None` outside a repo.
    pub git_sha: Option<String>,
    /// `git status --porcelain` non-empty; `None` when git was unavailable.
    pub git_dirty: Option<bool>,
}

/// The run profile in force, pinned: the settings database's ACTIVE `run` row, its body rendered to
/// the TOML document `vike_core::RunProfile::from_toml_str` parses (the daemon loads the row the
/// same way — no profile FILE is read anywhere, decision 0111).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    /// the active `run` row's NAME; empty when the settings store could not be read.
    pub row: String,
    /// the store was read and the row's body rendered.
    pub loaded: bool,
    /// it parsed+validated as a [`vike_core::RunProfile`].
    pub valid: bool,
    /// [`fnv1a64_hex`] of the rendered body; `None` if the store was unreadable.
    pub sha_fnv1a64: Option<String>,
    /// the gzipped copy's bundle name; `None` if the copy failed.
    pub archive_name: Option<String>,
    /// store-read or parse/validation error, if any.
    pub error: Option<String>,
}

/// The ACTIVE `run` row as the collector receives it: the row's name and its body rendered to TOML
/// (`vike_secrets::profile_store::render_run_toml`). The BIN reads the settings database; this
/// library takes the answer as a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunProfileRow {
    /// the row's name, as `vike-cli config bootstrap-run` / `config activate run` wrote it.
    pub name: String,
    /// the body, rendered.
    pub toml: String,
}

/// One frozen evidence file: what it is called in the bundle, where it came from, and its sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedFile {
    pub archive_name: String,
    pub source_path: String,
    pub source_bytes: u64,
    pub gz_bytes: u64,
    pub mtime_ms: i64,
    pub mtime_utc: String,
}

/// The journal segments frozen for the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalCollection {
    pub dir: String,
    pub segments_total: usize,
    pub segments_selected: usize,
    pub files: Vec<CollectedFile>,
}

/// The trace-log files frozen for the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogCollection {
    pub dir: String,
    pub files_total: usize,
    pub files_selected: usize,
    pub files: Vec<CollectedFile>,
}

/// One engine's triage counts from the latest `Snap` (full [`vike_exec::EngineSnapshot`]s ride in
/// `engine_snapshot.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSummary {
    pub venue: String,
    pub symbol: String,
    pub orders: usize,
    pub positions: usize,
}

/// The latest reachable OMS state, extracted from the journal's most recent `Snap` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineState {
    /// a `Snap` was found (`false` = no journal / no snapshot yet; still valid).
    pub reachable: bool,
    pub journal_dir: Option<String>,
    /// the `hash` (its `state_hash`) the runtime stamped on the `Snap`.
    pub state_hash_stored: Option<u64>,
    pub state_hash_hex: Option<String>,
    /// [`vike_exec::state_hash`] recomputed over the restored engines; a divergence from the
    /// stored value is itself diagnostic.
    pub state_hash_recomputed: Option<u64>,
    pub engine_count: usize,
    pub engines: Vec<EngineSummary>,
    /// the full-snapshot JSON's bundle name; `None` if unreachable / write failed.
    pub snapshot_archive: Option<String>,
}

impl EngineState {
    /// No journal dir, or no `Snap` in it: still a valid manifest section.
    pub(super) fn unreachable(dir: Option<&Path>) -> Self {
        EngineState {
            reachable: false,
            journal_dir: dir.map(|d| d.display().to_string()),
            state_hash_stored: None,
            state_hash_hex: None,
            state_hash_recomputed: None,
            engine_count: 0,
            engines: Vec::new(),
            snapshot_archive: None,
        }
    }
}

/// The bundle's plain-JSON index of everything frozen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentManifest {
    /// fixed tag identifying the artifact kind.
    pub kind: String,
    /// manifest schema version.
    pub schema: u32,
    pub created_ms: i64,
    pub created_utc: String,
    pub window: WindowInfo,
    pub build: GitBuildInfo,
    pub profile: Option<ProfileInfo>,
    pub journal: Option<JournalCollection>,
    pub logs: Option<LogCollection>,
    pub engine_state: EngineState,
    /// process environment, sorted, secret values masked ([`redact_env`]).
    pub env: Vec<(String, String)>,
}

/// Assemble the manifest from collected pieces. Pure; REDACTS the environment itself
/// ([`redact_env`]), so it is secret-safe even if a caller forgets.
pub fn build_manifest(
    now_ms: i64,
    since_ms: i64,
    build: GitBuildInfo,
    profile: Option<ProfileInfo>,
    journal: Option<JournalCollection>,
    logs: Option<LogCollection>,
    engine_state: EngineState,
    env_raw: &[(String, String)],
) -> IncidentManifest {
    let cutoff_ms = now_ms.saturating_sub(since_ms);
    IncidentManifest {
        kind: "vike-incident-bundle".to_string(),
        schema: 1,
        created_ms: now_ms,
        created_utc: utc_stamp_rfc3339(now_ms),
        window: WindowInfo {
            since_ms,
            cutoff_ms,
            cutoff_utc: utc_stamp_rfc3339(cutoff_ms),
            now_ms,
            now_utc: utc_stamp_rfc3339(now_ms),
        },
        build,
        profile,
        journal,
        logs,
        engine_state,
        env: redact_env(env_raw),
    }
}
