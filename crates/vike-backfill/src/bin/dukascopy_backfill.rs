//! Runnable Dukascopy tick backfill into the DataFusion hist store.
//!
//! Usage:
//!   dukascopy_backfill <ROOT_DIR> <SYMBOL> <START_MS> <END_MS> [INTERVAL]
//!
//! Fetches `[START_MS, END_MS]` tick history for SYMBOL (e.g. EURUSD) from Dukascopy's keyless
//! `.bi5` datafeed, appends the quotes into the hist store rooted at ROOT_DIR, then — if INTERVAL is
//! given (e.g. "1m") — resamples the stored quotes into OHLCV bars. Idempotent: re-running the same
//! window is a no-op (0 rows). History-only (Dukascopy publishes with a T+1 lag).

use std::process::ExitCode;

use vike_backfill::cli::{CliSpec, log_config};
use vike_backfill::dukascopy::{backfill_dukascopy_quotes, resample_and_store_bars};
use vike_data::{DataFusionHist, TsRange};

const USAGE: &str = "\
usage: dukascopy_backfill <ROOT_DIR> <SYMBOL> <START_MS> <END_MS> [INTERVAL]

Fetch tick history for SYMBOL from Dukascopy keyless .bi5 datafeed, append the quotes to the
DataFusion hist store at ROOT_DIR, then — if INTERVAL is given — resample the stored quotes into
OHLCV bars. Idempotent: re-running the same window writes 0 rows. History-only (Dukascopy
publishes with a T+1 lag).

The arguments are POSITIONAL, in this order; the first four are required:
  ROOT_DIR    hist-store root directory (created if absent)
  SYMBOL      Dukascopy instrument, e.g. EURUSD
  START_MS    window start, epoch MILLISECONDS (inclusive)
  END_MS      window end, epoch MILLISECONDS (inclusive)
  INTERVAL    optional bar interval to resample into, e.g. 1m

  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "dukascopy_backfill",
    usage: USAGE,
    valued: &[],
    toggles: &[],
    // <ROOT_DIR> <SYMBOL> <START_MS> <END_MS> [INTERVAL] — the fifth is optional.
    positionals: 5,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _log_guards = vike_log::init(log_config("dukascopy-backfill"));
    tracing::info!("dukascopy-backfill starting");
    if args.len() < 5 {
        eprintln!(
            "dukascopy_backfill: ROOT_DIR, SYMBOL, START_MS and END_MS are all required\n\n{USAGE}"
        );
        return ExitCode::from(2);
    }
    let root = &args[1];
    let symbol = &args[2];
    let start_ms: i64 = match args[3].parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("bad START_MS {:?}: {e}", args[3]);
            return ExitCode::FAILURE;
        }
    };
    let end_ms: i64 = match args[4].parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("bad END_MS {:?}: {e}", args[4]);
            return ExitCode::FAILURE;
        }
    };
    let interval = args.get(5).cloned();

    let hist = match DataFusionHist::open(root) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("open hist store at {root}: {e}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!("fetching dukascopy {symbol} [{start_ms}, {end_ms}] -> {root} ...");
    match backfill_dukascopy_quotes(&hist, symbol, start_ms, end_ms) {
        Ok(n) => tracing::info!("appended {n} quote rows (0 = window already ingested)"),
        Err(e) => {
            tracing::error!("backfill failed: {e}");
            return ExitCode::FAILURE;
        }
    }

    if let Some(iv) = interval {
        match resample_and_store_bars(&hist, symbol, &iv, TsRange::of(start_ms, end_ms)) {
            Ok(n) => tracing::info!("resampled {n} {iv} bars"),
            Err(e) => {
                tracing::error!("resample failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
