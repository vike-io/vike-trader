//! The study verb's production route, end to end (owner's rule, 2026-11-15: a research study is a FILE,
//! built, loaded and sandboxed exactly like a strategy file): the real builder builds each committed
//! fixture study's folder (`crates/vike-user-research/tests/fixture_user_data/research/studies/rust/`,
//! `poly_mm` included) into an artifact, a real child process applies the real jail, the real loader opens
//! it, and the backtesthub's own halves run it (`vike_backtesthub::study_sandbox::run_in_child` here,
//! `serve_child` in the child). Each study's outcome must equal the SAME study compiled into this binary
//! and run directly, bit for bit: every metric's bits, every artifact, or the same refusal in the same
//! words. Both runs' one backtest is the same `HarnessSim` in this process, and `poly_mm`'s cells name
//! Rust strategy FILES (`strategies/market_making/spread_maker.rs`, `strategies/trend/trailing_scalper.rs`):
//! each is built by the real builder and run in its own sandboxed `strategy-sandbox backtest` child, the
//! route every backtest takes, and every cell must RUN (no failure).
//!
//! `harness = false`: this binary is ALSO both children. Started as `<this> strategy-sandbox study|backtest
//! <builds> <sha>` it runs the crate's real verb (`vike_strategy_sandbox::run`), jail and all, which needs
//! a process with one thread and no descriptor above stderr — a libtest harness has neither.

use std::process::ExitCode;

const NAME: &str = "study_child";
/// The build this binary states at both ends of its sandboxed children's pipes, as their parent and as
/// the child (the shipped binary hands in `vike_buildinfo::summary`).
const BUILD: &str = "study_child";

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
    #[cfg(target_os = "linux")]
    return linux::main();
    #[cfg(not(target_os = "linux"))]
    {
        println!("SKIP: a Rust study file runs only on Linux");
        ExitCode::SUCCESS
    }
}

// The fixture studies, compiled into THIS test binary for the direct run each sandboxed run is compared
// with (a test binary; no shipped one compiles a study).
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/alpha_pool/alpha_pool.rs"]
pub mod user_alpha_pool;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/book_export/book_export.rs"]
pub mod user_book_export;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/covered_call/covered_call.rs"]
pub mod user_covered_call;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/deep_hedging/deep_hedging.rs"]
pub mod user_deep_hedging;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/event_calibration/event_calibration.rs"]
pub mod user_event_calibration;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/fixture_bars/fixture_bars.rs"]
pub mod user_fixture_bars;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/fixture_fit/fixture_fit.rs"]
pub mod user_fixture_fit;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/forward_report/forward_report.rs"]
pub mod user_forward_report;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/funding_carry/funding_carry.rs"]
pub mod user_funding_carry;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/funding_spike_effect/funding_spike_effect.rs"]
pub mod user_funding_spike_effect;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/impact_fit/impact_fit.rs"]
pub mod user_impact_fit;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/instrument_graph/instrument_graph.rs"]
pub mod user_instrument_graph;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/microstructure/microstructure.rs"]
pub mod user_microstructure;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/option_pricing_network/option_pricing_network.rs"]
pub mod user_option_pricing_network;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/pairs_reversion/pairs_reversion.rs"]
pub mod user_pairs_reversion;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/poly_mm/poly_mm.rs"]
#[expect(
    dead_code,
    unused_imports,
    reason = "a test target is built under `cfg(test)`, so `poly_mm`'s own `#[cfg(test)] mod tests` \
              compiles here; its tests run in `vike-user-research`'s pipeline binary, not in this one"
)]
pub mod user_poly_mm;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/rl_episode/rl_episode.rs"]
pub mod user_rl_episode;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/sharpe_lstm/sharpe_lstm.rs"]
pub mod user_sharpe_lstm;
#[cfg(target_os = "linux")]
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/text_base_rate/text_base_rate.rs"]
pub mod user_text_base_rate;

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;

    use vike_backtesthub::harness_sim::HarnessSim;
    use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
    use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickTrade};
    use vike_ml_prediction::FitImportance;
    use vike_ml_training::seam::test_support::ScriptedLearner;
    use vike_strategy_builder::render;
    use vike_user_research::{StudyContext, StudyError, StudyFn, StudyOutcome};

    const TIER: &str = "../vike-user-research/tests/fixture_user_data/research/studies/rust";

    /// One run compared: the study's name, its entry, its artifact (its path under the builds), its
    /// context and its recipe.
    type Case<'a> = (&'a str, StudyFn, &'a str, &'a dyn Fn() -> StudyContext, String);

    pub(crate) fn main() -> ExitCode {
        // This binary plays the hub: the boot's step 0 first, so no descriptor it inherited (CI's runner
        // hands it two pipes) reaches the study process, which would refuse it.
        let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
        if let Err(why) = vike_sandbox::probe() {
            println!("SKIP: this box cannot apply the jail ({why}); nothing to prove here");
            return ExitCode::SUCCESS;
        }
        let builds = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("study-child-builds");
        // A strategy file a study's backtest names is built as the composition root has it built: by
        // the builder, into the folder the backtest's sandboxed child finds it in.
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
        let bars = build("fixture_bars", &builds);
        let fit = build("fixture_fit", &builds);
        let poly = build("poly_mm", &builds);
        let micro = build("microstructure", &builds);
        let book = build("book_export", &builds);
        let funding = build("funding_spike_effect", &builds);
        let sharpe = build("sharpe_lstm", &builds);
        let calibration = build("event_calibration", &builds);
        let rag = build("text_base_rate", &builds);
        let covered = build("covered_call", &builds);
        let impact = build("impact_fit", &builds);
        // Names `vike-simulator` (decision 0291), and runs its episode on a thread inside the jail.
        let rl = build("rl_episode", &builds);
        let pool = build("alpha_pool", &builds);
        let forward = build("forward_report", &builds);
        let graph = build("instrument_graph", &builds);
        let pairs = build("pairs_reversion", &builds);
        let carry = build("funding_carry", &builds);

        let pairs_context = || {
            let (store, window) = super::user_pairs_reversion::fixture_store::store();
            StudyContext::new(store, window, scratch())
        };
        let carry_context = || {
            let (store, window) = super::user_funding_carry::fixture_store::store();
            StudyContext::new(store, window, scratch())
        };
        let pairs_recipe = |extra: &str| {
            let list = super::user_pairs_reversion::fixture_store::SYMBOLS
                .map(|s| format!("\"{s}\""))
                .join(", ");
            format!(
                "venue = \"fixture\"\ninterval = \"1h\"\nsymbols = [{list}]\n\
                 significance = \"one_percent\"\n{extra}"
            )
        };
        let carry_recipe = {
            use super::user_funding_carry::fixture_store::{PERPETUAL, SPOT, VENUE};
            format!(
                "venue = \"{VENUE}\"\nspot_symbol = \"{SPOT}\"\nperpetual_symbol = \"{PERPETUAL}\"\n\
                 interval = \"1h\"\n"
            )
        };
        // Name `vike-options` (the study surface, decision 0284) and, for the hedge, `vike-simulator`.
        let pricing = build("option_pricing_network", &builds);
        let hedging = build("deep_hedging", &builds);

        let plain = || {
            let (store, window) = seeded_store();
            StudyContext::new(store, window, scratch())
        };
        let tape = || {
            let (store, window) = tape_store();
            StudyContext::new(store, window, scratch())
        };
        let learning = || plain().with_learner(Arc::new(importance_learner()));
        let funding_plain = || {
            let (store, window) = funding_store();
            StudyContext::new(store, window, scratch())
        };
        let funding_recipe =
            format!("venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"1h\"");
        let poly_plain = || StudyContext::new(poly_store(), TimestampRange::all(), scratch());
        let (event_store, event_recipe) =
            super::user_event_calibration::synthetic::store_and_recipe(60);
        let events = || StudyContext::new(event_store.clone(), TimestampRange::all(), scratch());
        let poly_sim =
            || poly_plain().with_sim(Arc::new(HarnessSim::new(workspace().join("strategies"))));
        // Three saved runs the parent reads for the study (book W21), and the bars they are measured on.
        let pool_runs = || super::user_alpha_pool::fixture::context(scratch());
        // A saved backtest's ledger and the account's fills (book chapter 22), both read through the parent.
        let forward_runs = || super::user_forward_report::fixture::context(scratch());
        let poly_recipe = format!(
            "lane = \"both\"\nstrategy = \"both\"\n\
             [[universe]]\nfamily = \"btc-5m\"\ntoken = \"{TOKEN}\"\nend_date_ms = {END_MS}\n"
        );

        let walking = || {
            let (store, window) = walk_store();
            StudyContext::new(store, window, scratch()).with_learner(Arc::new(importance_learner()))
        };
        let sharpe_recipe = format!(
            "venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"1m\"\n[learner]\nsteps = 8\n"
        );

        let books = || StudyContext::new(book_store(), TimestampRange::all(), scratch());
        let book_recipe =
            "venue = \"binance\"\nsymbol = \"BTCUSDT\"\nlevels = 2\nhorizon_events = 1\n";

        let texts = || {
            let window = super::user_text_base_rate::fixture_store::window();
            StudyContext::new(super::user_text_base_rate::fixture_store::store(), window, scratch())
        };

        // The covered call: its one backtest is a real one (`HarnessSim`), whose profile trades the chosen
        // calls as `[data.options]` legs through the shipped file `options/covered_call.rs`, built by the
        // builder and run in its own sandboxed child, the chain read back through the study's store.
        let covered_sim = || {
            StudyContext::new(option_store(), option_window(), scratch())
                .with_sim(Arc::new(HarnessSim::new(workspace().join("strategies"))))
        };
        // `assumed_half_spread`: the measured median the study's own doc states (its chain carries no quote).
        let covered_recipe =
            "venue = \"deribit\"\nchain = \"BTC\"\nunderlying_venue = \"deribit\"\n\
                              underlying = \"BTCUSDC.P\"\ninterval = \"1h\"\nassumed_half_spread = 0.053\n"
                .to_string();

        // `impact_fit` over a store with no journal: the `execution_fills` read crosses the jail and
        // the study answers its empty fit the same both ways.
        let impact_recipe = "venue = \"binance\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\nwindow = 3\nexec_time = 1";
        // The catalog the parent reads and hands over: the jailed study opens no file for it.
        let catalog = || {
            let window = super::user_instrument_graph::fixture_store::window();
            StudyContext::new(
                super::user_instrument_graph::fixture_store::store(),
                window,
                scratch(),
            )
        };
        let options = |hours: i64| {
            let (store, window) = options_store(hours);
            StudyContext::new(store, window, scratch()).with_learner(Arc::new(importance_learner()))
        };
        let options_short = || options(9);
        let options_long = || options(40);
        let pricing_recipe = "venue = \"deribit\"\nchain = \"U\"\nunderlying_venue = \"deribit\"\n\
                              underlying = \"U-PERP\"\ninterval = \"1h\"\ntrain_fraction = 0.5\n";
        let hedging_recipe = "venue = \"deribit\"\nsymbol = \"U-PERP\"\ninterval = \"1h\"\n\
                              volatility = 0.5\ntrain_fraction = 0.5\ncommission = 0.1\n[learner]\n\
                              kind = \"neural\"\npath_steps = 5\nprice_column = 0\nfee_rate = 0.001\n";
        let cases: [Case<'_>; 31] = [
            ("book_export", super::user_book_export::run, &book, &books, book_recipe.into()),
            (
                "book_export",
                super::user_book_export::run,
                &book,
                &books,
                format!("{book_recipe}interval_ms = 1\n"),
            ),
            (
                "microstructure",
                super::user_microstructure::run,
                &micro,
                &tape,
                "bucket_volume = 2\nwindow = 2".into(),
            ),
            (
                "microstructure",
                super::user_microstructure::run,
                &micro,
                &tape,
                "bucket_volume = 2\ndollar_bucket = 3\n\
                 contract_value = { kind = \"inverse\", per_unit = 1.0 }"
                    .into(),
            ),
            ("microstructure", super::user_microstructure::run, &micro, &plain, String::new()),
            (
                "option_pricing_network",
                super::user_option_pricing_network::run,
                &pricing,
                &options_short,
                pricing_recipe.into(),
            ),
            (
                "deep_hedging",
                super::user_deep_hedging::run,
                &hedging,
                &options_long,
                hedging_recipe.into(),
            ),
            ("rl_episode", super::user_rl_episode::run, &rl, &plain, "target = 1".into()),
            (
                "pairs_reversion",
                super::user_pairs_reversion::run,
                &pairs,
                &pairs_context,
                pairs_recipe(""),
            ),
            (
                "pairs_reversion",
                super::user_pairs_reversion::run,
                &pairs,
                &pairs_context,
                pairs_recipe("clustering = \"k_means\"\n"),
            ),
            (
                "pairs_reversion",
                super::user_pairs_reversion::run,
                &pairs,
                &pairs_context,
                "venue = \"fixture\"\ninterval = \"1h\"".into(),
            ),
            (
                "funding_carry",
                super::user_funding_carry::run,
                &carry,
                &carry_context,
                carry_recipe.clone(),
            ),
            (
                "funding_carry",
                super::user_funding_carry::run,
                &carry,
                &carry_context,
                format!("{carry_recipe}entry = 0.0002\nexit = 0.0001\n"),
            ),
            ("fixture_bars", super::user_fixture_bars::run, &bars, &plain, "period = 2".into()),
            ("fixture_bars", super::user_fixture_bars::run, &bars, &plain, "period = 4".into()),
            (
                "fixture_bars",
                super::user_fixture_bars::run,
                &bars,
                &plain,
                "symbol = \"ETHUSDT\"".into(),
            ),
            ("fixture_fit", super::user_fixture_fit::run, &fit, &learning, "seed = 7".into()),
            ("fixture_fit", super::user_fixture_fit::run, &fit, &plain, String::new()),
            // Double machine learning with Gaussian-process nuisances, fitted inside the jail.
            (
                "funding_spike_effect",
                super::user_funding_spike_effect::run,
                &funding,
                &funding_plain,
                funding_recipe,
            ),
            (
                "event_calibration",
                super::user_event_calibration::run,
                &calibration,
                &events,
                event_recipe,
            ),
            ("impact_fit", super::user_impact_fit::run, &impact, &plain, impact_recipe.into()),
            ("poly_mm", super::user_poly_mm::run, &poly, &poly_sim, poly_recipe.clone()),
            ("poly_mm", super::user_poly_mm::run, &poly, &poly_plain, poly_recipe),
            ("sharpe_lstm", super::user_sharpe_lstm::run, &sharpe, &walking, sharpe_recipe),
            (
                "text_base_rate",
                super::user_text_base_rate::run,
                &rag,
                &texts,
                super::user_text_base_rate::fixture_store::recipe(),
            ),
            ("covered_call", super::user_covered_call::run, &covered, &covered_sim, covered_recipe),
            (
                "alpha_pool",
                super::user_alpha_pool::run,
                &pool,
                &pool_runs,
                super::user_alpha_pool::fixture::RECIPE.into(),
            ),
            (
                "forward_report",
                super::user_forward_report::run,
                &forward,
                &forward_runs,
                super::user_forward_report::fixture::RECIPE.into(),
            ),
            // The forward run with every opt-in key: its mount, the live forecasts read point in time
            // (`Call::RunForward`, `Call::ForecastsAsOf`) and the drift alarms, the same in the jail.
            (
                "forward_report",
                super::user_forward_report::run,
                &forward,
                &forward_runs,
                super::user_forward_report::fixture::EVERY_KEY_RECIPE.into(),
            ),
            (
                "instrument_graph",
                super::user_instrument_graph::run,
                &graph,
                &catalog,
                super::user_instrument_graph::fixture_store::recipe(),
            ),
            // A store that serves no kept list: the verb's refusal, the same in the jail.
            (
                "instrument_graph",
                super::user_instrument_graph::run,
                &graph,
                &plain,
                super::user_instrument_graph::fixture_store::recipe(),
            ),
        ];
        let me = std::env::current_exe().expect("this binary's path");
        let mut cells_run = Vec::new();
        let mut graphs_run = 0;
        for (name, entry, artifact, ctx, recipe) in cases {
            let params: toml::Value = toml::from_str(&recipe).expect("a recipe");
            let direct = entry(&ctx(), &params);
            let sandboxed = vike_backtesthub::study_sandbox::run_in_child(
                &me,
                &builds,
                artifact,
                &ctx(),
                &params,
            );
            println!("ok: {name} [{}]: {}", recipe.replace('\n', " "), same(&direct, &sandboxed));
            // The tape case must MEASURE, not refuse the same way twice.
            if name == "microstructure" && !recipe.is_empty() {
                assert!(direct.is_ok(), "microstructure must run over the tape: {direct:?}");
            }
            if let (true, Ok(outcome)) = (name == "poly_mm", &sandboxed) {
                let metric = |m: &str| outcome.metric_value(m).unwrap_or(f64::NAN);
                let (runs, failed) = (metric("runs"), metric("failed"));
                println!("poly_mm: {runs} cell(s) ran, {failed} failed");
                for (file, body) in outcome.artifacts() {
                    println!("-- {file}\n{body}");
                }
                cells_run.push((runs, failed));
            }
            if name == "text_base_rate" {
                // The retrieval ran in the jail, not only refused the same way.
                let rate = sandboxed.as_ref().ok().and_then(|o| o.metric_value("side_hit_rate"));
                assert_eq!(rate, Some(1.0), "text_base_rate in the jail: {sandboxed:?}");
            }
            if name == "covered_call" {
                covered_call_ran_the_engine(&sandboxed);
            }
            if let (true, Ok(outcome)) = (name == "instrument_graph", &sandboxed) {
                // The graph and the lead-lag network were built in the jail, from the parent's catalog.
                let edges = outcome.metric_value("edges");
                assert_eq!(edges, Some(5.0), "instrument_graph in the jail: {sandboxed:?}");
                graphs_run += 1;
            }
        }
        assert_eq!(graphs_run, 1, "instrument_graph ran on its catalog once");
        // `poly_mm`'s cells backtest Rust strategy FILES: every one ran, none failed.
        assert_eq!(cells_run.len(), 1, "poly_mm ran with a backtest once: {cells_run:?}");
        let (runs, failed) = cells_run[0];
        assert!(
            runs > 0.0 && failed == 0.0,
            "every poly_mm cell must run: {runs} ran, {failed} failed"
        );

        // An artifact that was never built: the loader's refusal, as the study's error, and no run.
        let missing = vike_backtesthub::study_sandbox::run_in_child(
            &me,
            &builds,
            &format!("sdk-dev/00000000/{}.so", "0".repeat(64)),
            &plain(),
            &toml::from_str("").unwrap(),
        );
        match missing {
            Err(StudyError::Study(why)) if why.contains("could not be loaded") => {
                println!("ok: an unbuilt study is refused: {why}");
            }
            other => panic!("an unbuilt study must be the loader's refusal, got {other:?}"),
        }
        println!("{}: ok", super::NAME);
        ExitCode::SUCCESS
    }

    /// The checkout this test builds against.
    fn workspace() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The `cargo` a nested build runs.
    fn cargo() -> String {
        std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
    }

    /// The study folder `name` built with the production builder, this binary's profile; its sha.
    fn build(name: &str, builds: &Path) -> String {
        let workspace = workspace();
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(TIER).join(name);
        let mut files: Vec<(String, String)> = std::fs::read_dir(&folder)
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
        let cargo = cargo();
        let env = render::child_env(&std::env::vars().collect());
        let (artifact, sha) = render::build_study_file(
            name,
            &files,
            builds,
            &workspace,
            &cargo,
            &env,
            render::Profile::of_this_binary(),
            &render::CargoHomePin::disabled(),
        )
        .unwrap_or_else(|e| panic!("the study template must build {name}:\n{e}"));
        println!("built {name}: {} (sha256 {sha})", artifact.display());
        relative_artifact(&artifact, builds)
    }

    /// `artifact` relative to `builds`, `/`-separated: the builder's answer a hub hands its sandboxed
    /// child (`sdk-N/<folder>/<sha256>.so`, decision 0296).
    fn relative_artifact(artifact: &Path, builds: &Path) -> String {
        let relative = artifact.strip_prefix(builds).expect("the artifact lies under its builds");
        relative.iter().map(|part| part.to_string_lossy()).collect::<Vec<_>>().join("/")
    }

    /// The direct run and the sandboxed one, compared: the same outcome (every metric's BITS, every
    /// artifact) or the same refusal (variant and words). What they were, in a line.
    fn same(
        direct: &Result<StudyOutcome, StudyError>,
        sandboxed: &Result<StudyOutcome, StudyError>,
    ) -> String {
        match (direct, sandboxed) {
            (Ok(a), Ok(b)) => {
                let bits = |o: &StudyOutcome| -> Vec<(String, u64)> {
                    o.metrics().iter().map(|(n, v)| (n.clone(), v.to_bits())).collect()
                };
                assert_eq!(bits(a), bits(b), "the metrics, bit for bit");
                assert_eq!(a.artifacts(), b.artifacts(), "the artifacts, byte for byte");
                format!(
                    "{} metric(s), {} artifact(s), identical",
                    a.metrics().len(),
                    a.artifacts().len()
                )
            }
            (Err(a), Err(b)) => {
                assert_eq!(format!("{a:?}"), format!("{b:?}"), "the same refusal");
                format!("the same refusal: {a}")
            }
            _ => panic!("the two runs disagree: directly {direct:?}, sandboxed {sandboxed:?}"),
        }
    }

    const VENUE: &str = "binance";
    const SYMBOL: &str = "BTCUSDT";
    const TOKEN: &str = "1234567890";
    const END_MS: i64 = 1_790_000_300_000;

    fn seeded_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
        let store = HistoryStoreMemory::new();
        let bars: Vec<Bar> = (0..5)
            .map(|i| {
                vike_marketdata::test_support::flat_bar_unit_volume_on(
                    SYMBOL,
                    60_000 * i,
                    100.0 + i as f64,
                )
            })
            .collect();
        store.append_bars(VENUE, SYMBOL, "1m", &bars, Some("fixture")).unwrap();
        (Arc::new(store), TimestampRange::of(0, 60_000 * 4))
    }

    /// One trade tape for `VENUE`/`SYMBOL` (the shared `HistoryStoreMemory` keeps no ticks); every
    /// other read is empty.
    struct TapeStore(Vec<TickTrade>);

    impl HistoryStore for TapeStore {
        fn scan_trades(
            &self,
            venue: &str,
            symbol: &str,
            range: TimestampRange,
        ) -> Result<Vec<TickTrade>, HistoryStoreError> {
            let inside =
                |ts: i64| range.start.is_none_or(|s| ts >= s) && range.end.is_none_or(|e| ts <= e);
            let ours = venue == VENUE && symbol == SYMBOL;
            Ok(self.0.iter().filter(|t| ours && inside(t.timestamp)).cloned().collect())
        }
        vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_book_updates,
            scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
    }

    /// The trade tape `crates/vike-user-research/tests/pipeline.rs`'s `tape_store` works by hand.
    fn tape_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
        use vike_marketdata::test_support::trade;
        let trades = vec![
            trade(1_000, 100.0, 2.0, false),
            trade(2_000, 101.0, 1.0, true),
            trade(3_000, 102.0, 1.0, false),
            trade(4_000, 101.0, 2.0, true),
            trade(5_000, 103.0, 2.0, false),
        ];
        (Arc::new(TapeStore(trades)), TimestampRange::of(0, 10_000))
    }

    /// `crates/vike-user-research/tests/pipeline.rs`'s `funding_store`, the same thirty days: hourly
    /// bars through three volatility regimes and an eight-hourly funding series, a third of it spikes.
    fn funding_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
        const HOUR: i64 = 3_600_000;
        let unit = |i: i64, step: f64| (i as f64 * step).fract();
        let store = HistoryStoreMemory::new();
        let (mut close, mut prices, mut funding) = (100.0, Vec::new(), Vec::new());
        for h in 0..720i64 {
            let volatility = 0.002 * (1 + (h / 48) % 3) as f64;
            let open = close;
            close = open * (1.0 + volatility * (2.0 * unit(h, 0.618_033_988_749_895) - 1.0));
            let volume = 1_000.0 * volatility * (0.5 + unit(h, 0.414_213_562_373_095));
            prices.push(Bar::ohlcv(
                h * HOUR,
                open,
                open.max(close),
                open.min(close),
                close,
                volume,
            ));
            if h % 8 == 0 {
                let rate = if unit(h / 8, 0.754_877_666_246_693) < 0.35 { 0.001 } else { 0.0001 };
                let at = Bar::ohlcv(h * HOUR, 0.0, 0.0, 0.0, 0.0, 0.0);
                funding.push(Bar { funding: Some(rate), ..at });
            }
        }
        store.append_bars(VENUE, SYMBOL, "1h", &prices, Some("fixture-prices")).unwrap();
        store.append_bars(VENUE, SYMBOL, "funding", &funding, Some("fixture-funding")).unwrap();
        (Arc::new(store), TimestampRange::of(0, 720 * HOUR))
    }

    /// 300 one-minute bars whose closes take seeded steps of up to 1% either way (the pipeline test's
    /// walk), and the window over them: enough rows for `sharpe_lstm` to fit and judge.
    fn walk_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
        let store = HistoryStoreMemory::new();
        let mut close = 100.0;
        let bars: Vec<Bar> = (0..300_i64)
            .map(|i| {
                close *= 1.0 + 0.01 * (((i * 7_919) % 2_001) as f64 / 1_000.0 - 1.0);
                vike_marketdata::test_support::flat_bar_unit_volume_on(SYMBOL, 60_000 * i, close)
            })
            .collect();
        store.append_bars(VENUE, SYMBOL, "1m", &bars, Some("fixture")).unwrap();
        (Arc::new(store), TimestampRange::of(0, 60_000 * 299))
    }

    /// The options studies' store, as `vike-user-research`'s pipeline seeds it: `hours` hourly bars of a
    /// future zigzagging around 100, and each later hour a chain snapshot (calls and puts at 90, 100 and
    /// 110, a week out) marked by Black–Scholes on a smile.
    fn options_store(hours: i64) -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
        use vike_marketdata::OptionKind;
        const HOUR_MS: i64 = 3_600_000;
        let store = HistoryStoreMemory::new();
        let close = |h: i64| 100.0 + ((h * 7) % 11) as f64 - 5.0;
        let bars: Vec<Bar> = (0..hours)
            .map(|h| {
                vike_marketdata::test_support::flat_bar_unit_volume_on(
                    "U-PERP",
                    h * HOUR_MS,
                    close(h),
                )
            })
            .collect();
        store.append_bars("deribit", "U-PERP", "1h", &bars, Some("fixture")).unwrap();
        for h in 1..hours {
            let (ts, spot) = (h * HOUR_MS + HOUR_MS - 1, close(h - 1));
            let expiry_ms = ts + 7 * 24 * HOUR_MS;
            let years = vike_options::years_to_expiry_ms(expiry_ms, ts);
            let mut rows = Vec::new();
            for strike in [90.0, 100.0, 110.0] {
                for kind in [OptionKind::Call, OptionKind::Put] {
                    let iv = 0.5 + 2.0 * (strike / spot - 1.0) * (strike / spot - 1.0);
                    rows.push(vike_data::ChainRow {
                        ts,
                        underlying: "U".into(),
                        instrument: format!("U-{strike}-{kind:?}"),
                        expiry_ms,
                        strike,
                        kind,
                        bid: None,
                        ask: None,
                        mark: vike_options::black_scholes_price(spot, strike, years, iv, kind, 0.0),
                        iv: Some(iv),
                        open_interest: None,
                        volume: None,
                        delta: None,
                        gamma: None,
                        theta: None,
                        vega: None,
                    });
                }
            }
            store
                .append_chain_snapshot("deribit", "U", &rows, Some(&format!("chain-{h}")))
                .unwrap();
        }
        (Arc::new(store), TimestampRange::all())
    }

    fn poly_store() -> Arc<dyn HistoryStore + Send + Sync> {
        use vike_marketdata::test_support::{quote, trade};
        let store = HistoryStoreMemory::new();
        let from = END_MS - 300_000;
        let quotes: Vec<_> = (0..20)
            .map(|i| quote(from + 10_000 * i, 0.40 + 0.001 * i as f64, 0.42 + 0.001 * i as f64))
            .collect();
        let trades: Vec<_> =
            (0..7).map(|i| trade(from + 30_000 * i, 0.41, 5.0, i % 2 == 0)).collect();
        store.append_quotes("polymarket", TOKEN, &quotes, Some("fixture")).unwrap();
        store.append_trades("polymarket", TOKEN, &trades, Some("fixture")).unwrap();
        // One book snapshot per quote: the L2 lane fills resting quotes against a book.
        let books: Vec<L2BookUpdate> = quotes
            .iter()
            .zip(1u64..)
            .map(|(q, sequence)| L2BookUpdate {
                timestamp: q.timestamp,
                local_timestamp: 0,
                sequence,
                kind: L2BookUpdateKind::Snapshot,
                tick_size: 0.001,
                bids: vec![L2BookLevel { price: q.bid, qty: 100.0 }],
                asks: vec![L2BookLevel { price: q.ask, qty: 100.0 }],
                symbol: String::new(),
            })
            .collect();
        store.append_book_updates("polymarket", TOKEN, &books, Some("fixture")).unwrap();
        Arc::new(store)
    }

    /// A recorded book chain and nothing else: the shared `HistoryStoreMemory` holds no book.
    struct BookStore(Vec<L2BookUpdate>);

    impl HistoryStore for BookStore {
        fn scan_book_updates(
            &self,
            _venue: &str,
            _symbol: &str,
            range: TimestampRange,
        ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
            let inside =
                |t: i64| range.start.is_none_or(|s| t >= s) && range.end.is_none_or(|e| t <= e);
            Ok(self.0.iter().filter(|u| inside(u.timestamp)).cloned().collect())
        }
        vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_trades,
            scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
    }

    /// Mids 100 -> 100.5 -> 101 -> 100.5 on a whole-number grid, one millisecond apart.
    fn book_store() -> Arc<dyn HistoryStore + Send + Sync> {
        let event = |timestamp, sequence, kind, bids: &[(f64, f64)], asks: &[(f64, f64)]| {
            let levels =
                |side: &[(f64, f64)]| side.iter().map(|&(p, q)| L2BookLevel::new(p, q)).collect();
            L2BookUpdate {
                timestamp,
                local_timestamp: 0,
                sequence,
                kind,
                tick_size: 1.0,
                bids: levels(bids),
                asks: levels(asks),
                symbol: String::new(),
            }
        };
        use L2BookUpdateKind::{Delta, Snapshot};
        Arc::new(BookStore(vec![
            event(1000, 1, Snapshot, &[(99.0, 1.0), (98.0, 2.0)], &[(101.0, 3.0), (102.0, 4.0)]),
            event(1001, 2, Delta, &[], &[(101.0, 0.0)]),
            event(1002, 3, Delta, &[(100.0, 5.0)], &[]),
            event(1003, 4, Delta, &[], &[(101.0, 1.0)]),
        ]))
    }

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    /// 2024-01-05 00:00 UTC, a Friday.
    const FRIDAY_0: i64 = 1_704_412_800_000;

    /// The covered call's window: fifteen days from a Friday.
    fn option_window() -> TimestampRange {
        TimestampRange::of(FRIDAY_0, FRIDAY_0 + 15 * DAY)
    }

    /// A deribit-shaped store (the same as `crates/vike-user-research/tests/pipeline.rs`'s `option_store`):
    /// a linear underlying's hourly bars (50,000, then 56,000 on the 12th, then 51,000) and a coin-book chain's
    /// ragged rows at the hour's last millisecond. The study chooses the 12th's 55,000 call on the 5th and the
    /// 19th's 57,000 on the 12th.
    fn option_store() -> Arc<dyn HistoryStore + Send + Sync> {
        use vike_data::ChainRow;
        use vike_marketdata::OptionKind;
        let store = HistoryStoreMemory::new();
        let close = |open: i64| match open {
            o if o < FRIDAY_0 + 7 * DAY => 50_000.0,
            o if o < FRIDAY_0 + 8 * DAY => 56_000.0,
            _ => 51_000.0,
        };
        let bars: Vec<Bar> = (0..15 * 24)
            .map(|i| {
                let open = FRIDAY_0 + i * HOUR;
                vike_marketdata::test_support::flat_bar_unit_volume_on(
                    "BTCUSDC.P",
                    open,
                    close(open),
                )
            })
            .collect();
        store.append_bars("deribit", "BTCUSDC.P", "1h", &bars, None).unwrap();
        let row = |instrument: &str, kind, strike, expiry_days: i64, ts, mark, iv| ChainRow {
            ts,
            underlying: "BTC".to_string(),
            instrument: instrument.to_string(),
            expiry_ms: FRIDAY_0 + expiry_days * DAY + 8 * HOUR,
            strike,
            kind,
            bid: None,
            ask: None,
            mark: Some(mark),
            iv,
            open_interest: None,
            volume: Some(1.0),
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
        };
        let (call, put) = (OptionKind::Call, OptionKind::Put);
        let rows = [
            row(
                "BTC-12JAN24-55000-C",
                call,
                55_000.0,
                7,
                FRIDAY_0 + 6 * HOUR - 1,
                400.0,
                Some(0.5),
            ),
            row(
                "BTC-12JAN24-55000-P",
                put,
                55_000.0,
                7,
                FRIDAY_0 + 6 * HOUR - 1,
                4_000.0,
                Some(0.5),
            ),
            row("BTC-12JAN24-60000-C", call, 60_000.0, 7, FRIDAY_0 + 6 * HOUR - 1, 90.0, Some(0.5)),
            row(
                "BTC-19JAN24-55000-C",
                call,
                55_000.0,
                14,
                FRIDAY_0 + 6 * HOUR - 1,
                700.0,
                Some(0.5),
            ),
            row(
                "BTC-12JAN24-55000-C",
                call,
                55_000.0,
                7,
                FRIDAY_0 + 11 * HOUR - 1,
                420.0,
                Some(0.5),
            ),
            row(
                "BTC-19JAN24-57000-C",
                call,
                57_000.0,
                14,
                FRIDAY_0 + 7 * DAY + 2 * HOUR - 1,
                350.0,
                None,
            ),
            row(
                "BTC-12JAN24-55000-C",
                call,
                55_000.0,
                7,
                FRIDAY_0 + 7 * DAY + 7 * HOUR - 1,
                990.0,
                None,
            ),
            row(
                "BTC-19JAN24-57000-C",
                call,
                57_000.0,
                14,
                FRIDAY_0 + 7 * DAY + 10 * HOUR - 1,
                360.0,
                None,
            ),
        ];
        for r in &rows {
            store.append_chain_snapshot("deribit", "BTC", std::slice::from_ref(r), None).unwrap();
        }
        Arc::new(store)
    }

    /// The covered call's ONE backtest, run by the engine in the jailed study's run, by hand. The profile
    /// names no `commission` and `options/covered_call.rs` states none, so no fill pays a fee: no venue
    /// table, neither on a leg nor at delivery (the owner's ruling of 2026-11-18; `crates/vike-backtest/tests/option_legs.rs`'s
    /// `a_leg_pays_no_venue_fee_when_none_is_named` is the same rule). The cover bought at 50,000; the
    /// strategy file picks the 12th's 55,000 call by delta over its 60,000 (on their marks 400 and 90 at spot
    /// 50,000 a week out: implied vols near 0.68 and 0.76, deltas near 0.17 and 0.05, the first nearer 0.3)
    /// and sells it at its NEXT priced row, marked 420, at that mark less the recipe's assumed half-spread of
    /// 5.3% (the chain carries no quote): 397.74, settled at its Friday expiry 1,000 in the money at 56,000;
    /// the 19th's 57,000 (the week's one candidate) at its next row, marked 360, so 340.92, out of the money
    /// at expiry. Equity 1,000,000 − 50,000 + 397.74 − 1,000 + 340.92 + the cover at 51,000 = 1,000,738.66.
    fn covered_call_ran_the_engine(sandboxed: &Result<StudyOutcome, StudyError>) {
        let outcome = sandboxed.as_ref().expect("the covered call ran");
        let metric = |m: &str| outcome.metric_value(m).unwrap_or(f64::NAN);
        println!(
            "covered_call: {} week(s), return {}, {} trade(s), win rate {}",
            metric("weeks"),
            metric("total_return"),
            metric("trades"),
            metric("win_rate")
        );
        for (file, body) in outcome.artifacts() {
            println!("-- {file}\n{body}");
        }
        assert_eq!(metric("weeks"), 2.0);
        assert_eq!(metric("trades"), 2.0, "both calls sold and settled; the cover is held");
        assert!(
            (metric("total_return") - 738.66 / 1_000_000.0).abs() < 1e-12,
            "the ledger by hand"
        );
        assert_eq!(metric("win_rate"), 0.5, "the first call lost, the second won");
    }

    fn importance_learner() -> ScriptedLearner {
        ScriptedLearner::constant(0.75).with_importance(FitImportance {
            n_features: 2,
            gain: vec![1.5, 0.5],
            splits: vec![3, 1],
        })
    }

    /// A path the study may be handed as its scratch: the parent's; the jailed study cannot write it.
    fn scratch() -> PathBuf {
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("study-child-scratch")
    }
}
