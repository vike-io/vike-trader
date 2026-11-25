//! `vike-study` — the study runner that can FIT, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/vike-study.rs`'s whole body until the multicall merge. It moved here so the
//! same code can be reached BOTH by that binary (kept, so `CARGO_BIN_EXE_*` and every installed
//! path still resolve) and by the `vike-backend` dispatcher, which links one copy of the DataFusion
//! closure for every tool instead of one per binary. `main` became [`run`], taking argv as a
//! PARAMETER — this module reads no environment and no `std::env::args()`, which is what lets the
//! dispatcher own the process's single sweep.
//!
//! ⚠ **And it moved AGAIN on 2026-11-10**, from `vike-studio-core` (behind its `study-cli` feature)
//! to this crate, ungated: a study this tool runs may ask for one backtest, and the one
//! implementation of that seam (`crate::harness_sim::HarnessSim`) drives the harness, which
//! `vike-studio-core` no longer links because the desktop links it
//! (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of that
//! day). Everything this tool needs (`clap`, `vike-utility/cli`, the history route) was already a normal
//! edge here, so the feature went with the move. The "Why a feature-gated bin in THIS crate" section
//! below is that crate's history, kept as written.
//!
//! ⚠ **And since 2026-11-16 it FINDS the trainer.** With no `--lightgbm` the run takes the one the
//! project installed (`crate::backtesthub_server::installed_trainer`, the same lookup the daemon's
//! `RunStudy` makes), so a box holding the release's `bin/lightgbm/` fits with no flag. `--lightgbm`
//! still wins; with neither, the run has NO learner exactly as before. A found binary is checked
//! like a named one, so a broken install (no `PROVENANCE`, another tag) fails the run, as it fails
//! the daemon's.
//!
//! Everything below is the binary's own documentation, unchanged.
//! mint a run.
//!
//! Until this binary existed, nothing in the workspace could fit a compiled study at all. The one
//! caller of [`run_study_plan`] was the Studio, which fits nothing (no trainer binary ships off
//! Linux), so it supplies no learner and a study needing a
//! gradient-boosted classifier per fold answered `StudyError::NoLearner` on every invocation —
//! *"the documented ceiling of a host with no LightGBM binary"*. The cohort study is exactly such
//! a study, which left the whole compiled tier unrunnable in practice rather than in principle.
//!
//! ```text
//! vike-study --study cohort --recipe r.toml --from 2026-04-07T05 --to 2026-08-05T05 \
//!            --lightgbm <path>/lightgbm                       # history over the wire
//! # local files: start a key-less loopback datahub on them first, then run the line above
//! vike-backend datahub --store <project>/market_data/hist
//! ```
//!
//! # Why a feature-gated bin in THIS crate
//!
//! The host must sit strictly above [`run_study_plan`] (this crate's layer: 35 since 2026-09-28,
//! 50 when this was written) and be able to ROUTE a history.
//! ⚠ Until 2026-09-25 that sentence went on "which, on the local arm, means constructing a
//! `HistoryStoreDataFusion`"; the owner closed the local arm that day, so routing no longer constructs one
//! and `study-cli` stops forwarding the DataFusion engine. Two further things in this
//! paragraph moved on 2026-09-23: the rank was written as 55 and this crate declared 50, and the
//! requirement is now routing rather than opening, because
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` made the WIRE this tool's default.
//! Neither changes the conclusion below, and that is why the paragraph is corrected rather than
//! rewritten. `vike-cli` (layer 65 then) cleared the layer bound — it declares 30 now, below this
//! crate, so the question no longer arises — but that crate is deliberately LIGHT: it reaches
//! history through `vike-datahub-client`, over the wire, and names no DataFusion at all;
//! `scripts/ci_feature_suite.sh`'s `light-consumers` lane compiles it as the
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
//! published results were measured at — while `vike_ml_training::GbdtLearner` knows nothing about any study
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

use vike_data::HistoryStore;
use vike_data::TimestampRange;
use vike_datahub_client::route::{
    datahub_address_for_bin, history_route, open_routed_history, routed_hub_keys,
};
use vike_ml_training::{GbdtLearner, NeuralLearner};
use vike_model::ClockLive;
use vike_model::run_request::flag_vocab::store_flag_removed;
use vike_runs::RunConfig;
use vike_study_runner::listing::StudyTier;
use vike_study_runner::study_dispatch::{StudyRunPlan, read_recipe, run_study_plan};
use vike_user_research::StudyLearner;

/// Run one user study FILE (built by the strategy builder, run in the jailed study process) over the
/// hist store and leave a run behind.
///
/// History is read from the datahub the config.datahub_addr row names (or the compiled loopback
/// default). For local files, start a key-less datahub on them first: `vike-backend datahub
/// --store DIR`.
///
/// ⚠ **The command line is read by `clap` (decision 0155), and that ENDS this file's exact-token
/// reader.** The hand-rolled `arg` it carried matched only `--flag value`, on purpose, so that
/// `--study=x` stayed ignored; the owner ruled `--flag=value` accepted on every program, so
/// `--study=x`, `--runs-root=x` and `--lightgbm=PATH` are read now (and `--store=DIR` is still
/// refused by name).
#[derive(clap::Parser, Debug)]
#[command(name = "vike-study")]
struct Args {
    /// The study's folder name under user_data/research/studies/rust/.
    #[arg(long, value_name = "NAME")]
    study: String,
    /// Its .toml recipe; an optional [learner] table carries the GBDT base params.
    #[arg(long, value_name = "FILE")]
    recipe: String,
    /// The window start: YYYY-MM-DD, YYYY-MM-DDTHH, or bare unix SECONDS.
    #[arg(long, value_name = "WHEN")]
    from: String,
    /// The window end, in the same grammar.
    #[arg(long, value_name = "WHEN")]
    to: String,
    /// The PINNED trainer. Omitted, the project's own <project>/bin/lightgbm/lightgbm; with neither,
    /// the study runs with NO learner and a fitting study refuses BY NAME rather than silently
    /// producing nothing.
    #[arg(long, value_name = "PATH")]
    lightgbm: Option<String>,
    /// Per-fit scratch root (default: <runs-root>/../scratch/vike-study, the project folder, never
    /// the system temp directory).
    #[arg(long, value_name = "DIR")]
    scratch: Option<String>,
    /// Where the run is minted (default: user_data/runs).
    #[arg(long, value_name = "DIR")]
    runs_root: Option<String>,
    /// REFUSED since 2026-09-25: every history read goes through a datahub (decision 0084).
    #[arg(long, num_args = 0..=1, hide = true)]
    store: Option<Option<String>>,
}

// ⚠ `boundary_ms` and `learner_params` MOVED to `vike_study_runner::study_dispatch` (then `vike_studio_core`'s) with stage 7: the
// WIRE runner (`crate::studio_run::run_study_local`) needs the same window grammar and the same
// `[learner]` reader. A copy would be two answers to "what does `--from 1785906000` mean".
use vike_study_runner::study_dispatch::{boundary_ms, learner_params};

/// `args` is the command line WITHOUT the program name (the multicall's tail, or the bin's
/// `argv[1..]`).
pub fn run(
    env: &std::collections::HashMap<String, String>,
    args: &[String],
) -> std::process::ExitCode {
    let version = format!("vike-study {}", env!("CARGO_PKG_VERSION"));
    let parsed: Args =
        match vike_utility::cli::Program::new("vike-study", &version).try_parse_from(args) {
            Ok(parsed) => parsed,
            Err(exit) => exit.emit(),
        };
    let Args { study, recipe: recipe_path, from, to, lightgbm, scratch, runs_root, store } = parsed;

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
    // `crates/vike-cli/src/cmd/report/journal/mod.rs` could not. A study ALWAYS reads history, so *no
    // flag* was not already spoken for by a third, store-free path; the tearsheet's was, which is
    // why that one needed its own `--datahub` selector.
    //
    // ⚠ **`--store DIR` is REFUSED since 2026-09-25, and this is the one reader where that BREAKS
    // command lines.** It was this tool's local opt-out and, as this comment used to say, "exactly
    // what every existing command line already passes". The owner closed the local READ door
    // anyway, knowing the cost, so those lines now fail loudly with the replacement named rather
    // than reading over the wire while their author believes a directory was read.
    // Every spelling: `store` is a declared flag with an optional value, so a bare trailing
    // `--store` and the inline `--store=DIR` reach this refusal as well as `--store DIR`.
    if store.is_some() {
        eprintln!("{}", store_flag_removed("vike-study"));
        return std::process::ExitCode::FAILURE;
    }
    let route = history_route(datahub_address_for_bin(env).as_deref());
    // ONE disclosure: the ADDRESS, never a directory — the resolved root is the SERVER's, and a path
    // printed here would be a guess about another box's filesystem.
    eprintln!("vike-study: history from {}", route.label());
    // ⚠ `Arc::from`, NOT `Arc::new`: the opener hands back a `Box<dyn HistoryStore + Send + Sync>` and
    // `Arc::new` would build an `Arc<Box<dyn …>>`, which is not an `Arc<dyn HistoryStore + Send + Sync>`.
    // The keys are read here, under THIS bin's warn-and-continue disposition (`routed_hub_keys`);
    // a malformed `VIKE_NO_LIVE_KEYS` refuses the start, as in every other root.
    let credentials = match vike_secrets::open_credential_store(env) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("vike-study: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let keys = routed_hub_keys(crate::binutil::backtest_device_key(&credentials), route.hub());
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::from(open_routed_history(&route, keys));

    let runs_root = runs_root.map_or_else(
        || {
            use vike_paths::state_path::{PROJECT_USER_DATA_DIR, RUNS_SUBDIR};
            Path::new(PROJECT_USER_DATA_DIR).join(RUNS_SUBDIR)
        },
        PathBuf::from,
    );
    let scratch = scratch.map_or_else(
        || vike_study_runner::study_dispatch::default_study_scratch(&runs_root),
        PathBuf::from,
    );

    // The flag, else the trainer the project installed (the daemon's lookup, under the same override).
    let settings_override = env.get("VIKE_SETTINGS_DIR").map(String::as_str);
    let cwd = std::env::current_dir().ok();
    let installed = || {
        cwd.as_deref()
            .and_then(|cwd| crate::backtesthub_server::installed_trainer(settings_override, cwd))
    };
    // A recipe whose `[learner]` says `kind = "neural"` fits networks through the project's installed
    // network trainer, run here outside the study's jail (decision 0292), as the daemon does.
    let neural = match vike_study_runner::study_dispatch::neural_spec(&params) {
        Ok(neural) => neural,
        Err(e) => {
            eprintln!("vike-study: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let neural_trainer = || {
        cwd.as_deref().and_then(|cwd| {
            crate::backtesthub_server::installed_neural_trainer(settings_override, cwd)
        })
    };
    // Absent is a WORKING state, not an error path: the study refuses by name, and a run made with
    // no learner is a different experiment rather than a failed one.
    let learner: Option<Arc<dyn StudyLearner>> = match neural {
        Some(spec) => match neural_trainer() {
            None => {
                eprintln!(
                    "vike-study: the recipe asks for a network and the project's bin/vike-ml-neural-training/ \
                     holds no trainer — running with NO learner; a fitting study will refuse"
                );
                None
            }
            Some(bin) => {
                eprintln!("vike-study: learner={} (a network)", bin.display());
                match NeuralLearner::new(&bin, &scratch, &spec) {
                    Ok(l) => Some(Arc::new(l)),
                    Err(e) => {
                        eprintln!("vike-study: {e}");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            }
        },
        None => match lightgbm.map(PathBuf::from).or_else(installed) {
            None => {
                eprintln!(
                    "vike-study: no --lightgbm and no trainer in the project's bin/lightgbm/ — running \
                 with NO learner; a fitting study will refuse"
                );
                None
            }
            Some(bin) => {
                let (base, from_recipe) = match learner_params(&params) {
                    Ok(read) => read,
                    Err(e) => {
                        eprintln!("vike-study: {e}");
                        return std::process::ExitCode::FAILURE;
                    }
                };
                eprintln!(
                    "vike-study: learner={} base params from {}",
                    bin.display(),
                    if from_recipe {
                        "the recipe's [learner] table"
                    } else {
                        "LightGBM's own defaults"
                    }
                );
                match GbdtLearner::new(&bin, &scratch, base) {
                    Ok(l) => Some(Arc::new(l)),
                    Err(e) => {
                        eprintln!("vike-study: {e}");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            }
        },
    };

    let plan = StudyRunPlan {
        name: study.clone(),
        dir: crate::study_sandbox::study_dir(&study),
        tier: StudyTier::Rust,
        params,
        // The operator's own spelling, deliberately not canonicalized — `RunConfig`'s rule.
        config: RunConfig { path: Some(recipe_path.clone()), name: None },
        store,
        window: TimestampRange::of(from_ms, to_ms),
        scratch,
        runs_root,
        produced_by: "vike-study".to_string(),
        // This binary cannot name its commit: `vike-backtesthub` carries no build-info dependency,
        // and adding one to a library crate to stamp a bin would be the wrong trade. `None` is the
        // documented value for a producer that cannot name one.
        git_sha: None,
        learner,
        // A study may ask for one backtest (`ctx.sim()`), and this host can run one: the harness is
        // in this crate's closure, as it is the daemon's (`crate::studio_run::run_study_local`).
        // A strategy FILE its profile names is read under the project's `strategies/` folder, relative
        // to the working directory as this tool's other defaults are (`--runs-root`'s).
        sim: Some(Arc::new(crate::harness_sim::HarnessSim::new(PathBuf::from(
            vike_paths::state_path::PROJECT_STRATEGIES_DIR,
        )))),
        // A study is a FILE (owner's rule, 2026-11-15): built by the strategy builder and run in the
        // jailed study process this program starts (`crate::study_sandbox`).
        file_runner: Some(crate::study_sandbox::runner()),
    };

    match run_study_plan(plan, &ClockLive) {
        Ok(run) => {
            println!("run {} -> {}", run.run_id, run.dir.display());
            for (k, v) in run.outcome.metrics() {
                println!("{k} = {}", vike_studio_core::study_run::StudyMetric::of(k, *v).text());
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-study: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
