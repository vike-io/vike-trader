//! Runnable ClickHouse → hist-store backfill for the 1-second spot reference series
//! (`data_history.spot_1s`). Reads ClickHouse SELECT-only and ingests a `kind=quote` tick series
//! per symbol — the spot feed `vike_backtest::CheapNp` samples for σ and `s_now` (port backlog G8).
//!
//! Usage:
//!   clickhouse_spot_backfill --from YYYY-MM-DD --to YYYY-MM-DD
//!       [--symbol BTCUSDT] [--market spot] [--venue spot]
//!       [--store DIR] [--ch-bin clickhouse-client] [--keep-tmp] [--dry-run]
//!
//! Iterates every UTC day in `[--from, --to]` inclusive; per day it runs one
//! `clickhouse-client --query "... FINAL ... FORMAT Parquet"` export (streamed to a temp file),
//! decodes it a row group at a time and appends under a `clickhouse:spot:{symbol}:{day}` commit
//! key — so re-running a day is idempotent. A failed day is logged and skipped; one bad day never
//! aborts the range. `--dry-run` prints the resolved days + the exact queries and exits without
//! touching ClickHouse or the store.
//!
//! Two source traps are handled in [`vike_backfill::clickhouse_spot`], not here, but they are the
//! reason this bin exists rather than an ad-hoc export: the table is a `ReplacingMergeTree(_v)`
//! (so the query is `FINAL`), and ~11 % of its seconds are missing (so the series is stored RAW
//! and SPARSE — `fair_value::trailing_sigma` does the 1-second-grid interpolation itself).

use std::path::PathBuf;
use std::process::ExitCode;

use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, scratch_root, store_root};
use vike_backfill::clickhouse_poly::run_export;
use vike_backfill::clickhouse_spot::{MARKET, VENUE, ingest_spot_file, spot_query};
use vike_data::DataFusionHist;
use vike_model::scratch::ScratchDir;
use vike_model::time::{civil_from_days, days_from_civil, days_in_range, parse_ymd};

fn ymd_str(y: i64, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn next_day(y: i64, m: u32, d: u32) -> String {
    let (ny, nm, nd) = civil_from_days(days_from_civil(y, m, d) + 1);
    ymd_str(ny, nm, nd)
}

const USAGE: &str = "\
usage: clickhouse_spot_backfill --from YYYY-MM-DD --to YYYY-MM-DD
                                [--symbol BTCUSDT] [--market spot] [--venue spot]
                                [--store DIR] [--ch-bin clickhouse-client]
                                [--keep-tmp] [--dry-run]

SELECT-only export of the 1-second spot reference series (`data_history.spot_1s`) from a local
ClickHouse into the hist store as a kind=quote tick series — the spot feed CheapNp samples for
sigma and s_now. One day per export, idempotent by a `clickhouse:spot:{symbol}:{day}` commit key;
a failed day is logged and skipped without aborting the range.

  --from DATE     first UTC day, YYYY-MM-DD (inclusive)
  --to DATE       last UTC day, YYYY-MM-DD (inclusive)
  --symbol SYM    source symbol (default BTCUSDT)
  --market M      source market column value (default spot)
  --venue V       venue label to store under (default spot)
  --store DIR     hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --ch-bin BIN    the clickhouse client to spawn (default clickhouse-client)
  --keep-tmp      keep the per-day Parquet exports instead of deleting them
  --dry-run       print the resolved days and the exact queries, then exit without touching
                  ClickHouse or the store
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "clickhouse_spot_backfill",
    usage: USAGE,
    valued: &["--from", "--to", "--symbol", "--market", "--venue", "--store", "--ch-bin"],
    toggles: &["--keep-tmp", "--dry-run"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _log_guards = vike_log::init(log_config("clickhouse-spot-backfill"));

    let (Some(from), Some(to)) = (arg(&args, "--from"), arg(&args, "--to")) else {
        eprintln!("clickhouse_spot_backfill: --from and --to are required\n\n{USAGE}");
        return ExitCode::from(2);
    };

    let symbol = arg(&args, "--symbol").unwrap_or_else(|| "BTCUSDT".to_string());
    let market = arg(&args, "--market").unwrap_or_else(|| MARKET.to_string());
    let venue = arg(&args, "--venue").unwrap_or_else(|| VENUE.to_string());
    let ch_bin = arg(&args, "--ch-bin").unwrap_or_else(|| "clickhouse-client".to_string());
    let keep_tmp = has_flag(&args, "--keep-tmp");
    let dry_run = has_flag(&args, "--dry-run");
    // ONE environment sweep for this bin, shared by the store root and the scratch root — so the
    // two cannot resolve against different `$VIKE_SETTINGS_DIR` answers.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let root = store_root(arg(&args, "--store").as_deref(), &env);

    let (start, end) = match (parse_ymd(&from), parse_ymd(&to)) {
        (Ok(s), Ok(e)) => (s, e),
        (Err(e), _) | (_, Err(e)) => {
            tracing::error!("bad --from/--to: {e}");
            return ExitCode::FAILURE;
        }
    };
    let days = days_in_range((start.0, start.1, start.2), (end.0, end.1, end.2));
    if days.is_empty() {
        tracing::error!("empty day range (--from after --to?): {from}..{to}");
        return ExitCode::FAILURE;
    }

    tracing::info!(
        "clickhouse-spot-backfill: {} day(s) [{from}, {to}], {symbol}/{market} -> \
         venue={venue} at {}",
        days.len(),
        root.display()
    );

    if dry_run {
        for (y, m, d) in &days {
            let day = ymd_str(*y, *m, *d);
            let nxt = next_day(*y, *m, *d);
            tracing::info!("dry-run {day} query:\n{}", spot_query(&symbol, &market, &day, &nxt));
        }
        tracing::info!("dry-run: nothing exported, nothing written");
        return ExitCode::SUCCESS;
    }

    let store = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    // An OWNED staging directory under `<project>/tmp`, removed when this guard drops at the end of
    // `main` — including on the panic path. It used to be a FIXED name under the system temp
    // directory, which leaked every export it ever staged and, on a box where CI and agents run as
    // different users, handed whoever created it first permanent ownership. See
    // `vike_backfill::cli::scratch_root` for both halves of that argument.
    let staged = match ScratchDir::create_in(&scratch_root(&env), "clickhouse_spot_backfill") {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("create scratch dir: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ `--keep-tmp` gives up ownership HERE rather than at the end of `main`. Its whole promise is
    // that the staged exports outlive the run, and a guard that ran on some exit paths and not
    // others would honour the flag depending on which error fired.
    let (tmp_dir, _staged): (PathBuf, Option<ScratchDir>) = if keep_tmp {
        let kept = staged.keep();
        tracing::info!(dir = %kept.display(), "--keep-tmp: staged exports are left in place");
        (kept, None)
    } else {
        (staged.path().to_path_buf(), Some(staged))
    };

    let (mut total, mut days_ok, mut days_err) = (0usize, 0usize, 0usize);

    for (y, m, d) in &days {
        let day = ymd_str(*y, *m, *d);
        let nxt = next_day(*y, *m, *d);
        let dest = tmp_dir.join(format!("spot_{symbol}_{day}.parquet"));

        match run_export(&ch_bin, &spot_query(&symbol, &market, &day, &nxt), &dest)
            .and_then(|()| ingest_spot_file(&store, &dest, &venue, &symbol, &day))
        {
            Ok(n) => {
                total += n;
                days_ok += 1;
                tracing::info!("{day}: {n} spot quotes");
            }
            Err(e) => {
                days_err += 1;
                tracing::error!("{day}: spot backfill failed: {e}");
            }
        }
        if !keep_tmp {
            let _ = std::fs::remove_file(&dest);
        }
    }

    tracing::info!(
        "clickhouse-spot-backfill done: {days_ok} day(s) ok, {days_err} failed, {total} quotes"
    );
    if days_err > 0 && days_ok == 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
