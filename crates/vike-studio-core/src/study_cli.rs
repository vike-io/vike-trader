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
//!            --lightgbm <path>/lightgbm                       # history over the wire
//! vike-study --study cohort --recipe r.toml --from 2026-04-07T05 --to 2026-08-05T05 \
//!            --store <project>/market_data/hist --lightgbm <path>/lightgbm   # local opt-out
//! ```
//!
//! # Why a feature-gated bin in THIS crate
//!
//! The host must sit strictly above [`run_study_plan`] (layer 50) and be able to ROUTE a history —
//! which, on the local arm, means constructing a `DataFusionHist`. ⚠ Two things in this
//! paragraph moved on 2026-09-23: the rank was written as 55 and this crate declares 50, and the
//! requirement is now routing rather than opening, because
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` made the WIRE this tool's default.
//! Neither changes the conclusion below, and that is why the paragraph is corrected rather than
//! rewritten. `vike-cli` (layer 65) clears the layer bound, but that crate is deliberately
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
use vike_data::TsRange;
use vike_datahub_client::route::{datahub_addr_for_bin, history_route, open_routed_history};
use vike_ml::GbdtLearner;
use vike_model::LiveClock;
use vike_model::runs::RunConfig;
use vike_user_research::StudyLearner;

const USAGE: &str = "\
vike-study --study NAME --recipe FILE --from WHEN --to WHEN
           [--store DIR] [--lightgbm PATH] [--scratch DIR] [--runs-root DIR]

Run one COMPILED user study over the hist store and leave a run behind.

  --study NAME      the study's folder name under user_data/research/studies/rust/
  --recipe FILE     its .toml recipe; an optional [learner] table carries the GBDT base params
  --from/--to WHEN  YYYY-MM-DD, YYYY-MM-DDTHH, or bare unix SECONDS
  --store DIR       read history from local Parquet at this root. OMITTED, the study reads it
                    from the datahub at $VIKE_DATAHUB_ADDR (or the compiled default), which is
                    the 0084 route every other reader takes
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

// ⚠ `boundary_ms` and `learner_params` MOVED to `crate::study_dispatch` with stage 7 — this module
// is `#[cfg(feature = "study-cli")]` because its route can open a concrete `DataFusionHist` on
// the local arm (it did so DIRECTLY until 2026-09-23; the gate is unchanged in force and one step
// further away), and the WIRE runner needs the same window grammar and the same `[learner]`
// reader without needing DataFusion.
// A copy would be two answers to "what does `--from 1785906000` mean", one of which only some
// builds compile.
use crate::study_dispatch::{boundary_ms, learner_params};

pub fn run(
    env: &std::collections::HashMap<String, String>,
    args: &[String],
) -> std::process::ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return std::process::ExitCode::SUCCESS;
    }

    let (Some(study), Some(recipe_path), Some(from), Some(to)) =
        (arg(args, "--study"), arg(args, "--recipe"), arg(args, "--from"), arg(args, "--to"))
    else {
        eprintln!("vike-study: --study, --recipe, --from and --to are all required\n");
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

    // WHERE the history comes from.
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` gave the store ONE reader; this
    // was the last CLI that was not asking over the wire.
    //
    // ⚠ NO FLAG IS THE WIRE here — the canonical shape, and this tool can take it silently where
    // `crates/vike-report/src/tearsheet_cli.rs` could not. A study ALWAYS reads history, so *no
    // flag* was not already spoken for by a third, store-free path; the tearsheet's was, which is
    // why that one needed its own `--datahub` selector. `--store DIR` is the local opt-out, and it
    // is exactly what every existing command line already passes — none of them change meaning.
    let store_flag = arg(args, "--store");
    let local_root = Path::new(store_flag.as_deref().unwrap_or(""));
    let route = history_route(store_flag.as_deref().map(Path::new), datahub_addr_for_bin(env));
    // ONE disclosure covering both arms: on the wire it names the ADDRESS and never a directory,
    // because the resolved root is the SERVER's and a path printed here would be a guess about
    // another box's filesystem.
    eprintln!("vike-study: history from {}", route.label(local_root));
    let store: StoreHandle = match open_routed_history(&route, local_root, env) {
        // ⚠ `Arc::from`, NOT `Arc::new`: the opener hands back a `Box<dyn HistStore + Send + Sync>`
        // and `Arc::new` would build an `Arc<Box<dyn …>>`, which is not this crate's `StoreHandle`.
        Ok(s) => Arc::from(s),
        Err(e) => {
            eprintln!("vike-study: {e}");
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
