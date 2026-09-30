//! **A/B for storage-study item #5**: what does per-(day, family) write granularity actually buy?
//!
//! The claim under test, from `storage-design-comparison.md`: this store commits at PER-SYMBOL
//! granularity where the source data, the premium vendor and the industry standard are all
//! per-(day, family). One Polymarket BTC-5m day is 562 tokens = 562 series = ~1,160 commits, and
//! every commit pays a measured ~50-70ms FIXED floor (lock acquire, manifest read, WAL + fsync,
//! manifest publish + fsync) regardless of how few rows it carries. Under #5 that day is ~3 commits.
//!
//! Databento's default delivery is one file per dataset per schema per DAY with all symbols
//! together (`split_symbols` defaults to `False`); kdb+ writes one partition per table per day with
//! `sym` as a column; even LEAN packs many contracts into one zip per underlying per day. vike is
//! the outlier, and #5 is a 1-2 week change touching append/scan/compaction/retention/gaps/quality/
//! coverage plus a store migration. So MEASURE before committing to it.
//!
//! ## What this measures, and what it does not
//!
//! **Measured (honestly):** the write cost of N commits vs 1 commit for the SAME rows. Lane A is
//! today's shape — one keyed append per symbol, each its own series. Lane B is #5's write shape —
//! all rows in ONE series under one commit. That difference IS the fixed-floor claim, and it is the
//! part worth a week of work.
//!
//! NOTE: as of the grouped-series work this lane B is the REAL implementation
//! (`append_quotes_grouped` + per-symbol `scan_quotes` through the group), not the proxy the
//! first revision used. The read-back below goes symbol by symbol exactly as a consumer would.
//!
//! **Deliberately NOT measured here:** the symbol column and read-side row-group pruning #5 also
//! needs. Lane B writes one series without a symbol column, because adding one is the schema change
//! the real work entails — so B is a LOWER BOUND on #5's write cost (the real thing pays a little
//! more to encode the column) and says nothing about read cost. The read side is already measured
//! elsewhere: `archive-store-report.md` scans 1 of 562 tokens from a single 1.20 GiB day file in
//! 1,226 ms touching 8.3% of compressed bytes, via exactly the symbol-pruning #5 would rely on.
//!
//! `#[ignore]`d — it writes real Parquet and takes tens of seconds. Run:
//! `cargo test -p vike-data --features hist-datafusion --test write_granularity_ab -- --ignored --nocapture`
#![cfg(feature = "hist-datafusion")]

use std::time::Instant;

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::QuoteTick;

/// Shaped like the real workload the claim is about: one Polymarket BTC-5m UTC day.
const SYMBOLS: usize = 562;
const ROWS_PER_SYMBOL: usize = 200;

fn quotes(symbol: &str, n: usize, base_ts: i64) -> Vec<QuoteTick> {
    (0..n)
        .map(|i| QuoteTick {
            ts: base_ts + i as i64 * 10,
            local_ts: base_ts + i as i64 * 10 + 1,
            bid: 0.50 + (i % 7) as f64 * 0.001,
            ask: 0.51 + (i % 5) as f64 * 0.001,
            bid_size: 10.0 + i as f64,
            ask_size: 12.0 + i as f64,
            symbol: symbol.to_string(),
        })
        .collect()
}

#[test]
#[ignore]
fn write_granularity_per_symbol_vs_per_family() {
    let base_ts = 1_785_024_000_000i64;
    let total_rows = SYMBOLS * ROWS_PER_SYMBOL;

    // Build EVERY row up front, outside BOTH timers, so the lanes differ only in commit
    // granularity. An earlier version generated lane A's rows inside its own timed loop, charging
    // it for 112,400 allocations lane B never paid — ~0.2% here, and it did not change the verdict,
    // but a benchmark that flatters one side is not evidence.
    let per_symbol: Vec<(String, String, Vec<QuoteTick>)> = (0..SYMBOLS)
        .map(|s| {
            let sym = format!("TOK{s:04}");
            let key = format!("bench:{sym}");
            let rows = quotes(&sym, ROWS_PER_SYMBOL, base_ts);
            (sym, key, rows)
        })
        .collect();
    let all: Vec<QuoteTick> =
        per_symbol.iter().flat_map(|(_, _, rows)| rows.iter().cloned()).collect();
    assert_eq!(all.len(), total_rows);

    // ---- Lane A: TODAY — one series per symbol, one commit each --------------------------------
    let dir_a = tempfile::tempdir().unwrap();
    let store_a = DataFusionHist::open(dir_a.path()).unwrap();
    let t0 = Instant::now();
    for (sym, key, rows) in &per_symbol {
        store_a.append_quotes("bench", sym, rows, Some(key)).unwrap();
    }
    let a_elapsed = t0.elapsed();

    // ---- Lane B: ITEM #5's write shape — one series, ONE commit --------------------------------
    // Identical rows, identical count; only the commit granularity differs.
    let dir_b = tempfile::tempdir().unwrap();
    let store_b = DataFusionHist::open(dir_b.path()).unwrap();
    let t1 = Instant::now();
    store_b.append_quotes_grouped("bench", "btc-5m", &all, Some("bench:family")).unwrap();
    let b_elapsed = t1.elapsed();

    // ---- READ side, TIMED: 562 per-symbol scans against each layout ---------------------------
    // Grouping trades cheap writes for reads that must filter. Timing only the write would report
    // a large win while hiding where it is paid back, so both sides are measured.
    let r0 = Instant::now();
    let a_rows: usize = (0..SYMBOLS)
        .map(|s| store_a.scan_quotes("bench", &format!("TOK{s:04}"), TsRange::all()).unwrap().len())
        .sum();
    let a_read = r0.elapsed();
    let r1 = Instant::now();
    let b_rows: usize = (0..SYMBOLS)
        .map(|s| store_b.scan_quotes("bench", &format!("TOK{s:04}"), TsRange::all()).unwrap().len())
        .sum();
    let b_read = r1.elapsed();
    assert_eq!(a_rows, total_rows, "lane A row count");
    assert_eq!(b_rows, total_rows, "lane B row count — same data, one commit");

    let a_ms = a_elapsed.as_secs_f64() * 1000.0;
    let b_ms = b_elapsed.as_secs_f64() * 1000.0;
    let du = |p: &std::path::Path| -> u64 {
        fn walk(p: &std::path::Path) -> u64 {
            let Ok(rd) = std::fs::read_dir(p) else { return 0 };
            rd.filter_map(|e| e.ok())
                .map(|e| {
                    let path = e.path();
                    if path.is_dir() {
                        walk(&path)
                    } else {
                        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                    }
                })
                .sum()
        }
        walk(p)
    };

    eprintln!(
        "\n=== write granularity A/B — {SYMBOLS} symbols x {ROWS_PER_SYMBOL} rows = {total_rows} rows ==="
    );
    eprintln!(
        "A  per-symbol (today)   {a_ms:9.1} ms   {SYMBOLS} commits   {:6.2} ms/commit   {:8} KB on disk",
        a_ms / SYMBOLS as f64,
        du(dir_a.path()) / 1024
    );
    eprintln!(
        "B  per-family (item #5) {b_ms:9.1} ms   1 commit      {b_ms:6.2} ms/commit   {:8} KB on disk",
        du(dir_b.path()) / 1024
    );
    let ar = a_read.as_secs_f64() * 1000.0;
    let br = b_read.as_secs_f64() * 1000.0;
    eprintln!(
        "\nread  {SYMBOLS} per-symbol scans:   A {ar:9.1} ms   B {br:9.1} ms   ({:.1}x {})",
        if br > ar { br / ar } else { ar / br },
        if br > ar { "SLOWER grouped" } else { "faster grouped" }
    );
    // Row groups in the grouped part — the unit `symbol_col` statistics can skip. ONE row group
    // means pruning cannot skip anything and every read pays for the whole part, so the read number
    // above is measuring in-engine FILTERING, not pruning. `write_parquet` only bounds the row-group
    // size for `WriteProfile::Sealed`; grouped writes use `Hot`, which leaves arrow's 1,048,576-row
    // default — so any part under a million rows is a single row group.
    fn count_row_groups(p: &std::path::Path) -> usize {
        let Ok(rd) = std::fs::read_dir(p) else { return 0 };
        rd.filter_map(|e| e.ok())
            .map(|e| {
                let path = e.path();
                if path.is_dir() {
                    count_row_groups(&path)
                } else if path.extension().and_then(|x| x.to_str()) == Some("parquet") {
                    std::fs::File::open(&path)
                        .ok()
                        .and_then(|f| {
                            datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(f).ok()
                        })
                        .map_or(0, |b| b.metadata().num_row_groups())
                } else {
                    0
                }
            })
            .sum()
    }
    eprintln!(
        "\ngrouped part row groups: {}   (1 => symbol statistics prune NOTHING)",
        count_row_groups(dir_b.path())
    );
    eprintln!("\nWRITE speedup: {:.1}x   ({:.1} ms saved)", a_ms / b_ms, a_ms - b_ms);
    eprintln!(
        "NET write+read: A {:9.1} ms   B {:9.1} ms   {:.2}x",
        a_ms + ar,
        b_ms + br,
        (a_ms + ar) / (b_ms + br)
    );
    eprintln!(
        "per-commit fixed floor implied by A: {:.1} ms",
        (a_ms - b_ms) / (SYMBOLS - 1) as f64
    );
}
