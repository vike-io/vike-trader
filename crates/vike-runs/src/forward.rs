//! A FORWARD run (book chapter 22): a strategy file mounted live on an account, recorded as a run so a
//! forward test can be found, listed and held against its backtest.
//!
//! The tradehub mints one when a mount opens (`crates/vike-tradehub/src/tradehub_cli/forward_runs.rs`): the
//! manifest and its report are written THEN, with the manifest last as for every run, so a forward test that is
//! still trading is a FINISHED run directory whose test has not ended. The end is a document of its own,
//! [`FORWARD_END_FILE`], written by [`end_forward_run`] when the mount closes, after the completion marker: its
//! absence is an ordinary answer (the test is still running, or the node stopped without an unmount), which is
//! the [`META_FILE`] argument for writing a document after the marker.

use std::path::Path;

use serde::{Deserialize, Serialize};
use vike_model::time::utc_rfc3339;

use super::read::read_doc;
use super::{
    FORWARD_END_FILE, MANIFEST_SCHEMA, RunConfig, RunDir, RunManifest, RunPersistError,
    RunReadError, create_run_dir, read_manifest, write_json, write_run,
};

#[cfg(doc)]
use super::META_FILE;

/// The [`RunManifest::kind`] a forward run carries.
///
/// ⚠ A kind of its own, never `backtest`: a forward run records a test that is still trading or one the market
/// ran, and nothing re-derives it, so a saved-run retention that prunes old `backtest` runs must never count
/// these (unit RUN-RETENTION counts the `backtest` kind only).
pub const FORWARD_RUN_KIND: &str = "forward";

/// The version of [`ForwardRun`]'s and [`ForwardRunEnd`]'s shapes.
pub const FORWARD_RUN_SCHEMA: u32 = 1;

/// What a forward run records at its start: the manifest's `detail` and the run's report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForwardRun {
    /// [`FORWARD_RUN_SCHEMA`] at write time.
    #[serde(default)]
    pub schema: u32,
    /// The id the core keys the mount by (`vike_core::strategy_state::mount_id_with`).
    pub mount_id: String,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// The account the mount trades, `venue/LABEL` (`DEFAULT` for the venue's unlabelled account).
    pub account: String,
    /// The strategy file, as the mount named it under the node's `strategies/` folder.
    pub strategy_file: String,
    /// The sha256 of the bytes the node read and compiled.
    pub strategy_sha256: String,
    /// The `[strategy.params]` table the mount was built from.
    #[serde(default)]
    pub params: vike_model::params::Params,
    /// The backtest run to hold this forward test against, when one was named. Absent from the JSON while
    /// `None`; the tradehub names none yet (no profile key reaches it: the daemon profile is stored as rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_run: Option<String>,
    /// When the mount opened, epoch milliseconds (UTC).
    pub started_ms: i64,
}

/// [`FORWARD_END_FILE`]'s content: when the mount closed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForwardRunEnd {
    /// [`FORWARD_RUN_SCHEMA`] at write time.
    #[serde(default)]
    pub schema: u32,
    /// When the mount closed, epoch milliseconds (UTC).
    pub ended_ms: i64,
}

/// Mint and write the forward run of `run` under `runs_root`: [`create_run_dir`] at the mount's second, then the
/// report and the manifest LAST ([`write_run`]). `git_sha` is the producing binary's commit, or `None`.
pub fn write_forward_run(
    runs_root: &Path,
    run: &ForwardRun,
    git_sha: Option<&str>,
) -> Result<RunDir, RunPersistError> {
    let second = run.started_ms.div_euclid(1000);
    let dir = create_run_dir(runs_root, second, None)?;
    let detail = serde_json::to_value(run).map_err(|e| RunPersistError::Serialize {
        file: super::MANIFEST_FILE,
        why: e.to_string(),
    })?;
    let manifest = RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: dir.run_id.clone(),
        kind: FORWARD_RUN_KIND.to_string(),
        produced_by: "vike-tradehub".to_string(),
        started_at: utc_rfc3339(second),
        // The RECORD finished writing at the mount; the forward test's own end is [`FORWARD_END_FILE`].
        finished_at: utc_rfc3339(second),
        git_sha: git_sha.map(str::to_string),
        strategy_sdk: None,
        fingerprint: None,
        config: RunConfig {
            path: Some(run.strategy_file.clone()),
            name: Some(run.mount_id.clone()),
        },
        detail,
        kept_rows: Vec::new(),
    };
    write_run(&dir.path, &manifest, run)?;
    Ok(dir)
}

/// Write the END of the forward run in `dir`: [`FORWARD_END_FILE`], the one writer of it. Written after the
/// completion marker, so it is optional by construction (see this module's doc).
pub fn end_forward_run(dir: &Path, ended_ms: i64) -> Result<(), RunPersistError> {
    let end = ForwardRunEnd { schema: FORWARD_RUN_SCHEMA, ended_ms };
    write_json(&dir.join(FORWARD_END_FILE), FORWARD_END_FILE, &end)
}

/// Read the forward run in `dir`: what it recorded at its start and, when it has ended, the end's time. A run
/// that is not of [`FORWARD_RUN_KIND`] is refused as a [`RunReadError::Parse`] of its manifest, in words; an
/// absent [`FORWARD_END_FILE`] is `None`, never an error.
pub fn read_forward_run(dir: &Path) -> Result<(ForwardRun, Option<i64>), RunReadError> {
    let manifest = read_manifest(dir)?;
    let path = dir.join(super::MANIFEST_FILE);
    if manifest.kind != FORWARD_RUN_KIND {
        return Err(RunReadError::Parse {
            path,
            why: format!("the run is of kind `{}`, not a `{FORWARD_RUN_KIND}` run", manifest.kind),
        });
    }
    let run: ForwardRun = serde_json::from_value(manifest.detail)
        .map_err(|e| RunReadError::Parse { path, why: e.to_string() })?;
    let ended = match read_doc::<ForwardRunEnd>(dir, FORWARD_END_FILE) {
        Ok(end) => Some(end.ended_ms),
        Err(RunReadError::Missing { .. }) => None,
        Err(other) => return Err(other),
    };
    Ok((run, ended))
}
