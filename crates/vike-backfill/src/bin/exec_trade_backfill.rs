//! `exec_trade_backfill` — import a venue account TRADE-HISTORY EXPORT (JSON) into the Tier-2
//! `kind=exec_fill` execution log (unified-journaling #2, historical feeder). The offline twin of
//! the live `JournalMaterializer`: both write the same series, so live + historical fills converge.
//!
//! Usage:
//!   exec_trade_backfill --file <myTrades.json> --symbol BTCUSDT [--venue binance] [--store DIR]
//!
//! `--file` is a venue trade-history export (v1: Binance `myTrades` JSON — needs no credentials).
//! Idempotent: the commit-key embeds the fill window, so re-importing the same export is a store
//! no-op. `--store` defaults to `$VIKE_HIST_STORE` else `<repo>/market_data/hist`.

use vike_backfill::cli::{CliSpec, arg, store_root};
use vike_backfill::exec_import::parse_binance_my_trades;
use vike_data::{DataFusionHist, HistStore};

const USAGE: &str = "\
usage: exec_trade_backfill --file PATH --symbol SYM [--venue V] [--store DIR]

Import a Binance `myTrades` JSON export as exec fills into the hist store. Idempotent per fill
window: re-importing the same export is a no-op.

  --file PATH    the exported myTrades JSON (required)
  --symbol SYM   the symbol those fills belong to (required)
  --venue V      venue label to store under (default binance)
  --store DIR    hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "exec_trade_backfill",
    usage: USAGE,
    valued: &["--file", "--symbol", "--venue", "--store"],
    toggles: &[],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);

    let Some(file) = arg(&args, "--file") else {
        eprintln!("exec_trade_backfill: --file is required\n\n{USAGE}");
        std::process::exit(2);
    };
    let Some(symbol) = arg(&args, "--symbol") else {
        eprintln!("error: --symbol is required");
        std::process::exit(2);
    };
    let venue = arg(&args, "--venue").unwrap_or_else(|| "binance".to_string());
    let root = store_root(arg(&args, "--store").as_deref(), &std::env::vars().collect());

    let json = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading {file}: {e}");
            std::process::exit(1);
        }
    };
    let rows = match parse_binance_my_trades(&json, &venue, &symbol) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: parsing {file}: {e:?}");
            std::process::exit(1);
        }
    };
    if rows.is_empty() {
        println!("no trades in {file}");
        return;
    }

    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: opening store {}: {e:?}", root.display());
            std::process::exit(1);
        }
    };
    // idempotent per fill window — re-importing the same export is a no-op
    let (first, last) = (rows.first().unwrap().ts, rows.last().unwrap().ts);
    let key = format!("import-{venue}-{symbol}-{first}-{last}");
    match hist.append_exec_fills(&venue, &symbol, &rows, Some(&key)) {
        Ok(n) => println!(
            "imported {n} exec fills for {venue}/{symbol} [{first}..{last}] into {}",
            root.display()
        ),
        Err(e) => {
            eprintln!("error: appending: {e:?}");
            std::process::exit(1);
        }
    }
}
