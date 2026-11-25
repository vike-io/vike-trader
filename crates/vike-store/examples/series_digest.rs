//! `series_digest` — one series' row count, first and last timestamps, a checksum over every row in
//! timestamp order, its row symbols and its commit keys, read through the store's own engine.
//!
//! It is the before/after evidence of a store rename (`vike-cli data history rename`, decision 0290) on
//! a COPY of a real store: digest the old series, rename, digest the new one, compare. The checksum
//! (FNV-1a 64 over each row's fields, `vike_utility::fnv`) leaves out the row's own symbol, which a
//! rename rewrites on purpose; the symbols the rows read back are printed on a line of their own.
//! The store is opened READ-ONLY and walked one UTC day at a time, so memory is one day's rows. An
//! example rather than a `[[bin]]` for `store_rates`' reason: a probe run by hand joins no release.
//!
//! ```text
//! cargo run --release -p vike-store --example series_digest -- \
//!     --store /path/to/hist --kind bar --venue oanda --symbol EUR_USD --interval 5s --keys-out keys.txt
//! ```

use std::collections::BTreeSet;
use std::error::Error;

use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::floor_ms_to_utc_day;
use vike_store::HistoryStoreDataFusion;
use vike_utility::cli::Program;
use vike_utility::fnv::Fnv1a64;

/// The command line: one per-symbol series, and where its keys go.
#[derive(clap::Parser, Debug)]
#[command(
    name = "series_digest",
    about = "One series' rows, edges, checksum, row symbols and commit keys, read-only"
)]
struct Args {
    /// The hist store root (opened READ-ONLY).
    #[arg(long, value_name = "ROOT")]
    store: String,
    /// `bar` or `quote`.
    #[arg(long, value_name = "K")]
    kind: String,
    /// The venue.
    #[arg(long, value_name = "V")]
    venue: String,
    /// The series' symbol.
    #[arg(long, value_name = "S")]
    symbol: String,
    /// The bar interval (`bar` only).
    #[arg(long, value_name = "I")]
    interval: Option<String>,
    /// Write the series' commit keys here, sorted, one per line.
    #[arg(long, value_name = "FILE")]
    keys_out: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Args =
        Program::new("series_digest", concat!("series_digest ", env!("CARGO_PKG_VERSION"))).parse();
    let store = HistoryStoreDataFusion::open_read_only(&args.store)?;
    let kind: StoreKindId = args.kind.parse()?;
    let id = SeriesId::per_symbol(
        kind,
        args.venue.as_str(),
        args.symbol.as_str(),
        args.interval.clone(),
    );
    let coverage = store.series_coverage(&id)?;
    let mut digest = Fnv1a64::new();
    let mut fold = |fields: &[u64]| {
        for field in fields {
            digest.write(&field.to_le_bytes());
        }
    };
    let (mut rows, mut first, mut last) = (0u64, None, None);
    let mut symbols = BTreeSet::new();
    let mut day = floor_ms_to_utc_day(coverage.first_timestamp);
    while coverage.rows > 0 && day <= coverage.last_timestamp {
        let range = TimestampRange::of(day, day + MS_PER_DAY - 1);
        let stamps: Vec<i64> = match kind {
            StoreKindId::Bar => {
                let interval = args.interval.as_deref().ok_or("--interval is required on `bar`")?;
                let bars = store.load_bars(&args.venue, &args.symbol, interval, range)?;
                for b in &bars {
                    fold(&[
                        b.timestamp as u64,
                        b.open.to_bits(),
                        b.high.to_bits(),
                        b.low.to_bits(),
                        b.close.to_bits(),
                        b.volume.to_bits(),
                        b.funding.map_or(u64::MAX, f64::to_bits),
                    ]);
                }
                bars.iter().map(|b| b.timestamp).collect()
            }
            StoreKindId::Quote => {
                let quotes = store.scan_quotes(&args.venue, &args.symbol, range)?;
                for q in &quotes {
                    fold(&[
                        q.timestamp as u64,
                        q.local_timestamp as u64,
                        q.bid.to_bits(),
                        q.ask.to_bits(),
                        q.bid_size.to_bits(),
                        q.ask_size.to_bits(),
                    ]);
                }
                symbols.extend(quotes.iter().map(|q| q.symbol));
                quotes.iter().map(|q| q.timestamp).collect()
            }
            other => return Err(format!("{other}: this probe reads bar and quote series").into()),
        };
        rows += stamps.len() as u64;
        first = first.or(stamps.first().copied());
        last = stamps.last().copied().or(last);
        day += MS_PER_DAY;
    }
    let mut keys = store.series_commits(&id)?;
    keys.sort();
    println!("series    {} {} {} {:?}", id.kind, id.venue, id.symbol, id.interval);
    println!(
        "manifest  rows {} parts {} first {} last {}",
        coverage.rows, coverage.parts, coverage.first_timestamp, coverage.last_timestamp
    );
    println!("read      rows {rows} first {first:?} last {last:?}");
    println!("checksum  {:016x}", digest.finish());
    println!("symbols   {symbols:?}");
    println!("keys      {} (first {:?}, last {:?})", keys.len(), keys.first(), keys.last());
    if let Some(path) = &args.keys_out {
        std::fs::write(path, keys.join("\n") + "\n")?;
    }
    Ok(())
}
