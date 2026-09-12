//! `backtest` — the run driver, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/backtest.rs`'s body until the multicall merge. `main` became [`run`], and
//! TWO pieces of ambient state became parameters rather than moving with it:
//!
//! * the ENVIRONMENT, because `crates/vike-ops/tests/settings_registry.rs` asks libraries to take
//!   configuration as parameters and only binaries to read the process;
//! * the CLOCK, because `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` is a ratchet keeping
//!   ambient clock reads out of the library tree. A composition root may read a clock; everything
//!   below it takes the timestamp. The bin supplies the real one.
//!
//! Both ratchets are satisfied by threading, never by exemption. Everything below is the binary's
//! own documentation, unchanged.
//!
//! Usage (the operator-facing copy is the `USAGE` const below, which `backtest --help` prints):
//!   backtest --help | --version
//!   backtest --list
//!   backtest run.toml [--store DIR] [--json]
//!   backtest sweep.toml [--store DIR] [--json] [--rank-by sharpe|return|max_dd|equity|multi]
//!                       [--optimizer grid|euler|tpe|genetic]
//!                       [--euler-depth N] [--trials N] [--seed S]
//!   backtest data <fetch|fetch-starter|seed-demo|export|rm> …   (`backtest data --help`)
//!
//! ⚠ **The five data-management flags are RETIRED (ruling 12).** `--seed-demo`, `--fetch`,
//! `--fetch-starter`, `--export` and `--rm-series` were never about backtesting — they fetch from
//! venues, write the store, export from it and DELETE from it — so the operator-facing spelling is
//! `vike-cli data <sub>` now, and each old flag is refused by name
//! ([`refuse_a_retired_data_flag`]). The WORK did not move and could not: every one of them opens a
//! `DataFusionHist`, and `vike-cli` links no DataFusion at all — so `vike-cli data` SPAWNS this
//! binary as `backtest data <sub>` ([`run_data`]), which is also the spelling to type by hand on a
//! box that has an engine and no `vike-cli`.
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
//! two modes differ) and the strategy to run. `--store` picks the hist-store root; it falls back
//! to `$VIKE_HIST_STORE`, then `<repo>/market_data/hist` (same convention as `vike-backfill`'s backfill
//! bins, e.g. `eod_backfill`). `--json` prints `BacktestReport` (or, for a sweep, `SweepReport`)
//! as pretty JSON instead of the human table.
//!
//! **A finished single run also PERSISTS** (`persist_run` below): a `<run_id>` directory under
//! `<project>/user_data/runs/` gets `manifest.json` — the COMMON, kind-agnostic manifest documented
//! on `crate::runs::RunManifest` — beside `report.json`, the same `BacktestReport` `--json`
//! prints. Until this existed the bin saved NOTHING, so there was no history and nothing a UI could
//! list. It is ADDITIVE: the report reaches stdout first and a persist failure is reported on
//! stderr without changing the exit code, so saving a run can never become a way to lose one.
//! `--json` stdout is byte-identical to before.
//!
//! ⚠ **Only the single-run path persists.** A sweep, a euler refinement and a `--optimizer tpe`
//! run each answer with a `SweepReport` — a different document, and a run KIND of its own rather
//! than a backtest with extra rows — so they print exactly as before and write nothing. Giving them
//! a `kind` and a `detail` of their own is the obvious next step and is deliberately not smuggled
//! in here.
//!
//! A profile with a `[sweep]` table (`profile.is_sweep()`) runs `harness::run_sweep` instead of
//! a single `run_backtest`: the strategy runs once per point in the sweep's cartesian parameter
//! grid, and the result is a ranked `SweepReport` table rather than one `BacktestReport`.
//! `--rank-by` picks the ranking metric (default `sharpe`). A VALID value is ignored — not an
//! error — on a non-sweep profile, which is what distinguishes it from `--optimizer`: it names how
//! to ORDER results, not what work to do. ⚠ An INVALID value is refused on either profile shape
//! now, where the old ladder swallowed a typo on the non-sweep one. `--rank-by multi` ranks by the
//! composite `crate::objective` multi-metric score instead (`objective::multi_metric` with default
//! `MultiMetricParams`) — rows gain a `score` column/field; the four classic metric names keep the
//! score-clearing classic path on the grid, and their output is byte-identical.
//!
//! **`--optimizer` names the search METHOD, and it is the ONE selector** (ruling 13: there is no
//! `optimize` verb and no `compute` verb — the word lives in the flag). It replaced a hand-written
//! ladder that parsed each flag inside the branch that used it; [`parse_search_flags`] carries what
//! that cost and the four defects it produced. `--search` is RETIRED and refused by name.
//!
//! `--optimizer euler` replaces the exhaustive cartesian grid with a bounded successive-halving
//! refinement (`harness::euler::EulerSearch`): the coarse grid runs once, then the per-axis step is
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
//! (`harness::tpe::TpeSearch`) — the smart alternative to enumerating the grid, converging on good
//! params in `--trials` backtests (default 64) by modelling which regions score well. `--seed`
//! (default 0) makes it fully reproducible. It reuses `--rank-by` as its objective (rows carry a
//! `score`) and returns the same ranked `SweepReport`.
//!
//! `--optimizer genetic` is the population search (`harness::genetic::GeneticSearch`) — the FOURTH
//! method, wired here by the follow-up `crates/vike-backtest/src/harness/genetic.rs` named when it
//! landed without a dispatch. Its genome is INDICES into the authored `[sweep]` axes, so unlike
//! euler and tpe it explores no point BETWEEN the values somebody wrote down; what it buys instead
//! is a combinatorial search whose reachable set is exactly the grid's, at a budget derived from
//! the space and hard-capped at what enumeration would have cost. Every sizing knob
//! (`population`, `generations`, `max_evaluations`) derives from the space and has NO flag —
//! deliberately, for now: this PR wires the method, and each knob is a spending decision that owes
//! its own argument and its own `METHOD_FLAGS` row. `--seed` is the one input it takes, and it is
//! REQUIRED rather than defaulted — [`require_seed`] carries that argument in full.
//!
//! `--optimizer grid` (the default) is byte-identical to before, and deliberately so: under one of
//! the four classic `--rank-by` metrics it is the one combination whose rows carry NO `score`, and
//! it is exactly what `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm spawns and prints
//! verbatim (that arm absorbed `vike-cli sweep`'s when ruling 13 deleted the second verb).
//! [`run`]'s evaluator construction is where that is kept and argued.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use crate::binutil::{arg, has_flag, store_root};
// ⚠ `Optimizer` is imported for its METHODS, not to be named: `optimizer_for` returns a
// `Box<dyn harness::Optimizer>` by path, but `GridSearch.name()` — how `parse_search_flags` reads
// the default method's spelling instead of writing a literal — needs the trait in scope.
use crate::harness::{self, BacktestProfile, Optimizer, RankMetric};
use crate::runs;
use crate::search::EulerConfig;
// Gated exactly as the module is: a `datafusion-store`-only build has no `starter`, and an
// unconditional import made that configuration fail to compile — a build CI runs and I did not.
#[cfg(feature = "venue-fetch")]
use crate::starter;
use vike_data::DataFusionHist;
// The TRAIT, so the ONE `Arc<dyn HistStore + Send + Sync>` binding above the sweep branch can be
// annotated. The unsize coercion used to happen implicitly at four `Arc::new(store)` call sites,
// one per ladder arm; it happens once now, and the annotation is what performs it.
use vike_data::HistStore;
use vike_data::demo as demo_tape;

// `periods_per_year` (the Sharpe annualization factor) now lives in `harness::report` as the SINGLE
// source of truth, so the single-run path here and the sweep rank an identical profile on the same
// Sharpe scale. Imported below as `harness::report::periods_per_year`.

/// What `--help` prints. A const rather than the module doc above, because only a const can reach a
/// user: that doc is for a reader of this file, `backtest --help` is for everyone else.
const USAGE: &str = "\
usage: backtest PROFILE.toml [--store DIR] [--json]
       backtest SWEEP.toml [--rank-by sharpe|return|max_dd|equity|multi]
                [--optimizer grid|euler|tpe|genetic]
                [--euler-depth N] [--trials N] [--seed S]
       backtest --addr [HOST:PORT]
       backtest --list
       backtest data <subcommand> [options]     (see `backtest data --help`)

  --addr [ADDR]    STAY ALIVE and serve the seven COMPUTE verbs over the node protocol —
                   RunBacktest, RunSlice, RunSweep, RunWalkforward, RunSweepProfile,
                   RunWalkforwardProfile, ListStrategies. The VALUE IS OPTIONAL: a bare --addr
                   serves on the configured address (VIKE_BACKTEST_ADDR, then
                   config.backtest_addr, then 127.0.0.1:7880); --addr HOST:PORT overrides it.
                   The FLAG is what says 'become a daemon' — no profile is ever implied.
                   A non-loopback address is refused unless VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 is
                   set AND node keys are configured: this server compiles Rhai the client sends.
                   The DATA verbs (LoadBars, ListSeries, Backfill, ...) live on the other daemon,
                   `vike-backend datahub`, and are refused here by name
  PROFILE.toml     the harness profile TOML (data slice + engine costs + strategy), as the first
                   POSITIONAL argument. REQUIRED unless --list. A profile with a [sweep] table
                   runs a parameter search instead of one backtest.
  --profile PATH   the same thing, the older spelling. KEPT: `vike-cli backtest --local` SPAWNS
                   this binary with it. Giving BOTH is an error — they may name two different
                   files, so it is refused rather than resolved.
  --store DIR      hist-store root; falls back to $VIKE_HIST_STORE, then <repo>/market_data/hist
  --json           print the report as pretty JSON instead of the human table
  --list           print every registered strategy name and exit — no store, no profile needed
  data <sub>       get data INTO the store, out of it, and OUT of existence. The operator-facing
                   spelling is `vike-cli data <sub>`, which spawns this; on a box with only the
                   engine, `backtest data <sub>` is the same thing typed directly. It replaced
                   the five flags --seed-demo/--fetch/--fetch-starter/--export/--rm-series, each
                   of which is now refused by name
  --rank-by M      sweep ranking metric (default sharpe); `multi` is the composite objective.
                   Ignored — not an error — on a profile with no [sweep] table
  --optimizer M    the parameter-search METHOD: grid (default, the exhaustive cartesian product),
                   euler (bounded successive-halving refinement), tpe (Bayesian ask/tell) or
                   genetic (a population search over the grid's own points, which needs --seed).
                   Replaces the retired --search, which is now an error naming this flag
  --euler-depth N  euler halving depth (default 3). EULER ONLY — refused, not discarded, under
                   another method; past the cap it is refused rather than silently clamped
  --trials N       tpe trial budget (default 64). TPE ONLY — refused under another method
  --seed S         the reproducibility seed. TPE AND GENETIC — refused under grid and euler. On
                   tpe it defaults to 0; on genetic it is REQUIRED, because a genetic run reports
                   one sample of a distribution and a seed nobody typed is a constant the answer
                   silently depends on
  -h, --help       print this and exit 0
  -V, --version    print the version and exit 0";

/// What `backtest data --help` prints — the five operations ruling 12 moved, as one subcommand.
///
/// A const of its own rather than five more rows in [`USAGE`], because that is the shape of the
/// move: `backtest --help` now names ONE line where it named five, and the detail lives with the
/// verb that carries it. The wording mirrors `crates/vike-cli/src/cmd/data.rs`'s `USAGE` on
/// purpose — the same five operations, spelled the same way, reached by whichever of the two
/// binaries the operator has.
const DATA_USAGE: &str = "\
usage: backtest data fetch VENUE:SYMBOL:INTERVAL (--days N | --from LABEL --to LABEL) [--store DIR]
       backtest data fetch-starter [--store DIR]
       backtest data seed-demo [--store DIR]
       backtest data export VENUE:SYMBOL:INTERVAL --out FILE [--from LABEL] [--to LABEL]
                [--store DIR]
       backtest data rm --kind K --venue V (--symbol S [--interval I] | --group G)
                [--produced-by PREFIX] [--dry-run] [--yes] [--store DIR] [--json]

⚠ `vike-cli data <sub>` is the operator-facing spelling and takes the same words; it SPAWNS this
  binary, because opening a hist store needs DataFusion and that binary links none. Use this one
  directly on a box that has an engine and no vike-cli.

  fetch SPEC       pull REAL public bars into the store and exit — no credentials needed.
                   SPEC is VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h). Needs a window:
                   --days N counts back from now, or --from/--to take epoch-ms or YYYY-MM-DDTHH.
                   Re-fetching the same window writes nothing. Requires the `venue-fetch`
                   feature — the release binary and the container image have it
  fetch-starter    download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue and
                   no credentials) and load it. For a box a venue cannot be reached from —
                   a geoblock, a locked-down network. Verified against its published SHA256SUMS;
                   safe to re-run. Same `venue-fetch` feature as fetch
  seed-demo        write the SYNTHETIC demo tape into the store and exit. Venue `demo`, a
                   closed-form curve, NOT market data; it is the slice the shipped
                   `user_data/profiles/backtest.toml` names, so a fresh install can run that
                   profile immediately. Safe to re-run: a second seed writes nothing
  export SPEC      write one series from the store to a standalone Parquet file (--out FILE),
                   optionally bounded by --from/--to — INDEPENDENTLY, unlike fetch's window:
                   an export slices what the store already holds, so one bound alone is
                   meaningful and neither is required. Any venue the store holds, `demo` included
  rm               DELETE stored series, IRREVERSIBLY, and exit. Selects on the four series
                   dimensions: --kind and --venue are REQUIRED, and an omitted
                   --symbol/--group/--interval is a wildcard over that dimension. Prints the
                   PLAN first — the resolved store root and the rung that chose it, then every
                   matched series with its rows/bytes/days and the commit keys that wrote it.
                   --produced-by asserts that EVERY key of EVERY matched series carries that
                   prefix (a producer path from STORE_KINDS resolves to its prefix); one foreign
                   key refuses the whole run and deletes nothing. It is REQUIRED for a sweep and
                   optional for a fully-named series. --dry-run stops after the plan; otherwise
                   --yes, or type `delete N series` at a terminal. There is no --force

  --store DIR      hist-store root; falls back to $VIKE_HIST_STORE, then <repo>/market_data/hist
  --json           on `rm`, print the plan/outcome document instead of the human rendering
  -h, --help       print this and exit 0";

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
    studio: Option<crate::compute_server::StudioRunTable>,
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

    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` instead of
    // vike-log's `<exe_dir>/logs` last resort (`target/debug/logs/…`, which `cargo clean` deletes).
    // `$VIKE_LOG_DIR` still wins; no project above the CWD still lands beside the exe.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "backtest".to_string(),
        project_dir: std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_log_dir(&cwd)),
        ..Default::default()
    });

    if has_flag(args, "--list") {
        for name in harness::STRATEGIES {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }
    // ⚠ **RULING 12 — the five data-management operations left this binary's FLAG surface.**
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.7: `--fetch`,
    // `--fetch-starter`, `--seed-demo`, `--export` and `--rm-series` fetch from venues, write the
    // store, export from it and DELETE from it — none of which is backtesting — so their
    // operator-facing spelling is `vike-cli data <sub>` now.
    //
    // ⚠ **What moved is the SURFACE, not the code, and that distinction is load-bearing.** Every
    // one of these opens a `DataFusionHist`, and `vike-cli` is DataFusion-FREE by construction
    // (its manifest argues every edge; CI's `light-consumers` lane asserts it). So `vike-cli data`
    // reaches them by SPAWNING this binary — `crates/vike-cli/src/cmd/data.rs`'s `engine_argv` and
    // `rm_engine_argv` build exactly the argv below. Deleting the implementation would delete
    // `vike-cli data fetch|seed-demo|rm` with it. A reader who "finishes" the ruling by removing
    // this arm breaks the verb the ruling moved the work TO.
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
    // …and the five old spellings, refused by name. `crates/vike-backtest/src/backtest_cli.rs`'s
    // `parse_search_flags` retired `--search` the same way and carries the argument: a second
    // spelling that can be given a DIFFERENT value is a resolution nobody can make safely, and an
    // alias would keep the old surface forever. ARGV TRIAGE — nothing is opened, per the rule
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
        // resolves constants CRATE-wide, so importing `vike_model::state_path::USER_DATA_DIR_ENV`
        // would make this read invisible to `crates/vike-ops/tests/settings_registry.rs`. That is
        // the same trade `vike-cli`'s dispatcher makes for the same variable.
        vike_model::state_path::project_user_data_dir_from(
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
    // connection can be accepted, and `install` is once-per-process.
    match parse_addr_flag(args) {
        Ok(AddrFlag::Absent) => {}
        Ok(flag) => return run_serve(vars, args, flag, studio),
        Err(e) => {
            eprintln!("backtest: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    // ⚠ **ARGV TRIAGE BEFORE ANY I/O**, and it is a design property rather than tidiness: refusing
    // a command line must not load a profile and must not open (which means CREATE — every
    // `DataFusionHist::open` `create_dir_all`s its root) a store. It is the rule `--help` and
    // `--version` already obey at the top of this function, extended to the flags that name WORK.
    // `crates/vike-backtest/tests/optimizer_cli.rs`'s `a_refused_flag_never_opens_the_store` is what
    // makes it assertable: it names a `--store` path that must not exist afterwards.
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

    let profile = match BacktestProfile::from_path(&PathBuf::from(&profile_path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("backtest: failed to load profile {profile_path:?}: {e}");
            return ExitCode::from(2);
        }
    };

    // ⚠ A SEARCH WAS ASKED FOR AND THERE IS NOTHING TO SEARCH. Every one of these flags used to be
    // read INSIDE `if profile.is_sweep()` below, so `backtest run.toml --optimizer tpe --trials 500`
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
    if search.requested && !profile.is_sweep() {
        eprintln!(
            "backtest: --optimizer names a parameter SEARCH, but {profile_path:?} has no [sweep] \
             table — there is nothing to search. Add one, or drop the search flags"
        );
        return ExitCode::from(2);
    }

    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("backtest: failed to open hist store at {root:?}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ONE handle, UNSIZED ONCE. `run_backtest` and every evaluator constructor take
    // `Arc<dyn HistStore + Send + Sync>`; the ladder this replaced wrote `Arc::new(store)` at FOUR
    // call sites, one per arm, and coerced at each. The annotation on this `let` is what performs
    // the coercion now, and it is why `vike_data::HistStore` is imported at the top of this file.
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);

    if profile.is_sweep() {
        // The `(objective, label)` pair, built ONCE. The ladder spelled this block VERBATIM TWICE
        // (the tpe arm and the euler arm) and a THIRD way inline in the grid arm.
        //
        // ⚠ Declared BEFORE the evaluator: `StoreEvaluator::new` borrows the objective for the
        // evaluator's whole life, and locals drop in reverse declaration order.
        let (objective, label) = match search.rank {
            RankChoice::Metric(m) => (m.objective(), m.name().to_string()),
            RankChoice::Multi => {
                (crate::objective::multi_metric(Default::default()), "multi".to_string())
            }
        };

        // ⚠ Resolved HERE, once, at the composition root — not inside three library adapters. The
        // concrete adapters (`run_sweep`/`run_sweep_with`/`run_sweep_euler`) still call
        // `SweepExec::from_env` themselves for the datahub and their own tests, so the
        // `("vike-backtest", "VIKE_SWEEP_SEQUENTIAL")` row on
        // `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` does not move. Calling the
        // existing `from_env()` from here adds no `env::var` literal for the scanner to find, so
        // that gate sees nothing new either — and it must stay `from_env()`: a `from_vars(&map)`
        // twin would need a SECOND registry row for the same `(name, krate)` pair with a different
        // layer.
        //
        // ⚠ TPE used to be handed a hard-coded `SweepExec::Sequential` and now gets whatever this
        // resolves. The output is byte-identical BY THE POOL RULE, not by the exec value:
        // `StoreEvaluator::evaluate` enters rayon only when `exec == Parallel && batch.len() > 1`,
        // and `TpeSearch::search` submits width-1 batches because each proposal reads the whole
        // observation history. Stated rather than assumed, because if that clause ever loosens TPE
        // silently gains a pool per single backtest.
        let exec = harness::SweepExec::from_env();

        // ⚠ **ONE evaluator, and WHICH CONSTRUCTOR is a PRESERVATION decision rather than a
        // preference.** `grid` with one of the four classic `--rank-by` metrics is the ONE
        // combination whose rows have their `score` CLEARED (`harness::report_from_outcome`'s
        // `RankBy::Metric` arm), and it is EXACTLY the invocation
        // `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm spawns and prints verbatim —
        // with `scripts/cli_mcp_smoke.sh` running it. Building one uniform objective evaluator would
        // move that shipped wire TWICE over: every row would gain a `score` key, and `rank_by`
        // would change STRING for three of the four metrics, because `RankMetric` serializes
        // `snake_case` (`total_return`) while `RankMetric::name` answers with the CLI short name
        // (`return`) and `RankBy` is `#[serde(untagged)]`.
        //
        // euler and tpe have ALWAYS stamped a score under a metric rank — both build
        // `m.objective()` and label it `m.name()` — and this keeps that true too. Keying on
        // `--rank-by` alone would have preserved one promise by retracting the other. The two-input
        // match is what keeps both, and it is still ONE construction site and ONE evaluator value:
        // the scope item is "not one per arm", and this is not one per arm.
        let eval = match (search.method, search.rank) {
            (Method::Grid, RankChoice::Metric(m)) => {
                harness::StoreEvaluator::classic(&profile, store, m, exec)
            }
            _ => harness::StoreEvaluator::new(&profile, store, &objective, label, exec),
        };
        let eval = match eval {
            Ok(e) => e,
            // The SAME prefix the adapters produced, so nothing an operator greps changes: today
            // the params-not-a-table refusal already comes out of this constructor inside
            // `run_sweep_exec` and reaches stderr as `backtest: sweep run failed: …`.
            Err(e) => {
                eprintln!("backtest: sweep run failed: {e}");
                return ExitCode::FAILURE;
            }
        };

        // ONE construction match. PR 3's genetic method costs exactly one arm here and one in
        // `parse_search_flags`.
        let method = optimizer_for(search.method);

        // ONE call, through the ONE door: `optimize` runs `require_overridable_params` and
        // `accepts` before any loop starts, which is what makes `PointEvaluator::evaluate`
        // infallible by type. Nothing may call `Optimizer::search` directly.
        let harness::Optimized { report: sweep_report, summary } =
            match harness::optimize(method.as_ref(), &profile, &eval) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("backtest: sweep run failed: {e}");
                    return ExitCode::FAILURE;
                }
            };

        // The METHOD's own cost line, on stderr so a `--json` stdout stays a clean document, and
        // BEFORE the report — where both hand-written copies printed theirs. euler's is
        // `EulerBudget`'s `Display` and tpe's is the line `TpeSearch::search` now assembles from its
        // own config and its own ranked rows; the grid says nothing, exactly as before.
        if let Some(line) = summary {
            eprintln!("{line}");
        }

        // ONE serialize-or-print tail. The tpe arm copied this block WHOLESALE before its early
        // `return`; that copy and that `return` are gone.
        if has_flag(args, "--json") {
            match serde_json::to_string_pretty(&sweep_report) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("backtest: failed to serialize sweep report: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            print!("{sweep_report}");
        }

        return ExitCode::SUCCESS;
    }

    // The run's own clock read, taken BEFORE the work: it is both the manifest's `started_at` and
    // the seconds half of the run id, so the directory name and the document inside it cannot
    // disagree about when this run began. Read HERE rather than inside `crate::runs`
    // because a crate carrying `backtest == paper == live` contains no ambient clock read at all —
    // `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` is the gate, and time is an INPUT.
    let started_at = now_unix_secs();
    // `store` is ALREADY the trait object (one `let`, one coercion, above the sweep branch), so the
    // single-run path hands it over as-is rather than re-wrapping a concrete handle.
    let result = match harness::run_backtest(&profile, store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("backtest: run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Read here rather than after printing: `finished_at` must measure the RUN, not the terminal.
    let finished_at = now_unix_secs();

    let report = harness::BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    );

    if has_flag(args, "--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("backtest: failed to serialize report: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{report}");
    }

    // ⚠ **AFTER printing, and never fatal.** Persisting is additive: a run whose directory cannot
    // be created has still computed its numbers, and they have already reached stdout by the time
    // this line runs. The failure is NAMED on stderr rather than swallowed — an operator who
    // believes their runs are being saved and finds an empty `user_data/runs/` is worse off than
    // one who was told — but it does not change the exit code, because refusing to exit 0 over a
    // filesystem that would not take a metadata file would make saving a run a new way to lose one.
    //
    // Both lines go to stderr for the reason every other diagnostic in this binary does: `--json`
    // stdout is a machine contract and stays byte-identical.
    match persist_run(&profile, &profile_path, &root, &report, started_at, finished_at) {
        Ok(dir) => eprintln!("backtest: run saved to {}", dir.display()),
        Err(why) => eprintln!("backtest: run NOT saved — {why}"),
    }

    ExitCode::SUCCESS
}

/// Write the finished run to `<project>/user_data/runs/<run_id>/` — `manifest.json` (the COMMON
/// manifest every producer writes identically) beside `report.json` (this run's `BacktestReport`,
/// unchanged).
///
/// ⚠ **The path is resolved HERE, in the binary.** `vike_model::state_path::user_runs_dir` walks up
/// from the working directory for the project marker — the SAME walk every sibling `user_data/`
/// resolver uses, so a run cannot land in one project while the strategy that produced it was read
/// from another. A library may not make that call (`crates/vike-ops/tests/settings_registry.rs`'s
/// `LIBRARY_PIN`): it would be reading global state its caller can neither see nor override, which
/// is why `crate::runs` takes the directory as a parameter and resolves nothing.
///
/// ⚠ **DECLARED RESIDUAL: `VIKE_USER_DATA_DIR` does not move the runs directory.** It redirects the
/// `user_data` root for the indicator load above (through `project_user_data_dir_from`), and
/// `user_runs_dir` has no override parameter, so an operator who redirects `user_data` gets their
/// indicators from the override and their runs from the project. Closing that means an
/// override-aware `user_runs_dir_from` beside `RUNS_SUBDIR` in `vike-model` — that crate's edit,
/// not this one's.
///
/// Failures come back as a STRING rather than as an error type: every one of them ends up in the
/// same one-line stderr message, and there is no caller that could act on the difference.
fn persist_run(
    profile: &BacktestProfile,
    profile_path: &str,
    store_root: &Path,
    report: &harness::BacktestReport,
    started_at: i64,
    finished_at: i64,
) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let runs_root = vike_model::state_path::user_runs_dir(&cwd)
        .ok_or_else(|| format!("no project directory above {}", cwd.display()))?;

    // Creating the directory is what MINTS the id, which is why it happens before the manifest is
    // built rather than after — `crate::runs::RunManifest::run_id` carries that argument.
    let run = runs::create_run_dir(&runs_root, started_at).map_err(|e| e.to_string())?;

    let manifest = runs::RunManifest {
        run_id: run.run_id.clone(),
        kind: "backtest".to_string(),
        // The BINARY's name, spelled literally — the same distinction `--version` above makes, and
        // for the same reason: `CARGO_PKG_NAME` is `vike-backtest`, which is not what was invoked.
        produced_by: "backtest".to_string(),
        started_at: runs::utc_rfc3339(started_at),
        finished_at: runs::utc_rfc3339(finished_at),
        // ⚠ `None`, stated rather than left to be inferred. Naming the commit this binary was built
        // from means `vike-buildinfo`, which resolves it at COMPILE time from its own `build.rs`,
        // and `vike-backtest` does not depend on that crate. Shelling `git rev-parse` here instead
        // would answer about the working DIRECTORY rather than about this binary — exactly the
        // confusion that crate exists to end — so the honest answer is that this producer cannot
        // name its build. The key is still written (as `null`), because "does not know" and "wrote
        // no such field" are different answers to a reader.
        git_sha: None,
        config: runs::RunConfig {
            // As the operator spelled it, not canonicalized: this is the string they can paste back.
            path: Some(profile_path.to_string()),
            name: profile.name.clone(),
        },
        // Everything below here is a BACKTEST's business and nests, so a listing that has never
        // heard of a backtest still renders every field above it.
        detail: serde_json::json!({
            "strategy": profile.strategy.name,
            "data": {
                "kind": match profile.data.kind {
                    harness::DataKind::Bar => "bar",
                    harness::DataKind::Tick => "tick",
                },
                "interval": profile.data.interval,
                "from": profile.data.from,
                "to": profile.data.to,
                // `resolved_series` so BOTH profile spellings — `venue` × `symbols` and the
                // cross-venue `[[data.series]]` array — record the same thing: what was loaded.
                "series": profile
                    .data
                    .resolved_series()
                    .iter()
                    .map(|s| format!("{}:{}", s.venue, s.symbol))
                    .collect::<Vec<_>>(),
            },
            // Which hist store the run read. The profile names a slice; only this says where the
            // bytes behind it came from, and `--store`/`$VIKE_HIST_STORE`/the repo default are
            // three different answers on one box.
            "store": store_root.display().to_string(),
        }),
    };

    runs::write_run(&run.path, &manifest, report).map_err(|e| e.to_string())?;
    Ok(run.path)
}

// ─── `backtest data`: the five operations ruling 12 moved off the flag surface ───────────────────

/// The verb that carries them. Spelled once, because [`run`] routes on it and
/// [`profile_from_args`] refuses it as a POSITIONAL for exactly that reason.
const DATA_SUBCOMMAND: &str = "data";

/// The five data-management flags ruling 12 retired, and what each became: `(flag, engine_sub,
/// replacement, tail)`. `tail` is the one sentence that is not shared.
///
/// ⚠ **`--rm-series`' tail says NOTHING WAS DELETED in as many words, and that is the reason this
/// is a table of four columns rather than three.** An operator whose cleanup script now exits 2 must
/// not be left reading the refusal as "it may have partially run" — a delete verb's refusal owes
/// that sentence in a way a fetch's does not.
///
/// ⚠ **`engine_sub` is a COLUMN because deriving it was wrong on the one row that deletes.** It was
/// computed as `flag.trim_start_matches("--")`, which is the [`DATA_SUBS`] name for four of the
/// five rows and `rm-series` for the fifth — a subcommand [`triage_data_argv`] refuses. So the
/// refusal for the retired DELETE flag sent an engine-only operator to `backtest data rm-series`,
/// which answers "unknown `data` subcommand". A `contains` test could not see it either, because
/// `"rm-series"` contains `"rm"`; `the_retired_sub_is_a_real_data_subcommand` compares against
/// [`DATA_SUBS`] instead, which is the table that decides.
///
/// A table rather than five hand-written `if`s so [`refuse_a_retired_data_flag`] and this file's own
/// `every_retired_data_flag_names_its_replacement` iterate the same rows.
const RETIRED_DATA_FLAGS: &[(&str, &str, &str, &str)] = &[
    ("--fetch", "fetch", "vike-cli data fetch VENUE:SYMBOL:INTERVAL --days N", ""),
    ("--fetch-starter", "fetch-starter", "vike-cli data fetch-starter", ""),
    ("--seed-demo", "seed-demo", "vike-cli data seed-demo", ""),
    ("--export", "export", "vike-cli data export VENUE:SYMBOL:INTERVAL --out FILE", ""),
    (
        "--rm-series",
        "rm",
        "vike-cli data rm --kind K --venue V …",
        " NOTHING WAS DELETED — this refusal happened before a store was opened.",
    ),
];

/// Refuse one of the five retired spellings by name, echoing what was written.
///
/// `None` when argv carries none of them, so [`run`] falls through unchanged. Checked in
/// [`RETIRED_DATA_FLAGS`] order, so a line carrying two of them names the first — which is enough:
/// the message tells the operator the whole family moved.
///
/// ⚠ [`flag_given`], not `has_flag`: `--fetch=binance:BTCUSDT:1h` is a spelling `arg` accepts and
/// `has_flag` never sees, and a retirement that missed the inline form would let exactly the
/// scripted invocations through — the ones nobody is watching.
///
/// ⚠ **The adjacent-prefix landmine, checked**: `flag_given(args, "--fetch")` is NOT tripped by
/// `--fetch-starter` (`has_flag` is exact-token and `arg` is anchored on `--fetch=`), which is why
/// the two can be separate rows answering with separate replacements rather than one row that
/// misnames half the traffic. [`flag_given`]'s own doc carries the same check for `--seed`.
fn refuse_a_retired_data_flag(args: &[String]) -> Option<ExitCode> {
    let (flag, sub, replacement, tail) =
        RETIRED_DATA_FLAGS.iter().find(|(f, _, _, _)| flag_given(args, f))?;
    let inline = format!("{flag}=");
    let written = args
        .iter()
        .find(|a| a.as_str() == *flag || a.starts_with(&inline))
        .cloned()
        .unwrap_or_else(|| (*flag).to_string());
    eprintln!(
        "backtest: {flag} is retired — data management moved to `vike-cli data` (ruling 12 of \
         docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md). You wrote \
         {written:?}; write `{replacement}` instead, or `backtest {DATA_SUBCOMMAND} {sub}` on a \
         box with only this engine.{tail}"
    );
    Some(ExitCode::from(2))
}

/// Every flag `backtest data` accepts, and whether it takes a VALUE.
///
/// ⚠ It exists so [`triage_data_argv`] can refuse an unknown option and an inline value on a
/// BOOLEAN — and the second half is a live defect being closed, not tidiness.
/// `vike_analytics::binutil::has_flag` is exact-token by design and its own doc declares the
/// residual: "`--json=1`, `--yes=true` and `--dry-run=yes` are still silently ignored on every bin
/// in this family". On THIS verb that residual DELETES: `--rm-series --yes --dry-run=1` ran the
/// removal, because `--dry-run=1` is not the token `has_flag` looks for while `--yes` is. Ruling 12
/// moves the operator-facing surface onto the parser that already refuses both spellings
/// (`crates/vike-cli/src/cmd/args.rs`'s `no_value`), and this table is what gives the engine's own
/// door the same strength rather than leaving it open behind the CLI's.
const DATA_FLAGS: &[(&str, bool)] = &[
    ("--store", true),
    ("--json", false),
    ("--days", true),
    ("--from", true),
    ("--to", true),
    ("--out", true),
    ("--kind", true),
    ("--venue", true),
    ("--symbol", true),
    ("--group", true),
    ("--interval", true),
    ("--produced-by", true),
    ("--dry-run", false),
    ("--yes", false),
];

/// The `data` subcommands, and whether each takes a `VENUE:SYMBOL:INTERVAL` positional.
const DATA_SUBS: &[(&str, bool)] = &[
    ("fetch", true),
    ("fetch-starter", false),
    ("seed-demo", false),
    ("export", true),
    ("rm", false),
];

/// ARGV TRIAGE for `backtest data <sub>`, returning the one positional the subcommand takes.
///
/// PURE — no store, no profile, no environment, no clock — and it runs BEFORE any arm, so a refused
/// command line opens (which means CREATES — every `DataFusionHist::open` `create_dir_all`s its
/// root) nothing. That is the rule `crates/vike-backtest/tests/optimizer_cli.rs`'s
/// `a_refused_flag_never_opens_the_store` pins, extended to this verb.
///
/// What it judges and what it deliberately does not: it judges the SHAPE of the command line — an
/// unknown option, a boolean given a value, a valued flag given none, a positional where the
/// subcommand takes none (or missing where it does). It judges no VALUE: which venues exist, which
/// kinds the store partitions by and what a window means are the arms' own, and a second roster
/// here would be a second list to keep in step. That is the split
/// `crates/vike-cli/src/cmd/data.rs`'s module doc already draws between shape and roster.
fn triage_data_argv(sub: &str, rest: &[String]) -> Result<Option<String>, String> {
    let (_, takes_spec) = DATA_SUBS.iter().find(|(name, _)| *name == sub).ok_or_else(|| {
        format!(
            "unknown `data` subcommand '{sub}' (expected {})",
            DATA_SUBS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" | ")
        )
    })?;

    let mut spec: Option<String> = None;
    let mut it = rest.iter();
    while let Some(token) = it.next() {
        if token.starts_with("--") {
            let (name, inline) = match token.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v)),
                None => (token.clone(), None),
            };
            let Some((_, valued)) = DATA_FLAGS.iter().find(|(f, _)| *f == name) else {
                return Err(format!("unknown option '{token}' on `data {sub}`"));
            };
            if !*valued {
                // ⚠ The `--dry-run=1` hole, closed. See [`DATA_FLAGS`].
                if inline.is_some() {
                    return Err(format!(
                        "{name} takes no value, and {token:?} was SILENTLY IGNORED before — which \
                         on `data rm` meant a run written as a rehearsal performed the deletion"
                    ));
                }
                continue;
            }
            if inline.is_some() {
                continue;
            }
            match it.next() {
                Some(v) if v.starts_with("--") => {
                    return Err(format!(
                        "{name} requires a value, but the next argument is another flag ({v})"
                    ));
                }
                Some(_) => continue,
                None => return Err(format!("{name} requires a value")),
            }
        }
        match &spec {
            None => spec = Some(token.clone()),
            Some(already) => {
                return Err(format!(
                    "unexpected extra argument '{token}' (the spec is already '{already}')"
                ));
            }
        }
    }

    match (*takes_spec, &spec) {
        (true, None) => Err(format!(
            "`data {sub}` needs a VENUE:SYMBOL:INTERVAL spec, e.g. `backtest data {sub} \
             binance:BTCUSDT:1h …`"
        )),
        (false, Some(extra)) => {
            Err(format!("'{extra}': `data {sub}` takes no VENUE:SYMBOL:INTERVAL spec"))
        }
        _ => Ok(spec),
    }
}

/// Route `backtest data <sub>`. `rest` is everything after the `data` word.
///
/// Each arm is the SAME function the retired flag called, handed the sub-slice — so this is a
/// rename of the door, never a second implementation. The `venue-fetch` refusals move with their
/// arms and name the new spelling.
fn run_data(
    vars: &std::collections::HashMap<String, String>,
    rest: &[String],
    now_unix_secs: &dyn Fn() -> i64,
) -> ExitCode {
    if rest.first().is_none_or(|a| a == "-h" || a == "--help" || a == "help") {
        // stdout + exit 0, the rule `--help` already obeys above: help is output a user pipes into
        // a pager, not a diagnostic. A bare `backtest data` is the same question with no verb.
        println!("{DATA_USAGE}");
        return ExitCode::SUCCESS;
    }
    let sub = rest[0].clone();
    let args = &rest[1..];
    let spec = match triage_data_argv(&sub, args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest data: {e}\n\n{DATA_USAGE}");
            return ExitCode::from(2);
        }
    };
    let _ = (&spec, now_unix_secs);

    match sub.as_str() {
        "seed-demo" => run_seed_demo(vars, args),
        "rm" => run_rm_series(vars, args),
        "fetch" => {
            #[cfg(feature = "venue-fetch")]
            {
                run_fetch(vars, args, spec.as_deref().unwrap_or_default(), now_unix_secs)
            }
            #[cfg(not(feature = "venue-fetch"))]
            {
                eprintln!(
                    "backtest data fetch: this build has no venue fetch — it was compiled without \
                     the `venue-fetch` feature, so it can reach no venue. The shipped release \
                     binary and the container image both have it; a `cargo build` of this crate \
                     does not unless you ask for it. `backtest data seed-demo` needs no network \
                     and works in every build."
                );
                ExitCode::from(2)
            }
        }
        "fetch-starter" => {
            #[cfg(feature = "venue-fetch")]
            {
                run_fetch_starter(vars, args)
            }
            #[cfg(not(feature = "venue-fetch"))]
            {
                eprintln!(
                    "backtest data fetch-starter: this build has no network fetch — it was \
                     compiled without the `venue-fetch` feature. The shipped release binary and \
                     the container image both have it. `backtest data seed-demo` needs no network \
                     and works in every build."
                );
                ExitCode::from(2)
            }
        }
        "export" => {
            #[cfg(feature = "venue-fetch")]
            {
                run_export(vars, args, spec.as_deref().unwrap_or_default())
            }
            #[cfg(not(feature = "venue-fetch"))]
            {
                eprintln!(
                    "backtest data export: this build has no export — it was compiled without the \
                     `venue-fetch` feature, which carries the Parquet writer's caller."
                );
                ExitCode::from(2)
            }
        }
        // Unreachable: [`triage_data_argv`] refused every other spelling above. Spelled as a
        // refusal rather than `unreachable!()` so a new row in [`DATA_SUBS`] with no arm here is a
        // message instead of a panic in a binary an operator is running against their own store.
        other => {
            eprintln!("backtest data: '{other}' has no arm in this build\n\n{DATA_USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `data seed-demo` — the SYNTHETIC tape, and the answer to an empty store that needs no network.
///
/// ⚠ THE EMPTY-STORE ANSWER, and it needs no profile and no strategy registry, only a store root.
///
/// A clean install has an empty hist store, so the shipped example profile — which names a slice —
/// reports a run with no trades, and nothing distinguishes that from a strategy that never fired.
/// `vike_data::demo` writes a tape the shipped profile already names;
/// `crates/vike-cli/tests/demo_tape_profile.rs` holds the two in agreement, so this writes not
/// "some data" but THE data the next command reads.
///
/// Deliberately on this binary rather than in `vike-cli`: writing a store needs `DataFusionHist`,
/// and vike-cli is DataFusion-free BY CONSTRUCTION (the `light-consumers` CI lane asserts it). The
/// tool that CONSUMES hist data is the honest place for the command that creates some — which is
/// why ruling 12 moved the SPELLING and not this function.
fn run_seed_demo(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    let seeded = match demo_tape::seed(&store) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: seeding {} failed: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    // The provenance line FIRST, because a synthetic tape that is not announced as one is the
    // hazard this whole feature carries: a result computed on invented prices reads exactly like a
    // result computed on real ones.
    println!(
        "seeded SYNTHETIC demo bars (venue `{}` — a closed-form curve, NOT market data) into {}",
        demo_tape::DEMO_VENUE,
        root.display()
    );
    let mut written = 0usize;
    for done in &seeded {
        written += done.rows;
        println!(
            "  {}/{} {}  {} bars{}",
            demo_tape::DEMO_VENUE,
            done.slice.symbol,
            done.slice.interval,
            done.slice.len(),
            if done.rows == 0 { "  (already present — nothing written)" } else { "" }
        );
    }
    if written == 0 {
        println!("this store already held the demo tape; nothing was written.");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod data_subcommand_tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    /// Every retired flag names a live replacement, in both written spellings, and refuses. The
    /// TABLE is what is iterated, so a sixth retirement joins by adding a row.
    #[test]
    fn every_retired_data_flag_names_its_replacement() {
        for (flag, _, replacement, _) in RETIRED_DATA_FLAGS {
            assert!(
                replacement.starts_with("vike-cli data "),
                "{flag} must name the verb the ruling moved it to, got {replacement:?}"
            );
            for written in [(*flag).to_string(), format!("{flag}=x")] {
                assert!(
                    refuse_a_retired_data_flag(std::slice::from_ref(&written)).is_some(),
                    "{written} must be refused, not read as absent"
                );
            }
        }
        assert!(
            refuse_a_retired_data_flag(&argv(&["--json"])).is_none(),
            "an unrelated flag falls through"
        );
    }

    /// ⚠ The adjacent-prefix pair, pinned: `--fetch-starter` must refuse under its OWN row, naming
    /// its own replacement, rather than under `--fetch`'s. `has_flag` is exact-token and `arg` is
    /// anchored on `--fetch=`, so this holds — and it is the property that would break first if
    /// either helper were loosened.
    #[test]
    fn fetch_starter_is_not_swallowed_by_the_fetch_row() {
        let args = argv(&["--fetch-starter"]);
        let hit = RETIRED_DATA_FLAGS.iter().find(|(f, _, _, _)| flag_given(&args, f));
        assert_eq!(hit.map(|(f, _, _, _)| *f), Some("--fetch-starter"));
    }

    /// ⚠ **The engine subcommand a retirement names must be one [`DATA_SUBS`] actually serves.**
    ///
    /// It was DERIVED (`flag.trim_start_matches("--")`), which is right for four rows and wrong for
    /// the fifth: `--rm-series` yielded `rm-series`, so the refusal for the one retired flag that
    /// DELETES told an engine-only operator to run `backtest data rm-series`, which
    /// [`triage_data_argv`] answers with "unknown `data` subcommand". The end-to-end test could not
    /// catch it — it asserts the message CONTAINS the sub, and `"rm-series"` contains `"rm"` — so
    /// the check has to be against the table that decides rather than against the message.
    #[test]
    fn the_retired_sub_is_a_real_data_subcommand() {
        for (flag, sub, _, _) in RETIRED_DATA_FLAGS {
            assert!(
                DATA_SUBS.iter().any(|(name, _)| name == sub),
                "{flag} names `backtest data {sub}`, which is not a subcommand this binary serves \
                 ({:?})",
                DATA_SUBS.iter().map(|(n, _)| *n).collect::<Vec<_>>()
            );
        }
    }

    /// The triage refuses SHAPE and nothing else: an unknown option, a boolean given a value, a
    /// valued flag given none, and the positional's presence-or-absence per subcommand.
    #[test]
    fn the_triage_refuses_shape_and_defers_every_value() {
        assert_eq!(
            triage_data_argv("fetch", &argv(&["binance:BTCUSDT:1h", "--days", "180"])).unwrap(),
            Some("binance:BTCUSDT:1h".to_string())
        );
        assert_eq!(triage_data_argv("seed-demo", &argv(&[])).unwrap(), None);
        // The value is not judged here — a nonsense venue and a nonsense window both pass, and the
        // arm's own error names what it could not do.
        assert_eq!(
            triage_data_argv("fetch", &argv(&["nope:NOPE:99z", "--days", "-4"])).unwrap(),
            Some("nope:NOPE:99z".to_string())
        );

        assert!(
            triage_data_argv("nope", &argv(&[])).unwrap_err().contains("unknown `data` subcommand")
        );
        assert!(
            triage_data_argv("rm", &argv(&["--bogus"])).unwrap_err().contains("unknown option")
        );
        assert!(
            triage_data_argv("rm", &argv(&["--kind"])).unwrap_err().contains("requires a value")
        );
        assert!(
            triage_data_argv("rm", &argv(&["--kind", "--venue"]))
                .unwrap_err()
                .contains("another flag")
        );
        assert!(
            triage_data_argv("export", &argv(&["--out", "x"])).unwrap_err().contains("needs a")
        );
        assert!(triage_data_argv("seed-demo", &argv(&["x:y:z"])).unwrap_err().contains("takes no"));
    }

    /// ⚠ **The `--dry-run=1` hole, and it is the reason this triage exists at all.** `has_flag` is
    /// exact-token, so `--dry-run=1` was silently ignored while `--yes` beside it was honoured —
    /// a command line written as a rehearsal performed the deletion. It is a REFUSAL now, and the
    /// message says what it used to do.
    #[test]
    fn a_boolean_given_a_value_is_refused_rather_than_ignored() {
        for spelling in ["--dry-run=1", "--yes=true", "--json=1"] {
            let err = triage_data_argv("rm", &argv(&["--kind", "bar", "--venue", "x", spelling]))
                .unwrap_err();
            assert!(err.contains("takes no value"), "{spelling}: {err}");
        }
        let err = triage_data_argv(
            "rm",
            &argv(&["--kind", "bar", "--venue", "x", "--dry-run=1", "--yes"]),
        )
        .unwrap_err();
        assert!(err.contains("rehearsal"), "the message says what it used to do: {err}");
    }
}

/// The `data fetch` body. Split out of [`run_data`] because it is the one arm that touches the
/// NETWORK, and a reader auditing what this binary can reach should find that in one place rather
/// than inside a 400-line dispatcher.
///
/// Compiled only under `venue-fetch` — see [`crate::fetch`]'s module doc for why that feature is
/// off by default and must stay so.
#[cfg(feature = "venue-fetch")]
fn run_fetch(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    raw_spec: &str,
    now_unix_secs: &dyn Fn() -> i64,
) -> ExitCode {
    use crate::fetch;

    let spec = match fetch::parse_spec(raw_spec) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    // ONE timestamp parser for the whole binary: the profile loader's. A second implementation
    // here would agree with a profile's `from`/`to` only by luck, and the divergence would show up
    // as a fetched window that does not line up with the window a later run asks for.
    let parse_ts = |s: &str| crate::harness::profile::parse_ts(s).map_err(|e| e.to_string());
    let (from_ms, to_ms) = match fetch::window(
        arg(args, "--days").as_deref(),
        arg(args, "--from").as_deref(),
        arg(args, "--to").as_deref(),
        now_unix_secs,
        &parse_ts,
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let root = fetch::store_root_for(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };

    // Announced BEFORE the call, because this is the one command here that blocks on a remote host
    // for an unbounded time and a silent terminal reads as a hang.
    println!(
        "fetching {}/{} {} from {from_ms} to {to_ms} into {} …",
        spec.venue,
        spec.symbol,
        spec.interval,
        root.display()
    );
    let done = match fetch::fetch_into(&store, &spec, from_ms, to_ms) {
        Ok(d) => d,
        Err(e) => {
            // The venue's own message, verbatim: a geoblock, a delisted symbol and a bad interval
            // all arrive here, they read differently, and paraphrasing them would lose the one
            // detail that tells them apart.
            eprintln!("backtest: fetching {}/{} failed: {e}", spec.venue, spec.symbol);
            return ExitCode::from(2);
        }
    };
    match done.span_ms {
        // A venue answering a NARROWER window than asked is ordinary (listing date, retention), and
        // is invisible unless the answer says what actually arrived.
        Some((first, last)) => println!(
            "  {} bars returned, covering {first}..={last}; {} rows written{}",
            done.fetched,
            done.written,
            if done.written == 0 { " (this window was already present)" } else { "" }
        ),
        None => println!(
            "  the venue returned NO bars for this window — check the symbol, the interval, and \
             whether the instrument existed then."
        ),
    }
    ExitCode::SUCCESS
}

/// `data fetch-starter` — the published dataset, for a box that cannot reach a venue.
///
/// Compiled only under `venue-fetch`; see [`crate::starter`] for why the same feature covers both
/// and what the download does and does not verify.
#[cfg(feature = "venue-fetch")]
fn run_fetch_starter(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> ExitCode {
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    // ⚠ `<project>/tmp`, NEVER the operating system's temp directory — production scratch must land
    // where the project folder is, and `crates/vike-ops/tests/system_temp_gate.rs` refuses
    // otherwise. Two reasons it gives, both about a box that is not this one: inside the container
    // the system temp is not the host's and does not survive a restart, so the same path resolves
    // somewhere else on an operator's machine, silently; and the system temp is emptied by
    // something we do not control, while a leak in it is invisible until a filesystem fills (26,851
    // leaked scratch directories, 215 GB, measured on the build box).
    //
    // `ScratchDir` owns what it creates and removes it on every path out of here, error ones
    // included — a download that leaks a Parquet file per attempt is the same leak wearing our name.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tmp_root = vike_model::state_path::project_tmp_dir_from(
        vars.get("VIKE_SETTINGS_DIR").map(String::as_str),
        &cwd,
    )
    // No project above the working directory — a bare checkout, or a binary run from elsewhere.
    // The store's own parent is then the honest fallback: it is where this command is writing
    // anyway, so a file that briefly appears beside it cannot surprise anyone.
    .unwrap_or_else(|| root.parent().unwrap_or(&root).join("tmp"));
    if let Err(e) = std::fs::create_dir_all(&tmp_root) {
        eprintln!("backtest: cannot create {}: {e}", tmp_root.display());
        return ExitCode::from(2);
    }
    let scratch = match vike_model::scratch::ScratchDir::create_in(&tmp_root, "starter") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot create a scratch directory in {}: {e}", tmp_root.display());
            return ExitCode::from(2);
        }
    };
    println!("downloading the starter dataset into {} …", root.display());
    match starter::fetch_into(&store, scratch.path(), |m| println!("  {m}")) {
        Ok(done) => {
            let mut rows = 0usize;
            for d in &done {
                rows += d.rows;
                println!(
                    "  {} — {} bytes, sha256 {}, {} rows{}",
                    d.file,
                    d.bytes,
                    d.sha256,
                    d.rows,
                    if d.rows == 0 { "  (already present)" } else { "" }
                );
            }
            if rows == 0 {
                println!("this store already held the starter dataset; nothing was written.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backtest: {e}");
            ExitCode::from(2)
        }
    }
}

/// `data export` — one series out of the store as a standalone Parquet file.
///
/// The producer half of the starter dataset (`scripts/publish_starter_data.sh` calls exactly this),
/// and useful on its own: handing somebody a slice without handing them the store's internals.
#[cfg(feature = "venue-fetch")]
fn run_export(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    raw_spec: &str,
) -> ExitCode {
    let Some(out) = arg(args, "--out") else {
        eprintln!("backtest: --export needs --out FILE");
        return ExitCode::from(2);
    };
    // The same VENUE:SYMBOL:INTERVAL grammar `--fetch` takes, but WITHOUT its venue roster: this
    // reads the store, so any venue the store holds is exportable, including `demo`.
    let parts: Vec<&str> = raw_spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        eprintln!("backtest: --export takes VENUE:SYMBOL:INTERVAL, not {raw_spec:?}");
        return ExitCode::from(2);
    }
    let parse_ts = |s: &str| crate::harness::profile::parse_ts(s).map_err(|e| e.to_string());
    let range = match (arg(args, "--from"), arg(args, "--to")) {
        (None, None) => vike_data::hist::TsRange::all(),
        (from, to) => {
            let conv = |v: Option<String>| -> Result<Option<i64>, String> {
                v.map(|s| parse_ts(&s)).transpose()
            };
            match (conv(from), conv(to)) {
                (Ok(start), Ok(end)) => vike_data::hist::TsRange { start, end },
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };

    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    match store.export_bars_parquet(Path::new(&out), parts[0], parts[1], parts[2], range) {
        Ok(rows) => {
            // The row count is the load-bearing half of this line: a publishing script that
            // exported an EMPTY slice would otherwise upload a valid file nobody can use.
            println!("exported {rows} bars of {}/{} {} to {out}", parts[0], parts[1], parts[2]);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backtest: exporting {raw_spec} failed: {e}");
            ExitCode::from(2)
        }
    }
}

// ─── `data rm`: emptying the store ─────────────────────────────────────────────────────────────

/// `backtest data rm` — DELETE stored series, irreversibly.
///
/// # Why this verb is on the ENGINE
///
/// For `data export`'s reason, one step further: emptying a store needs `DataFusionHist`, and
/// `vike-cli` is DataFusion-FREE by construction (its manifest argues every edge; CI's
/// `light-consumers` lane asserts it). `vike-cli data rm` is a route to this arm, exactly as
/// `vike-cli data fetch` is a route to `data fetch`. It is a first-class verb here rather than
/// only a spawn target because an operator ON the box — which is where a cleanup happens, and
/// where the 2026-09-07 the CI box cleanup DID happen — should not need a datahub to empty their own
/// store.
///
/// # ⚠ The plan LEADS with the store, and it leads on STDOUT
///
/// `binutil::store_root` already logs the resolved root and the rung that chose it, through
/// `tracing::info!` — which `RUST_LOG` silences. "Which store" is the question a destructive verb
/// must answer before "which series", and a store does not MERGE: a resolution that moved is
/// invisible until it has destroyed the wrong tree. So the first line of every run, dry or not,
/// carries `StoreRoot`'s `Display` — the path and the sentence saying why it is that path.
///
/// # The confirmation
///
/// `--yes`, or a TERMINAL on which the operator types `delete N series` for the N this plan
/// matched. Binding the confirmation to a fact of the plan is what makes a line copied from a
/// previous run against a different plan fail to match, with no token and no state.
///
/// ⚠ **No `--yes` and no terminal is a REFUSAL, never a read.** `yes | backtest data rm …` is
/// the exact failure this exists to prevent, and a pipe is indistinguishable from a person once you
/// have decided to read one.
fn run_rm_series(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    use std::io::IsTerminal;
    use vike_data::removal::SeriesSelector;

    // ⚠ FIRST, before the store is opened: the root AND the rung that chose it, on stdout.
    let resolved =
        crate::binutil::store_root_resolved(arg(args, "--store").map(PathBuf::from), vars);
    let json = has_flag(args, "--json");
    // The store LEADS, on stdout for a human and on stderr under `--json` (where stdout is the
    // document and the same fact rides `store_root`/`store_rung` inside it).
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }

    let selector = SeriesSelector {
        kind: arg(args, "--kind").unwrap_or_default(),
        venue: arg(args, "--venue").unwrap_or_default(),
        symbol: arg(args, "--symbol"),
        group: arg(args, "--group"),
        interval: arg(args, "--interval"),
    };
    if let Err(e) = selector.validate_shape() {
        eprintln!("backtest data rm: {e}\n\n{DATA_USAGE}");
        return ExitCode::from(2);
    }
    // Resolved BEFORE the store is opened: a `--produced-by` spelling is a fact about the argument,
    // and refusing a typo without touching a store is the cheaper failure.
    let produced_by =
        match arg(args, "--produced-by").map(|s| vike_data::store_kind::resolve_produced_by(&s)) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                eprintln!("backtest data rm: {e}");
                return ExitCode::from(2);
            }
            None => None,
        };
    // ⚠ The SWEEP rule, and it is the engine's as much as the CLI's: this arm is reachable directly.
    if selector.is_sweep() && produced_by.is_none() {
        eprintln!(
            "backtest data rm: `{}` matches more than one series, so --produced-by is \
             REQUIRED. Deleting a whole sweep by name alone is what that assertion exists to \
             replace; name every dimension instead, or pass the commit-key prefix the rows carry.",
            selector.describe()
        );
        return ExitCode::from(2);
    }

    let store = match DataFusionHist::open(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", resolved.root.display());
            return ExitCode::from(2);
        }
    };
    let plan = match vike_data::removal::plan_removal(&store, &selector, produced_by.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("backtest data rm: {e}");
            return ExitCode::from(2);
        }
    };

    let dry_run = has_flag(args, "--dry-run");
    // ⚠ Under `--json` the plan goes to STDERR, not nowhere. Stdout is the document and nothing
    // else, but a run that is about to ask a human to type `delete N series` must have SHOWN them
    // what N is made of — and without this the operator was prompted having seen no plan at all.
    // Same stream every diagnostic in this workspace uses, and the same shape
    // `crates/vike-cli/src/cmd/engine.rs`'s `run_capturing_stdout` already gives the engine's own
    // lines.
    for line in plan.lines() {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    // A provenance refusal is reported and deletes nothing, whether or not this was a dry run.
    if let Err(refusals) = plan.verdict() {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, None, Some(&refusals)));
        }
        eprintln!("backtest data rm: provenance REFUSED — nothing was deleted");
        return ExitCode::from(2);
    }
    // ⚠ `--dry-run` WINS over `--yes`: a rehearsal must not require stripping a flag.
    if dry_run {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, None, None));
        } else {
            println!("--dry-run: nothing was deleted");
        }
        return ExitCode::SUCCESS;
    }
    // "Nothing matched" is a SUCCESS on the same rung as a delete: `delete_series` is idempotent,
    // and a cleanup that fails on re-run is a cleanup nobody re-runs.
    if plan.matched() == 0 {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, Some(&Default::default()), None));
        }
        return ExitCode::SUCCESS;
    }
    if let Err(e) = confirm_removal(&plan, has_flag(args, "--yes"), std::io::stdin().is_terminal())
    {
        eprintln!("backtest data rm: {e}");
        return ExitCode::from(2);
    }

    match vike_data::removal::execute_removal(&store, &plan) {
        Ok(outcome) => {
            if json {
                println!("{}", rm_series_json(&resolved, &plan, Some(&outcome), None));
            } else {
                for id in &outcome.deleted {
                    println!("deleted {}", vike_data::removal::describe_id(id));
                }
                for (id, why) in &outcome.failed {
                    eprintln!("FAILED {}: {why}", vike_data::removal::describe_id(id));
                }
                println!(
                    "{} of {} series deleted",
                    outcome.deleted.len(),
                    outcome.deleted.len() + outcome.failed.len()
                );
            }
            // One broken series is one SKIPPED series (`run_maintenance`'s rule) — reported, the
            // rest continue, and the exit is non-zero so a wrapper knows to look.
            if outcome.is_clean() { ExitCode::SUCCESS } else { ExitCode::from(2) }
        }
        Err(e) => {
            eprintln!("backtest data rm: {e}");
            ExitCode::from(2)
        }
    }
}

/// The confirmation gate — PURE over its two inputs, so both branches are unit-tested rather than
/// only reachable from a terminal.
///
/// `stdin_is_terminal` is a PARAMETER for that reason; the caller reads the real one.
fn confirm_removal(
    plan: &vike_data::removal::RemovalPlan,
    yes: bool,
    stdin_is_terminal: bool,
) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(format!(
            "refusing to delete {} series without --yes: stdin is not a terminal, so there is \
             nobody to confirm. A confirmation read from a PIPE is not a confirmation — \
             `yes | backtest data rm …` is exactly what this refuses.",
            plan.matched()
        ));
    }
    let want = format!("delete {} series", plan.matched());
    eprintln!("type `{want}` to confirm, or anything else to abort:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| format!("reading the confirmation: {e}"))?;
    if line.trim() == want {
        Ok(())
    } else {
        Err(format!("not confirmed (expected `{want}`) — nothing was deleted"))
    }
}

/// The `--json` document for `data rm`.
///
/// ⚠ It carries the RESOLVED store root and its RUNG, and that is the one fact that cannot survive
/// a prose round trip: `vike-cli data rm --json` wraps this document rather than re-deriving the
/// root, because the CLI resolves nothing (the engine runs in another process) and a path it
/// guessed at would be the confident-sounding wrong answer.
fn rm_series_json(
    resolved: &vike_model::store_path::StoreRoot,
    plan: &vike_data::removal::RemovalPlan,
    outcome: Option<&vike_data::removal::RemovalOutcome>,
    refusals: Option<&[String]>,
) -> String {
    let doc = serde_json::json!({
        "store_root": resolved.root.display().to_string(),
        "store_rung": resolved.rung.as_str(),
        "store_rung_why": resolved.rung.why(),
        "selector": plan.selector,
        "produced_by": plan.produced_by,
        "matched": plan.matched(),
        "rows": plan.rows(),
        "bytes": plan.bytes(),
        "series": plan.series,
        // `null` for a dry run and for a refusal — the two cases where nothing was attempted.
        "outcome": outcome,
        // `null` when the assertion held; the per-series refusals otherwise.
        "refused": refusals,
    });
    serde_json::to_string_pretty(&doc).expect("a tree of plain data; serialization is total")
}

#[cfg(test)]
mod rm_series_tests {
    use super::*;
    use vike_data::removal::{RemovalPlan, SeriesSelector};

    fn plan(matched: usize) -> RemovalPlan {
        let series = (0..matched)
            .map(|i| {
                vike_data::removal::PlannedSeries::new(
                    vike_data::SeriesId::per_symbol(
                        "bar",
                        "hyperliquid",
                        format!("S{i}"),
                        Some("1h".to_string()),
                    ),
                    Default::default(),
                    vec!["panel_bars:1".to_string()],
                    Some("panel_bars:"),
                )
            })
            .collect();
        RemovalPlan {
            selector: SeriesSelector::new("bar", "hyperliquid"),
            produced_by: Some("panel_bars:".to_string()),
            series,
        }
    }

    /// `--yes` is the non-interactive form and the ONLY one — a deliberate, greppable token in
    /// shell history.
    #[test]
    fn yes_confirms_with_or_without_a_terminal() {
        confirm_removal(&plan(3), true, false).unwrap();
        confirm_removal(&plan(3), true, true).unwrap();
    }

    /// ⚠ **The rule this verb exists to keep.** No `--yes` and no terminal REFUSES; it never falls
    /// back to reading, because `yes | backtest data rm …` is indistinguishable from a person
    /// once you have decided to read a pipe.
    #[test]
    fn no_yes_and_no_terminal_refuses_rather_than_reading() {
        let err = confirm_removal(&plan(3), false, false).unwrap_err();
        assert!(err.contains("not a terminal"), "{err}");
        assert!(err.contains("3 series"), "the refusal names what it did not delete: {err}");
    }
}

/// What `--addr` asked for. Three states, because the flag takes an OPTIONAL value.
///
/// ⚠ **The FLAG is what says "become a daemon", never the absence of a profile** — the owner's
/// distinction when they refused `--serve`, `backtest serve`, `backtest listen` and
/// `backtest daemon` on 2026-09-10. `--addr` names a THING (the socket to bind), the way `--out`
/// names a file, and being given one is what makes this process stay alive. So a bare `backtest`
/// with no profile is still the old argument error, not an accidental daemon.
#[derive(Debug, PartialEq, Eq)]
enum AddrFlag {
    /// No `--addr` at all — run one backtest and exit, exactly as before ruling 7.
    Absent,
    /// `--addr` with no value: serve on the CONFIGURED address. This is the normal use, and it is
    /// the whole point of the value being optional — the owner's *"MAY WE SET THIS SOMEWHERE AND
    /// NOT MENTION IT ALL THE TIME??"*.
    Configured,
    /// `--addr <host:port>` (or `--addr=<host:port>`): serve THERE, overriding every lower rung.
    Explicit(String),
}

/// Parse the OPTIONAL-VALUE `--addr` flag out of argv.
///
/// The rule for "did a value follow", spelled out because an optional-value flag is where CLIs
/// usually get this wrong: the next token is the VALUE only when it exists and does not begin with
/// `-`. So `backtest --addr` and `backtest --addr --json` both mean [`AddrFlag::Configured`], while
/// `backtest --addr 0.0.0.0:9999` means [`AddrFlag::Explicit`]. The `=` form is accepted too,
/// because an operator who writes `--addr=1.2.3.4:9` in a systemd `ExecStart=` should not discover
/// at runtime that this binary takes only the spaced form.
///
/// ⚠ A BLANK value is an ERROR rather than a fall-through to the configured rung. `--addr ""` in a
/// unit file is a mistake — an empty string cannot be a socket — and silently reading it as "the
/// configured address" would bind somewhere the operator did not name and never say so.
fn parse_addr_flag(args: &[String]) -> Result<AddrFlag, String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(rest) = a.strip_prefix("--addr=") {
            if rest.trim().is_empty() {
                return Err("--addr= was given an empty value".to_string());
            }
            return Ok(AddrFlag::Explicit(rest.to_string()));
        }
        if a == "--addr" {
            return match args.get(i + 1) {
                Some(v) if !v.starts_with('-') => {
                    if v.trim().is_empty() {
                        Err("--addr was given an empty value".to_string())
                    } else {
                        Ok(AddrFlag::Explicit(v.clone()))
                    }
                }
                _ => Ok(AddrFlag::Configured),
            };
        }
    }
    Ok(AddrFlag::Absent)
}

// ---------------------------------------------------------------------------------------------
// The parameter-search flags: ONE pure parser, run before any I/O.
//
// Ruling 13 of `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` refused a separate
// `optimize` verb — the flags stay on `backtest` and the word "optimizer" lives in the FLAG. What
// this replaced was a hand-written ladder in [`run`] that parsed each flag INSIDE the branch that
// used it, so any flag belonging to a branch not taken was never read and never validated. That is
// one cause with four faces, and every one of them exited 0:
//
//   * `--optimizer tpe --search bogus` ran tpe (the `--optimizer` arm returned before `--search`);
//   * `--search euler --trials 0` ignored a `0` the tpe arm treats as fatal;
//   * `--search grid --euler-depth 99` silently discarded a euler-only flag — and so did
//     `--euler-depth abc`, because the malformed-value check lived in the untaken branch;
//   * `--optimizer=tpe` ran the GRID, because `binutil::arg` matched an exact token only.
//
// The rule this file now encodes, in one sentence: **every knob belongs to exactly one method, and
// a knob handed to a method that does not own it is a REFUSAL rather than a silent discard.**
// ---------------------------------------------------------------------------------------------

/// Which parameter-search METHOD `--optimizer` named, carrying the config THAT method — and only
/// that method — takes. A knob cannot be parsed on a run that selected another method, because the
/// arm that parses it is the arm that names it.
///
/// ⚠ `PartialEq` but NOT `Eq`: `TpeConfig` holds an `f64` gamma and derives only `PartialEq`, so a
/// reflexive derive here does not compile. `Copy` is what lets the evaluator match and
/// [`optimizer_for`] each read it without a move.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Method {
    Grid,
    Euler(EulerConfig),
    Tpe(harness::TpeConfig),
    /// ⚠ `harness::genetic::GeneticConfig` BY MODULE PATH, where `TpeConfig` one line up is a
    /// re-export. Not an oversight on either side: `crates/vike-backtest/src/harness/mod.rs`'s
    /// `pub use` block states that only the SEAM is vocabulary and a method's knowledge stays at
    /// the method's module path, and `genetic`'s own `pub mod` comment says it deliberately joins
    /// no re-export block. Adding one here to make the two lines match would reverse a decision
    /// this file has no standing to reverse — and `EulerConfig` is already a third spelling
    /// (`crate::search`), so there is no uniformity left to buy.
    Genetic(harness::genetic::GeneticConfig),
}

/// What `--rank-by` selected. `multi` picks the composite objective; the four classic names keep
/// the score-clearing classic shape on the grid path (see [`run`]'s evaluator construction for why
/// that distinction is load-bearing rather than cosmetic).
#[derive(Debug, Clone, Copy, PartialEq)]
enum RankChoice {
    Metric(RankMetric),
    Multi,
}

/// Everything argv said about the search, resolved and validated in one pure act.
#[derive(Debug)]
struct SearchFlags {
    method: Method,
    rank: RankChoice,
    /// Whether argv asked for a SEARCH at all — an explicit `--optimizer`, or any method-owned
    /// knob. A profile with no `[sweep]` table then refuses instead of silently running one
    /// backtest. ⚠ `--rank-by` is deliberately NOT counted: it names how to ORDER results, not what
    /// work to do, and its ignore on a non-sweep profile is documented behaviour.
    requested: bool,
}

/// Which METHODS own each method-specific knob. Driving the ownership check from a table rather
/// than from three hand-written `if`s is what makes a fourth method's knob join the rule by adding
/// one row — and what lets this file's own `a_knob_is_refused_by_every_method_that_does_not_own_it`
/// iterate the whole matrix instead of hand-listing cases that go stale.
///
/// ⚠ **The second element is a SET, and `--seed` is why.** This was one owner per flag until the
/// genetic method was wired, and a seed is genuinely the same knob under both owners — the same
/// parse, the same meaning, the same `u64` reaching a searcher's constructor. The two shapes that
/// were rejected, because each gives up the property this table exists for:
///
/// * **Two rows** (`("--seed", "tpe")` and `("--seed", "genetic")`). A flag's owners would then be
///   scattered across rows with nothing binding them, the refusal below would fire on whichever
///   row it met first — so `--optimizer genetic --seed 7` would be refused BY THE TPE ROW — and the
///   matrix test's "refused by every method that is not this row's owner" would be asserting
///   something false about the sibling.
/// * **Dropping `--seed` from the table** and letting each method parse its own. That is the
///   hand-written ladder this file was built to delete: the flag would be silently discarded by
///   grid and euler again, which is defect (b) exactly.
///
/// A SET keeps the rule intact in both directions and changes nothing about a single-owner flag —
/// a one-element slice renders and refuses byte-identically to the `&str` it replaced, which is
/// what `--trials` and `--euler-depth` are here to keep proving. An EMPTY slice would refuse the
/// flag under every method, which is the safe direction for a typo; `every_method_flag_names_only
/// _real_methods` is what makes the entry itself checkable rather than merely safe.
const METHOD_FLAGS: &[(&str, &[&str])] = &[
    ("--euler-depth", &["euler"]),
    ("--trials", &["tpe"]),
    // ⚠ The reproducibility seed is a knob BOTH stochastic searchers take, and the only
    // asymmetry is what their ABSENCE means — tpe defaults to 0, genetic refuses. That belongs
    // in the construction arms (where a method's own semantics live), not here: this table
    // answers "may this flag be written at all", and the answer is yes for both.
    ("--seed", &["tpe", "genetic"]),
];

/// Whether `flag` was WRITTEN at all, in either valued spelling.
///
/// ⚠ Neither half alone is enough, and both misses are live: `has_flag` is exact-token so it never
/// sees `--euler-depth=99`, and `arg` answers `None` for a TRAILING bare `--search` with no value
/// token after it. An ownership rule written with one of them leaks exactly the argv the other
/// catches.
///
/// Local to this file rather than a fifth parser in `crates/vike-analytics/src/binutil.rs`: that
/// module is a layer-20 home shared by six bins, and this is a one-file need.
///
/// ⚠ The adjacent-name landmine, checked: `flag_given(args, "--seed")` is NOT tripped by
/// `--seed-demo` — `has_flag` is exact-token and `arg` is anchored on `--seed=`. Same for
/// `--fetch`/`--fetch-starter`.
fn flag_given(args: &[String], flag: &str) -> bool {
    has_flag(args, flag) || arg(args, flag).is_some()
}

/// The value of a VALUED flag, refusing the written-but-value-less spelling instead of reading it
/// as absent.
///
/// ⚠ **This is defect (d)'s last spelling, and it is the same failure — a DIFFERENT ANSWER rather
/// than a refusal.** [`arg`] answers `None` for a TRAILING bare `--optimizer`: the token is found,
/// there is no `=`, and there is no next token. So `backtest sweep.toml --optimizer` read as "no
/// `--optimizer` flag", fell to the `GridSearch` default, ran the exhaustive grid to completion and
/// exited 0 with no diagnostic — and on a profile with no `[sweep]` table it also slipped past the
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

/// Parse `--rank-by`, `--optimizer` and the per-method knobs out of argv. PURE — no store, no
/// profile, no environment, no clock.
fn parse_search_flags(args: &[String]) -> Result<SearchFlags, String> {
    // ⚠ `--search` IS RETIRED, IT IS REFUSED RATHER THAN ALIASED, AND IT IS CHECKED FIRST.
    //
    // The argument, at the call site because the brief asked for a decision and this is where it is
    // made. An alias cannot be made safe here: both spellings can be given with DIFFERENT values,
    // so every resolution of `--optimizer tpe --search euler` is a guess — and today's guess
    // ("`--optimizer` wins, silently") IS the first defect. An alias either keeps that precedence,
    // in which case the defect survives under a new name, or adds a conflict error, at which point
    // the operator must learn `--optimizer` anyway and the alias bought a permanent second spelling
    // for nothing. Deleting the second selector fixes it BY CONSTRUCTION rather than by adding a
    // check: after this there is only one selector, so there is nothing left for it to disagree
    // with. It also costs no compatibility — nothing in this tree passes `--search`, and the two
    // arm that SPAWNS this binary (`crates/vike-cli/src/cmd/backtest.rs`) sends only
    // `--profile`/`--store`/`--rank-by`/`--optimizer` and the method knobs — never `--search`.
    //
    // The refusal echoes what the operator typed and names the replacement, which is
    // `vike_config::refuse_removed_env`'s shape applied to a flag. Checked FIRST, so
    // `--optimizer tpe --search bogus` refuses on `--search` — that argv IS the first defect.
    if flag_given(args, "--search") {
        let written = args
            .iter()
            .find(|a| *a == "--search" || a.starts_with("--search="))
            .cloned()
            .unwrap_or_else(|| "--search".to_string());
        return Err(format!(
            "--search is retired — the flag is now --optimizer grid|euler|tpe|genetic. You wrote \
             {written:?}; write `--optimizer <method>` instead"
        ));
    }

    // ⚠ ONE widening worth naming: an INVALID `--rank-by` value is now refused on a profile with no
    // `[sweep]` table too, where the ladder — which lived inside `if profile.is_sweep()` — ignored
    // it. A VALID value's documented ignore is untouched; what changed is that a typo is no longer
    // silently swallowed on one of the two profile shapes. Nothing spawns a bogus one:
    // `crates/vike-cli/src/cmd/backtest.rs` validates the value in its own parser before it builds
    // an argv at all.
    let rank = match arg(args, "--rank-by") {
        Some(s) if s.eq_ignore_ascii_case("multi") => RankChoice::Multi,
        Some(s) => match RankMetric::from_str_ci(&s) {
            Some(m) => RankChoice::Metric(m),
            None => {
                return Err(format!(
                    "invalid --rank-by {s:?} (expected sharpe|return|max_dd|equity|multi)"
                ));
            }
        },
        None => RankChoice::Metric(RankMetric::default()),
    };

    // The method NAME. The default is read off `GridSearch` rather than spelled, so the flag's
    // default and `Optimizer::name` cannot drift into two literals.
    //
    // ⚠ Through [`required_value`], NOT through `arg`: a trailing bare `--optimizer` answers `None`
    // there, which is "no flag given" and therefore the GRID — the flag's default silently
    // overruling the flag. That doc carries the argument; this is the one line that has to use it.
    let named = required_value(args, "--optimizer", "expected grid|euler|tpe|genetic")?;
    let mut requested = named.is_some();
    let name = named.unwrap_or_else(|| harness::sweep::GridSearch.name().to_string());

    // ⚠ OWNERSHIP BEFORE VALUE. Every knob is checked for ownership before ANY of them is parsed,
    // so a knob that does not belong reports THAT rather than reporting a parse error about a value
    // nobody wanted — and so the answer to `--trials 0` is uniform across the three methods instead
    // of being fatal on one path and ignored on two.
    for (flag, owners) in METHOD_FLAGS {
        if flag_given(args, flag) {
            requested = true;
            if !owners.iter().any(|o| name.eq_ignore_ascii_case(o)) {
                // ⚠ EVERY owner, joined — not the first one. A refusal that named only one of a
                // two-owner flag's methods would send an operator who typed `--optimizer grid
                // --seed 7` to tpe while genetic, which may be the method they actually wanted,
                // went unmentioned. For a single-owner row this renders the bare owner name, so
                // `--trials` and `--euler-depth` refuse with the string they always did.
                return Err(format!(
                    "{flag} is a {} flag, but this run selected the {name:?} optimizer. It \
                     was silently discarded before; it is refused now, because a knob that \
                     configures a search you did not select cannot do what it says",
                    owners.join(" or ")
                ));
            }
        }
    }

    let method = if name.eq_ignore_ascii_case(harness::sweep::GridSearch.name()) {
        Method::Grid
    } else if name.eq_ignore_ascii_case("euler") {
        Method::Euler(EulerConfig::with_depth(parse_euler_depth(args)?))
    } else if name.eq_ignore_ascii_case("tpe") {
        // `.unwrap_or(0)` — tpe's documented default, unchanged. See [`parse_seed`] for why the
        // ABSENCE is resolved here, in each method's own arm, rather than inside the parser.
        Method::Tpe(harness::TpeConfig::new(parse_trials(args)?, parse_seed(args)?.unwrap_or(0)))
    } else if name.eq_ignore_ascii_case("genetic") {
        Method::Genetic(harness::genetic::GeneticConfig::new(require_seed(args)?))
    } else {
        // ⚠ `--optimizer grid` and `--optimizer euler` were both hard ERRORS before this
        // (`expected tpe`); two of the three values the flag must accept did not work at all.
        return Err(format!("invalid --optimizer {name:?} (expected grid|euler|tpe|genetic)"));
    };

    Ok(SearchFlags { method, rank, requested })
}

/// `--euler-depth`, RANGE-CHECKED rather than clamped.
///
/// ⚠ §7.5 of the design, signed off. `EulerConfig::with_depth` clamps silently at
/// `EulerConfig::MAX_DEPTH_CAP`, and `harness::EulerBudget`'s `max_depth` then reports a depth the
/// operator never typed — in the very line whose whole job is to report the budget. The clamp stays
/// exactly as it is (`crates/vike-backtest/src/search.rs` is untouched by this seam and its pure
/// tests gate unmoved code); the refusal lives ABOVE it, here.
///
/// `0` is legal and means coarse-grid-only, per `with_depth`'s own doc — so this is a range check,
/// not a positive-integer one.
fn parse_euler_depth(args: &[String]) -> Result<u32, String> {
    match required_value(args, "--euler-depth", "expected an integer")? {
        None => Ok(EulerConfig::DEFAULT_MAX_DEPTH),
        Some(s) => match s.parse::<u32>() {
            Ok(d) if d <= EulerConfig::MAX_DEPTH_CAP => Ok(d),
            Ok(d) => Err(format!(
                "--euler-depth {d} is past the cap of {} — it was silently clamped before, which \
                 made the budget line report a depth you did not ask for",
                EulerConfig::MAX_DEPTH_CAP
            )),
            Err(_) => Err(format!("invalid --euler-depth {s:?} (expected an integer)")),
        },
    }
}

/// `--trials` — tpe's budget. The positive-integer rule is unchanged; what changed is that it is
/// now the ONLY answer to a `--trials` value, because a non-tpe run refuses the flag outright.
fn parse_trials(args: &[String]) -> Result<usize, String> {
    match required_value(args, "--trials", "expected a positive integer")? {
        None => Ok(harness::TpeConfig::DEFAULT_TRIALS),
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("invalid --trials {s:?} (expected a positive integer)")),
        },
    }
}

/// `--seed` — the reproducibility knob tpe AND genetic take. ONE parse, and it answers `None` for
/// "not written" rather than substituting a default.
///
/// ⚠ **The default moved OUT of this function when the flag gained a second owner, and that is the
/// whole of the change.** The VALUE's rules are a property of the flag and stay here — one `u64`
/// parser, one refusal string, one `required_value` guard against the value-less spelling, so the
/// two methods cannot start disagreeing about what `--seed abc` means. Its ABSENCE is a property of
/// the METHOD and belongs in the method's own arm: tpe substitutes `0` (its documented, shipped
/// default), genetic refuses through [`require_seed`]. Returning `u64` with a baked-in `0` would
/// have forced the genetic arm to distinguish "not written" from "written as 0" — two argv that
/// mean different things, collapsed into one value before anybody could tell them apart.
fn parse_seed(args: &[String]) -> Result<Option<u64>, String> {
    match required_value(args, "--seed", "expected a u64")? {
        None => Ok(None),
        Some(s) => {
            s.parse::<u64>().map(Some).map_err(|_| format!("invalid --seed {s:?} (expected a u64)"))
        }
    }
}

/// `--seed`, REQUIRED — the genetic arm's disposition of an absent seed.
///
/// ⚠ **Silently defaulting it is the one thing the searcher's author ruled out**, and the refusal
/// is this file honouring a decision made one layer down rather than a preference of its own.
/// `harness::genetic::GeneticConfig::new` takes the seed as a required PARAMETER, and its doc says
/// why in as many words: "a defaulted seed is a hidden constant that changes results silently".
/// This binary is the only caller that could hand it one. Writing `GeneticConfig::new(0)` here
/// would re-introduce at the call site exactly what that signature refuses — the type would still
/// look strict while nothing in the tree expressed the requirement any more.
///
/// **Why this differs from tpe, which defaults `--seed` to 0 one arm up.** Not because one search
/// is more stochastic than the other — they are equally seeded. Because tpe's default is a SHIPPED
/// WIRE: `crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote one,
/// so `vike-cli backtest --local --optimizer tpe --trials 128` spawns this binary with no seed and
/// must keep running. Requiring one there would break a caller that exists. Genetic has no callers
/// yet, so it is the one method that can still afford to require the input, and the choice is
/// available exactly once — the moment it ships with a default, it has the same constraint tpe has.
///
/// ⚠ This is NOT the "one flag, two fates" defect [`parse_search_flags`] exists to prevent. That
/// class is a flag SILENTLY doing different things under different methods; here the flag's
/// PRESENCE means the identical thing under both owners (same parser, same `u64`, same
/// reproducibility), and what differs is whether its absence is allowed — with the disallowed side
/// a named refusal an operator reads, not an alternative behaviour they never learn about. It is
/// the shape `--produced-by` already has on `data rm`: required for one selector, optional for
/// another, refused by name rather than guessed at.
///
/// It costs nothing to be wrong about: the refusal is argv triage, so it opens no store
/// (`crates/vike-backtest/tests/optimizer_cli.rs`'s `a_refused_flag_never_opens_the_store` is the
/// property), and the fix is one token.
fn require_seed(args: &[String]) -> Result<u64, String> {
    parse_seed(args)?.ok_or_else(|| {
        "--optimizer genetic requires --seed <u64>. It is not defaulted, deliberately: a genetic \
         search reports ONE sample of a distribution, and a seed nobody typed is a constant the \
         result silently depends on — write `--seed 7` (any u64) and the run is reproducible from \
         your own shell history"
            .to_string()
    })
}

/// The ONE place a [`Method`] becomes a trait object.
///
/// ⚠ The three methods are deliberately NOT re-exported from `harness::` — `harness/mod.rs`'s own
/// comment says only the seam is vocabulary and a method's knowledge belongs in the method's file
/// — so these are module paths. `EulerSearch::NAME` and `TpeSearch::NAME` are private consts for
/// the same reason (widening them for the sake of a match would reverse that decision), which is
/// why the `"euler"`/`"tpe"` literals in [`parse_search_flags`] are pinned against
/// `Optimizer::name` by this file's own `every_optimizer_spelling_builds_the_method_it_names`
/// rather than read out of the methods.
fn optimizer_for(method: Method) -> Box<dyn harness::Optimizer> {
    match method {
        Method::Grid => Box::new(harness::sweep::GridSearch),
        Method::Euler(cfg) => Box::new(harness::euler::EulerSearch::new(cfg)),
        Method::Tpe(cfg) => Box::new(harness::tpe::TpeSearch::new(cfg)),
        Method::Genetic(cfg) => Box::new(harness::genetic::GeneticSearch::new(cfg)),
    }
}

/// The value-taking flags REACHABLE on the profile path, so a flag's VALUE is never counted as the
/// positional profile.
///
/// ⚠ Deliberately not every flag this file knows. `--kind`/`--venue`/`--symbol`/`--group`/
/// `--interval`/`--produced-by`/`--out`/`--from`/`--to`/`--days` all belong to [`run_data`]'s
/// subcommand, which has already RETURNED by the time [`profile_from_args`] runs — a `data` line
/// never reaches the profile path at all. `--addr` cannot appear at all either:
/// [`parse_addr_flag`] answers `AddrFlag::Absent` only when argv holds no `--addr`
/// token in EITHER spelling, so reaching the profile path proves there is none. That is what keeps
/// this table short and lets it carry no optional-value concept — the one thing a positional
/// scanner cannot express.
const PROFILE_PATH_VALUED: &[&str] =
    &["--profile", "--store", "--rank-by", "--optimizer", "--euler-depth", "--trials", "--seed"];

/// The profile, in either spelling: `backtest my.toml` (ruling 14) or `backtest --profile my.toml`.
///
/// ⚠ **`--profile` CANNOT retire, and that is a wire fact rather than a preference.**
/// `crates/vike-cli/src/cmd/backtest.rs`'s local arm builds `vec!["--profile".into(), …]` and
/// SPAWNS this binary, and `scripts/cli_mcp_smoke.sh` runs it. So the positional is ADDITIVE.
/// (It used to be TWO arms; `vike-cli sweep` was deleted by ruling 13 and its search flags moved
/// onto `vike-cli backtest`, which spawns the same way.)
///
/// ⚠ **BOTH is a REFUSAL, not a precedence rule.** Two spellings that may name two different files
/// is the same defect class as two searcher selectors: picking a winner silently answers a question
/// the operator did not know they had asked.
///
/// `Ok(None)` means "none given" — the caller's own arm, because that is the one case with no
/// specific mistake to name and therefore the one that still prints the usage text.
///
/// The scan skips a valued flag's VALUE by table rather than by "does the next token start with
/// `-`", because `--store dir` puts a bare token in argv that is emphatically not a profile.
fn profile_from_args(args: &[String]) -> Result<Option<String>, String> {
    let flagged = arg(args, "--profile");
    let mut bare: Vec<&String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // A bare valued flag consumes the next token. An INLINE `--flag=v` consumes nothing, and is
        // skipped by the `starts_with('-')` arm below like any other flag token.
        if PROFILE_PATH_VALUED.contains(&a.as_str()) {
            let _ = it.next();
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        bare.push(a);
    }
    match (flagged, bare.as_slice()) {
        // ⚠ `data` IS a positional, so ruling 12's subcommand and ruling 14's profile collide on
        // exactly one word — and [`run`] resolves it by routing only when `data` comes FIRST. That
        // leaves this case: `backtest --optimizer tpe data rm …` reaches here with `data` as the
        // profile, and without this arm it loads a file called `data`, fails on the open and names
        // a path the operator never typed. Refused by name instead, saying where the word goes.
        //
        // The declared cost: a profile literally NAMED `data` (no extension) is no longer
        // reachable positionally. `--profile data` still reaches it, which is what the arm says.
        (None, [p]) if p.as_str() == DATA_SUBCOMMAND => Err(format!(
            "`{DATA_SUBCOMMAND}` is a SUBCOMMAND here, not a profile, and it must come FIRST — \
             write `backtest {DATA_SUBCOMMAND} <fetch|fetch-starter|seed-demo|export|rm> …`. If \
             you really meant a profile file called `{DATA_SUBCOMMAND}`, spell it `--profile \
             {DATA_SUBCOMMAND}`"
        )),
        (Some(p), []) => Ok(Some(p)),
        (None, [p]) => Ok(Some((*p).clone())),
        (None, []) => Ok(None),
        (Some(p), [b]) => Err(format!(
            "the profile was given twice — --profile {p:?} and the positional {b:?}. They may name \
             two different files, so this is refused rather than resolved: give it once"
        )),
        (flagged, extra) => Err(format!(
            "more than one profile was given: {}{extra:?}. Give exactly one",
            flagged.map(|p| format!("--profile {p:?} and ")).unwrap_or_default()
        )),
    }
}

/// The `--addr` value LADDER, mirroring `datahub`'s exactly: an explicit flag value, then
/// `VIKE_BACKTEST_ADDR`, then `config.backtest_addr`, then `vike_config::DEFAULT_BACKTEST_ADDR`.
///
/// ⚠ The env rung is not read here, and that is the point. `vike_config::Config::apply_env` folds
/// `VIKE_BACKTEST_ADDR` OVER the file layer before this function sees it, so `configured` already
/// carries whichever of the two won — which keeps this workspace's "one loader decides precedence"
/// property intact and stops a second, subtly different ladder existing inside a binary. Pure, so
/// the ladder is unit-testable without a settings directory and without an environment.
fn resolve_serve_addr(flag: &AddrFlag, configured: Option<&str>) -> String {
    match flag {
        AddrFlag::Explicit(v) => v.clone(),
        _ => configured
            .map(str::to_string)
            .unwrap_or_else(|| vike_config::DEFAULT_BACKTEST_ADDR.to_string()),
    }
}

/// Become the COMPUTE daemon: resolve the address, load the node keys, classify the bind, open the
/// store and serve the seven verbs forever.
///
/// The composition order is the data daemon's, deliberately (`vike_datahub::datahub_cli`'s `run`):
/// settings, then keys, then the BIND DECISION — before the store is opened and before a listener
/// exists — then the listener, then `serve_authed`. A refused configuration must cost nothing and
/// touch nothing.
///
/// ⚠ **A refusal EXITS 2** rather than degrading, and that is this invocation's own rule rather than
/// the workspace's: serving is the only thing `--addr` was asked to do, so "do not bind" and "do not
/// run" are the same decision. Exit 2 is the refused-configuration family this binary already uses
/// for a rejected argument — not `FAILURE`, because nothing was tried and failed.
///
/// `studio` is the injected [`crate::compute_server::StudioRunTable`] the composition root handed
/// down; see this function's mount comment for why a `None` here is a build fact rather than an
/// omission.
fn run_serve(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    flag: AddrFlag,
    studio: Option<crate::compute_server::StudioRunTable>,
) -> ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};

    use vike_datahub_client::bind::{BindDecision, ServerAuth, bind_decision};

    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk — the ONE fact this
    // root pulls out of the swept map to find its project, and spelled as a LITERAL for the reason
    // the indicator read above gives (the settings registry's map-lookup sweep resolves constants
    // crate-wide, so importing the constant would make the read invisible to the gate).
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    let settings_dir = std::env::current_dir()
        .ok()
        .and_then(|cwd| vike_model::state_path::project_settings_dir_from(settings_override, &cwd));
    // A settings directory is OPTIONAL (a checkout run from anywhere has none) but a MALFORMED file
    // is fatal: on the path that opens a socket, a config this box has and cannot parse must never
    // be silently skipped.
    let settings = match vike_config::load(settings_dir.as_deref(), vars) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest --addr: {e}");
            return ExitCode::from(2);
        }
    };
    for w in &settings.warnings {
        eprintln!("backtest --addr: {w}");
    }
    let addr = resolve_serve_addr(&flag, settings.config.backtest_addr.as_deref());

    // The node keys, from the NODE store (`node.env`, falling back to the venue-key file only while
    // a box has not migrated) — the same pair, under the same domain separator, the data daemon
    // authenticates with (`crate::compute_server`'s `serve_authed` carries why one pair rather than
    // two). `vike_secrets`, not `vike_bridge_core::credentials`: the canonical wrapper drags the
    // ureq/tungstenite/rustls transport stack, and nothing here needs a transport.
    //
    // ⚠ A store that EXISTS and cannot be READ is NOT "no credentials". The first is a permissions
    // bug that silently drops this server to unauthenticated; the second is the ordinary
    // unconfigured state. They must never look the same to an operator — and on a NON-LOOPBACK bind
    // the difference decides whether this process starts at all, so the two refusals below say
    // which case they are.
    let mut store_unreadable = false;
    let credentials: std::collections::HashMap<String, String> =
        match vike_secrets::resolve_node_keys(
            settings_override,
            vike_model::credential_keys::is_platform_key,
        ) {
            Ok((resolved, _source)) => {
                if let Some(w) = &resolved.warning {
                    eprintln!("backtest --addr: {w}");
                }
                if let Some(w) = &resolved.legacy {
                    eprintln!("backtest --addr: {w}");
                }
                resolved.secrets.into_map()
            }
            Err(e) => {
                eprintln!(
                    "backtest --addr: credential store PRESENT but UNREADABLE ({e}) — any \
                     configured node keys were NOT loaded. On a LOOPBACK bind this server is about \
                     to serve UNAUTHENTICATED behind the bind guard alone; on a NON-LOOPBACK one it \
                     will REFUSE TO START. Either way the cause is that file: fix its permissions \
                     and restart"
                );
                store_unreadable = true;
                Default::default()
            }
        };
    let keys = vike_datahub_client::node_auth::node_keys_from_vars(&credentials);

    // Classify the bind BEFORE the store opens or the listener binds — the same guard, from the
    // same function, the data daemon runs (`vike_datahub_client::bind`). It matters at least as
    // much here: what a key-less non-loopback bind would expose on THIS daemon is a Rhai compiler
    // running source the client supplied.
    //
    // The opt-in keeps the datahub's spelling, `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1`, on purpose: it
    // consents to "this box's node protocol may be reached off-box", which is one posture decision
    // for one box, and a second variable would let an operator believe they had answered it while
    // the other daemon still refused.
    let allow_public = vars.get("VIKE_DATAHUB_ALLOW_PUBLIC_BIND").map(String::as_str) == Some("1");
    let resolved_addrs: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    match bind_decision(&resolved_addrs, allow_public, ServerAuth::of(keys.as_ref())) {
        BindDecision::Proceed => {}
        BindDecision::ProceedExposed(exposed) => {
            eprintln!(
                "backtest --addr: binding a NON-LOOPBACK address {exposed} \
                 (VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1). Node keys ARE configured, so every connection \
                 must authenticate — but the handshake is PLAINTEXT and authenticates the \
                 CONNECTION, not each frame, so keep an SSH tunnel or a VPN in front of it"
            );
        }
        BindDecision::RefuseUnauthenticated(exposed) if store_unreadable => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback and this server could not READ its \
                 credential store, so it has no node keys to authenticate with — refusing to \
                 start. This is NOT a missing configuration: the store is present and its keys may \
                 well be correct. Fix the file's permissions (`vike-cli secrets path` prints it) \
                 and restart"
            );
            return ExitCode::from(2);
        }
        BindDecision::RefuseUnauthenticated(exposed) => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback and this server has NO node keys — \
                 refusing to start. VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 consents to being REACHABLE; \
                 it is not consent to compile and run RHAI THE CLIENT SUPPLIES, unauthenticated, \
                 for anyone who can open a socket. Either set VIKE_DATAHUB_OBSERVE_KEY and \
                 VIKE_DATAHUB_CONTROL_KEY in the credential store, or put the address back on \
                 127.0.0.1 and reach it with `ssh -L`"
            );
            return ExitCode::from(2);
        }
        BindDecision::Refuse(exposed) => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback — refusing to start. This server is \
                 meant to be reached over an SSH tunnel (its handshake is plaintext even when node \
                 keys ARE set). If this box genuinely must listen on a trusted network, set \
                 VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 — and set the node keys first, so what is \
                 exposed is authenticated"
            );
            return ExitCode::from(2);
        }
    }

    // The store this daemon computes NEXT TO. `--store` still applies, and so does the shared
    // precedence behind it: compute-to-data means the profile crosses the wire and the history does
    // not, which only holds if this process opens the same root the data daemon serves.
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("backtest --addr: failed to open hist store at {root:?}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("backtest --addr: failed to bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("backtest --addr: listening on {addr}, store {}", root.display());

    // ⚠ `studio` is `None` on a bare `cargo run -p vike-backtest --bin backtest -- --addr`, and
    // `Some` under `vike-backend backtest --addr`. That is a LAYER fact, not an omission: the three
    // Studio verbs run `vike_studio_core`'s slice runners, and that crate sits ABOVE this one in the
    // layer graph, so only a composition root that can name both can hand them down.
    // `crates/vike/src/main.rs`'s `backtest_main` does. A daemon without the table advertises none
    // of the three and refuses them by name — the `FEATURE_BACKFILL` shape, applied to a mount
    // rather than to a feature.
    match crate::compute_server::serve_authed(listener, Arc::new(store), studio, keys) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("backtest --addr: serve loop ended with an error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The PURE half of the search-flag gate. `crates/vike-backtest/tests/optimizer_cli.rs` is the
/// shipped-binary half and is where the four defects are proven as an operator experiences them;
/// these are the table-driven pins a spawned-binary test cannot buy — a forgotten
/// [`METHOD_FLAGS`] or [`PROFILE_PATH_VALUED`] row reddens HERE, by name, rather than turning into
/// a silently accepted knob or a flag value read as a profile path.
#[cfg(test)]
mod search_flag_tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Every name `--optimizer` accepts, paired with the argv that makes the SELECTION COMPLETE.
    ///
    /// Only `genetic` needs anything, and the pair exists because of it: its `--seed` is REQUIRED
    /// ([`require_seed`]), so `["--optimizer", "genetic"]` alone is an error and a bare list of
    /// names could no longer drive these tests. Written as a table rather than special-cased at
    /// four call sites, so a fifth method with its own required input joins by adding a row.
    const METHODS: &[(&str, &[&str])] =
        &[("grid", &[]), ("euler", &[]), ("tpe", &[]), ("genetic", &["--seed", "7"])];

    /// [`METHODS`] with the extra argv dropped — for the checks that only need the NAMES, and
    /// where naming a method incompletely is the point (an ownership refusal fires before any
    /// method is constructed, so it must not depend on the selection being complete).
    fn method_names() -> Vec<&'static str> {
        METHODS.iter().map(|(name, _)| *name).collect()
    }

    /// Every method named in [`METHOD_FLAGS`] is a method [`parse_search_flags`] actually accepts.
    ///
    /// ⚠ **This became load-bearing when the owner column became a SET, and it did not exist
    /// before.** With one owner per flag a typo'd name ("tpee") merely made the flag universally
    /// refused, and the matrix test below still passed — every method reaches its `else` branch and
    /// the refusal names the typo, so the assertion holds. In a TWO-element set the same typo is
    /// worse and just as quiet: one method keeps its access, the other silently loses it, and the
    /// matrix test is satisfied either way because it reads the owner set as the definition of
    /// truth. Checking the names against the selector closes the loop.
    #[test]
    fn every_method_flag_names_only_real_methods() {
        for (flag, owners) in METHOD_FLAGS {
            assert!(!owners.is_empty(), "{flag} must be owned by at least one method");
            for owner in owners.iter() {
                let extra = METHODS
                    .iter()
                    .find(|(name, _)| name == owner)
                    .unwrap_or_else(|| panic!("{flag} names {owner:?}, which is not a method"))
                    .1;
                let mut a: Vec<&str> = vec!["--optimizer", owner];
                a.extend_from_slice(extra);
                let flags = parse_search_flags(&argv(&a))
                    .unwrap_or_else(|e| panic!("{flag}'s owner {owner:?} must select: {e}"));
                assert_eq!(
                    optimizer_for(flags.method).name(),
                    *owner,
                    "{flag} must name the method whose Optimizer::name is {owner:?}"
                );
            }
        }
    }

    /// Every method name `--optimizer` accepts builds the method whose `Optimizer::name` answers
    /// with that same string — and the absent flag builds the grid.
    ///
    /// ⚠ This is what REPLACES reading `EulerSearch::NAME` and `TpeSearch::NAME`, which are private
    /// consts deliberately (`harness/mod.rs`: a method's knowledge belongs in the method's file).
    /// Widening them for the sake of a match would reverse that; pinning the round trip keeps the
    /// two literals in [`parse_search_flags`] from drifting away from the methods they name.
    #[test]
    fn every_optimizer_spelling_builds_the_method_it_names() {
        for (name, extra) in METHODS {
            let mut a: Vec<&str> = vec!["--optimizer", name];
            a.extend_from_slice(extra);
            let flags = parse_search_flags(&argv(&a)).expect("a valid method");
            assert_eq!(
                optimizer_for(flags.method).name(),
                *name,
                "--optimizer {name} must build the method whose name() is {name:?}"
            );
            assert!(flags.requested, "an explicit --optimizer is a REQUESTED search");
        }
        let default = parse_search_flags(&argv(&[])).expect("no flags is the default");
        assert_eq!(optimizer_for(default.method).name(), harness::sweep::GridSearch.name());
        assert!(!default.requested, "no search flag is no search REQUEST");
    }

    /// Every knob is refused by every method that does not own it, and accepted by EVERY method
    /// that does — driven from [`METHOD_FLAGS`] × [`METHODS`], so a fifth method or a fifth knob
    /// joins this check by adding one row rather than by somebody remembering.
    ///
    /// ⚠ **The widening to owner SETS is exactly where this test could have stopped meaning
    /// anything, so read what it now asserts.** For each flag, the method list is partitioned by
    /// the flag's own owner set: every owner must ACCEPT it and every non-owner must REFUSE it.
    /// A single-owner flag therefore gets the identical three-refusals-one-accept check it always
    /// had — `--trials` and `--euler-depth` are here to keep proving the rule did not soften into
    /// "somebody owns it, so let it through" — while `--seed` gets two accepts and two refusals.
    /// The refusal must name EVERY owner, not just the first: a two-owner flag whose refusal named
    /// one method would be a worse message than the one it replaced.
    ///
    /// ⚠ The values are VALID for the owning method on purpose (`3`, `8`, `7`), so nothing but the
    /// ownership rule can produce these refusals: a "fix" that merely hoisted the range checks out
    /// of the branches would leave this red.
    ///
    /// ⚠ The owning-method ACCEPT is spelled with that method's completing argv from [`METHODS`],
    /// because `--optimizer genetic --seed 7` is a complete selection and `--optimizer genetic`
    /// alone is not. The REFUSAL half deliberately is NOT: ownership is checked before any method
    /// is constructed, so an incomplete selection must still produce the ownership error — which is
    /// the ordering `a_knob_another_method_owns_is_refused_when_genetic_was_named` pins from the
    /// shipped binary's side.
    #[test]
    fn a_knob_is_refused_by_every_method_that_does_not_own_it() {
        for (flag, owners) in METHOD_FLAGS {
            let value = if *flag == "--euler-depth" { "3" } else { "8" };
            for (name, extra) in METHODS {
                let owns = owners.contains(name);
                let mut a: Vec<&str> = vec!["--optimizer", name];
                // The method's completing argv — unless the knob under test IS that argv, in which
                // case adding both would put one flag in argv twice.
                if owns && !extra.contains(flag) {
                    a.extend_from_slice(extra);
                }
                a.extend_from_slice(&[flag, value]);
                let got = parse_search_flags(&argv(&a));
                if owns {
                    assert!(got.is_ok(), "{flag} must be accepted by its owner {name}: {got:?}");
                } else {
                    let msg = got.expect_err(&format!("{flag} under {name} must be refused"));
                    assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
                    for owner in owners.iter() {
                        assert!(
                            msg.contains(owner),
                            "…and EVERY method that owns it, {owner} included: {msg}"
                        );
                    }
                }
            }
            // No `--optimizer` at all is the GRID, and a knob for an unchosen method is still one.
            if !owners.contains(&"grid") {
                assert!(parse_search_flags(&argv(&[flag, value])).is_err());
            }
            // The INLINE spelling too — the exact place an ownership rule written with `has_flag`
            // leaks, and a bare trailing knob, the place one written with `arg` alone leaks. Both
            // under a method that does NOT own the flag, picked out of the method list rather than
            // hard-coded, because `--seed`'s arrival means no single name is a non-owner of
            // everything any more.
            let stranger = method_names()
                .into_iter()
                .find(|n| !owners.contains(n))
                .expect("no knob is owned by every method");
            assert!(
                parse_search_flags(&argv(&["--optimizer", stranger, &format!("{flag}={value}")]))
                    .is_err()
            );
            assert!(parse_search_flags(&argv(&["--optimizer", stranger, flag])).is_err());
        }
    }

    /// `--trials 0` is refused under EVERY method. Under `tpe` the reason is the positive-integer
    /// check (which was always right); under `grid`/`euler` it is the ownership refusal. The point
    /// of the test is that the ANSWER is uniform — before this, the same value was fatal on one
    /// path and silently discarded on two.
    #[test]
    fn a_zero_trial_budget_is_never_silently_accepted() {
        for name in method_names() {
            let msg = parse_search_flags(&argv(&["--optimizer", name, "--trials", "0"]))
                .expect_err("--trials 0 must be refused under every method");
            assert!(msg.contains("--trials"), "the refusal must name the flag: {msg}");
        }
    }

    /// **The selector's own value-less spelling, and every knob's under its OWNING method.**
    ///
    /// `arg` answers `None` for a trailing bare token, so `--optimizer` with nothing after it read
    /// as "no `--optimizer` flag" and therefore as the GRID: the flag's default silently overruling
    /// the flag. Its two siblings were already covered — bare `--search` by
    /// `the_retired_search_flag_is_refused_in_every_spelling`, `--optimizer=` by `arg` answering
    /// `Some("")` — which is exactly why this one was easy to miss.
    ///
    /// The knob half is the same hole and had the same shape as defect (b): under a NON-owning
    /// method a bare trailing knob was already refused (ownership goes through [`flag_given`]),
    /// while under its OWNER it silently ran the default. Driven from [`METHOD_FLAGS`], so a fourth
    /// method's knob joins by adding a row.
    #[test]
    fn a_value_less_flag_is_refused_rather_than_read_as_absent() {
        let msg = parse_search_flags(&argv(&["--optimizer"]))
            .expect_err("a bare --optimizer must not read as the GRID");
        assert!(msg.contains("--optimizer"), "the refusal must name the flag: {msg}");
        for method in method_names() {
            assert!(msg.contains(method), "…and the methods it takes: {msg}");
        }
        // …and it is refused wherever it sits, not only as the last token: a profile after it is
        // eaten as its VALUE and refused as a bogus method, which is also not a silent grid.
        assert!(parse_search_flags(&argv(&["--optimizer", "my.toml"])).is_err());

        // ⚠ EVERY owner, not the first: a two-owner knob has two arms that could each default it
        // silently, and `--seed` is the one where an owner genuinely has no default to fall back
        // on — a bare `--seed` under `genetic` must refuse as a value-less FLAG, never collapse
        // into `require_seed`'s missing-seed message, because those say different things.
        for (flag, owners) in METHOD_FLAGS {
            for owner in owners.iter() {
                let msg = parse_search_flags(&argv(&["--optimizer", owner, flag]))
                    .expect_err("a value-less knob under its own method must not default silently");
                assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
                assert!(
                    msg.contains("no value"),
                    "…and say the flag was written WITHOUT ONE, rather than reporting it as \
                     absent: {msg}"
                );
            }
        }
    }

    /// **`genetic` requires `--seed`; `tpe` still defaults one.** The ONE place the two owners of
    /// a single flag deliberately differ, pinned as a PAIR so neither can drift onto the other's
    /// disposition unnoticed — a "consistency" edit that gave genetic a default, or one that made
    /// tpe refuse, reddens here rather than in a shipped wire. [`require_seed`] carries the
    /// argument for the asymmetry.
    ///
    /// ⚠ The `0` row is the reason [`parse_seed`] answers `Option` instead of baking the default
    /// in: a WRITTEN `--seed 0` and an ABSENT `--seed` are different argv that must stay
    /// distinguishable, and a `u64`-returning parser collapses them before any arm can tell.
    #[test]
    fn genetic_requires_a_seed_and_tpe_still_defaults_one() {
        let msg = parse_search_flags(&argv(&["--optimizer", "genetic"]))
            .expect_err("a genetic run with no seed must be refused");
        assert!(msg.contains("--seed"), "the refusal must name the flag: {msg}");
        assert!(msg.contains("genetic"), "…and the method that requires it: {msg}");

        // The operator's value REACHES the config, and a different value reaches it differently —
        // the pin against a hard-coded `GeneticConfig::new(0)` that every other test here passes.
        for seed in [0u64, 7, u64::MAX] {
            let a = ["--optimizer", "genetic", "--seed", &seed.to_string()].map(str::to_string);
            let flags = parse_search_flags(&a).expect("an explicit seed completes the selection");
            assert_eq!(
                flags.method,
                Method::Genetic(harness::genetic::GeneticConfig::new(seed)),
                "--seed {seed} must be the seed the searcher is built with"
            );
        }

        // …and tpe's absent-seed default is UNCHANGED at 0. Not a preference: it is a shipped wire
        // — `crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote
        // one, so a spawned `--optimizer tpe --trials 128` arrives here with no seed at all.
        assert_eq!(
            parse_search_flags(&argv(&["--optimizer", "tpe"])).expect("tpe needs no seed").method,
            Method::Tpe(harness::TpeConfig::new(harness::TpeConfig::DEFAULT_TRIALS, 0))
        );
    }

    /// `--search` is refused in every spelling, including the bare trailing one that `arg` alone
    /// answers `None` for — and including the argv that IS the first defect.
    #[test]
    fn the_retired_search_flag_is_refused_in_every_spelling() {
        for a in [
            vec!["--optimizer", "tpe", "--search", "bogus"],
            vec!["--search", "euler"],
            vec!["--search=grid"],
            vec!["--search"],
        ] {
            let msg = parse_search_flags(&argv(&a)).expect_err("--search is retired");
            assert!(msg.contains("--search"), "the refusal echoes what was written: {msg}");
            assert!(msg.contains("--optimizer"), "…and names the replacement: {msg}");
        }
    }

    /// The euler depth range check, on both edges of the cap. `0` is legal (coarse-grid-only).
    #[test]
    fn an_out_of_range_euler_depth_is_refused_rather_than_clamped() {
        for depth in ["0", "1"] {
            assert!(
                parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", depth]))
                    .is_ok()
            );
        }
        let cap = EulerConfig::MAX_DEPTH_CAP.to_string();
        assert!(
            parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", &cap])).is_ok()
        );
        let over = (EulerConfig::MAX_DEPTH_CAP + 1).to_string();
        let msg = parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", &over]))
            .expect_err("past the cap is a refusal, not a clamp");
        assert!(msg.contains(&cap), "the refusal must name the cap: {msg}");
        assert!(
            parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", "abc"])).is_err()
        );
    }

    /// `--rank-by` is NOT a search request: it names how to ORDER results, and its ignore on a
    /// non-sweep profile is documented behaviour this PR deliberately leaves alone.
    #[test]
    fn rank_by_is_parsed_but_does_not_count_as_a_search_request() {
        let flags = parse_search_flags(&argv(&["--rank-by", "return"])).expect("a valid metric");
        assert_eq!(flags.rank, RankChoice::Metric(RankMetric::TotalReturn));
        assert!(!flags.requested, "--rank-by alone must not make a non-sweep profile refuse");
        assert_eq!(
            parse_search_flags(&argv(&["--rank-by", "MULTI"])).expect("case-insensitive").rank,
            RankChoice::Multi
        );
        assert!(parse_search_flags(&argv(&["--rank-by", "bogus"])).is_err());
    }

    /// The positional profile, and the table that keeps a flag's VALUE from being read as one.
    ///
    /// ⚠ The `PROFILE_PATH_VALUED` loop is the point: a valued flag added to this file without a
    /// row there would silently turn its value into a profile path, and that reddens HERE with the
    /// flag named rather than at a user's terminal.
    #[test]
    fn no_flag_value_can_be_mistaken_for_the_positional_profile() {
        assert_eq!(profile_from_args(&argv(&["my.toml"])).unwrap().as_deref(), Some("my.toml"));
        assert_eq!(
            profile_from_args(&argv(&["--profile", "my.toml"])).unwrap().as_deref(),
            Some("my.toml"),
            "the older spelling two `crates/vike-cli/` arms SPAWN with must keep working"
        );
        assert_eq!(profile_from_args(&argv(&[])).unwrap(), None, "none given is not an error here");
        assert_eq!(
            profile_from_args(&argv(&["--json", "my.toml"])).unwrap().as_deref(),
            Some("my.toml"),
            "a valueless toggle consumes nothing"
        );

        // ⚠ `--profile` is skipped: its value IS the profile, which is the case asserted above.
        // Every OTHER row is a flag whose value must never be mistaken for one.
        for flag in PROFILE_PATH_VALUED.iter().filter(|f| **f != "--profile") {
            assert_eq!(
                profile_from_args(&argv(&[flag, "VALUE"])).unwrap(),
                None,
                "{flag}'s VALUE must never be read as the profile"
            );
            // …and the profile still resolves when it sits after that flag's pair.
            assert_eq!(
                profile_from_args(&argv(&[flag, "VALUE", "my.toml"])).unwrap().as_deref(),
                Some("my.toml"),
                "{flag} VALUE my.toml"
            );
        }
    }

    /// Two profiles — in either combination of spellings — is a REFUSAL naming both, never a
    /// precedence rule. Picking a winner silently answers a question the operator did not know they
    /// had asked, which is the same defect class as two searcher selectors.
    #[test]
    fn giving_the_profile_twice_is_refused_and_names_both() {
        let msg = profile_from_args(&argv(&["--profile", "a.toml", "b.toml"]))
            .expect_err("both spellings is a refusal");
        assert!(msg.contains("a.toml") && msg.contains("b.toml"), "names both: {msg}");
        let msg = profile_from_args(&argv(&["a.toml", "b.toml"]))
            .expect_err("two positionals is a refusal");
        assert!(msg.contains("a.toml") && msg.contains("b.toml"), "names both: {msg}");
    }
}
