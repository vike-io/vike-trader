//! **The research verbs' answers**: what a study run on the backtest server reports
//! ([`WireStudyReport`], the text `Response::StudyReport` carries), the server's studies
//! ([`WireStudyListing`]), a page of its saved runs ([`WireRunPage`]) and a page of one run's file
//! ([`WireRunArtifact`]). Studies run ONLY on the server (the owner's ruling of 2026-11-19), so the
//! Studio's Research pane lists, runs and reads through these and never touches the PC's own folders.
//!
//! The server writes these and the desktop and `vike-cli research study` read them, so a key renamed on
//! one side is a compile error on the other rather than a pane gone blank.
//!
//! ⚠ **Sized for the tunnel.** A desktop reaches the server over an ssh tunnel of 13-40 KB/s: a run row
//! is about 150 bytes, so a [`RUNS_PAGE_SIZE`] page is about 30 KB, and one [`RUN_ARTIFACT_PAGE`] is
//! about 87 KB once base64-encoded. The study list carries names only, never a file's text.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// The runs a page carries when the caller names no size.
pub const RUNS_PAGE_SIZE: usize = 200;

/// The most runs one page may carry, whatever the caller asks for.
pub const RUNS_PAGE_SIZE_MAX: usize = 1000;

/// The most run-scan diagnostics a page carries ([`WireRunPage::diagnostic_total`] counts them all).
pub const RUN_DIAGNOSTICS_MAX: usize = 50;

/// The most bytes of a run's file one `ReadRunArtifact` answers, and what it answers when asked for 0.
pub const RUN_ARTIFACT_PAGE: u64 = 64 * 1024;

/// Which tier a study is in: the folder under the server's `research/studies/` it was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudyTierWire {
    /// `research/studies/rhai`: interpreted, run inside the backtest server.
    Rhai,
    /// `research/studies/rust`: a study FILE, built and run in a sandboxed child of the server.
    Rust,
}

/// The server's studies, as `Request::ListStudies` answers them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireStudyListing {
    /// The study folder on the SERVER's box (`<project>/user_data/research/studies`), named so a user
    /// knows where to put a study.
    pub root: String,
    /// The studies, the `rhai` tier in name order, then the `rust` one.
    pub studies: Vec<WireListedStudy>,
    /// Everything the scan could not use, one line each, naming the fix.
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

/// One study of the server's folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireListedStudy {
    /// The study's folder name, which `StudyRunRequestWire::study` names it by.
    pub name: String,
    /// The tier it was found in.
    pub tier: StudyTierWire,
    /// Its recipe files (`*.toml` in its folder) by file name, in name order, which
    /// `StudyRunRequestWire::recipe` names one by.
    #[serde(default)]
    pub recipes: Vec<String>,
}

/// One page of the server's saved runs, as `Request::ListRuns` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireRunPage {
    /// The runs folder on the SERVER's box (`<project>/user_data/runs`).
    pub root: String,
    /// How many runs the folder holds in all.
    pub total: usize,
    /// Which page this is, from 0 (the newest runs).
    pub page: usize,
    /// Runs a page, as held.
    pub page_size: usize,
    /// The page's runs, in the scan's order (oldest first, by directory name).
    pub runs: Vec<WireRunRow>,
    /// The first [`RUN_DIAGNOSTICS_MAX`] things the scan could not use, one line each.
    #[serde(default)]
    pub diagnostics: Vec<String>,
    /// How many diagnostics the scan had in all.
    #[serde(default)]
    pub diagnostic_total: usize,
}

impl WireRunPage {
    /// Page `page` of `runs` (the whole scan, oldest first), `page_size` a page (0 means
    /// [`RUNS_PAGE_SIZE`], more than [`RUNS_PAGE_SIZE_MAX`] is held there): page 0 is the newest
    /// `page_size` runs, page 1 the ones before them. A page past the oldest run is empty.
    pub fn of(
        root: String,
        runs: &[WireRunRow],
        diagnostics: &[String],
        page: usize,
        page_size: usize,
    ) -> Self {
        let page_size = match page_size {
            0 => RUNS_PAGE_SIZE,
            n => n.min(RUNS_PAGE_SIZE_MAX),
        };
        let end = runs.len().saturating_sub(page.saturating_mul(page_size));
        let start = end.saturating_sub(page_size);
        Self {
            root,
            total: runs.len(),
            page,
            page_size,
            runs: runs[start..end].to_vec(),
            diagnostics: diagnostics.iter().take(RUN_DIAGNOSTICS_MAX).cloned().collect(),
            diagnostic_total: diagnostics.len(),
        }
    }
}

/// One saved run as the runs list shows it: the COMMON manifest fields and not one number from any
/// producer's kind-specific subtree, so a study's Sharpe and a backtest's never share a column (R6,
/// `vike_studio_core::study_run`'s module doc).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRunRow {
    /// The run's directory name, its address.
    pub run_id: String,
    /// The manifest's `kind`, verbatim.
    pub kind: String,
    /// Which binary produced it.
    pub produced_by: String,
    /// When it started, RFC-3339 UTC as the manifest carries it.
    pub started_at: String,
    /// Whether the run's report file is there.
    pub has_report: bool,
}

/// Bytes of one file of a saved run, as `Request::ReadRunArtifact` answers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRunArtifact {
    /// The run, as asked.
    pub run_id: String,
    /// The file, as asked.
    pub file: String,
    /// The first byte these are.
    pub offset: u64,
    /// The file's whole size.
    pub total: u64,
    /// The bytes, STANDARD base64 (a file may be binary: a network's `model.onnx`).
    pub data_base64: String,
}

impl WireRunArtifact {
    /// Read `bytes` of `file` from `offset` of a file `total` bytes long, as the server answers them.
    pub fn of(run_id: &str, file: &str, offset: u64, total: u64, bytes: &[u8]) -> Self {
        Self {
            run_id: run_id.to_string(),
            file: file.to_string(),
            offset,
            total,
            data_base64: STANDARD.encode(bytes),
        }
    }

    /// The bytes, decoded.
    pub fn data(&self) -> Result<Vec<u8>, String> {
        STANDARD.decode(&self.data_base64).map_err(|e| {
            format!("{} of run {}: the answer is not base64 ({e})", self.file, self.run_id)
        })
    }
}

/// What `RunStudy` answers, as the JSON text of `Response::StudyReport`.
///
/// ⚠ **A run that was NOT saved still answers its numbers, but only to a client that asked**
/// (`crate::proto::StudyRunRequestWire::report_unsaved_result`): the study ran, and the run directory,
/// an artifact or the manifest could not be written. Then `run_id`, `dir` and `manifest` are absent and
/// [`Self::not_saved`] says why. A client that did not ask (every `vike-cli` before this field) is
/// answered `Response::Error` with the runner's own sentence, so it can never read an unsaved result as
/// a saved run.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireStudyReport {
    /// The minted run's id, also its directory's name. Absent when the run was not saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The run directory, a path on the SERVER's box. Absent when the run was not saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// The manifest as written (`vike_runs::RunManifest`'s JSON, its `detail` the study's
    /// `vike_studio_core::study_run::StudyDetail`). Absent when the run was not saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,
    /// Why the result was not saved, in the runner's words. Absent for a saved run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_saved: Option<String>,
    /// The study's name, as the request named it.
    #[serde(default)]
    pub study: String,
    /// Every metric, in the study's emission order.
    pub metrics: Vec<WireStudyMetric>,
    /// The artifacts the study returned, by name, with their size.
    #[serde(default)]
    pub artifacts: Vec<WireStudyArtifact>,
}

/// One metric as a study report carries it: `vike_studio_core::study_run::StudyMetric`'s shape (a
/// non-finite value is `value: null` and its tag, never a number it was rounded into) plus `text`, the
/// one printed form (`StudyMetric::text`: six decimals, or `NaN` / `inf` / `-inf`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireStudyMetric {
    /// The metric's name, as the study emitted it.
    pub name: String,
    /// The value, or `None` when it is non-finite (then [`Self::nonfinite`] says which).
    pub value: Option<f64>,
    /// `NaN`, `inf` or `-inf` for a non-finite value, else `None`.
    pub nonfinite: Option<String>,
    /// The value as every surface prints it.
    pub text: String,
}

/// One artifact a study returned: its file name and its size in bytes (on disk for a saved run, the
/// returned text's for an unsaved one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStudyArtifact {
    /// The artifact's file name in the run directory.
    pub name: String,
    /// Its size in bytes.
    pub bytes: u64,
}

#[path = "study_tests.rs"]
#[cfg(test)]
mod study_tests;
