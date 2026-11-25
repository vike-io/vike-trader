//! The incident bundle's manifest DTOs and their pure assembly ([`build_manifest`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::redact_env;
use vike_model::time::epoch_ms_to_utc_timestamp_ms;

// ---- manifest DTOs (Deserialize so tests round-trip the artifact) ----

/// The incident window, echoed into the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentWindow {
    pub since_ms: i64,
    pub cutoff_ms: i64,
    pub cutoff_utc: String,
    pub now_ms: i64,
    pub now_utc: String,
}

/// Package / git / build provenance of the binary that produced the bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentBuild {
    pub pkg_name: String,
    pub pkg_version: String,
    /// `"debug"` / `"release"` (`cfg!(debug_assertions)`).
    pub build_profile: String,
    /// `git rev-parse HEAD`; `None` outside a repo.
    pub git_sha: Option<String>,
    /// `git status --porcelain` non-empty; `None` when git was unavailable.
    pub git_dirty: Option<bool>,
}

/// The run profile in force, pinned: the run part of the settings database's ACTIVE daemon row
/// (decision 0297), rendered to
/// the TOML document `vike_config::ProfileRun::from_toml_str` parses (the daemon loads the row the
/// same way — no profile FILE is read anywhere, decision 0111).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentProfile {
    /// the active daemon row's NAME; empty when the settings store could not be read.
    pub row: String,
    /// the store was read and the row's body rendered.
    pub loaded: bool,
    /// it parsed+validated as a [`vike_config::ProfileRun`].
    pub valid: bool,
    /// [`fnv1a64_hex`](vike_utility::fnv::fnv1a64_hex) of the rendered body; `None` if the
    /// store was unreadable.
    pub sha_fnv1a64: Option<String>,
    /// the gzipped copy's bundle name; `None` if the copy failed.
    pub archive_name: Option<String>,
    /// store-read or parse/validation error, if any.
    pub error: Option<String>,
}

/// The ACTIVE daemon row's run part as the collector receives it: the row's name and its run part
/// rendered to TOML
/// (`vike_secrets::profile_store::render_run_toml`). The BIN reads the settings database; this
/// library takes the answer as a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentProfileRunRow {
    /// the daemon row's name, the one `vike-cli config profile create run` wrote the run part into.
    pub name: String,
    /// the body, rendered.
    pub toml: String,
}

/// One frozen evidence file: what it is called in the bundle, where it came from, and its sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentFile {
    pub archive_name: String,
    pub source_path: String,
    pub source_bytes: u64,
    pub gz_bytes: u64,
    pub mtime_ms: i64,
    pub mtime_utc: String,
}

/// The journal segments frozen for the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentJournal {
    pub dir: String,
    pub segments_total: usize,
    pub segments_selected: usize,
    pub files: Vec<IncidentFile>,
}

/// The trace-log files frozen for the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentLogs {
    pub dir: String,
    pub files_total: usize,
    pub files_selected: usize,
    pub files: Vec<IncidentFile>,
}

/// One engine's triage counts from the latest `Snapshot` (full [`vike_execution::EngineSnapshot`]s ride in
/// `engine_snapshot.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentEngineSummary {
    pub venue: String,
    pub symbol: String,
    pub orders: usize,
    pub positions: usize,
}

/// The latest reachable OMS state, extracted from the journal's most recent `Snapshot` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentEngineState {
    /// a `Snapshot` was found (`false` = no journal / no snapshot yet; still valid).
    pub reachable: bool,
    pub journal_dir: Option<String>,
    /// the `hash` (its `state_hash`) the runtime stamped on the `Snapshot`.
    pub state_hash_stored: Option<u64>,
    pub state_hash_hex: Option<String>,
    /// [`vike_execution::state_hash`] recomputed over the restored engines; a divergence from the
    /// stored value is itself diagnostic.
    pub state_hash_recomputed: Option<u64>,
    pub engine_count: usize,
    pub engines: Vec<IncidentEngineSummary>,
    /// the full-snapshot JSON's bundle name; `None` if unreachable / write failed.
    pub snapshot_archive: Option<String>,
}

impl IncidentEngineState {
    /// No journal dir, or no `Snapshot` in it: still a valid manifest section.
    pub(super) fn unreachable(dir: Option<&Path>) -> Self {
        IncidentEngineState {
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
    pub window: IncidentWindow,
    pub build: IncidentBuild,
    pub profile: Option<IncidentProfile>,
    pub journal: Option<IncidentJournal>,
    pub logs: Option<IncidentLogs>,
    pub engine_state: IncidentEngineState,
    /// process environment, sorted, secret values masked ([`redact_env`]).
    pub env: Vec<(String, String)>,
}

/// Assemble the manifest from collected pieces. Pure; REDACTS the environment itself
/// ([`redact_env`]), so it is secret-safe even if a caller forgets.
pub fn build_manifest(
    now_ms: i64,
    since_ms: i64,
    build: IncidentBuild,
    profile: Option<IncidentProfile>,
    journal: Option<IncidentJournal>,
    logs: Option<IncidentLogs>,
    engine_state: IncidentEngineState,
    env_raw: &[(String, String)],
) -> IncidentManifest {
    let cutoff_ms = now_ms.saturating_sub(since_ms);
    IncidentManifest {
        kind: "vike-incident-bundle".to_string(),
        schema: 1,
        created_ms: now_ms,
        created_utc: epoch_ms_to_utc_timestamp_ms(now_ms),
        window: IncidentWindow {
            since_ms,
            cutoff_ms,
            cutoff_utc: epoch_ms_to_utc_timestamp_ms(cutoff_ms),
            now_ms,
            now_utc: epoch_ms_to_utc_timestamp_ms(now_ms),
        },
        build,
        profile,
        journal,
        logs,
        engine_state,
        env: redact_env(env_raw),
    }
}
