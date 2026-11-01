//! `exec_trade_backfill` — import a venue account TRADE-HISTORY EXPORT (JSON) into the Tier-2
//! `kind=exec_fill` execution log (unified-journaling #2, historical feeder). The offline twin of
//! the live `JournalMaterializer`: both write the same series, so live + historical fills converge.
//!
//! Usage:
//!   exec_trade_backfill --file <myTrades.json> --symbol BTCUSDT [--venue binance] [--store DIR]
//!
//! `--file` is a venue trade-history export (v1: Binance `myTrades` JSON — needs no credentials).
//! Idempotent: the commit-key embeds the fill window, so re-importing the same export is a store
//! no-op. `--store` defaults to `<repo>/market_data/hist` in a checkout, else the project's own.

use vike_backfill::cli::{parse, store_root};
use vike_backfill::exec_import::parse_binance_my_trades;
use vike_data::HistStore;
use vike_store::DataFusionHist;

/// Import a Binance `myTrades` JSON export as exec fills into the hist store. Idempotent per fill
/// window: re-importing the same export is a no-op.
#[derive(clap::Parser, Debug)]
struct Args {
    /// The exported myTrades JSON
    #[arg(long)]
    file: String,
    /// The symbol those fills belong to
    #[arg(long)]
    symbol: String,
    /// Venue label to store under
    #[arg(long, default_value = "binance")]
    venue: String,
    /// Hist-store root (default: <repo>/market_data/hist, else the project's own)
    #[arg(long)]
    store: Option<String>,
}

fn main() {
    // ⚠ FIRST — `--help`, `--version` and a usage error answer before any store or file is touched.
    let Args { file, symbol, venue, store } = parse("exec_trade_backfill");
    let root = store_root(store.as_deref(), &std::env::vars().collect());

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
