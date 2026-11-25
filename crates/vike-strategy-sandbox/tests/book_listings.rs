//! **The book's listings, run by CI** ("Machine Learning Trading Systems in Rust", `content/book/`, an mdBook).
//! Every code listing in the book is a research study FILE (decision 0284), `content/book/listings/<name>/`:
//! `<name>.rs` with any sibling `.rs` modules, and at most one recipe, `<name>.toml` (absent = an empty
//! recipe). This binary builds each folder with the real builder, runs it in the real jail
//! (`vike_backtesthub::study_sandbox::run_in_child`, as `study_child` does), renders what it returned
//! (`book_listings/page.rs`: the metrics as a table, each `.tsv` artifact as a table, each
//! `chart_<name>.tsv` as an SVG chart) and compares that page with the committed include
//! `content/book/src/generated/<name>.md`, which a chapter shows under the code
//! (`{{#include generated/<name>.md}}`). A difference fails: the drift-gate family, as
//! `CLAUDE.md`'s "Generated artifacts and the public mirror" states it.
//!
//! - **The fixed data** a listing runs on, in a store of its own over the whole window
//!   (`book_listings/data.rs` is that contract): the synthetic test tape (`vike_data::demo`'s `seed`, venue
//!   `demo`), plus ONLY the files of `content/book/data/` its recipe DECLARES, `book_data = ["<file>", …]`
//!   (absent = none). An undeclared file in that folder is never read, so a leftover on a lane's checkout
//!   changes no page; a declared file that is missing is refused. A declared `.jsonl` file at the top of
//!   `content/book/data/` is bars, each row under its own `venue`, `symbol` and `interval` (a funding series
//!   is interval `funding`, its rate in the `funding` key); a declared `order_book/<name>.jsonl` is recorded
//!   book events, on the lane its name says (`_book_` or `_depth_`; that folder's README holds its own
//!   regenerate command). vike makes the bar sample, never a hand:
//!   `content/book/data/regenerate.sh` fetches it from Binance with the shipped `vike-cli data history fetch`
//!   through a datahub and gatewayhub of a SCRATCH project, and writes it with `vike-cli data history export
//!   --format jsonl`. The ONE command, from the dev box: `just book-sample <branch>` (it runs the script on
//!   a lane and unpacks the files here). A small slice only (a few MB in all), never a full history;
//!   Binance's is the one third-party market data allowed (the owner, 2026-11-16).
//! - **The backtest and the learner**: every listing's context offers the daemon's own backtest
//!   (`vike_backtesthub::harness_sim::HarnessSim`, a strategy FILE built and run in its own jailed child)
//!   and, where the pinned trainer is (`bin/lightgbm/lightgbm`, the convention every trainer suite
//!   resolves), `vike_ml_training::GbdtLearner` with the recipe's `[learner]` table read as `vike-study`
//!   reads it. On a box without the trainer a listing that FITS answers `StudyError::NoLearner`, and this run leaves
//!   it to the `lightgbm` job, which runs this binary with `-- --ignored` (`scripts/ci_lightgbm_suite.sh`):
//!   there the trainer is REQUIRED, every listing is compared, and each reason not to run (no binary, no
//!   jail, no book) is a failure, never a skip. A recipe whose `[learner]` says `kind = "neural"` is
//!   fitted by `vike_ml_training::NeuralLearner` over the network trainer, which only that job builds
//!   (`vike-ml-neural-training`, release, in a target directory of its own): a network repeats bit for
//!   bit on one box only (decision 0292), so its page is compared on the job's box and nowhere else.
//!   That job fires on an edit of this crate, of the network trainer, or of
//!   `content/book/` (`xtask/src/ci/tables/roster.rs`'s `LIGHTGBM_CRATES`, `xtask/src/ci/tables/readers.rs`'s
//!   `LIGHTGBM_JOB_INPUT_TREES`); decision 0138's amendment of 2026-11-16 states what it does not see.
//! - **The chart convention**: `chart_<name>.tsv`, the header `time\tvalue\tseries\tkind`, one row per
//!   point: time in epoch milliseconds (UTC), the value, the series' name, and its kind (`line`, `bars` or
//!   `points`, one kind per series). A chart whose first column is `x` instead (the header
//!   `x\tvalue\tseries\tkind`: a reliability diagram, a curve) is drawn on a NUMERIC axis, labelled with
//!   numbers rather than days; a time chart draws exactly as before it (`book_listings/page.rs`'s
//!   `COMMITTED_DRAWINGS` pins one of each). It is drawn as an SVG written HERE, in plain text: CI has no GPU to
//!   rasterize with (`docs/gui-contact-sheet.md`'s "GUI verification splits on RASTERIZATION"), and text
//!   compares byte for byte, with no new dependency.
//! - **Regenerating**: run this test in a lane. A page that differs is printed whole between
//!   `=== derived <path> ===` and `=== end ===`; copy it over the committed file and LOOK at it.
//! - It also fails a listing no page includes, and a generated page whose listing is gone.
//! - The public mirror withholds `content/`, so the book is read at RUN time and its absence is a loud
//!   SKIP, never a compile error.
//!
//! `harness = false`: this binary is also the jailed child (`<this> strategy-sandbox study <builds>
//! <sha>`), which needs a process with one thread and no descriptor above stderr.

use std::process::ExitCode;

const NAME: &str = "book_listings";
/// The build this binary states at both ends of its sandboxed children's pipes, as their parent and as
/// the child (the shipped binary hands in `vike_buildinfo::summary`).
const BUILD: &str = "book_listings";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("strategy-sandbox") {
        return vike_strategy_sandbox::run(
            &args[1..],
            vike_backtesthub::strategy_sandbox::serve_child,
            vike_backtesthub::study_sandbox::serve_child,
            BUILD,
        );
    }
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            println!("{NAME}: test");
        }
        return ExitCode::SUCCESS;
    }
    // `-- --ignored` is how `scripts/ci_lightgbm_suite.sh` runs a suite, on the box with the pinned trainer.
    let trainer_job = args.iter().any(|a| a == "--ignored");
    #[cfg(target_os = "linux")]
    return linux::main(trainer_job);
    #[cfg(not(target_os = "linux"))]
    {
        assert!(!trainer_job, "the lightgbm job runs the book's listings on Linux only");
        println!("SKIP: a research study file runs only on Linux");
        ExitCode::SUCCESS
    }
}

#[cfg(target_os = "linux")]
#[path = "book_listings/page.rs"]
mod page;

#[cfg(target_os = "linux")]
#[path = "book_listings/data.rs"]
mod data;

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;

    use vike_backtesthub::harness_sim::HarnessSim;
    use vike_data::TimestampRange;
    use vike_ml_training::{GbdtLearner, NeuralLearner};
    use vike_strategy_builder::render;
    use vike_study_runner::study_dispatch::{learner_params, neural_spec};
    use vike_user_research::{StudyContext, StudyError};

    use super::data::{self, BookData};
    use super::page;

    pub(crate) fn main(trainer_job: bool) -> ExitCode {
        // This binary plays the hub: the boot's step 0 first, so no descriptor it inherited reaches the
        // study process, which would refuse it.
        let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
        // In the lightgbm job every reason not to run is a failure: a green there means the listings ran.
        let skip = |why: String| {
            assert!(
                !trainer_job,
                "the lightgbm job must run the book's listings, and cannot: {why}"
            );
            println!("SKIP: {why}");
            ExitCode::SUCCESS
        };
        let book = workspace().join("content/book");
        let listings = book.join("listings");
        if !listings.is_dir() {
            return skip(format!(
                "{} is absent (the public mirror withholds `content/`): no listing to run",
                listings.display()
            ));
        }
        if let Err(why) = vike_sandbox::probe() {
            return skip(format!(
                "this box cannot apply the jail ({why}); the book's listings did not run"
            ));
        }
        // The convention every trainer suite resolves
        // (`crates/vike-ml-training/tests/gbdt_learner_smoke.rs`'s `binary_path`): no environment
        // variable says where the binary is.
        let trainer = workspace().join("bin/lightgbm/lightgbm");
        let trainer = trainer.exists().then_some(trainer);
        assert!(
            trainer.is_some() || !trainer_job,
            "the lightgbm job found no LightGBM binary at {}/bin/lightgbm/lightgbm",
            workspace().display()
        );
        page::self_check();
        data::self_check();
        use_the_strategy_builder();
        // The builder's own first step (`render::prefetch`, once at its start): fill the cargo home with
        // every crate the two templates reach, so the jailed `--offline` builds below find them on a
        // runner whose cargo home never built them (a lightgbm-job runner failed on exactly that). A
        // failure is said and not fatal, as the builder treats it: a warm cargo home still builds.
        let child_env = render::child_env(&std::env::vars().collect());
        if let Err(why) = render::prefetch(&workspace(), &cargo(), &child_env) {
            println!("prefetch: {why}");
        }

        let names = folders(&listings);
        assert!(!names.is_empty(), "{} holds no listing", listings.display());
        let generated = book.join("src/generated");
        let pages = pages(&book.join("src"));
        let builds = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("book-listings-builds");
        let me = std::env::current_exe().expect("this binary's path");
        let (mut stale, mut fitting, mut compared) = (Vec::new(), Vec::new(), 0);
        let mut network_trainer: Option<PathBuf> = None;
        for name in &names {
            let include = format!("generated/{name}.md");
            assert!(
                pages.iter().any(|text| text.contains(&include)),
                "listing {name}: no page under content/book/src includes `{include}`"
            );
            let folder = listings.join(name);
            let artifact = build(name, &folder, &builds);
            let params = recipe(name, &folder);
            let sample = BookData::read(&book.join("data"), &params)
                .unwrap_or_else(|why| panic!("listing {name}: {why}"));
            for series in sample.describe() {
                println!("data: {name}: {series}");
            }
            // A recipe whose `[learner]` names a network is fitted by the network trainer, which only the
            // lightgbm job builds: a network is bit-identical per box only (decision 0292), and that job
            // runs on one.
            let network = neural_spec(&params).unwrap_or_else(|e| panic!("listing {name}: {e}"));
            let binary = match network {
                Some(_) if trainer_job => {
                    Some(network_trainer.get_or_insert_with(build_the_network_trainer).clone())
                }
                Some(_) => None,
                None => trainer.clone(),
            };
            let ctx = context(&sample, name, &params, network.as_deref(), binary.as_deref());
            let outcome = vike_backtesthub::study_sandbox::run_in_child(
                &me, &builds, &artifact, &ctx, &params,
            );
            if binary.is_none() && matches!(outcome, Err(StudyError::NoLearner(_))) {
                // Never compared HERE: its page is the lightgbm job's (`scripts/ci_lightgbm_suite.sh`),
                // which this crate's edits and every `content/book/` change fire.
                println!(
                    "fits a model: {name}, compared by the lightgbm job, where the trainer is"
                );
                fitting.push(name.clone());
                continue;
            }
            compared += 1;
            let derived = page::render(name, &outcome)
                .unwrap_or_else(|why| panic!("listing {name}: its result cannot be drawn: {why}"));
            let path = generated.join(format!("{name}.md"));
            if std::fs::read_to_string(&path).ok().as_deref() == Some(derived.as_str()) {
                println!("ok: {name}");
            } else {
                println!("=== derived {} ===\n{derived}=== end ===", path.display());
                stale.push(name.clone());
            }
        }
        let orphans: Vec<String> = std::fs::read_dir(&generated)
            .map(|dir| {
                dir.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|file| !names.iter().any(|name| *file == format!("{name}.md")))
            .collect();
        assert!(
            orphans.is_empty(),
            "{} holds pages no listing derives (delete them): {orphans:?}",
            generated.display()
        );
        assert!(
            stale.is_empty(),
            "the committed result of {stale:?} is not what the listing returns now: copy each page \
             printed above over its file and look at it"
        );
        println!(
            "{}: ok, {compared} listing(s) compared, {} left to the lightgbm job: {fitting:?}",
            super::NAME,
            fitting.len()
        );
        // The libtest summary `scripts/ci_lightgbm_suite.sh` counts: a job that compared none is red.
        println!(
            "test result: ok. {compared} passed; 0 failed; {} ignored; 0 measured; 0 filtered out",
            fitting.len()
        );
        ExitCode::SUCCESS
    }

    /// The builder a listing's backtest (`ctx.sim()`, the daemon's own `HarnessSim`) builds a strategy
    /// FILE with, into the folder its sandboxed child finds it in, as `study_child` sets it.
    fn use_the_strategy_builder() {
        let strategy_builds = vike_backtesthub::studio_run::strategy_files_dir();
        vike_backtesthub::strategy_sandbox::use_build(super::BUILD.to_string());
        vike_backtesthub::strategy_sandbox::use_builder(Box::new(move |path, text, _sdk| {
            let artifact = render::build_strategy_file(
                text,
                path,
                &strategy_builds,
                &workspace(),
                &cargo(),
                &render::child_env(&std::env::vars().collect()),
                render::Profile::of_this_binary(),
                &render::CargoHomePin::disabled(),
            )
            .map_err(|e| e.to_string())?;
            let sha = artifact.file_stem().map(|s| s.to_string_lossy().into_owned());
            let sha256 = sha.ok_or_else(|| format!("{} names no sha", artifact.display()))?;
            // The builder's answer: the artifact's path under the builds directory (decision 0296).
            let artifact = relative_artifact(&artifact, &strategy_builds);
            Ok(vike_backtesthub::strategy_sandbox::BuiltFile { sha256, artifact })
        }));
    }

    /// The network trainer of this checkout (`vike-ml-neural-training`, the process a study host drives
    /// outside the jail, decision 0292), built release in a target directory of its own: the outer
    /// `cargo test` holds the workspace's, and a debug network trains too slowly for a listing.
    fn build_the_network_trainer() -> PathBuf {
        let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("book-network-trainer");
        let binary = "vike-ml-neural-training";
        let status = std::process::Command::new(cargo())
            .current_dir(workspace())
            .args(["build", "--release", "-p", binary, "--bin", binary, "--target-dir"])
            .arg(&target)
            .status()
            .unwrap_or_else(|e| panic!("cargo build of the network trainer: {e}"));
        assert!(status.success(), "the network trainer does not build: {status}");
        target.join("release").join(binary)
    }

    /// The `cargo` a nested build runs.
    fn cargo() -> String {
        std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
    }

    /// The checkout this test builds against.
    fn workspace() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The names of the folders directly under `dir`, sorted.
    fn folders(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The text of every chapter page under `src` (the generated pages are no chapter).
    fn pages(src: &Path) -> Vec<String> {
        std::fs::read_dir(src)
            .unwrap_or_else(|e| panic!("{}: {e}", src.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .map(|p| std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
            .collect()
    }

    /// The listing's recipe, `<name>.toml`, or the empty one.
    fn recipe(name: &str, folder: &Path) -> toml::Value {
        let path = folder.join(format!("{name}.toml"));
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: not a recipe: {e}", path.display())),
            Err(_) => toml::Value::Table(toml::Table::new()),
        }
    }

    /// A listing's context: the fixed data (`book_listings/data.rs`) in a store of its own over the
    /// whole window, the daemon's backtest, and the learner where its trainer is: the network trainer
    /// at `trainer` for a recipe whose `[learner]` names a `network` (its spec), else LightGBM there with
    /// the base parameters of the recipe's `[learner]` table, both as `vike-study` reads them.
    fn context(
        sample: &BookData,
        name: &str,
        params: &toml::Value,
        network: Option<&str>,
        trainer: Option<&Path>,
    ) -> StudyContext {
        let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let ctx =
            StudyContext::new(sample.store(), TimestampRange::all(), tmp.join("book-scratch"))
                .with_sim(Arc::new(HarnessSim::new(workspace().join("strategies"))));
        let Some(binary) = trainer else { return ctx };
        if let Some(spec) = network {
            let learner = NeuralLearner::new(binary, &tmp.join("book-learner").join(name), spec)
                .unwrap_or_else(|e| panic!("listing {name}: no network learner: {e}"));
            return ctx.with_learner(Arc::new(learner));
        }
        let learner = GbdtLearner::new(
            binary,
            &tmp.join("book-learner").join(name),
            learner_params(params).unwrap_or_else(|e| panic!("listing {name}: {e}")).0,
        )
        .unwrap_or_else(|e| {
            panic!("listing {name}: no LightGBM learner over {}: {e}", binary.display())
        });
        ctx.with_learner(Arc::new(learner))
    }

    /// The listing folder built with the production builder, this binary's profile; its sha.
    fn build(name: &str, folder: &Path, builds: &Path) -> String {
        let mut files: Vec<(String, String)> = std::fs::read_dir(folder)
            .unwrap_or_else(|e| panic!("{}: {e}", folder.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .map(|p| {
                let file = p.file_name().unwrap().to_string_lossy().into_owned();
                (file, std::fs::read_to_string(&p).unwrap())
            })
            .collect();
        files.sort();
        let (artifact, _) = render::build_study_file(
            name,
            &files,
            builds,
            &workspace(),
            &cargo(),
            &render::child_env(&std::env::vars().collect()),
            render::Profile::of_this_binary(),
            &render::CargoHomePin::disabled(),
        )
        .unwrap_or_else(|e| panic!("listing {name} does not build:\n{e}"));
        relative_artifact(&artifact, builds)
    }

    /// `artifact` relative to `builds`, `/`-separated: the builder's answer a hub hands its sandboxed
    /// child (`sdk-N/<folder>/<sha256>.so`, decision 0296).
    fn relative_artifact(artifact: &Path, builds: &Path) -> String {
        let relative = artifact.strip_prefix(builds).expect("the artifact lies under its builds");
        relative.iter().map(|part| part.to_string_lossy()).collect::<Vec<_>>().join("/")
    }
}
