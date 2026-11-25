//! **What the backtest server answers about its STUDIES and its saved RUNS** (the owner's ruling of
//! 2026-11-19: studies run only on the server, and the Studio's Research pane lists, runs and reads them
//! here): the study a `RunStudy` names ([`listed_study`]), its recipe and window, the report it answers
//! ([`study_report`]), and the three reads `ListStudies` ([`study_listing`]), `ListRuns` ([`runs_page`])
//! and `ReadRunArtifact` ([`run_artifact`]).
//!
//! ⚠ **Nothing a client names reaches a path outside the server's two folders.** A study is accepted only
//! if this server's own listing lists it for that tier, a recipe only if the study's folder lists it, a
//! run id goes through `vike_runs::finished_run_dir`, and a run's file only if the run's manifest declares
//! it as a study artifact, it passes `vike_user_research::valid_artifact_name` and it is a regular file
//! (never a link out of the run).
//!
//! ⚠ **The numbers of a run that could not be SAVED are answered too, to a client that asked.** The
//! runner hands them back beside the reason (`vike_study_runner::study_run::StudyRunError::NotPersisted`); a client
//! that set `StudyRunRequestWire::report_unsaved_result` gets them with
//! `vike_backtesthub_client::study::WireStudyReport::not_saved`, and every other client gets the runner's
//! own sentence as an error, so an older `vike-cli` never reads an unsaved result as a saved run.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use vike_backtesthub_client::proto::StudyRunRequestWire;
use vike_backtesthub_client::study::{
    RUN_ARTIFACT_PAGE, StudyTierWire, WireListedStudy, WireRunArtifact, WireRunPage, WireRunRow,
    WireStudyArtifact, WireStudyListing, WireStudyMetric, WireStudyReport,
};
use vike_data::TimestampRange;
use vike_paths::state_path::{RHAI_SUBDIR, RUST_SUBDIR};
use vike_runs::RunConfig;
use vike_studio_core::study_run::{StudyDetail, StudyMetric};
use vike_study_runner::listing::{ListedStudy, StudyTier, list_studies};
use vike_study_runner::study_dispatch::boundary_ms;
use vike_study_runner::study_dispatch::{read_recipe, recipes};
use vike_study_runner::study_run::{StudyRun, StudyRunError};
use vike_user_research::{StudyOutcome, valid_artifact_name};

/// The tier as the wire names it.
fn tier_wire(tier: StudyTier) -> StudyTierWire {
    match tier {
        StudyTier::Rhai => StudyTierWire::Rhai,
        StudyTier::Rust => StudyTierWire::Rust,
    }
}

/// The study `name` of `tier` under `studies_root` (`None` is the Rust tier, what every request before
/// the field meant), as this server's own listing found it, or the refusal naming where a study belongs.
pub(crate) fn listed_study(
    studies_root: &Path,
    name: &str,
    tier: Option<StudyTierWire>,
) -> Result<ListedStudy, String> {
    let (tier, subdir) = match tier {
        Some(StudyTierWire::Rhai) => (StudyTier::Rhai, RHAI_SUBDIR),
        Some(StudyTierWire::Rust) | None => (StudyTier::Rust, RUST_SUBDIR),
    };
    let listing = list_studies(studies_root);
    listing.studies.into_iter().find(|s| s.tier == tier && s.name == name).ok_or_else(|| {
        format!(
            "no {subdir} study called {name:?} on this server: a study is a FOLDER under {}, so \
             copy it there and run it again — nothing was run",
            studies_root.join(subdir).display()
        )
    })
}

/// The recipe TEXT a request carries, parsed (an empty text is the empty recipe).
pub(crate) fn recipe_text(text: &str) -> Result<toml::Value, String> {
    toml::from_str(text).map_err(|e| format!("recipe TOML did not parse: {e}"))
}

/// The recipe FILE `file` of `study`'s folder, read on this server, and the `config` its run records:
/// refused beside recipe text (one run, one recipe) and unless the folder lists it.
pub(crate) fn recipe_file(
    study: &ListedStudy,
    file: &str,
    text: &str,
) -> Result<(toml::Value, RunConfig), String> {
    if !text.trim().is_empty() {
        return Err(format!(
            "the request names the recipe file {file:?} AND carries recipe text: one run, one recipe"
        ));
    }
    let listed = recipes(&study.dir);
    let Some(path) = listed.iter().find(|p| p.file_name().is_some_and(|n| n == file)) else {
        return Err(format!(
            "study {} has no recipe {file:?} in {} (its recipes: {:?}) — nothing was run",
            study.name,
            study.dir.display(),
            recipe_names(&study.dir)
        ));
    };
    let params = read_recipe(path)?;
    let label = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    Ok((params, RunConfig { path: Some(path.display().to_string()), name: label }))
}

/// The window a request asks about: each bound `YYYY-MM-DD`, `YYYY-MM-DDTHH` or bare unix SECONDS
/// (`vike_study_runner::study_dispatch::boundary_ms`, the grammar `vike-study` shares), or EMPTY for
/// unbounded on that side (the Studio's "whole store").
pub(crate) fn study_window(from: &str, to: &str) -> Result<TimestampRange, String> {
    let bound = |text: &str| match text.trim() {
        "" => Ok(None),
        text => boundary_ms(text).map(Some).ok_or_else(|| {
            "--from/--to must be YYYY-MM-DD, YYYY-MM-DDTHH or unix seconds (empty: unbounded)"
                .to_string()
        }),
    };
    let window = TimestampRange { start: bound(from)?, end: bound(to)? };
    if matches!((window.start, window.end), (Some(start), Some(end)) if end <= start) {
        return Err("--to must be after --from".to_string());
    }
    Ok(window)
}

/// What `RunStudy` answers for one run of the study `request` named: the saved run, the unsaved
/// result (only when the request asked for it), or the runner's refusal in its own words.
pub(crate) fn study_report(
    request: &StudyRunRequestWire,
    ran: Result<StudyRun, StudyRunError>,
) -> Result<WireStudyReport, String> {
    match ran {
        Ok(run) => {
            let manifest = serde_json::to_value(&run.manifest)
                .map_err(|e| format!("study report serialize failed: {e}"))?;
            // The size on disk, the bytes `ReadRunArtifact` pages (a network's `model.onnx` crosses the
            // contract as base64 and is written as its bytes).
            let on_disk = |name: &str| std::fs::metadata(run.dir.join(name)).map(|m| m.len()).ok();
            Ok(WireStudyReport {
                run_id: Some(run.run_id.clone()),
                dir: Some(run.dir.display().to_string()),
                manifest: Some(manifest),
                not_saved: None,
                study: request.study.clone(),
                metrics: wire_metrics(&run.outcome),
                artifacts: wire_artifacts(&run.outcome, on_disk),
            })
        }
        Err(StudyRunError::NotPersisted { outcome, why }) if request.report_unsaved_result => {
            Ok(WireStudyReport {
                not_saved: Some(why.to_string()),
                study: request.study.clone(),
                metrics: wire_metrics(&outcome),
                artifacts: wire_artifacts(&outcome, |_| None),
                ..WireStudyReport::default()
            })
        }
        Err(refused) => Err(refused.to_string()),
    }
}

/// ⚠ METRICS GO THROUGH `StudyMetric`, never through a raw f64: `serde_json` writes NaN and ±inf as
/// `null`, which silently loses exactly the tag `vike_studio_core::study_run::nonfinite_tag` carries. Each
/// also carries its `text`, the ONE printed form (`StudyMetric::text`), so `vike-cli research study` and
/// the Studio print what `vike-study` does.
fn wire_metrics(outcome: &StudyOutcome) -> Vec<WireStudyMetric> {
    outcome
        .metrics()
        .iter()
        .map(|(name, value)| {
            let metric = StudyMetric::of(name, *value);
            let text = metric.text();
            WireStudyMetric {
                name: metric.name,
                value: metric.value,
                nonfinite: metric.nonfinite,
                text,
            }
        })
        .collect()
}

/// Every artifact by name and size: `on_disk` when the run was saved, else the returned text's length.
fn wire_artifacts(
    outcome: &StudyOutcome,
    on_disk: impl Fn(&str) -> Option<u64>,
) -> Vec<WireStudyArtifact> {
    outcome
        .artifacts()
        .iter()
        .map(|(name, body)| WireStudyArtifact {
            name: name.clone(),
            bytes: on_disk(name).unwrap_or(body.len() as u64),
        })
        .collect()
}

/// The file names of a study folder's recipes, in [`recipes`]' order.
fn recipe_names(dir: &Path) -> Vec<String> {
    let listed = recipes(dir);
    listed.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect()
}

/// `ListStudies`: every study of `studies_root`, both tiers, with its recipe FILE NAMES (never a file's
/// text), and every scan failure in words.
pub(crate) fn study_listing(studies_root: &Path) -> WireStudyListing {
    let listing = list_studies(studies_root);
    WireStudyListing {
        root: listing.root.display().to_string(),
        studies: listing
            .studies
            .iter()
            .map(|s| WireListedStudy {
                name: s.name.clone(),
                tier: tier_wire(s.tier),
                recipes: recipe_names(&s.dir),
            })
            .collect(),
        diagnostics: listing.diagnostics.iter().map(ToString::to_string).collect(),
    }
}

/// `ListRuns`: page `page` of every run under `runs_root`, newest page first
/// (`vike_backtesthub_client::study::WireRunPage::of`), each row the COMMON manifest fields only. Read through
/// `vike_runs::scan_runs_common`: the runs, diagnostics and order the listing (`vike_study_runner::listing::list_runs`)
/// scans, with no `detail` built, since a row renders none and a saved bar run's `detail` could hold its day stamps
/// (decision 0078's amendments of 2026-11-19).
pub(crate) fn runs_page(runs_root: &Path, page: usize, page_size: usize) -> WireRunPage {
    let scan = vike_runs::scan_runs_common(runs_root);
    let rows: Vec<WireRunRow> = scan
        .runs
        .iter()
        .map(|r| WireRunRow {
            run_id: r.run_id.clone(),
            kind: r.manifest.kind.clone(),
            produced_by: r.manifest.produced_by.clone(),
            started_at: r.manifest.started_at.clone(),
            has_report: r.report.is_some(),
        })
        .collect();
    let diagnostics: Vec<String> = scan.diagnostics.iter().map(ToString::to_string).collect();
    WireRunPage::of(scan.root.display().to_string(), &rows, &diagnostics, page, page_size)
}

/// `ReadRunArtifact`: up to `length` bytes (0 and anything above it is
/// `vike_backtesthub_client::study::RUN_ARTIFACT_PAGE`) from `offset` of the artifact `file` of the
/// finished run `run_id` — a file the run's manifest declares, under a valid artifact name, and a
/// regular file of the run (a link or a folder is never read).
pub(crate) fn run_artifact(
    runs_root: &Path,
    run_id: &str,
    file: &str,
    offset: u64,
    length: u64,
) -> Result<WireRunArtifact, String> {
    let dir = vike_runs::finished_run_dir(runs_root, run_id).map_err(|e| e.to_string())?;
    let manifest = vike_runs::read_manifest(&dir).map_err(|e| e.to_string())?;
    let declared =
        StudyDetail::of(&manifest).is_some_and(|d| d.artifacts.iter().any(|a| a == file));
    if !declared {
        return Err(format!(
            "run {run_id} declares no artifact {file:?}: only a study run's own artifacts are read"
        ));
    }
    valid_artifact_name(file).map_err(|why| format!("artifact name {file:?}: {why}"))?;
    let path = dir.join(file);
    let unreadable = |e: std::io::Error| format!("{}: {e}", path.display());
    let meta = std::fs::symlink_metadata(&path).map_err(unreadable)?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "{}: not a regular file of the run (a link or a folder is never read)",
            path.display()
        ));
    }
    let total = meta.len();
    let length = match length {
        0 => RUN_ARTIFACT_PAGE,
        n => n.min(RUN_ARTIFACT_PAGE),
    };
    let mut handle = std::fs::File::open(&path).map_err(unreadable)?;
    handle.seek(SeekFrom::Start(offset.min(total))).map_err(unreadable)?;
    let mut bytes = Vec::new();
    handle.take(length).read_to_end(&mut bytes).map_err(unreadable)?;
    Ok(WireRunArtifact::of(run_id, file, offset, total, &bytes))
}

#[path = "study_files_tests.rs"]
#[cfg(test)]
mod study_files_tests;
