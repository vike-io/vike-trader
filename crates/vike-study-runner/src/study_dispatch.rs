//! **Running a study the way a SURFACE has to run one**: address it the way
//! [`crate::listing::list_studies`] named it, and reach [`crate::study_run`]'s ONE persistence path
//! whichever tier it turned out to be.
//!
//! # Why this module exists at all
//!
//! `crates/vike-study-runner/src/study_run.rs` is the persistence half and
//! `crates/vike-study-runner/src/listing.rs` is the enumeration half, and until this file there was
//! nothing joining them. A surface — the backtest server's `RunStudy`, the `vike-study` runner — holds a
//! [`crate::listing::ListedStudy`]: a NAME, a FOLDER and a [`StudyTier`]. A Rust study FILE runs
//! sandboxed, through its host's [`StudyFileRunner`]; [`crate::rhai_study::RhaiStudy::load`] takes a
//! folder. Neither is reachable from "the row the user clicked" without a `match` on the tier, and
//! a `match` on the tier written at every call site is the second copy of a rule that should have
//! one.
//!
//! So: one entry point, [`run_study_plan`], whose whole job is that `match` — and whose two arms
//! converge on the same persistence, so a run produced by an interpreted study and one produced by
//! a study file are indistinguishable on disk. That is R7's *"rhai OR rust, the author's
//! choice"* stated as behaviour rather than as intent.
//!
//! # Both tiers reach that persistence as a VALUE
//!
//! `crates/vike-study-runner/src/study_run.rs`'s `run_study_with` takes the study as a `&dyn Fn` (its
//! `StudyEntry`), so an interpreted study is called as itself (`crate::rhai_study::RhaiStudy::run`)
//! and a Rust study FILE as its host runs it, sandboxed ([`StudyFileRunner`]). It took a bare `fn`
//! pointer until 2026-11-15, when the compiled-in registry it served was deleted (owner's rule: a
//! study is a FILE, built, loaded and sandboxed exactly like a strategy file), and the thread-local
//! bridge an interpreted study crossed it by went with it.
//!
//! # What this module does NOT do
//!
//! It resolves no path — [`StudyRunPlan`] carries every directory as a field, filled in by the
//! BINARY (`crates/vike-paths/src/state_path.rs`'s `user_data_dir_beside` and the `*_SUBDIR`
//! constants), for the reason `crates/vike-study-runner/src/listing.rs`'s module doc gives. It
//! constructs no store, for the reason `study_run.rs`'s does. And it supplies **no learner** — see
//! [`run_study_plan`], where that ceiling is stated rather than left to be discovered by a study
//! that wanted to fit.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_data::HistoryStore;
use vike_data::TimestampRange;
use vike_model::Clock;
use vike_runs::RunConfig;

use vike_ml_training::{GbdtParams, ParamPoint, ParamValue};

use crate::listing::StudyTier;
use crate::rhai_study::RhaiStudy;
use crate::study_run::{StudyRun, StudyRunError, StudyRunRequest, run_study_with};
use vike_user_research::{StudyContext, StudyError, StudyLearner, StudyOutcome, StudySim};

/// A window boundary (`YYYY-MM-DD`, `YYYY-MM-DDTHH` or bare unix SECONDS) -> epoch MILLIseconds,
/// which is what [`TimestampRange`] speaks. The grammar is `vike_model::time::parse_window_boundary_secs`'s
/// (shared with the vikedata backfill); this adds only the unit [`TimestampRange`] needs, and a boundary
/// whose milliseconds overflow `i64` is no boundary.
///
/// ⚠ Here rather than in `vike_backtesthub::study_cli`: the WIRE runner (`vike_backtesthub::studio_run`'s
/// `run_study_local`) needs it without needing DataFusion.
pub fn boundary_ms(s: &str) -> Option<i64> {
    vike_model::time::parse_window_boundary_secs(s)?.checked_mul(1_000)
}

/// How a HOST runs a Rust study FILE (owner's rule, 2026-11-15: a study is a FILE, built, loaded and
/// sandboxed exactly like a strategy file): the study's folder, the context `run_study_with` built and
/// the recipe in, the study's outcome out. The host reads the folder, has it built and runs it in
/// the jailed study process (`vike_backtesthub::study_sandbox::runner`, a crate this one cannot
/// name); this crate runs no study file itself.
pub type StudyFileRunner = Arc<
    dyn Fn(&Path, &StudyContext, &toml::Value) -> Result<StudyOutcome, StudyError> + Send + Sync,
>;

/// The scratch directory a study may write into when its host names none: `scratch/vike-study`
/// BESIDE the runs root (`<runs_root>/../scratch/vike-study`), never the system temp directory —
/// the rule `crates/vike-ops/tests/hygiene/system_temp_gate.rs` and `temp_path_gate.rs` carry (a
/// fixed name under the system temp is a cross-user PermissionDenied trap). One derivation for the
/// two hosts that need a default.
pub fn default_study_scratch(runs_root: &Path) -> PathBuf {
    runs_root
        .parent()
        .map_or_else(|| PathBuf::from("scratch"), |p| p.join("scratch"))
        .join("vike-study")
}

/// The `[learner]` table -> the base parameter bag, and whether the recipe actually carried one.
///
/// Every key of the table but `kind` (the learner's choice, [`neural_spec`]'s) is ONE LightGBM parameter,
/// read by `GbdtParams::with_point`, the one reader every parameter point goes through: the names
/// `GbdtParams::apply` knows are honoured, an integer knob refuses a float or a value that does not fit it,
/// a float knob takes an integer as that number, and any other name or value is refused by name. An absent
/// key keeps LightGBM's default, so a partial table is meaningful rather than an error. The recipe itself
/// stays a `toml::Value` (a recipe is a user DOCUMENT, not a strategy parameter bag).
///
/// # Errors
/// A `[learner]` that is not a table, or a key or value `GbdtParams::apply` refuses, in its words.
///
/// ⚠ MOVED here from `crate::study_cli` with [`boundary_ms`], for the same reason.
pub fn learner_params(recipe: &toml::Value) -> Result<(GbdtParams, bool), String> {
    let Some(learner) = recipe.get("learner") else { return Ok((GbdtParams::default(), false)) };
    let Some(table) = learner.as_table() else {
        return Err("[learner]: expected a table of LightGBM parameters".to_string());
    };
    let values = table
        .iter()
        .filter(|(key, _)| key.as_str() != "kind")
        .map(|(key, value)| {
            let value = match value {
                toml::Value::Integer(i) => ParamValue::Int(*i),
                toml::Value::Float(f) => ParamValue::Float(*f),
                other => ParamValue::Text(other.to_string()),
            };
            (key.clone(), value)
        })
        .collect();
    GbdtParams::default()
        .with_point(&ParamPoint { values })
        .map(|params| (params, true))
        .map_err(|e| format!("[learner]: {e}"))
}

/// The network a recipe asks for, as the network trainer's spec text, or `None` for the tree learner.
///
/// A `[learner]` table with `kind = "neural"` names a NETWORK (`vike_ml_training::NeuralLearner`,
/// decision 0292): its other keys are the trainer's spec (`vike_ml_neural_training::trainer`'s
/// `NetworkSpec`: `network`, `hidden`, `steps`, `loss`, `epochs`, ...), handed over verbatim and
/// checked by the trainer, so a misspelt key is the fit's refusal. With no `kind`, or `kind = "gbdt"`,
/// the table is [`learner_params`]'s, as before; any other `kind` is refused.
pub fn neural_spec(recipe: &toml::Value) -> Result<Option<String>, String> {
    let Some(table) = recipe.get("learner").and_then(toml::Value::as_table) else {
        return Ok(None);
    };
    match table.get("kind").map(|kind| kind.as_str()) {
        None | Some(Some("gbdt")) => Ok(None),
        Some(Some("neural")) => {
            let mut spec = table.clone();
            spec.remove("kind");
            toml::to_string(&spec).map(Some).map_err(|e| format!("[learner]: {e}"))
        }
        Some(other) => Err(format!("[learner] kind {other:?}: expected \"gbdt\" or \"neural\"")),
    }
}

/// The extension a study RECIPE carries — one named configuration of a study, sitting beside the
/// entry file in the study's own folder.
///
/// Both tiers document the same layout and neither RUNNER reads a recipe:
/// `crates/vike-study-runner/src/rhai_study/mod.rs`'s folder sketch calls them *"recipes: named
/// configurations, loaded by the CALLER, not by this runner"*, and a study file's builder reads only
/// its `.rs` files. [`recipes`] is that caller's half.
pub const RECIPE_EXT: &str = "toml";

/// Every recipe in a study folder, in NAME order.
///
/// Sorted for the reason `crates/vike-study-runner/src/listing.rs`'s module doc gives about
/// directory order: a picker whose entries move between two calls is one nobody can read.
///
/// An absent or unreadable folder is an EMPTY list rather than an error, and that is a narrower
/// claim than it looks: a study folder with no recipe is the ordinary state (both tiers run a study
/// with no configuration at all — see [`empty_recipe`]), so "no recipes" is not a
/// failure to report. A folder that cannot be READ is already reported by the listing that produced
/// the row this is called for; a second diagnostic here would be the same fact twice.
pub fn recipes(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case(RECIPE_EXT))
        })
        .collect();
    found.sort();
    found
}

/// The recipe a study runs with when the operator picked none: an empty TOML table. A recipe is a
/// user DOCUMENT handed to the study whole, so it stays a `toml::Value` rather than a strategy's
/// `vike_model::params::Params` (that is `vike_studio_core::spec::empty_params`, a different thing).
pub fn empty_recipe() -> toml::Value {
    toml::Value::Table(toml::Table::new())
}

/// Read and parse one recipe, or say why not in one line an operator can act on.
///
/// ⚠ `toml::from_str`, NOT `text.parse()`. Under the workspace's pinned `toml = "1.1"`,
/// `FromStr for Value` parses a single VALUE rather than a DOCUMENT, so a recipe with more than one
/// key comes back as `unexpected content, expected nothing` pointing at the first space —
/// `crates/vike-study-runner/src/rhai_study/mod.rs`'s own test helper carries the same warning, from
/// having been written the other way once.
pub fn read_recipe(path: &Path) -> Result<toml::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: could not be read — {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: is not valid TOML — {e}", path.display()))
}

/// ONE study run, addressed as a listing addresses it and hosted as a binary hosts it.
///
/// Public fields and no constructor, the device
/// `crates/vike-study-runner/src/study_run.rs`'s `StudyRunRequest` uses and for its reason: a struct
/// literal cannot omit a field, so a field added here stops every caller until it decides what to
/// put there.
pub struct StudyRunPlan {
    /// The study's folder name — its address in both tiers.
    /// `crates/vike-study-runner/src/listing.rs`'s `ListedStudy::name`.
    pub name: String,
    /// The study's own folder: the interpreted tier loads its script from it, and a Rust study
    /// FILE's host reads its `.rs` files from it ([`StudyRunPlan::file_runner`]).
    pub dir: PathBuf,
    /// Which tier the listing found it in. The ONE thing this module branches on.
    pub tier: StudyTier,
    /// The recipe, already parsed. [`empty_recipe`] is the "no configuration" value.
    pub params: toml::Value,
    /// Which recipe drove the run, for the manifest — the file as the operator sees it, and its
    /// label. `crates/vike-runs/src/lib.rs`'s `RunConfig` carries the "not canonicalized" rule.
    pub config: RunConfig,
    /// The store the study reads. Built by the BINARY.
    pub store: Arc<dyn HistoryStore + Send + Sync>,
    /// The window the run is ASKED about.
    pub window: TimestampRange,
    /// A directory the study may write scratch into. Created by `run_study_with`; never deleted.
    pub scratch: PathBuf,
    /// `<project>/user_data/runs`.
    pub runs_root: PathBuf,
    /// The BINARY doing this, spelled literally — `RunManifest::produced_by`'s rule.
    pub produced_by: String,
    /// The learner a FITTING study is handed, or `None` for a host that cannot fit.
    ///
    /// ⚠ Present-and-`Option` rather than absent, and that is the change this field IS. It used to
    /// be absent, with `run_study_plan` hardcoding `None`, because the only host was the Studio —
    /// whose ML surface is the INFERENCE half only, so a fitting study could answer nothing but
    /// `StudyError::NoLearner`. A host that CAN fit (the backtest server, `vike-study`) hands its
    /// learner here; `None` says the box has none. The Studio runs no study since 2026-11-19.
    pub learner: Option<Arc<dyn StudyLearner>>,
    /// The ONE backtest a study may ask for, or `None` for a host that runs no backtest.
    /// `crates/vike-study-runner/src/study_run.rs`'s `StudyRunRequest::sim` carries the rule.
    pub sim: Option<Arc<dyn StudySim>>,
    /// How this host runs a Rust study FILE, sandboxed — the backtest server's
    /// (`vike_backtesthub::study_sandbox::runner`) — or `None` for a host that starts no sandbox,
    /// where a Rust study answers `StudyRunError::NoSandbox`.
    pub file_runner: Option<StudyFileRunner>,
    /// The commit that binary was built from, or `None` when it cannot name one.
    pub git_sha: Option<String>,
}

/// Resolve the tier, run the study, and leave a run behind — the ONE entry point a surface needs.
///
/// ⚠ **The learner comes from the PLAN**: a host that can fit (the backtest server's `RunStudy`, the
/// `vike-study` runner) constructs a `vike_ml_training::GbdtLearner` over the pinned LightGBM binary, or
/// a network trainer, and hands it here ([`StudyRunPlan::learner`]). `None` remains a working state,
/// not a degraded one: a run made with no learner is a different experiment, not a worse one, and
/// `run_study_with`'s manifest records which it was.
///
/// The window, the scratch directory and the runs root all come from the plan, so this function
/// resolves nothing and reads no environment.
pub fn run_study_plan(plan: StudyRunPlan, clock: &dyn Clock) -> Result<StudyRun, StudyRunError> {
    let StudyRunPlan {
        name,
        dir,
        tier,
        params,
        config,
        store,
        window,
        scratch,
        runs_root,
        produced_by,
        git_sha,
        learner,
        sim,
        file_runner,
    } = plan;
    let req = StudyRunRequest {
        study: &name,
        params: &params,
        store,
        learner,
        sim,
        window,
        scratch,
        runs_root: &runs_root,
        produced_by: &produced_by,
        git_sha,
        config,
        clock,
    };
    match tier {
        // A study FILE: built, loaded and run in the jailed study process by the host's runner, then
        // through the same persistence. A host with no runner refuses before anything runs.
        StudyTier::Rust => {
            let Some(file_runner) = file_runner else {
                return Err(StudyRunError::NoSandbox { name: name.clone() });
            };
            run_study_with(
                &|ctx: &StudyContext, params: &toml::Value| file_runner(&dir, ctx, params),
                req,
            )
        }
        // Compiled at RUN time from the folder, then through the same persistence. A load failure
        // is the study's own refusal — a folder with no entry file holds nothing that would ever
        // run, which `RhaiStudy::load` says in the words it fails with.
        StudyTier::Rhai => {
            let study = RhaiStudy::load(&dir)
                .map_err(|why| StudyRunError::Study { name: name.clone(), why })?;
            run_study_with(&|ctx: &StudyContext, params: &toml::Value| study.run(ctx, params), req)
        }
    }
}

#[path = "study_dispatch_tests.rs"]
#[cfg(test)]
mod study_dispatch_tests;
