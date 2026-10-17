//! Seed a deterministic demo bar series into a hist store so the Studio has something to run
//! against on a fresh checkout — for development and headless QA captures (the VIKE_SHOT flow),
//! NOT production data (real history comes from `vike-backfill`).
//!
//! ```sh
//! cargo run -p vike-studio --example studio_seed            # seeds ./market_data/hist
//! cargo run -p vike-studio --example studio_seed -- /tmp/demo
//! ```
//!
//! The store root is the one positional argument; no environment variable names it (decision
//! 0111).
//!
//! Writes 600 one-minute `binance/BTCUSDT` bars: a slow sine over a gentle drift, so an SMA
//! crossover (the default editor script) actually trades. Deterministic (no RNG, fixed epoch
//! base) and idempotent per run via the store's commit-key append — re-running replaces nothing
//! and duplicates nothing.

use vike_data::HistStore;
use vike_marketdata::Bar;
use vike_store::DataFusionHist;
use vike_util::cli::Program;

/// The command line, read by `clap` through `vike_util::cli::Program` (decision 0155): the store
/// root is the one optional positional.
#[derive(clap::Parser, Debug, PartialEq, Eq)]
#[command(
    name = "studio_seed",
    about = "Seed a deterministic demo bar series into a hist store, for development and QA captures"
)]
struct Cli {
    /// The store root.
    #[arg(value_name = "ROOT", default_value = "market_data/hist")]
    root: String,
}

fn main() {
    let Cli { root } =
        Program::new("studio_seed", concat!("studio_seed ", env!("CARGO_PKG_VERSION"))).parse();
    let store = DataFusionHist::open(&root).expect("open hist store");
    // Fixed base ts (2026-01-01 00:00 UTC) so re-runs write the identical series.
    let base_ts: i64 = 1_767_225_600_000;
    let bars: Vec<Bar> = (0..600)
        .map(|i| {
            let t = i as f64;
            // ~90-bar sine (amplitude 400) over a +2/bar drift, around 60k — visibly wavy on a
            // chart and rich in SMA(5)/SMA(20) crossings.
            let close = 60_000.0 + 2.0 * t + 400.0 * (t / 14.3).sin();
            let open = 60_000.0 + 2.0 * (t - 1.0) + 400.0 * ((t - 1.0) / 14.3).sin();
            let (high, low) = (open.max(close) + 25.0, open.min(close) - 25.0);
            Bar {
                ts: base_ts + 60_000 * i as i64,
                open,
                high,
                low,
                close,
                volume: 10.0 + (t / 5.0).cos().abs() * 90.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store
        .append_bars("binance", "BTCUSDT", "1m", &bars, Some("studio-seed:demo:v1"))
        .expect("append demo bars");
    println!("seeded 600 binance/BTCUSDT 1m demo bars into {root}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Cli, vike_util::cli::CliExit> {
        Program::new("studio_seed", "studio_seed 0").try_parse_from(line.iter().copied())
    }

    /// POSITIVE argv pins (decision 0155): the module doc's two lines, `studio_seed` and
    /// `studio_seed /tmp/demo`.
    #[test]
    fn the_documented_lines_are_accepted_with_their_meaning() {
        assert_eq!(parse(&[]).unwrap().root, "market_data/hist");
        assert_eq!(parse(&["/tmp/demo"]).unwrap().root, "/tmp/demo");
    }

    #[test]
    fn a_second_positional_is_a_usage_error() {
        let exit = parse(&["a", "b"]).unwrap_err();
        assert_eq!((exit.code, exit.to_stdout), (2, false));
    }
}
