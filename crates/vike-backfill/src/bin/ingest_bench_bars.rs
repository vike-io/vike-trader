//! Ingest the Python parquet bar-cache into the self-contained bench hist store — the pure-Rust
//! replacement for the retired `bench/export_python_db_to_rust.py` (deleted by `d23afcf9`, the
//! bench-exporter retirement; no venv / pandas / SQLite).
//! Idempotent by commit key, so re-runs are no-ops.
//!
//!   cargo run -p vike-backfill --bin ingest_bench_bars --release
//!
//! Source: `<app>/storage/parquet/<SYMBOL>/1m/2026-06.parquet` (columns ts,open,high,low,close,volume)
//! Dest:   `<rust>/market_data/bench_hist` (DataFusionHist: kind=bar/venue=binance/symbol=…/interval=1m)
//!
//! ⚠ **This bin read NO argv at all, so `ingest_bench_bars --help` PERFORMED THE INGEST.** That is
//! the worst shape in the family: `--help` is the first thing a person types to find out what a
//! command does, and the safest-looking invocation there is — here it opened a store, created a
//! directory and wrote bars. Every other bin in this crate at least *failed* on `--help`. The
//! triage below now runs before the log subscriber and before the store, so `--help` touches
//! nothing; `tests/help_cli.rs` asserts the exit status AND that no store directory appeared.

use std::path::Path;
use std::process::ExitCode;

use vike_backfill::cli::{CliSpec, log_config};
use vike_data::DataFusionHist;

const USAGE: &str = "\
usage: ingest_bench_bars

Ingest the Python parquet bar-cache into this checkout's self-contained bench hist store, for the
`cargo bench -p vike-backtest --features bench-hist` two-engine benchmark. Idempotent by commit
key, so a re-run is a no-op.

It takes NO options — both paths are fixed at compile time:
  source  <vike-trader-app>/storage/parquet/<SYMBOL>/1m/<MONTH>.parquet
  dest    <this checkout>/market_data/bench_hist  (kind=bar/venue=binance/interval=1m)

  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0";

/// This bin takes no options, so the whole contract is the three things every binary owes a caller.
const SPEC: CliSpec =
    CliSpec { bin: "ingest_bench_bars", usage: USAGE, valued: &[], toggles: &[], positionals: 0 };

const PARQUET: &str = r"C:\Projects\vike-trader-app\storage\parquet"; // external app cache (other repo)
const MONTH: &str = "2026-06";
// canonical column order — BTCUSDT is also the single-symbol bench series
const SYMBOLS: [&str; 5] = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "DOGEUSDT", "ADAUSDT"];

/// This checkout's `market_data/bench_hist`, derived at COMPILE time from the crate dir — so the ingest
/// writes to its OWN checkout and works from any worktree, never a hardcoded absolute path.
fn hist_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("market_data")
        .join("bench_hist")
}

fn main() -> ExitCode {
    // ⚠ FIRST — before the log subscriber, before the store, before the ingest. This is the bin
    // whose `--help` used to run the whole job.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }

    let _log_guards = vike_log::init(log_config("ingest-bench-bars"));
    tracing::info!("ingest-bench-bars starting");
    let root = hist_root();
    let hist = DataFusionHist::open(&root).expect("open bench hist store");
    let mut total = 0usize;
    for sym in SYMBOLS {
        let path = format!(r"{PARQUET}\{sym}\1m\{MONTH}.parquet");
        let key = format!("bench:{sym}:{MONTH}"); // idempotency: re-runs are no-ops
        let n = hist
            .append_bars_from_parquet(Path::new(&path), "binance", sym, "1m", Some(&key))
            .unwrap_or_else(|e| panic!("ingest {sym} from {path}: {e}"));
        tracing::info!("{sym}: {n} bars ingested");
        total += n;
    }
    tracing::info!("done — {total} bars in {} (commit-keyed; re-run is a no-op)", root.display());
    ExitCode::SUCCESS
}
