//! `backtest` — the run driver, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/backtest.rs`'s body until the multicall merge. `main` became [`run`], and
//! TWO pieces of ambient state became parameters rather than moving with it:
//!
//! * the ENVIRONMENT, because `crates/vike-ops/tests/settings_secrets/settings_registry.rs` asks libraries to take
//!   configuration as parameters and only binaries to read the process;
//! * the CLOCK, because `crates/vike-ops/tests/architecture/clock_pin.rs`'s `CLOCK_PIN` is a ratchet keeping
//!   ambient clock reads out of the library tree. A composition root may read a clock; everything
//!   below it takes the timestamp. The bin supplies the real one.
//!
//! Both ratchets are satisfied by threading, never by exemption. Everything below is the binary's
//! own documentation, unchanged.
//!
//! Usage (the operator-facing copy is the `USAGE` const below, which `backtest --help` prints):
//!   backtest --help | --version
//!   backtest --list
//!   backtest run.toml [--json]            (history from the datahub; `--store` is refused, 0084)
//!   backtest paramscan.toml [--json] [--rank-by sharpe|return|max_dd|equity|multi]
//!                       [--optimizer grid|euler|tpe|genetic]
//!                       [--euler-depth N] [--trials N] [--seed S]
//!   backtest data <fetch|fetch-starter|seed-demo|export|rm> …   (`backtest data --help`)
//!
//! ⚠ **The five data-management flags are RETIRED (ruling 12).** `--seed-demo`, `--fetch`,
//! `--fetch-starter`, `--export` and `--rm-series` were never about backtesting — they fetch from
//! venues, write the store, export from it and DELETE from it — so the operator-facing spelling is
//! `vike-cli data <sub>` now, and each old flag is refused by name
//! ([`refuse_a_retired_data_flag`]). The WORK did not move and could not: the writers open a
//! `DataFusionHist` and `export` encodes Parquet, and `vike-cli` links no DataFusion at all — so
//! `vike-cli data` SPAWNS this binary as `backtest data <sub>` ([`run_data`]), which is also the
//! spelling to type by hand on a box that has an engine and no `vike-cli`. ⚠ Since 2026-09-26
//! `data export` READS over the wire like every other reader and refuses `--store` by name
//! ([`DATA_READS`]); the writers keep theirs.
//!
//! ⚠ The profile is POSITIONAL (ruling 14 of
//! `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md`), and `--profile PATH` is KEPT as
//! the older spelling because `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm SPAWNS this
//! binary with it. Giving both is refused rather than resolved — see [`profile_from_args`], which
//! also refuses a lone `data` positional, since [`run`] routes that word to a subcommand.
//!
//! `--list` prints every registered strategy name (`harness::STRATEGIES`) and exits — no store,
//! no profile needed. Otherwise a profile is required: the profile (`BacktestProfile::from_path`)
//! names the data slice (bar or tick mode — see `harness::run_backtest`'s doc comment for how the
//! two modes differ) and the strategy to run. History is read from the datahub at the configured
//! address (loopback by default) — decision 0084; `--store` is refused on this path, with the
//! key-less local datahub named as its replacement, and `--archive` names the one other backend.
//! The `data` verbs that WRITE keep their `--store`; `data export`, which reads, does not.
//! `--json` prints `BacktestReport` (or, for a sweep, `ParamscanReport`) as pretty JSON instead of
//! the human table.
//!
//! **A finished single run also PERSISTS** (`persist_run` below): a `<run_id>` directory under
//! `<project>/user_data/runs/` gets `manifest.json` — the COMMON, kind-agnostic manifest documented
//! on `vike_model::runs::RunManifest` — beside `report.json`, the same `BacktestReport` `--json`
//! prints. Until this existed the bin saved NOTHING, so there was no history and nothing a UI could
//! list. It is ADDITIVE: the report reaches stdout first and a persist failure is reported on
//! stderr without changing the exit code, so saving a run can never become a way to lose one.
//! `--json` stdout is byte-identical to before.
//!
//! ⚠ **A SEARCH persists too, and this paragraph said the opposite until stage 5.** A grid, euler,
//! tpe or genetic search mints a parent run of its own (`open_search_run`/`write_search_run`) whose
//! `kind` is `trial_ledger::SEARCH_RUN_KIND` and whose `report.json` is a
//! `trial_ledger::TrialsDocument` rather than a `ParamscanReport` — a different document because it
//! is a different question, which is what the separate kind records.
//!
//! ⚠ **A search can also carry ANTI-OVERFITTING STATISTICS in that document, and that costs an
//! opt-in.** `--keep-trials returns` makes each trial retain a bucketed return vector
//! (`harness::sweep::ReturnBuckets`), which is the trial MATRIX
//! `vike_analytics::overfit::pbo_cscv` and `deflated_sharpe_with_effective_n` need and which the
//! search path used to destroy before a row existed; the numbers land as
//! `trial_ledger::OverfitStats` on that `report.json`, where `vike-cli backtest gate --fail-if`
//! names them as `overfit.pbo` and siblings. They are REPORT FIELDS and nothing prints them —
//! that is the owner's ruling, and it is what makes them actionable by a CI step rather than one
//! more line to read. [`KeepTrials`] carries what each mode retains and why `series` is still
//! refused.
//!
//! ⚠ **And since the `series_facts` merge it is content-ADDRESSED like a single run.** It records
//! the same `run_fingerprint::input_fingerprint` over the same facts and carries it in its run id,
//! for the one manifest parse per series it was already spending on its resume witness. The note
//! that used to stand here — a search records `fingerprint: null` and keeps the pid form, because
//! the collector costs two parses per series — described a cost that no longer exists;
//! `collect_data_fingerprint` and `run_detail_data` carry what changed.
//!
//! A profile with a `[paramscan]` table — or its permanent `[sweep]` alias —
//! (`profile.is_paramscan()`) runs `harness::run_paramscan` instead of
//! a single `run_backtest`: the strategy runs once per point in the sweep's cartesian parameter
//! grid, and the result is a ranked `ParamscanReport` table rather than one `BacktestReport`.
//! ⚠ **This BIN now SAYS `[paramscan]` too, and the exclusion that stood here is WITHDRAWN.** It
//! read "this bin's own usage text and refusals still say `[sweep]` and are deliberately untouched
//! by the rename" — true when written, and a surface that tells an operator to write the OLD
//! spelling of a section it has renamed is a rename half done. [`USAGE`] and the
//! no-table refusal name `[paramscan]`; `crates/vike-backtest/tests/optimizer_cli/flags.rs`'s
//! `a_search_on_a_profile_with_no_sweep_table_is_refused_rather_than_ignored` is the test that
//! moved with them.
//!
//! ⚠ **What did NOT change is what the binary ACCEPTS.** `[sweep]` is a PERMANENT serde alias
//! (`harness::profile`'s `#[serde(default, alias = "sweep")]`, whose doc calls removing it a
//! breaking change to every profile ever written, not a tidy-up), and every profile spelling it —
//! this repository's own fixtures included — still loads. This is a rename of what the binary SAYS,
//! never of what it READS.
//! `--rank-by` picks the ranking metric (default `sharpe`). A VALID value is ignored — not an
//! error — on a non-sweep profile, which is what distinguishes it from `--optimizer`: it names how
//! to ORDER results, not what work to do. ⚠ An INVALID value is refused on either profile shape
//! now, where the old ladder swallowed a typo on the non-sweep one. `--rank-by multi` ranks by the
//! composite `crate::search::objective` multi-metric score instead (`objective::multi_metric` with default
//! `MultiMetricParams`) — rows gain a `score` column/field; the four classic metric names keep the
//! score-clearing classic path on the grid, and their output is byte-identical.
//!
//! **`--optimizer` names the search METHOD, and it is the ONE selector** (ruling 13: there is no
//! `optimize` verb and no `compute` verb — the word lives in the flag). It replaced a hand-written
//! ladder that parsed each flag inside the branch that used it; [`parse_search_flags`] carries what
//! that cost and the four defects it produced. `--search` is RETIRED and refused by name.
//!
//! `--optimizer euler` replaces the exhaustive cartesian grid with a bounded successive-halving
//! refinement (`search::euler::EulerSearch`): the coarse grid runs once, then the per-axis step is
//! halved up to `--euler-depth` times (default 3) around the running best point. Each completed
//! halving doubles the effective resolution, for a small fraction of the backtests an equally fine
//! grid would cost — at the cost of being a LOCAL refinement (it can only descend into the basin the
//! coarse grid already found) and of requiring every `[sweep]` axis to be numeric AND
//! type-homogeneous (no mixed `[1, 2.5]`). The stderr budget line compares against the grid matching
//! the depth ACTUALLY reached, so a search that ran out of new candidates early reports the smaller,
//! honest saving. Scoring reuses `--rank-by` verbatim (the same objective seam), rows always carry
//! a `score`, and a one-line budget summary goes to stderr.
//!
//! `--optimizer tpe` is the Bayesian (Tree-structured Parzen Estimator) ask/tell search
//! (`search::tpe::TpeSearch`) — the smart alternative to enumerating the grid, converging on good
//! params in `--trials` backtests (default 64) by modelling which regions score well. `--seed`
//! (default 0) makes it fully reproducible. It reuses `--rank-by` as its objective (rows carry a
//! `score`) and returns the same ranked `ParamscanReport`.
//!
//! `--optimizer genetic` is the population search (`search::genetic::GeneticSearch`) — the FOURTH
//! method, wired here by the follow-up `crates/vike-backtest/src/search/genetic.rs` named when it
//! landed without a dispatch. Its genome is INDICES into the authored `[sweep]` axes, so unlike
//! euler and tpe it explores no point BETWEEN the values somebody wrote down; what it buys instead
//! is a combinatorial search whose reachable set is exactly the grid's, at a budget derived from
//! the space and hard-capped at what enumeration would have cost. Every sizing knob
//! (`population`, `generations`, `max_evaluations`) derives from the space and has NO flag —
//! deliberately, for now: this PR wires the method, and each knob is a spending decision that owes
//! its own argument and its own `METHOD_KNOBS` row. `--seed` is the one input it takes, and it is
//! REQUIRED rather than defaulted — the search selector's `require_seed` carries that argument in
//! full.
//!
//! `--optimizer grid` (the default) is byte-identical to before, and deliberately so: under one of
//! the four classic `--rank-by` metrics it is the one combination whose rows carry NO `score`, and
//! it is exactly what `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm spawns and prints
//! verbatim (that arm absorbed `vike-cli sweep`'s when ruling 13 deleted the second verb).
//! `crates/vike-backtest/src/backtest_cli/run_search.rs`'s `run_search` is where that evaluator is
//! constructed, and `search::select::uses_classic_evaluator` is where it is argued.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_analytics::binutil::{arg, has_flag};
// The SELECTOR — every method name, every knob's owner, every value parser and every refusal
// sentence — lives in `search::select` since stage 7, because `crate::compute_server`
// resolves the identical rule for a REMOTE run and two copies is the defect that stage killed.
// ⚠ `Optimizer` and `RankMetric` became TEST-ONLY here with that move: the value parsers and the
// `(method, rank)` evaluator match left this file, so the lib half names neither. The test module
// still reads `GridSearch.name()` (which needs the trait in scope) and builds a `RankMetric` to pin
// `--rank-by`'s parse, so the imports are gated rather than deleted.
use crate::search::select::{self, METHOD_KNOBS};
// The two OBSERVER types this file's argv door resolves and `select::arm_observers` arms, BY
// MODULE PATH: `harness/mod.rs`'s `pub use` block is the optimizer SEAM's vocabulary and re-exports
// neither, deliberately — an observer a caller arms on an evaluator is the evaluator module's own
// knowledge, the same rule that keeps `genetic::GeneticConfig` at its module path.
use crate::harness::optimize::{ProgressMode, TradeFloor};
use crate::harness::{self, BacktestProfile, RankChoice, SearchMethod};
#[cfg(test)]
use crate::harness::{Optimizer, RankMetric};
use crate::run_fingerprint;
// Still imported after the value parsers moved into `search::select`: this file's own
// `tests::search_flags` drives the euler cap through the REAL argv parser, and it
// reads the cap off the config rather than writing a number.
#[cfg(test)]
use crate::search::EulerConfig;
#[cfg(test)]
use vike_datahub_client::{DEFAULT_SEARCH_METHOD, SEARCH_METHODS};
// The search ARTIFACT's documents. UNGATED, so this import costs nothing a default build does not
// already pay — `crate::trial_ledger`'s module doc says why they live outside `harness`.
use crate::trial_ledger;
#[cfg(test)]
use vike_data::DataFusionHist;
#[cfg(test)]
use vike_data::HistStore;
use vike_model::runs;

mod args;
mod data_cmd;
mod open_store;
mod persist;
mod run_record;
mod run_search;
mod run_single;
mod search_flags;
mod search_run;
mod serve;
mod trials_cmd;
mod usage;

#[cfg(test)]
use self::args::PROFILE_PATH_VALUED;
use self::args::{flag_given, parse_addr_flag, profile_from_args};
#[cfg(doc)]
use self::data_cmd::DATA_READS;
#[cfg(test)]
use self::data_cmd::{
    DATA_READS, DATA_SUBS, RETIRED_DATA_FLAGS, confirm_removal, refuse_a_store_on_a_data_read,
    triage_data_argv,
};
use self::data_cmd::{refuse_a_retired_data_flag, run_data};
use self::open_store::{OpenedStore, open_store};
use self::persist::{run_detail_data, run_detail_realism};
#[cfg(test)]
use self::run_record::{collect_data_fingerprint, keep_at_stride};
use self::run_record::{run_series_from, run_trades_from};
use self::run_search::run_search;
use self::run_single::run_single;
#[cfg(test)]
use self::search_flags::parse_keep_trials;
use self::search_flags::{KeepTrials, parse_search_flags};
use self::serve::{daemon_before_logging, run_serve};
use self::trials_cmd::run_trials;
#[cfg(test)]
use self::trials_cmd::{TrialSort, parse_trial_sort};
#[cfg(test)]
use self::usage::SUBCOMMANDS;
use self::usage::{LIST_OPTIMIZERS_FLAG, USAGE};

/// Run the `backtest` verb.
///
/// ⚠ `studio` is the ruling-7 SEAM, and it is a parameter for a reason a caller cannot see from the
/// type: the three Studio verbs the `--addr` daemon serves run `vike_studio_core`'s slice runners,
/// and that crate sits ABOVE this one in the layer graph (55 against 50 — and it DEPENDS on this
/// crate, so the edge could never point the other way). Only a composition root that can name both
/// may hand them down. `crates/vike/src/main.rs`'s `backtest_main` passes
/// `Some(vike_studio_core::studio_run_table())`; the standalone `src/bin/backtest.rs` passes `None`
/// and the daemon then refuses those three by name while serving the other four. Every non-`--addr`
/// invocation ignores it entirely.
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    now_unix_secs: &dyn Fn() -> i64,
    // ⚠ What the CALLING BINARY can say about its own build — see `vike_model::runs::BuildStamp` for why
    // this is a parameter. `crates/vike/src/main.rs`'s `backtest_main` fills it; the standalone
    // `crates/vike-backtest/src/bin/backtest.rs` cannot and passes `None`.
    build: Option<runs::BuildStamp<'_>>,
    studio: Option<crate::compute_server::StudioRunTable>,
    // ⚠ The STUDY runner, the same INJECTED shape and for the same layer reason — see
    // `crate::compute_server`'s `StudyRunFactory`. ⚠ A FACTORY rather than a built runner: the
    // dispatcher may not resolve a project directory at all
    // (`crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s `the_dispatcher_starts_nothing`), so it NAMES
    // the constructor and the `--addr` arm below — which already owns this daemon's own walk —
    // hands it the runs root and the pinned trainer. Mounted SEPARATELY from `studio` because it is a
    // separately-negotiated capability: `vike-backend backtest --addr` fills both, the standalone
    // bin fills neither.
    study: Option<crate::compute_server::StudyRunFactory>,
) -> ExitCode {
    // ⚠ argv FIRST, and `--help`/`--version` BEFORE `vike_log::init` — answering either must not
    // create a log directory. `--help` was not recognised at all: it fell through to the
    // required-argument check below, so `backtest --help` answered "--profile <path> is required"
    // on stderr with exit 2, telling the user they had forgotten a flag they never meant to pass.

    if has_flag(args, "--help") || has_flag(args, "-h") {
        // stdout + exit 0: help is normal output a user pipes into a pager, not a diagnostic.
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if has_flag(args, "--version") || has_flag(args, "-V") {
        // `<name> <version>` — the shape every `--version` on the box prints (`git version 2.x`).
        // The name is spelled literally because `CARGO_PKG_NAME` is the PACKAGE (`vike-backtest`)
        // while this binary is `backtest`, and a `--version` answering with a name the caller did
        // not invoke is exactly what a bug report cannot use.
        println!("backtest {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    // ⚠ **SETTINGS BEFORE LOGGING — for the `--addr` DAEMON, and only for it.** Its rolling file's
    // level is the `preferences.log_file_level` row, so its settings load before the subscriber;
    // `serve::daemon_before_logging` carries why that order costs the daemon nothing. Every other
    // arm keeps its load where it was, after its own argv triage below.
    let daemon = match daemon_before_logging(vars, args) {
        Ok(d) => d,
        Err(code) => return code,
    };

    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` instead of
    // vike-log's `<exe_dir>/logs` last resort (`target/debug/logs/…`, which `cargo clean` deletes).
    // `$VIKE_LOG_DIR` still wins; no project above the CWD still lands beside the exe.
    let _log_guards = vike_log::init(log_config(
        daemon.as_ref().map(|(_, settings)| settings),
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::paths::state_path::project_log_dir(&cwd)),
    ));

    if has_flag(args, "--list") {
        for name in harness::STRATEGIES {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }
    // ⚠ **The optimizer LISTING — the door `search::select`'s two renderers said did not
    // exist.** Both carried a "BUILT AND DEFERRED: no door calls this" note naming this flag, and
    // this arm is what withdraws it. It RENDERS
    // `vike_datahub_client::SEARCH_METHODS` through those functions rather than naming a method
    // here, which is the whole content of their "one roster, every door" claim: a fifth method
    // reaches this listing with no edit in this file.
    //
    // Placed beside `--list` and for its reasons, not merely near it: it answers out of a const,
    // needs no store, no profile and no network, and it must sit ABOVE `parse_search_flags` and
    // `profile_from_args` so a listing is never asked for a profile it has no use for.
    //
    // ⚠ `has_flag(args, "--list")` above is EXACT-TOKEN, so this longer spelling does not trip it
    // and the order of the two arms is free — the adjacency landmine
    // `vike_datahub_client::flag_vocab`'s `spec` is anchored against, live in this file's own argv.
    //
    // ⚠ `--json` is read with `has_flag` too, so `--list-optimizers --json=1` prints the PLAIN
    // listing. That is the residual `vike_analytics::binutil`'s `has_flag` declares for every bare
    // boolean in this family, inherited here rather than newly created — closing it is the
    // unknown-argument triage that doc names, on every flag at once.
    //
    // ⚠ TWO residuals declared rather than left to be found, both SHARED with `--list` and neither
    // newly created here. This arm sits ABOVE the `data` and `trials` subcommand routing, so
    // `backtest data rm … --list-optimizers` prints the roster and exits 0 instead of reaching that
    // verb's own triage — exactly as `--list` does today. Moving one of the two below the routing
    // and not the other would leave two sibling listings in two places, which is how the next
    // reader "tidies" the wrong one; closing it for both is the same unknown-argument triage named
    // above. And nothing gates the SPELLING against argv itself: the arm reads
    // [`LIST_OPTIMIZERS_FLAG`], which
    // `the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares`
    // holds against the vocabulary row and the usage text — three surfaces, one const — but no test
    // drives this binary over the flag, because `run` needs an environment, a clock and a store
    // root. `crates/vike-backtest/tests/help_cli.rs` is where that would live.
    if has_flag(args, LIST_OPTIMIZERS_FLAG) {
        if has_flag(args, "--json") {
            println!("{}", select::optimizer_roster_json());
        } else {
            println!("{}", select::optimizer_roster_lines());
        }
        return ExitCode::SUCCESS;
    }
    // ⚠ **RULING 12 — the five data-management operations left this binary's FLAG surface.**
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.7: `--fetch`,
    // `--fetch-starter`, `--seed-demo`, `--export` and `--rm-series` fetch from venues, write the
    // store, export from it and DELETE from it — none of which is backtesting — so their
    // operator-facing spelling is `vike-cli data hist <verb>` now.
    //
    // ⚠ **What moved is the SURFACE, not the code, and that distinction is load-bearing.** Every
    // one of these opens a `DataFusionHist`, and `vike-cli` is DataFusion-FREE by construction
    // (its manifest argues every edge; CI's `light-consumers` lane asserts it). So `vike-cli data`
    // reaches them by SPAWNING this binary — `crates/vike-cli/src/cmd/data/hist/engine.rs`'s `engine_argv` and
    // `rm_engine_argv` build exactly the argv below. Deleting the implementation would delete
    // `vike-cli data hist fetch --source demo|starter` and the local `rm` with it. A reader who
    // "finishes" the ruling by removing this arm breaks the verb the ruling moved the work TO.
    //
    // ⚠ It is a SUBCOMMAND rather than five renamed flags for two reasons. The grammar then matches
    // `vike-cli data`'s one-for-one, so the argv translation is a rename rather than a re-shape and
    // both surfaces read as the same words; and it stays reachable BY HAND on a box that has an
    // engine and no `vike-cli` (which is every Windows box — no release publishes a `vike-cli`-less
    // engine, but `crates/vike-cli/src/cmd/engine.rs`'s module doc carries the mirror asymmetry).
    //
    // Placed FIRST among the pre-profile exits so `data` is never read as the POSITIONAL profile
    // (ruling 14) — [`profile_from_args`] refuses a lone `data` positional by name for the same
    // reason, since routing here is what makes that spelling mean something else.
    if args.first().is_some_and(|a| a == DATA_SUBCOMMAND) {
        return run_data(vars, &args[1..], now_unix_secs);
    }
    // ⚠ **ARTIFACT-ONLY**: no store, no profile, no socket. §6 of the CLI-surface design requires
    // the reading verbs to work "on a laptop with neither, on runs minted months ago", so this
    // routes BEFORE `DataFusionHist::open` — the same placement, and for the same reason, as the
    // `data` route above: otherwise `trials` reads as the POSITIONAL profile ([`SUBCOMMANDS`]
    // refuses that spelling).
    //
    // ⚠ It lives on the ENGINE binary rather than on `vike-cli` and that is a DEPENDENCY fact, not
    // a preference: `vike-cli` has no normal dependency on this crate (dev-only). Every document it
    // reads is plain JSON, so the operator-facing `vike-cli backtest trials` can be built later
    // over `serde_json::Value` or over a typed edge — a choice this binary does not make for it.
    if args.first().is_some_and(|a| a == TRIALS_SUBCOMMAND) {
        return run_trials(vars, &args[1..]);
    }
    // …and the five old spellings, refused by name.
    // `crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_search_flags` retired
    // `--search` the same way and carries the argument: a second spelling that can be given a
    // DIFFERENT value is a resolution nobody can make safely, and an alias would keep the old
    // surface forever. ARGV TRIAGE — nothing is opened, per the rule
    // `a_refused_flag_never_opens_the_store` pins.
    if let Some(code) = refuse_a_retired_data_flag(args) {
        return code;
    }

    // The ONE `std::env::vars()` sweep this binary performs, per the settings-registry rule that a
    // binary reads the environment and everything below it takes the map as a parameter. Two
    // consumers: the user-indicator directory just below, and `store_root` further down.

    // ⚠ **User indicators, installed BEFORE any strategy is compiled.** A profile naming the
    // `rhai` strategy (`crates/vike-backtest/src/harness/registry.rs`'s `strategy_by_name` arm —
    // the create->backtest keystone) compiles the user's OWN script here, and that script may call
    // their OWN indicators. Nothing else installs them: `run_backtest` and every sweep/walkforward
    // sibling reach `RhaiStrategy::compile` with no indicator argument anywhere in the signature,
    // by design (`vike_script::install_user_indicators`' doc argues why the set is process-wide),
    // so a binary that skips this call hands the author a script that COMPILES and then raises
    // function-not-found on every bar until the consecutive-error cap switches the strategy off —
    // a backtest that runs to completion and reports zero trades.
    //
    // Placed after the three informational exits (`--help`/`--version`/`--list`), which answer out
    // of the BUILD and must not read a directory, and before the profile load, which is the first
    // step that can compile a script.
    //
    // Diagnostics go to **stderr**: `--json` writes a machine report on stdout, and one rejected
    // indicator file must not make that unparseable. They are never fatal, for the reason
    // `load_and_install_user_indicators` gives — a half-edited file the profile never calls must
    // not fail the run.
    if let Some(user_data) = std::env::current_dir().ok().and_then(|cwd| {
        // `VIKE_USER_DATA_DIR` names the directory outright and wins over the project walk. Read
        // here — and spelled as a LITERAL — because the settings registry's map-lookup sweep
        // resolves constants CRATE-wide, so importing `vike_model::paths::state_path::USER_DATA_DIR_ENV`
        // would make this read invisible to `crates/vike-ops/tests/settings_secrets/settings_registry.rs`. That is
        // the same trade `vike-cli`'s dispatcher makes for the same variable.
        vike_model::paths::state_path::project_user_data_dir_from(
            vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
            &cwd,
        )
    }) {
        for line in vike_script::load_and_install_user_indicators(&user_data) {
            eprintln!("backtest: indicator not loaded — {line}");
        }
    }

    // ⚠ THE DAEMON ARM (ruling 7 of
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). It sits HERE — after
    // the informational exits and after the indicator install, before the profile requirement —
    // because `--addr` is the one invocation that needs no profile and DOES need the user's
    // indicators: this daemon compiles client-supplied Rhai, so the set must be installed before a
    // connection can be accepted, and `install` is once-per-process. Its flag and its settings were
    // read above, before the subscriber; a BLANK `--addr` read as no daemon there and is refused
    // here, at the place it always was.
    if let Some((flag, settings)) = daemon {
        return run_serve(vars, args, flag, settings, studio, study);
    }
    if let Err(e) = parse_addr_flag(args) {
        eprintln!("backtest: {e}\n\n{USAGE}");
        return ExitCode::from(2);
    }

    // ⚠ **ARGV TRIAGE BEFORE ANY I/O**, and it is a design property rather than tidiness: refusing
    // a command line must not load a profile and must not open (which means CREATE — every
    // `DataFusionHist::open` `create_dir_all`s its root) a store. It is the rule `--help` and
    // `--version` already obey at the top of this function, extended to the flags that name WORK.
    // `crates/vike-backtest/tests/optimizer_cli/flags.rs`'s `a_refused_flag_never_opens_the_store`
    // is what makes it assertable: it names a `--store` path that must not exist afterwards.
    //
    // Placed after every arm that returns without a profile (`--list`, `--seed-demo`, `--rm-series`,
    // `--fetch*`, `--export`, `--addr`), so none of them is newly constrained by it.
    let search = match parse_search_flags(args) {
        Ok(s) => s,
        Err(e) => {
            // ⚠ NO `{USAGE}` here, and that is deliberate. A refusal that names ONE flag answers
            // with one line naming that flag and the paste-ready fix; the help text is what you get
            // when you supplied nothing to talk about (the two arms below and `--addr`). Dumping
            // `USAGE` after a flag error also makes the flag's name unfindable in the noise — and
            // it made five of `optimizer_cli.rs`'s tests pass against the UNFIXED binary, because
            // every flag name they grep for appears somewhere in that const.
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let profile_path = match profile_from_args(args) {
        Ok(Some(p)) => p,
        // The "you gave me nothing" arm KEEPS its usage dump — there is no specific mistake to
        // name, and `crates/vike-backtest/tests/help_cli.rs`'s
        // `a_missing_profile_still_exits_non_zero_on_stderr` pins that this still names `--profile`.
        Ok(None) => {
            eprintln!(
                "backtest: a profile is required — `backtest <profile.toml>` (or --profile PATH, \
                 or --list to show strategies)\n\n{USAGE}"
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    // ⚠ The TEXT comes back with the profile and is threaded to the run record — never re-read at
    // persist time. An operator editing a profile while a long backtest runs is ordinary, and a
    // record holding bytes the run did not use is worse than no record: it looks authoritative.
    let (profile, profile_toml) =
        match BacktestProfile::from_path_with_text(&PathBuf::from(&profile_path)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("backtest: failed to load profile {profile_path:?}: {e}");
                return ExitCode::from(2);
            }
        };

    // ⚠ A SEARCH WAS ASKED FOR AND THERE IS NOTHING TO SEARCH. Every one of these flags used to be
    // read INSIDE `if profile.is_paramscan()` below, so `backtest run.toml --optimizer tpe --trials 500`
    // ran ONE ordinary backtest and exited 0 — 500 trials of Bayesian search requested, one run
    // delivered, no diagnostic anywhere. §7.3 of the design signs off the refusal; ruling 13 is what
    // makes it a check on THIS verb rather than on the `optimize` verb it was written for.
    //
    // `--rank-by` deliberately keeps its DOCUMENTED ignore (this module's doc: a VALID value is
    // "ignored — not an error — on a non-sweep profile"), and the distinction is real rather than a
    // rationalisation: `--rank-by` names how to ORDER results and is meaningless-but-harmless with
    // nothing to order, while `--optimizer` and its per-method knobs name WHAT WORK TO DO.
    //
    // The message names `--optimizer` outright, which it can: a method-owned knob given WITHOUT
    // that flag is already refused above (no knob is owned by the default method), so
    // `search.requested` here proves `--optimizer` was written.
    //
    // ⚠ Since stage 5 the message names WHICH flag asked, because `--keep-trials` and `--resume`
    // also set `requested` and a line that always blamed `--optimizer` would name a flag the
    // operator never wrote. Both new flags describe a search's ARTIFACT, and a profile with no
    // search space has no artifact to keep either.
    //
    // ⚠ **The list this renders is DERIVED, and it was a six-element LITERAL until the observer
    // doors landed.** It is `--optimizer`, then [`METHOD_KNOBS`], then [`SEARCH_PROPERTY_FLAGS`] —
    // the same three sources [`parse_search_flags`] sets `requested` from, in the same order, so
    // the sentence renders exactly the bytes it did before and cannot go short. A literal here was
    // a second copy of "what asks for a search" living in a different function from the code that
    // decides it: `requested` would be true, this filter would match nothing, and the refusal
    // would name no flag at all.
    if search.requested && !profile.is_paramscan() {
        let asked = std::iter::once("--optimizer")
            .chain(METHOD_KNOBS.iter().map(|(flag, _)| *flag))
            .chain(SEARCH_PROPERTY_FLAGS.iter().copied())
            .filter(|f| flag_given(args, f))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "backtest: {asked} names a parameter SEARCH, but {profile_path:?} has no [paramscan] \
             table — there is nothing to search. Add one, or drop the search flags"
        );
        return ExitCode::from(2);
    }

    let OpenedStore { provenance, concrete, store } = match open_store(vars, args) {
        Ok(v) => v,
        Err(code) => return code,
    };

    // ⚠ **`data.explain`: PLAN AND STOP.** Placed here — after the store is open and BEFORE the
    // sweep branch, the clock read and the runs-root walk — because a plan must report what the
    // store actually holds, and because a planning run must mint no run directory, take no
    // `started_at` and evaluate no grid point. It is the local twin of
    // `crate::compute_server`'s `explain_instead_of_running`, and both call the SAME
    // `crate::data_plan::explain_document`: a plan printed on the box and a plan returned over the
    // wire are one document, so `--local` stays a rehearsal for `--addr` here too.
    //
    // ⚠ Exit 0. A plan is a SUCCESS — it answered the question that was asked — and a non-zero rung
    // would make `vike-cli backtest run --explain-data` read as a failed run to every wrapper.
    if profile.data.explain {
        // ⚠ **On the WIRE arm there is NO rung, and `None` is the honest answer rather than a
        // missing one.** The local ladder still resolves a root — the disclosure below needs
        // something to fall back to — but nothing opened it, so reporting "the configured root
        // answered" would credit a rung that decided nothing on this run. The plan leads with the
        // ROUTE instead, which is the question "which store answered" actually has an answer to.
        let plan = match crate::data_plan::plan_data(
            &profile,
            store.as_ref(),
            &provenance,
            // ⚠ No ladder rung to credit: there is no local root any more. `plan_data`'s `rung`
            // parameter now receives `None` from ALL THREE of its callers, which makes it dead;
            // removing it is a separate change to `data_plan.rs` and its tests.
            None,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("backtest: {e}");
                return ExitCode::from(2);
            }
        };
        if has_flag(args, "--json") {
            match crate::data_plan::explain_document(&profile, &plan) {
                Ok(doc) => match serde_json::to_string_pretty(&doc) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("backtest: cannot render the data plan: {e}");
                        return ExitCode::from(2);
                    }
                },
                Err(e) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            match crate::data_plan::explain_lines(&profile, &plan) {
                Ok(lines) => {
                    for line in lines {
                        println!("{line}");
                    }
                }
                Err(e) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    // The run's own clock read, taken BEFORE the work: it is both the manifest's `started_at` and
    // the seconds half of the run id, so the directory name and the document inside it cannot
    // disagree about when this run began. Read HERE rather than inside `vike_model::runs`
    // because a crate carrying `backtest == paper == live` contains no ambient clock read at all —
    // `crates/vike-ops/tests/architecture/clock_pin.rs`'s `CLOCK_PIN` is the gate, and time is an INPUT.
    //
    // ⚠ Read ABOVE the sweep branch since stage 5: a SEARCH mints its parent run before it
    // evaluates anything (the ledger needs a directory), so both paths need the value here. For a
    // single-run profile the branch below is not entered, so this executes at exactly the point it
    // did before and the value cannot differ.
    let started_at = now_unix_secs();

    // ⚠ The runs root is resolved HERE, in the composition root, from the SAME environment map and
    // the SAME working directory the indicator load above used — so a run cannot land in one
    // project while the strategy that produced it was read from another, and `VIKE_USER_DATA_DIR`
    // moves BOTH. Hoisted above the sweep branch by stage 5 for the same reason `started_at` was:
    // a search needs it before it evaluates anything, and ONE resolution must decide for both
    // paths (`persist_run`'s doc carries why this is a parameter rather than a walk inside the
    // persisting function).
    let runs_root = std::env::current_dir().ok().and_then(|cwd| {
        // Spelled as a LITERAL, matching the indicator load above and for the reason it gives:
        // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s map-lookup sweep resolves constants
        // CRATE-wide, so importing `vike_model::paths::state_path::USER_DATA_DIR_ENV` would make this read
        // invisible to that gate.
        vike_model::paths::state_path::user_runs_dir_from(
            vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
            &cwd,
        )
    });

    if profile.is_paramscan() {
        return run_search(
            args,
            now_unix_secs,
            build,
            search,
            profile,
            profile_toml,
            profile_path,
            provenance,
            concrete,
            store,
            started_at,
            runs_root,
        );
    }

    run_single(
        args,
        now_unix_secs,
        build,
        profile,
        profile_toml,
        profile_path,
        provenance,
        concrete,
        store,
        started_at,
        runs_root,
    )
}

/// **The rolling log of one invocation of [`run`]**: the file prefix every arm shares, the
/// project's log directory, and — for the `--addr` daemon alone — the `preferences.log_file_level`
/// row as the FILE level (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`,
/// verdict 3). `daemon` is that arm's settings, loaded before the subscriber by
/// `serve::daemon_before_logging`; every other arm passes `None` and keeps `vike_log`'s compiled
/// default. `VIKE_LOG_FILE_LEVEL` beats either, inside `vike_log::init`.
fn log_config(
    daemon: Option<&vike_config::Settings>,
    project_dir: Option<PathBuf>,
) -> vike_log::LogConfig {
    let mut cfg = vike_log::LogConfig {
        file_prefix: "backtest".to_string(),
        project_dir,
        ..Default::default()
    };
    if let Some(settings) = daemon {
        cfg.file_level = settings.preferences.log_file_level.clone();
    }
    cfg
}

// ⚠ `fingerprint_series_ids` MOVED out of this file, to
// `crates/vike-backtest/src/run_fingerprint.rs`'s `planned_series_ids`, and the move is not
// tidiness. It is the ONE answer to "which stored series does this profile resolve to", and a
// SECOND consumer arrived that this file cannot serve: `crate::data_plan` runs the same resolution
// for the coverage gate and the `data.explain` plan, and both must work on the REMOTE route — where
// the store is a trait-only `Arc<dyn HistStore>` (a DEFAULT build) and this module does not compile
// at all (it needs `datafusion-store`). So the resolver lives at the lower feature level that both
// can reach. No `pub use` shim: every call site here names the canonical path.

/// Everything [`persist_run`] needs about a run, gathered into one value.
///
/// A struct rather than loose parameters for two reasons: clippy's `too_many_arguments` (the merge
/// gate is `-D warnings`) would refuse the eight this ends up carrying, and a call site that reads
/// as a list of named facts is what lets a later change ADD one without touching every caller.
/// `crates/vike-studio-core/src/study_run.rs`'s `RunFacts` is the same shape for the same reason.
struct BacktestRunFacts<'a> {
    profile: &'a BacktestProfile,
    /// As the operator spelled it on the command line, NOT canonicalized: a canonical path answers
    /// about the box the run happened on; their own spelling is what they can paste back.
    profile_path: &'a str,
    /// The profile's own TEXT, as it was parsed — read ONCE by [`run`], never re-read here. See
    /// `vike_model::runs::CONFIG_FILE` for why the config is text rather than a serialized struct.
    profile_toml: &'a str,
    /// Which hist store the run read. The profile names a slice; only this says where the bytes
    /// behind it came from — the datahub's ADDRESS, or the archive path on an `--archive` run.
    /// ⚠ It was `store_root: &Path` until 2026-09-25 and recorded the LOCAL root this process
    /// resolved even on a wire run, where nothing opened it. The local read door is closed, so a
    /// path here would have been false on EVERY run; it carries the one provenance [`run`] computes.
    store: &'a str,
    /// Where run directories live. A PARAMETER — resolved once by [`run`], which already owns the
    /// process environment sweep — so this function is testable at all and so ONE walk decides
    /// which project a process is in.
    runs_root: &'a Path,
    /// What the store held for every series this run reads, captured BEFORE the run — `None` for a
    /// producer that could not ask (nothing in this file today, but the field is an `Option` so a
    /// test can persist without a store).
    data: Option<&'a run_fingerprint::DataFingerprint>,
    /// The run's INPUT ADDRESS — `crate::run_fingerprint::input_fingerprint`. `None` when this
    /// producer had no config text to hash, which no production path is today.
    fingerprint: Option<&'a str>,
    /// See `vike_model::runs::BuildStamp`. `None` from the standalone engine binary.
    build: Option<runs::BuildStamp<'a>>,
    started_at: i64,
    finished_at: i64,
}

/// Write the finished run to `<runs_root>/<run_id>/` — `manifest.json` (the COMMON manifest every
/// producer writes identically) beside `report.json`, `series.json` and `trades.json`.
///
/// ⚠ **The path is NOT resolved here any more, and that is the point.** It used to call
/// `std::env::current_dir()` inside itself, which made this function untestable without mutating
/// the working directory — something `crates/vike-backtest/CLAUDE.md`'s test-grouping rule forbids,
/// and the reason it had no test at all. [`run`] resolves it now, through
/// `vike_model::paths::state_path::user_runs_dir_from`, from the SAME environment sweep and the SAME
/// working directory the indicator load uses.
///
/// ⚠ **The `VIKE_USER_DATA_DIR` residual this function used to declare is CLOSED.** It redirects
/// the runs directory now, because `user_runs_dir_from` exists beside `RUNS_SUBDIR` in `vike-model`
/// — which is exactly where the old doc said the fix belonged. An operator who redirects
/// `user_data` no longer gets their indicators from the override and their runs from the project.
///
/// ⚠ **The RESULT is persisted, not just the report.** `report.json` holds the ten derived scalars;
/// [`runs::RunSeries`] and [`runs::RunTrades`] hold what they were derived FROM, plus the
/// diagnostic counters that tell a zero-trade run apart from a broken one. Every one of those was
/// computed and dropped when this function took only a report.
///
/// Failures come back as a STRING rather than as an error type: every one of them ends up in the
/// same one-line stderr message, and there is no caller that could act on the difference.
fn persist_run(
    facts: &BacktestRunFacts<'_>,
    result: &vike_analytics::BacktestResult,
    report: &vike_analytics::report::BacktestReport,
) -> Result<PathBuf, String> {
    // Creating the directory is what MINTS the id, which is why it happens before the manifest is
    // built rather than after — `vike_model::runs::RunManifest::run_id` carries that argument.
    let run = runs::create_run_dir(facts.runs_root, facts.started_at, facts.fingerprint)
        .map_err(|e| e.to_string())?;

    let manifest = runs::RunManifest {
        schema: runs::MANIFEST_SCHEMA,
        run_id: run.run_id.clone(),
        kind: runs::BACKTEST_RUN_KIND.to_string(),
        // The BINARY's name, spelled literally — the same distinction `--version` above makes, and
        // for the same reason: `CARGO_PKG_NAME` is `vike-backtest`, which is not what was invoked.
        produced_by: "backtest".to_string(),
        started_at: runs::utc_rfc3339(facts.started_at),
        finished_at: runs::utc_rfc3339(facts.finished_at),
        git_sha: facts.build.and_then(|b| b.git_sha).map(str::to_string),
        fingerprint: facts.fingerprint.map(str::to_string),
        config: runs::RunConfig {
            path: Some(facts.profile_path.to_string()),
            name: facts.profile.name.clone(),
        },
        // Everything below here is a BACKTEST's business and nests, so a listing that has never
        // heard of a backtest still renders every field above it.
        detail: serde_json::json!({
            "strategy": facts.profile.strategy.name,
            // ⚠ Factored into [`run_detail_data`] so the SEARCH producer describes its slice with
            // the same keys. Byte-identical to the literal it replaced — this is a shared spelling,
            // not a shape change to a document already on people's disks.
            "data": run_detail_data(facts.profile, facts.data),
            // The COST MODEL, as the two keys a listing needs — see [`run_detail_realism`] for why
            // the whole stamp stays in `report.json`. Nothing above this line says what a fill was
            // charged, so a listing of forty runs could not tell the free ones from the costed
            // ones and every comparison across it was unsound.
            "realism": run_detail_realism(facts.profile),
            // Where the run's history came from. The profile names a slice; only this says where the
            // bytes behind it came from — the datahub it dialled, or the archive it read.
            "store": facts.store,
            // The whole identity line — commit, tree state, build timestamp, rustc and target.
            // NESTS, because only the sha is a question every kind of run answers; `null` when this
            // binary could not name its build, which is the standalone engine's ordinary state.
            "build": facts.build.and_then(|b| b.summary),
        }),
    };

    let series = run_series_from(result);
    let trades = run_trades_from(result);
    let extras = runs::RunExtras {
        config_toml: Some(facts.profile_toml),
        series: Some(&series),
        trades: Some(&trades),
    };
    runs::write_run_with(&run.path, &manifest, report, &extras).map_err(|e| e.to_string())?;
    Ok(run.path)
}

/// The parent run of a parameter search, opened BEFORE the search runs.
struct SearchRun {
    run_id: String,
    path: PathBuf,
    /// The id this search was resumed FROM, when it was — see
    /// `crate::trial_ledger::SearchHeader::resumed_from`.
    resumed_from: Option<String>,
}

/// The reading verb for a finished search. Spelled once, because [`run`] routes on it and
/// [`SUBCOMMANDS`] refuses it as a POSITIONAL profile for exactly that reason.
const TRIALS_SUBCOMMAND: &str = "trials";

/// The human table. `rank` is the row's position AFTER sorting; `#n` is its EVALUATION index, and
/// the two are deliberately separate columns — conflating them is how `<id>#<n>` stops being a
/// stable address.
fn render_trials(doc: &trial_ledger::TrialsDocument, rows: &[trial_ledger::TrialRecord]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let seed = doc.identity.seed.map(|v| format!(", seed {v}")).unwrap_or_default();
    let _ = writeln!(
        s,
        "search {} — {}, ranked by {}{} ({} evaluated, {} reused, {} failed)",
        doc.run_id,
        doc.identity.method,
        doc.identity.rank_by,
        seed,
        doc.evaluated,
        doc.reused,
        doc.failed
    );
    if doc.unreadable > 0 || doc.superseded > 0 {
        let _ = writeln!(
            s,
            "  ⚠ {} ledger line(s) unreadable, {} superseded by a later line",
            doc.unreadable, doc.superseded
        );
    }
    if rows.is_empty() {
        let _ =
            writeln!(s, "no trials recorded (keep-trials = {}) — nothing to rank", doc.keep_trials);
        return s;
    }
    for (i, t) in rows.iter().enumerate() {
        let rank = if i == 0 { "*1".to_string() } else { (i + 1).to_string() };
        let overrides =
            t.overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
        let score = if t.score.is_finite() { format!("{:.4}", t.score) } else { "n/a".to_string() };
        match (&t.metrics, &t.error) {
            (Some(m), _) => {
                let f = |k: &str| m.get(k).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                let _ = writeln!(
                    s,
                    "{rank:<4} #{:<5} {overrides:<30} score={score} ret={:.4} sharpe={:.4} \
                     max_dd={:.4} trades={}",
                    t.n,
                    f("total_return"),
                    f("sharpe"),
                    f("max_drawdown"),
                    m.get("n_trades").and_then(|v| v.as_u64()).unwrap_or(0)
                );
            }
            (None, Some(e)) => {
                let _ = writeln!(s, "{rank:<4} #{:<5} {overrides:<30} FAILED: {e}", t.n);
            }
            (None, None) => {
                let _ = writeln!(s, "{rank:<4} #{:<5} {overrides:<30} (no metrics recorded)", t.n);
            }
        }
    }
    s
}

/// The verb that carries them. Spelled once, because [`run`] routes on it and
/// [`profile_from_args`] refuses it as a POSITIONAL for exactly that reason.
const DATA_SUBCOMMAND: &str = "data";

/// The flags that ask for a SEARCH without naming a METHOD: every one of them a property of the
/// search itself rather than of a searcher.
///
/// ⚠ **None is a [`METHOD_KNOBS`] row and none may become one.** That table answers "may this flag
/// be written at all under the named method", and every one of these is meaningful under all four:
/// two describe the search's ARTIFACT (`harness::trial_ledger`'s documents, which every method
/// mints) and two arm OBSERVERS on the evaluator every method drives. A row there would refuse
/// `--min-trades 50` under three of the four methods for no reason at all —
/// `search::select`'s `resolve_min_trades` argues it and that module's
/// `the_trade_floor_is_owned_by_no_method` pins it.
///
/// ⚠ **ONE array, WALKED by the parser and RENDERED by the refusal** — the shape
/// `crate::data_plan`'s `OnGap::NAMES` established here. [`parse_search_flags`] sets `requested`
/// by iterating it, and [`run`]'s no-`[paramscan]`-table refusal renders `--optimizer`, then
/// [`METHOD_KNOBS`], then this. Until the observer doors landed those were two hand-written lists
/// in two functions, and a flag added to the parser's side alone would have produced a refusal
/// that named NO flag while `requested` was true.
const SEARCH_PROPERTY_FLAGS: &[&str] = &["--keep-trials", "--resume", "--min-trades", "--progress"];

/// Everything argv said about the search, resolved and validated in one pure act.
#[derive(Debug)]
struct SearchFlags {
    method: SearchMethod,
    rank: RankChoice,
    /// `--keep-trials`. See [`KeepTrials`].
    keep: KeepTrials,
    /// `--resume <id>` — the search run whose ledger warms this one. See
    /// `search::trials::TrialRecorder`.
    resume: Option<String>,
    /// `--min-trades` — the statistical-significance floor, resolved by
    /// `search::select::resolve_min_trades`. [`TradeFloor::DISARMED`] when unwritten, which
    /// is what makes threading it unconditionally byte-identical to not having it.
    floor: TradeFloor,
    /// `--progress` — which progress stream, resolved by
    /// `search::select::resolve_progress`. `ProgressMode::Auto` when unwritten, which emits
    /// only when stderr is a terminal.
    progress: ProgressMode,
    /// Whether argv asked for a SEARCH at all — an explicit `--optimizer`, any method-owned knob,
    /// or any [`SEARCH_PROPERTY_FLAGS`] member. A profile with no `[paramscan]` table then refuses
    /// instead of silently running one backtest. ⚠ `--rank-by` is deliberately NOT counted: it
    /// names how to ORDER results, not what work to do, and its ignore on a non-sweep profile is
    /// documented behaviour.
    requested: bool,
}

/// The value of a VALUED flag, refusing the written-but-value-less spelling instead of reading it
/// as absent.
///
/// ⚠ **This is defect (d)'s last spelling, and it is the same failure — a DIFFERENT ANSWER rather
/// than a refusal.** [`arg`] answers `None` for a TRAILING bare `--optimizer`: the token is found,
/// there is no `=`, and there is no next token. So `backtest sweep.toml --optimizer` read as "no
/// `--optimizer` flag", fell to the `GridSearch` default, ran the exhaustive grid to completion and
/// exited 0 with no diagnostic — and on a profile with no `[paramscan]` table it also slipped past the
/// defect-(e) refusal, which keys on `SearchFlags::requested`. Its two siblings were both caught:
/// bare `--search` by [`flag_given`], and `--optimizer=` because [`arg`] answers `Some("")` there
/// deliberately. The selector was the one spelling with neither guard.
///
/// The same hole silently DEFAULTED every knob under the method that owns it — `--optimizer tpe
/// --trials` ran `TpeConfig::DEFAULT_TRIALS`, `--optimizer euler --euler-depth` ran
/// `EulerConfig::DEFAULT_MAX_DEPTH`, `--seed` ran `0` — while the SAME argv under a non-owning
/// method was refused, because the ownership check goes through [`flag_given`]. One flag, two
/// fates, decided by which optimizer was named: defect (b)'s shape wearing a different flag.
///
/// It is a script's spelling, not just a typo: `--optimizer $METHOD` with `METHOD` unset collapses
/// to the bare token, exactly as `--optimizer="$METHOD"` collapses to `--optimizer=`.
///
/// [`has_flag`] rather than [`flag_given`] in the guard: the `arg` half of that predicate has just
/// answered `None`, so what is left to detect is exactly the bare token.
fn required_value(args: &[String], flag: &str, expected: &str) -> Result<Option<String>, String> {
    match arg(args, flag) {
        Some(v) => Ok(Some(v)),
        None if has_flag(args, flag) => Err(format!(
            "{flag} was written with no value ({expected}). A trailing `{flag}` read as ABSENT \
             before, which silently ran the default instead of refusing — write `{flag} <value>`"
        )),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
