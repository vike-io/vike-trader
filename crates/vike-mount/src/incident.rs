//! `incident` — freeze the perishable evidence of a live run into ONE timestamped bundle.
//!
//! The forensic trail is perishable: the command journal ([`vike_journal`], segments PRUNE), the
//! trace log ([`vike_log`], ROLLS, may be size-capped), the [`vike_core::RunProfile`] (the active
//! `run` row of the settings database, editable between runs) and the process environment (gone at
//! exit). Given a window (`--since <dur>`),
//! [`run_incident`](crate::incident::run_incident) copies into one timestamped directory:
//!   - journal segments and log-dir trace-log files whose last-write overlaps the window
//!     (gzipped, per file),
//!   - the active `run` row's body, rendered to the TOML its loader parses, + its FNV-1a64 hash (a
//!     later edit of the row is detectable),
//!   - the latest [`vike_exec::EngineSnapshot`] + `state_hash` from the journal's last `Snap`,
//!   - git SHA / build info, and
//!   - a REDACTED environment dump — secret-shaped values masked EXACTLY as the signers' manual
//!     `Debug` impls do (`crates/vike-bridge-core/src/signer.rs`: `***` + the last four chars), so
//!     a shared bundle never carries an API secret / private key.
//!
//! ## Read-only, additive
//! Nothing in the mount path calls it. It opens the journal only through the lock-free
//! [`vike_journal::CommandJournal::read_all`] (the "offline inspection tooling" entry point),
//! never the writer, so it is safe against a live node.
//!
//! ## Archive shape (per-file gz in a directory, NOT a single `tar.gz`)
//! Each file is gzipped individually ([`flate2`] `GzEncoder`) into `incident-<UTC>/…` beside a
//! plain `manifest.json`: a `.tar.gz` would need a `tar` crate this lane cannot compile-check.
//!
//! ## Purity for testability
//! Pure and unit-tested: [`select_overlapping`](crate::incident::select_overlapping) (window→file),
//! [`redact_env`](crate::incident::redact_env) /
//! [`redact_secret_value`](crate::incident::redact_secret_value) (masking), and
//! [`build_manifest`](crate::incident::build_manifest) (redacts internally: safe by construction).
//! [`run_incident_at`](crate::incident::run_incident_at) is a thin `std::fs` + `flate2` shell.

mod collect;
mod manifest;
mod pure;

pub use collect::{IncidentConfig, run_incident, run_incident_at};
pub use manifest::{
    CollectedFile, EngineState, EngineSummary, GitBuildInfo, IncidentManifest, JournalCollection,
    LogCollection, ProfileInfo, RunProfileRow, WindowInfo, build_manifest,
};
pub use pure::{
    fnv1a64_hex, is_secret_key, redact_env, redact_secret_value, select_overlapping,
    utc_stamp_compact, utc_stamp_rfc3339,
};

#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;

#[path = "incident_tests.rs"]
#[cfg(test)]
mod incident_tests;
