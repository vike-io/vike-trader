//! `poly_ch_backtest` — run a `BacktestProfile` over the LIVE Polymarket L2 recorder's ClickHouse
//! tables (`polymarket.book_events` / `polymarket.l1_quotes`), with no local Parquet copy and no
//! ingest step.
//!
//! This is the caller for the #691 backtest bridge ([`vike_backfill::backtest_bridge`]): it points
//! the vike-backtest harness' `run_backtest` at a [`ClickHousePolyHistStore`] instead of the
//! `DataFusionHist` the sibling `backtest` bin (in vike-backtest) opens off a directory. Because
//! `run_backtest` now takes `Arc<dyn HistStore + Send + Sync>`, the ENTIRE profile/registry/report
//! machinery — strategy resolution, tick-mode `replay_ticks`, binary-resolution settlement, the
//! `BacktestReport` metrics summary — is reused verbatim; only the store's bytes come from
//! ClickHouse. This lives in vike-backfill (not vike-backtest) because `ClickHousePolyHistStore`
//! does, and vike-backtest must never depend on vike-backfill — the bridge doc's "leaf binary that
//! can depend on it directly".
//!
//! ```sh
//! poly_ch_backtest --profile run.toml [--clickhouse-bin clickhouse-client] [--db polymarket] [--json]
//! poly_ch_backtest --profile sweep.toml [--rank-by sharpe|return|max_dd|equity]
//! poly_ch_backtest --list   # list registered strategy names (same registry as the `backtest` bin)
//! ```
//!
//! SWEEP profiles run here too: a `[sweep]` table dispatches to [`harness::run_sweep`] (ranked by
//! `--rank-by`, default `sharpe`) instead of one `run_backtest`, printing the same ranked
//! `SweepReport` table/JSON the sibling `backtest` bin prints. This used to be an explicit error,
//! on the premise that the sweep path was keyed to the concrete `DataFusionHist` and had not been
//! relaxed to the trait object. That premise is stale: `run_sweep`/`run_sweep_exec`/`run_sweep_with`
//! all take the SAME `Arc<dyn HistStore + Send + Sync>` `run_backtest` takes (their rayon fan-out
//! only clones the `Arc` and calls the dispatcher — see `harness::run_backtest`'s own doc), and
//! [`ClickHousePolyHistStore`] is `Send + Sync` (two `String` fields). Only the classic four
//! `RankMetric` names are wired; the `multi` objective, `--search euler` and `--optimizer tpe` stay
//! with the `backtest` bin for now (each takes the identical trait object, so wiring them here is
//! mechanical, not blocked).
//!
//! ⚠ CONCURRENCY IS SERVER LOAD HERE. Every sweep point is a WHOLE independent run, so N
//! concurrently-running points mean N concurrent `clickhouse-client` subprocesses and N
//! independently materialized tick slices against the box the live recorder is writing to (the latency box).
//! Nothing new is invented to bound that — the harness' existing knobs are the bound:
//! `VIKE_SWEEP_THREADS` (default `min(4, cores)`) caps how many points run at once, and
//! `VIKE_SWEEP_SEQUENTIAL=1` pins the whole process back to one-at-a-time. Size them against the
//! ClickHouse server's headroom, not against the core count. Concurrent points do not RACE: each
//! scan shells its own client and exports to its own collision-free temp file (`tmp_export_path`),
//! the same fan-out the sibling `poly_mm_batch` bin has always done over this store.
//!
//! The store is READ-ONLY (the recorder is the only writer of `polymarket.*`); this bin never
//! mutates ClickHouse. It shells out to `clickhouse-client` (`SELECT ... FORMAT Parquet`) once per
//! symbol, so it must run where that binary is authed against the recorder's ClickHouse (the latency box).

use std::process::ExitCode;
use std::sync::Arc;

use vike_backfill::backtest_bridge::{ClickHousePolyHistStore, DB};
use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, scratch_root};
use vike_backtest::harness::{self, BacktestProfile, RankMetric};
use vike_data::HistStore;

const USAGE: &str = "\
usage: poly_ch_backtest --profile PATH [--clickhouse-bin BIN] [--db NAME]
                        [--rank-by sharpe|return|max_dd|equity] [--json] [--trades]
       poly_ch_backtest --list

Run a BacktestProfile against the live recorder polymarket.* ClickHouse tables directly, so a
backtest reads recorded L2 with no local copy of it. A profile carrying a [sweep] table runs the
whole cartesian grid and prints a ranked comparison instead of one report.

  --profile PATH        the backtest profile TOML (required unless --list)
  --list                print the registry strategy names and exit 0
  --clickhouse-bin BIN  the clickhouse client to spawn (default clickhouse-client)
  --db NAME             the ClickHouse database to read
  --rank-by METRIC      sweep ranking metric: sharpe | return | max_dd | equity (sweeps only)
  --json                emit the report as JSON instead of the printable summary
  --trades              also dump the trade list (ignored on a [sweep] profile — one report per
                        point means there is no single trade list; re-run the chosen point alone)
  -h, --help            print this and exit 0
  -V, --version         print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "poly_ch_backtest",
    usage: USAGE,
    valued: &["--profile", "--clickhouse-bin", "--db", "--rank-by"],
    toggles: &["--list", "--json", "--trades"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _log_guards = vike_log::init(log_config("poly-ch-backtest"));

    if has_flag(&args, "--list") {
        for name in harness::STRATEGIES {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }

    let Some(profile_path) = arg(&args, "--profile") else {
        eprintln!(
            "poly_ch_backtest: --profile is required (or --list to show strategies)\n\n{USAGE}"
        );
        return ExitCode::from(2);
    };

    let profile = match BacktestProfile::from_path(std::path::Path::new(&profile_path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("poly_ch_backtest: failed to load profile {profile_path:?}: {e}");
            return ExitCode::from(2);
        }
    };

    let ch_bin = arg(&args, "--clickhouse-bin").unwrap_or_else(|| "clickhouse-client".to_string());
    let db = arg(&args, "--db").unwrap_or_else(|| DB.to_string());
    // `<project>/tmp` — where the bridge stages each ClickHouse export on its way into the decoder.
    // Resolved HERE because this is the composition root; the bridge takes it as a parameter and
    // reads no environment of its own. `scratch_root` also sweeps, which is what bounds the
    // staged-export population an aborted run leaves behind.
    let scratch = scratch_root(&std::env::vars().collect());
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(ClickHousePolyHistStore::new(ch_bin, db, scratch));

    // A `[sweep]` table runs the whole cartesian grid through `harness::run_sweep` over this SAME
    // store handle (module doc: the trait-object premise, and the concurrency-is-server-load
    // warning). A per-point failure is recorded as that row's `error`, never fatal — so a grid
    // where one window has no recorded ticks still prints every other point.
    if profile.is_sweep() {
        let rank_by = match arg(&args, "--rank-by") {
            None => RankMetric::default(),
            Some(s) => match RankMetric::from_str_ci(&s) {
                Some(m) => m,
                None => {
                    eprintln!(
                        "poly_ch_backtest: invalid --rank-by {s:?} (expected \
                         sharpe|return|max_dd|equity)"
                    );
                    return ExitCode::from(2);
                }
            },
        };
        // Explicit, not silent: a sweep has one report PER POINT, so there is no single trade list
        // to dump. Run the winning point as a single-profile backtest to get its `--trades`.
        if has_flag(&args, "--trades") {
            eprintln!(
                "poly_ch_backtest: --trades is ignored on a [sweep] profile (one report per point, \
                 no single trade list) — re-run the chosen point as a single-point profile"
            );
        }

        let sweep_report = match harness::run_sweep(&profile, store, rank_by) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("poly_ch_backtest: sweep run failed: {e}");
                return ExitCode::FAILURE;
            }
        };

        if has_flag(&args, "--json") {
            match serde_json::to_string_pretty(&sweep_report) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("poly_ch_backtest: failed to serialize sweep report: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            print!("{sweep_report}");
        }

        return ExitCode::SUCCESS;
    }

    let result = match harness::run_backtest(&profile, store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("poly_ch_backtest: run failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = harness::BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    );

    if has_flag(&args, "--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("poly_ch_backtest: failed to serialize report: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{report}");
    }

    // `--trades`: dump each closed round-trip to STDERR (side, hold time, entry/exit price, size,
    // pnl) so a maker backtest can be inspected fill-by-fill — is it capturing spread or riding
    // directional swings? Stderr keeps `--json` stdout clean.
    if has_flag(&args, "--trades") {
        eprintln!("# side entry_ts hold_ms size entry_px exit_px pnl");
        for t in &result.trades {
            eprintln!(
                "{} {} hold_ms={} size={:.0} entry_px={:.4} exit_px={:.4} pnl={:+.4}",
                if t.is_long { "LONG " } else { "SHORT" },
                t.entry_ts,
                t.exit_ts - t.entry_ts,
                t.size,
                t.entry_price,
                t.exit_price,
                t.pnl,
            );
        }
    }

    ExitCode::SUCCESS
}
