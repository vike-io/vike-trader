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
//!   backtest --seed-demo [--store DIR]
//!   backtest --rm-series --kind K --venue V […] [--produced-by PREFIX] [--dry-run] [--yes]
//!   backtest --profile run.toml [--store DIR] [--json]
//!   backtest --profile sweep.toml [--store DIR] [--json] [--rank-by sharpe|return|max_dd|equity|multi]
//!                                 [--search grid|euler] [--euler-depth N]
//!                                 [--optimizer tpe [--trials N] [--seed S]]
//!
//! `--list` prints every registered strategy name (`harness::STRATEGIES`) and exits — no store,
//! no profile needed. Otherwise `--profile` is required: the profile (`BacktestProfile::from_path`)
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
//! `--rank-by` picks the ranking metric (default `sharpe`; ignored — not an error — on a
//! non-sweep profile). `--rank-by multi` ranks by the composite `crate::objective`
//! multi-metric score instead (`harness::run_sweep_with` + `objective::multi_metric` with
//! default `MultiMetricParams`) — rows gain a `score` column/field; the four classic metric
//! names keep the original `run_sweep` path and output byte-identical.
//!
//! `--search euler` (default `grid`) replaces the exhaustive cartesian grid with a bounded
//! successive-halving refinement (`harness::run_sweep_euler`): the coarse grid runs once, then the
//! per-axis step is halved up to `--euler-depth` times (default 3, capped at 16) around the running
//! best point. Each completed halving doubles the effective resolution, for a small fraction of the
//! backtests an equally fine grid would cost — at the cost of being a LOCAL refinement (it can only
//! descend into the basin the coarse grid already found) and of requiring every `[sweep]` axis to be
//! numeric AND type-homogeneous (no mixed `[1, 2.5]`). The stderr budget line compares against the
//! grid matching the depth ACTUALLY reached, so a search that ran out of new candidates early
//! reports the smaller, honest saving. Scoring reuses
//! `--rank-by` verbatim (the same objective seam), rows always carry a `score`, and a one-line
//! budget summary goes to stderr. `--search grid` (the default) is completely untouched.
//!
//! `--optimizer tpe` is the Bayesian (Tree-structured Parzen Estimator) ask/tell search
//! (`harness::run_tpe`) — the smart alternative to enumerating the grid, converging on good params
//! in `--trials` backtests (default 64) by modelling which regions score well. `--seed` (default 0)
//! makes it fully reproducible. It takes precedence over `--search`, reuses `--rank-by` as its
//! objective (rows carry a `score`), and returns the same ranked `SweepReport`. Absent (the
//! default), the `--search grid|euler` path above is byte-identical to before.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use crate::binutil::{arg, has_flag, store_root};
use crate::harness::{self, BacktestProfile, RankMetric};
use crate::runs;
use crate::search::EulerConfig;
// Gated exactly as the module is: a `datafusion-store`-only build has no `starter`, and an
// unconditional import made that configuration fail to compile — a build CI runs and I did not.
#[cfg(feature = "venue-fetch")]
use crate::starter;
use vike_data::DataFusionHist;
use vike_data::demo as demo_tape;

// `periods_per_year` (the Sharpe annualization factor) now lives in `harness::report` as the SINGLE
// source of truth, so the single-run path here and the sweep rank an identical profile on the same
// Sharpe scale. Imported below as `harness::report::periods_per_year`.

/// What `--help` prints. A const rather than the module doc above, because only a const can reach a
/// user: that doc is for a reader of this file, `backtest --help` is for everyone else.
const USAGE: &str = "\
usage: backtest --profile PATH [--store DIR] [--json]
       backtest --profile SWEEP.toml [--rank-by sharpe|return|max_dd|equity|multi]
                [--search grid|euler] [--euler-depth N]
                [--optimizer tpe [--trials N] [--seed S]]
       backtest --list
       backtest --seed-demo [--store DIR]
       backtest --rm-series --kind K --venue V (--symbol S [--interval I] | --group G)
                [--produced-by PREFIX] [--dry-run] [--yes] [--store DIR] [--json]
       backtest --fetch VENUE:SYMBOL:INTERVAL (--days N | --from LABEL --to LABEL) [--store DIR]
       backtest --fetch-starter [--store DIR]
       backtest --export VENUE:SYMBOL:INTERVAL --out FILE [--from LABEL --to LABEL] [--store DIR]

  --profile PATH   the harness profile TOML (data slice + engine costs + strategy). REQUIRED
                   unless --list. A profile with a [sweep] table runs the parameter sweep.
  --store DIR      hist-store root; falls back to $VIKE_HIST_STORE, then <repo>/market_data/hist
  --json           print the report as pretty JSON instead of the human table
  --list           print every registered strategy name and exit — no store, no profile needed
  --seed-demo      write the SYNTHETIC demo tape into the store and exit — no profile needed.
                   Venue `demo`, a closed-form curve, NOT market data; it is the slice the
                   shipped `user_data/profiles/backtest.toml` names, so a fresh install can run
                   that profile immediately. Safe to re-run: a second seed writes nothing
  --fetch SPEC     pull REAL public bars into the store and exit — no credentials needed.
                   SPEC is VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h). Needs a window:
                   --days N counts back from now, or --from/--to take epoch-ms or YYYY-MM-DDTHH.
                   Re-fetching the same window writes nothing. Requires the `venue-fetch`
                   feature — the release binary and the container image have it
  --rm-series      DELETE stored series, IRREVERSIBLY, and exit. Selects on the four series
                   dimensions: --kind and --venue are REQUIRED, and an omitted
                   --symbol/--group/--interval is a wildcard over that dimension. Prints the
                   PLAN first — the resolved store root and the rung that chose it, then every
                   matched series with its rows/bytes/days and the commit keys that wrote it.
                   --produced-by asserts that EVERY key of EVERY matched series carries that
                   prefix (a producer path from STORE_KINDS resolves to its prefix); one foreign
                   key refuses the whole run and deletes nothing. It is REQUIRED for a sweep and
                   optional for a fully-named series. --dry-run stops after the plan; otherwise
                   --yes, or type `delete N series` at a terminal. There is no --force
  --fetch-starter  download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue and
                   no credentials) and load it. For a box a venue cannot be reached from —
                   a geoblock, a locked-down network. Verified against its published SHA256SUMS;
                   safe to re-run. Same `venue-fetch` feature as --fetch
  --export SPEC    write one series from the store to a standalone Parquet file (--out FILE),
                   optionally bounded by --from/--to. Any venue the store holds, `demo` included
  --rank-by M      sweep ranking metric (default sharpe); `multi` is the composite objective
  --search S       sweep search: grid (default) or euler successive-halving refinement
  --euler-depth N  euler halving depth (default 3, capped at 16)
  --optimizer tpe  Bayesian ask/tell search; takes precedence over --search
  --trials N       tpe trials (default 64)
  --seed S         tpe seed (default 0) — makes a tpe run reproducible
  -h, --help       print this and exit 0
  -V, --version    print the version and exit 0";

pub fn run(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    now_unix_secs: &dyn Fn() -> i64,
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

    // The published dataset, and the export that produces it. Both sit with the other pre-profile
    // exits, and both are compiled only under `venue-fetch` — a build without it refuses by name
    // rather than ignoring the flag, for the reason the `--fetch` arm below gives.
    if has_flag(args, "--fetch-starter") {
        #[cfg(feature = "venue-fetch")]
        {
            return run_fetch_starter(vars, args);
        }
        #[cfg(not(feature = "venue-fetch"))]
        {
            eprintln!(
                "backtest: this build has no network fetch — it was compiled without the \
                 `venue-fetch` feature. The shipped release binary and the container image both \
                 have it. `--seed-demo` needs no network and works in every build."
            );
            return ExitCode::from(2);
        }
    }
    if let Some(spec) = arg(args, "--export") {
        #[cfg(feature = "venue-fetch")]
        {
            return run_export(vars, args, &spec);
        }
        #[cfg(not(feature = "venue-fetch"))]
        {
            let _ = &spec;
            eprintln!(
                "backtest: this build has no `--export` — it was compiled without the \
                 `venue-fetch` feature, which carries the Parquet writer's caller."
            );
            return ExitCode::from(2);
        }
    }

    // The REAL-DATA half of the empty-store answer, beside the synthetic one below because they
    // are the same question asked by two kinds of user. Compiled only under `venue-fetch` (see
    // `crate::fetch`'s module doc for why that feature must stay off by default); a build without
    // it REFUSES the flag by name rather than ignoring it, because a fetch that silently does
    // nothing leaves the user believing they have data.
    if let Some(spec) = arg(args, "--fetch") {
        #[cfg(feature = "venue-fetch")]
        {
            return run_fetch(vars, args, &spec, now_unix_secs);
        }
        #[cfg(not(feature = "venue-fetch"))]
        {
            let _ = &spec;
            eprintln!(
                "backtest: this build has no venue fetch — it was compiled without the \
                 `venue-fetch` feature, so `--fetch` can reach no venue. The shipped release \
                 binary and the container image both have it; a `cargo build` of this crate does \
                 not unless you ask for it. `--seed-demo` needs no network and works in every \
                 build."
            );
            return ExitCode::from(2);
        }
    }

    // ⚠ THE EMPTY-STORE ANSWER, and it sits with the other pre-profile exits for the same reason
    // they do: it needs no profile and no strategy registry, only a store root.
    //
    // A clean install has an empty hist store, so the shipped example profile — which names a
    // slice — reports a run with no trades, and nothing distinguishes that from a strategy that
    // never fired. `vike_data::demo` writes a tape the shipped profile already names;
    // `crates/vike-cli/tests/demo_tape_profile.rs` holds the two in agreement, so this
    // writes not "some data" but THE data the next command reads.
    //
    // Deliberately on `backtest` rather than `vike-cli`: writing a store needs `DataFusionHist`,
    // and vike-cli is DataFusion-free BY CONSTRUCTION (the `light-consumers` CI lane asserts it).
    // The tool that CONSUMES hist data is the honest place for the command that creates some.
    if has_flag(args, "--seed-demo") {
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
        // hazard this whole feature carries: a result computed on invented prices reads exactly
        // like a result computed on real ones.
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
        return ExitCode::SUCCESS;
    }

    // The EMPTY-STORE answer's inverse, and it sits beside it for the same reason: it needs a store
    // root and nothing else. See [`run_rm_series`].
    if has_flag(args, "--rm-series") {
        return run_rm_series(vars, args);
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

    let Some(profile_path) = arg(args, "--profile") else {
        eprintln!(
            "backtest: --profile <path> is required (or --list to show strategies)\n\n{USAGE}"
        );
        return ExitCode::from(2);
    };

    let profile = match BacktestProfile::from_path(&PathBuf::from(&profile_path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("backtest: failed to load profile {profile_path:?}: {e}");
            return ExitCode::from(2);
        }
    };

    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("backtest: failed to open hist store at {root:?}: {e}");
            return ExitCode::FAILURE;
        }
    };

    if profile.is_sweep() {
        // `multi` selects the composite objective path; the four classic names keep the original
        // `run_sweep` comparator so default ranking/output is byte-identical. `Copy` (RankMetric is
        // `Copy`) so the several `match choice` sites below each read it without a move.
        #[derive(Clone, Copy)]
        enum RankChoice {
            Metric(RankMetric),
            Multi,
        }
        let choice = match arg(args, "--rank-by") {
            Some(s) if s.eq_ignore_ascii_case("multi") => RankChoice::Multi,
            Some(s) => match RankMetric::from_str_ci(&s) {
                Some(m) => RankChoice::Metric(m),
                None => {
                    eprintln!(
                        "backtest: invalid --rank-by {s:?} (expected sharpe|return|max_dd|equity|multi)"
                    );
                    return ExitCode::from(2);
                }
            },
            None => RankChoice::Metric(RankMetric::default()),
        };

        // `--optimizer tpe` is the Bayesian ask/tell search. It takes precedence over `--search`
        // and returns the same ranked `SweepReport`; absent, control falls through to the untouched
        // `--search grid|euler` path below (default output byte-identical to before).
        if let Some(optimizer) = arg(args, "--optimizer") {
            if !optimizer.eq_ignore_ascii_case("tpe") {
                eprintln!("backtest: invalid --optimizer {optimizer:?} (expected tpe)");
                return ExitCode::from(2);
            }
            let trials = match arg(args, "--trials") {
                None => harness::TpeConfig::DEFAULT_TRIALS,
                Some(s) => match s.parse::<usize>() {
                    Ok(n) if n > 0 => n,
                    _ => {
                        eprintln!("backtest: invalid --trials {s:?} (expected a positive integer)");
                        return ExitCode::from(2);
                    }
                },
            };
            let seed = match arg(args, "--seed") {
                None => 0u64,
                Some(s) => match s.parse::<u64>() {
                    Ok(v) => v,
                    _ => {
                        eprintln!("backtest: invalid --seed {s:?} (expected a u64)");
                        return ExitCode::from(2);
                    }
                },
            };
            // Reuse the SAME objective seam `--rank-by` selected; TPE never scores its own way.
            let (objective, label) = match choice {
                RankChoice::Metric(m) => (m.objective(), m.name().to_string()),
                RankChoice::Multi => {
                    (crate::objective::multi_metric(Default::default()), "multi".to_string())
                }
            };
            let cfg = harness::TpeConfig::new(trials, seed);
            let sweep_report =
                match harness::run_tpe(&profile, Arc::new(store), &objective, label, &cfg) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("backtest: tpe run failed: {e}");
                        return ExitCode::FAILURE;
                    }
                };
            // The run summary, on stderr so `--json` stdout stays a clean document.
            let best = sweep_report.rows.first().and_then(|r| r.score);
            eprintln!(
                "tpe: {trials} trials (seed {seed}), best score {}",
                best.map(|s| format!("{s:.4}")).unwrap_or_else(|| "n/a".to_string())
            );
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

        // `--search euler` is the opt-in successive-halving refinement; anything else (including
        // the absent flag) takes the untouched grid path below.
        let euler = match arg(args, "--search") {
            None => false,
            Some(s) if s.eq_ignore_ascii_case("grid") => false,
            Some(s) if s.eq_ignore_ascii_case("euler") => true,
            Some(s) => {
                eprintln!("backtest: invalid --search {s:?} (expected grid|euler)");
                return ExitCode::from(2);
            }
        };

        let run = if euler {
            let depth = match arg(args, "--euler-depth") {
                None => EulerConfig::DEFAULT_MAX_DEPTH,
                Some(s) => match s.parse::<u32>() {
                    Ok(d) => d,
                    Err(_) => {
                        eprintln!("backtest: invalid --euler-depth {s:?} (expected an integer)");
                        return ExitCode::from(2);
                    }
                },
            };
            let cfg = EulerConfig::with_depth(depth);
            // Reuse the SAME objective seam --rank-by already selects; euler never scores its own
            // way. Rows therefore always carry a `score` (the objective path's shape).
            let (objective, label) = match choice {
                RankChoice::Metric(m) => (m.objective(), m.name().to_string()),
                RankChoice::Multi => {
                    (crate::objective::multi_metric(Default::default()), "multi".to_string())
                }
            };
            harness::run_sweep_euler(&profile, Arc::new(store), &objective, label, &cfg).map(
                |(report, budget)| {
                    // The tradeoff, on stderr so `--json` stdout stays a clean document.
                    eprintln!("{budget}");
                    report
                },
            )
        } else {
            match choice {
                RankChoice::Metric(m) => harness::run_sweep(&profile, Arc::new(store), m),
                RankChoice::Multi => {
                    let objective = crate::objective::multi_metric(Default::default());
                    harness::run_sweep_with(&profile, Arc::new(store), &objective, "multi")
                }
            }
        };
        let sweep_report = match run {
            Ok(r) => r,
            Err(e) => {
                eprintln!("backtest: sweep run failed: {e}");
                return ExitCode::FAILURE;
            }
        };

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
    let result = match harness::run_backtest(&profile, Arc::new(store)) {
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

/// The `--fetch` body. Split out of [`run`] because it is the one arm that touches the NETWORK, and
/// a reader auditing what this binary can reach should find that in one place rather than inside a
/// 400-line dispatcher.
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

/// `--fetch-starter` — the published dataset, for a box that cannot reach a venue.
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

/// `--export` — one series out of the store as a standalone Parquet file.
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

// ─── `--rm-series`: emptying the store ──────────────────────────────────────────────────────────

/// `backtest --rm-series` — DELETE stored series, irreversibly.
///
/// # Why this verb is on the ENGINE
///
/// For `--export`'s reason, one step further: emptying a store needs `DataFusionHist`, and
/// `vike-cli` is DataFusion-FREE by construction (its manifest argues every edge; CI's
/// `light-consumers` lane asserts it). `vike-cli data rm` is a route to this arm, exactly as
/// `vike-cli data fetch` is a route to `--fetch`. It is a first-class verb here rather than only a
/// spawn target because an operator ON the box — which is where a cleanup happens, and where the
/// 2026-09-07 the CI box cleanup DID happen — should not need a datahub to empty their own store.
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
/// ⚠ **No `--yes` and no terminal is a REFUSAL, never a read.** `yes | backtest --rm-series …` is
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
        eprintln!("backtest --rm-series: {e}\n\n{USAGE}");
        return ExitCode::from(2);
    }
    // Resolved BEFORE the store is opened: a `--produced-by` spelling is a fact about the argument,
    // and refusing a typo without touching a store is the cheaper failure.
    let produced_by =
        match arg(args, "--produced-by").map(|s| vike_data::store_kind::resolve_produced_by(&s)) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                eprintln!("backtest --rm-series: {e}");
                return ExitCode::from(2);
            }
            None => None,
        };
    // ⚠ The SWEEP rule, and it is the engine's as much as the CLI's: this arm is reachable directly.
    if selector.is_sweep() && produced_by.is_none() {
        eprintln!(
            "backtest --rm-series: `{}` matches more than one series, so --produced-by is \
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
            eprintln!("backtest --rm-series: {e}");
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
        eprintln!("backtest --rm-series: provenance REFUSED — nothing was deleted");
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
        eprintln!("backtest --rm-series: {e}");
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
            eprintln!("backtest --rm-series: {e}");
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
             `yes | backtest --rm-series …` is exactly what this refuses.",
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

/// The `--json` document for `--rm-series`.
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
    /// back to reading, because `yes | backtest --rm-series …` is indistinguishable from a person
    /// once you have decided to read a pipe.
    #[test]
    fn no_yes_and_no_terminal_refuses_rather_than_reading() {
        let err = confirm_removal(&plan(3), false, false).unwrap_err();
        assert!(err.contains("not a terminal"), "{err}");
        assert!(err.contains("3 series"), "the refusal names what it did not delete: {err}");
    }
}
