//! `vike-study` — the study runner that can FIT, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/vike-study.rs`'s whole body until the multicall merge. It moved here so the
//! same code can be reached BOTH by that binary (kept, so `CARGO_BIN_EXE_*` and every installed
//! path still resolve) and by the `vike` dispatcher, which links one copy of the DataFusion closure
//! for every tool instead of one per binary. `main` became [`run`], taking argv as a PARAMETER —
//! this module reads no environment and no `std::env::args()`, which is what lets the dispatcher
//! own the process's single sweep.
//!
//! Everything below is the binary's own documentation, unchanged.
//! mint a run.
//!
//! Until this binary existed, nothing in the workspace could fit a compiled study at all. The one
//! caller of [`run_study_plan`] was the Studio, whose ML surface is the INFERENCE half only
//! (`crates/vike-studio-core/src/ml.rs`), so it supplies no learner and a study needing a
//! gradient-boosted classifier per fold answered `StudyError::NoLearner` on every invocation —
//! *"the documented ceiling of a host with no LightGBM binary"*. The cohort study is exactly such
//! a study, which left the whole compiled tier unrunnable in practice rather than in principle.
//!
//! ```text
//! vike-study --study cohort --recipe r.toml --from 2026-04-07T05 --to 2026-08-05T05 \
//!            --store <project>/market_data/hist --lightgbm <path>/lightgbm
//! ```
//!
//! # Why a feature-gated bin in THIS crate
//!
//! The host must sit strictly above [`run_study_plan`] (layer 55) and be able to construct a
//! `DataFusionHist`. `vike-cli` (layer 65) clears the layer bound, but that crate is deliberately
//! LIGHT — it reaches history through `vike-datahub-client`, over the wire, and names no
//! DataFusion at all; `scripts/ci_feature_suite.sh`'s `light-consumers` lane compiles it as the
//! default-features-off consumer precisely to keep that true. Buying a verb with that property
//! would have charged every `vike-cli` user for it.
//!
//! A bin beside the function it calls costs nothing instead: `study-cli` is OFF by default, so
//! `studio-standalone`'s structural check — the default normal-dep tree must carry no datafusion
//! crate — is untouched, and `required-features` keeps this bin out of every build that has not
//! asked for it.
//!
//! # The `[learner]` table, and the gap it closes
//!
//! A study's LightGBM base parameters are a property OF THE STUDY — the non-searched keys its
//! published results were measured at — while `vike_ml::GbdtLearner` knows nothing about any study
//! and the HOST is what constructs it. In the cohort study those five keys had no home a host
//! could reach: they live in a `#[cfg(test)]` module whose own doc calls itself *"their only
//! home"*, so any real run would have used LightGBM's defaults and quietly fitted a different
//! model than the one every recorded number came from.
//!
//! So the RECIPE carries them, under `[learner]`, and this binary reads that table. The recipe is
//! already the study's own file and already travels with it; the study ignores the table (its
//! reader is lenient, as the strategy tier's is); and nothing study-specific is compiled into this
//! generic verb. An absent table means `GbdtParams::default()` — LightGBM's own defaults, the
//! right answer for a caller with no opinion and the wrong one to assume in silence, so the run
//! PRINTS which of the two it used.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::listing::StudyTier;
use crate::{StoreHandle, StudyRunPlan, read_recipe, run_study_plan};
use vike_backtest::runs::RunConfig;
use vike_data::{DataFusionHist, TsRange};
use vike_ml::{GbdtLearner, GbdtParams};
use vike_model::LiveClock;
use vike_user_research::StudyLearner;

const USAGE: &str = "\
vike-study --study NAME --recipe FILE --from WHEN --to WHEN --store DIR
           [--lightgbm PATH] [--scratch DIR] [--runs-root DIR]

Run one COMPILED user study over the hist store and leave a run behind.

  --study NAME      the study's folder name under user_data/research/studies/rust/
  --recipe FILE     its .toml recipe; an optional [learner] table carries the GBDT base params
  --from/--to WHEN  YYYY-MM-DD, YYYY-MM-DDTHH, or bare unix SECONDS
  --store DIR       the hist store root the study reads
  --lightgbm PATH   the PINNED trainer. Omitted, the study runs with NO learner and a fitting
                    study refuses BY NAME rather than silently producing nothing
  --scratch DIR     per-fit scratch root (default: <runs-root>/../scratch/vike-study — the
                    project folder, never the system temp directory)
  --runs-root DIR   where the run is minted (default: user_data/runs)";

/// One `--flag value` lookup. Hand-rolled rather than a dependency: this binary takes eight flags,
/// and `vike_backfill::cli`'s parser lives in a crate this one may not name (the layer rule).
fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// `YYYY-MM-DD`, `YYYY-MM-DDTHH` or bare unix SECONDS -> epoch MILLIseconds, which is what
/// [`TsRange`] speaks. Seconds are accepted because every anchor in the cohort study's own record
/// is written that way (`--end-anchor 1785906000`), and re-deriving one by hand is how a window
/// silently moves.
fn boundary_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Some(n * 1_000);
    }
    if let Some((y, m, d, h)) = vike_model::time::parse_hour_label(s) {
        let secs = vike_model::time::days_from_civil(y, m, d) * 86_400 + i64::from(h) * 3_600;
        return Some(secs * 1_000);
    }
    let (y, m, d) = vike_model::parse_ymd(s).ok()?;
    Some(vike_model::time::days_from_civil(y, m, d) * 86_400 * 1_000)
}

/// The `[learner]` table -> the base parameter bag, and whether the recipe actually carried one.
///
/// An absent KEY keeps LightGBM's default, so a partial table is meaningful rather than an error:
/// a study that fixes two of the five says exactly that.
fn learner_params(recipe: &toml::Value) -> (GbdtParams, bool) {
    let d = GbdtParams::default();
    let Some(t) = recipe.get("learner") else { return (d, false) };
    let f = |k: &str, dv: f64| t.get(k).and_then(toml::Value::as_float).unwrap_or(dv);
    let u = |k: &str, dv: u32| {
        t.get(k).and_then(toml::Value::as_integer).and_then(|v| u32::try_from(v).ok()).unwrap_or(dv)
    };
    let p = GbdtParams {
        num_iterations: u("num_iterations", d.num_iterations),
        lambda_l1: f("lambda_l1", d.lambda_l1),
        lambda_l2: f("lambda_l2", d.lambda_l2),
        bagging_fraction: f("bagging_fraction", d.bagging_fraction),
        bagging_freq: u("bagging_freq", d.bagging_freq),
        ..d
    };
    (p, true)
}

pub fn run(args: &[String]) -> std::process::ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return std::process::ExitCode::SUCCESS;
    }

    let (Some(study), Some(recipe_path), Some(from), Some(to), Some(store_dir)) = (
        arg(args, "--study"),
        arg(args, "--recipe"),
        arg(args, "--from"),
        arg(args, "--to"),
        arg(args, "--store"),
    ) else {
        eprintln!("vike-study: --study, --recipe, --from, --to and --store are all required\n");
        eprintln!("{USAGE}");
        return std::process::ExitCode::FAILURE;
    };

    let (Some(from_ms), Some(to_ms)) = (boundary_ms(&from), boundary_ms(&to)) else {
        eprintln!("vike-study: --from/--to must be YYYY-MM-DD, YYYY-MM-DDTHH or unix seconds");
        return std::process::ExitCode::FAILURE;
    };
    if to_ms <= from_ms {
        eprintln!("vike-study: --to must be after --from");
        return std::process::ExitCode::FAILURE;
    }

    let params = match read_recipe(Path::new(&recipe_path)) {
        Ok(v) => v,
        Err(why) => {
            eprintln!("vike-study: {why}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let store: StoreHandle = match DataFusionHist::open(&store_dir) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("vike-study: opening the hist store at {store_dir}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let runs_root = arg(args, "--runs-root")
        .map_or_else(|| PathBuf::from("user_data").join("runs"), PathBuf::from);
    // Beside the runs root, NEVER the system temp directory: production scratch belongs in the
    // project folder (`crates/vike-ops/tests/system_temp_gate.rs` carries the container and
    // shared-box arguments), and a fixed name under the system temp is a cross-user
    // PermissionDenied trap (`temp_path_gate.rs`: whichever user creates it first owns it
    // forever, because nothing cleans that directory).
    let scratch = arg(args, "--scratch").map_or_else(
        || {
            runs_root
                .parent()
                .map_or_else(|| PathBuf::from("scratch"), |p| p.join("scratch"))
                .join("vike-study")
        },
        PathBuf::from,
    );

    // Absent is a WORKING state, not an error path: the study refuses by name, and a run made with
    // no learner is a different experiment rather than a failed one.
    let learner: Option<Arc<dyn StudyLearner>> = match arg(args, "--lightgbm") {
        None => {
            eprintln!(
                "vike-study: no --lightgbm — running with NO learner; a fitting study will refuse"
            );
            None
        }
        Some(bin) => {
            let (base, from_recipe) = learner_params(&params);
            eprintln!(
                "vike-study: learner={bin} base params from {}",
                if from_recipe {
                    "the recipe's [learner] table"
                } else {
                    "LightGBM's own defaults"
                }
            );
            match GbdtLearner::new(Path::new(&bin), &scratch, base) {
                Ok(l) => Some(Arc::new(l)),
                Err(e) => {
                    eprintln!("vike-study: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
    };

    let plan = StudyRunPlan {
        name: study.clone(),
        // Recorded rather than read: the compiled tier resolves through the generated registry and
        // never opens the folder.
        dir: PathBuf::from("user_data/research/studies/rust").join(&study),
        tier: StudyTier::Rust,
        params,
        // The operator's own spelling, deliberately not canonicalized — `RunConfig`'s rule.
        config: RunConfig { path: Some(recipe_path.clone()), name: None },
        store,
        window: TsRange::of(from_ms, to_ms),
        scratch,
        runs_root,
        produced_by: "vike-study".to_string(),
        // This binary cannot name its commit: `vike-studio-core` carries no build-info dependency,
        // and adding one to a library crate to stamp a bin would be the wrong trade. `None` is the
        // documented value for a producer that cannot name one.
        git_sha: None,
        learner,
    };

    match run_study_plan(plan, &LiveClock) {
        Ok(run) => {
            println!("run {} -> {}", run.run_id, run.dir.display());
            for (k, v) in run.outcome.metrics() {
                println!("{k} = {v}");
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-study: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
