//! Gate for the DataFusion+Parquet HistStore: bars round-trip bit-for-bit, ts-range filtering,
//! ingest idempotency + the manifest file-index, `resample_*_to_bars` == the parity-tested
//! consolidator, and (slice 4) `date=` partitioning + compaction + retention. Only compiled/run
//! with `--features hist-datafusion`.
#![cfg(feature = "hist-datafusion")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{
    BulkConfig, CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig,
    MaintenanceScheduler, RetentionPolicy, SeriesId, SourceRankPolicy, TsRange,
};
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, QuoteTick, SymbolProperties, TickScheme, TickTier, TradeTick,
    consolidate_trades,
};

fn bar(ts: i64, close: f64, funding: Option<f64>) -> Bar {
    Bar {
        ts,
        open: close - 0.5,
        high: close + 1.0,
        low: close - 1.5,
        close,
        volume: 10.0 + ts as f64 * 1e-6,
        funding,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Quote-literal helper: a QuoteTick at `ts` with `bid`=`val` and derived ask/sizes (symbol filled
/// on read, so left empty here). Keeps the schema-tolerance + compaction tests terse.
fn qt(ts: i64, val: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: val,
        ask: val + 0.1,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: String::new(),
    }
}

fn assert_bars_bit_eq(a: &[Bar], b: &[Bar]) {
    assert_eq!(a.len(), b.len(), "bar count");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        for (name, xv, yv) in [
            ("open", x.open, y.open),
            ("high", x.high, y.high),
            ("low", x.low, y.low),
            ("close", x.close, y.close),
            ("volume", x.volume, y.volume),
        ] {
            assert_eq!(xv.to_bits(), yv.to_bits(), "{name}[{i}] ({xv} vs {yv})");
        }
        assert_eq!(x.funding.map(f64::to_bits), y.funding.map(f64::to_bits), "funding[{i}]");
    }
}

#[test]
fn datafusion_bars_round_trip_bit_for_bit() {
    // Parquet append -> load preserves every bar field bit-for-bit (incl. Option<funding>).
    let bars: Vec<Bar> = (0..50)
        .map(|i| {
            let f = if i % 3 == 0 { Some(0.0001 * i as f64) } else { None };
            bar(1000 + i * 60_000, 100.0 + i as f64 * 0.25, f)
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
}

#[test]
fn datafusion_root_returns_the_open_path() {
    // `root()` hands back exactly the directory `open` was given — consumers (e.g. the Studio
    // persisting its saved-strategies/workspace JSON next to the store) derive paths from it.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df.root(), dir.path());
}

#[test]
fn datafusion_ts_range_is_inclusive_and_unknown_series_empty() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..10).map(|i| bar(i * 60_000, 100.0, None)).collect();
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let got =
        df.load_bars("binance", "BTCUSDT", "1m", TsRange::of(2 * 60_000, 5 * 60_000)).unwrap();
    assert_eq!(got.len(), 4, "ts 2,3,4,5 inclusive");
    assert_eq!(got.first().unwrap().ts, 2 * 60_000);
    assert_eq!(got.last().unwrap().ts, 5 * 60_000);

    // unknown series → empty, not an error
    assert!(df.load_bars("okx", "ETHUSDT", "1m", TsRange::all()).unwrap().is_empty());
}

#[test]
fn datafusion_multiple_parts_merge_ordered() {
    // two sealed writes to the same series -> two parquet parts; read merges + sorts by ts
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let first: Vec<Bar> = (5..10).map(|i| bar(i * 60_000, 100.0, None)).collect();
    let second: Vec<Bar> = (0..5).map(|i| bar(i * 60_000, 200.0, None)).collect();
    df.append_bars("binance", "BTCUSDT", "1m", &first, None).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &second, None).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 10);
    assert!(got.windows(2).all(|w| w[0].ts <= w[1].ts), "ts ascending across parts");
    assert_eq!(got[0].ts, 0);
}

#[test]
fn datafusion_ticks_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    let quotes: Vec<QuoteTick> = (0..20)
        .map(|i| QuoteTick {
            ts: i * 100,
            local_ts: 0,
            bid: 100.0 + i as f64,
            ask: 100.1 + i as f64,
            bid_size: 1.0,
            ask_size: 2.0,
            symbol: String::new(),
        })
        .collect();
    df.append_quotes("binance", "BTCUSDT", &quotes, None).unwrap();
    let gq = df.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(gq.len(), 20);
    assert_eq!(gq[0].symbol, "BTCUSDT", "symbol tagged on read");
    assert_eq!(gq[5].bid.to_bits(), quotes[5].bid.to_bits());
    assert_eq!(gq[5].ask_size.to_bits(), quotes[5].ask_size.to_bits());

    let trades: Vec<TradeTick> = (0..15)
        .map(|i| TradeTick {
            ts: i * 100,
            local_ts: 0,
            price: 50.0 + i as f64,
            size: 0.5,
            is_buyer_maker: i % 2 == 0,
            symbol: String::new(),
        })
        .collect();
    df.append_trades("binance", "BTCUSDT", &trades, None).unwrap();
    let gt = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(gt.len(), 15);
    assert_eq!(gt[3].is_buyer_maker, trades[3].is_buyer_maker);
    assert_eq!(gt[3].price.to_bits(), trades[3].price.to_bits());
}

/// A symbol containing URL-reserved characters must round-trip. `#` is the live case: the
/// Polymarket window convention is `<slug>#<outcome_index>` (`vike_backtest::CheapNp`'s symbol
/// grammar), and an unencoded `file://` path truncates at the fragment marker — the write lands on
/// disk and the read then reports "No files found ... Cannot infer schema from an empty location".
#[test]
fn symbols_with_url_reserved_characters_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let trades: Vec<TradeTick> = (0..5)
        .map(|i| TradeTick {
            ts: i * 1000,
            local_ts: 0,
            price: 0.2 + i as f64 * 0.01,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        })
        .collect();
    for symbol in ["btc-updown-5m-1775001300#0", "weird?q=1", "pct%20sign", "sp ace"] {
        df.append_trades("polymarket", symbol, &trades, None).unwrap();
        let got = df.scan_trades("polymarket", symbol, TsRange::all()).unwrap();
        assert_eq!(got.len(), 5, "{symbol} wrote but did not read back");
        assert_eq!(got[2].price.to_bits(), trades[2].price.to_bits(), "{symbol}");
    }
}

// ---- slice 2: manifest + idempotent ingest -------------------------------------------------

#[test]
fn append_is_idempotent_by_commit_key() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..8).map(|i| bar(i * 60_000, 100.0, None)).collect();

    // first ingest of a keyed batch writes; the SAME key is a no-op (never a value-dedup)
    let n1 = df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("batch-A")).unwrap();
    let n2 = df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("batch-A")).unwrap();
    assert_eq!(n1, 8, "first append writes all rows");
    assert_eq!(n2, 0, "re-appending the same commit key is a no-op");

    // exactly one copy is stored (idempotent), not two
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 8, "no duplication from the retried batch");

    // a DIFFERENT key with genuinely-distinct rows DOES append (batch-level, not value-level)
    let more: Vec<Bar> = (8..12).map(|i| bar(i * 60_000, 100.0, None)).collect();
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &more, Some("batch-B")).unwrap(), 4);
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 12);
}

#[test]
fn manifest_persists_and_reopen_reads() {
    // the manifest is the durable file index — a fresh store over the same root sees the data
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..6).map(|i| bar(i * 60_000, 100.0, None)).collect();
    {
        let df = DataFusionHist::open(dir.path()).unwrap();
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("k")).unwrap();
    }
    // reopen (new runtime, new session) — reads come off the on-disk manifest
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 6);
    // and idempotency survives the reopen (commit-log is on disk)
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("k")).unwrap(), 0);
}

#[test]
fn manifest_driven_read_prunes_non_overlapping_parts() {
    // three sealed parts at disjoint ts windows; a narrow query must return only the overlap.
    // (correctness proxy for file-level pruning — a non-overlapping part contributes nothing.)
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let part = |base: i64| -> Vec<Bar> {
        (0..5).map(move |i| bar(base + i * 1000, 100.0, None)).collect()
    };
    df.append_bars("binance", "BTCUSDT", "1m", &part(0), Some("a")).unwrap(); // ts 0..4000
    df.append_bars("binance", "BTCUSDT", "1m", &part(100_000), Some("b")).unwrap(); // 100k..104k
    df.append_bars("binance", "BTCUSDT", "1m", &part(200_000), Some("c")).unwrap(); // 200k..204k

    // query only the middle window → only part b's rows
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::of(100_000, 104_000)).unwrap();
    assert_eq!(got.len(), 5);
    assert_eq!(got.first().unwrap().ts, 100_000);
    assert_eq!(got.last().unwrap().ts, 104_000);

    // whole-range query → all three parts merge, ordered
    let all = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(all.len(), 15);
    assert!(all.windows(2).all(|w| w[0].ts <= w[1].ts));
}

// ---- slice 3: resample bridge (ticks -> bars, in-store) ------------------------------------

#[test]
fn resample_trades_to_bars_matches_consolidator() {
    // Ingest trades, resample them to 1m bars in-store, then confirm the stored bars equal the
    // parity-tested consolidator's output bit-for-bit — the bridge is just consolidate + round-trip.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let trades: Vec<TradeTick> = (0..300)
        .map(|i| TradeTick {
            ts: i * 1_000, // 1s apart → many per 1m bucket
            local_ts: 0,
            price: 100.0 + (i as f64 * 0.13).sin(),
            size: 0.5 + (i % 5) as f64 * 0.1,
            is_buyer_maker: i % 2 == 0,
            symbol: String::new(),
        })
        .collect();
    df.append_trades("binance", "BTCUSDT", &trades, Some("t1")).unwrap();

    let n =
        df.resample_trades_to_bars("binance", "BTCUSDT", "1m", TsRange::all(), Some("r1")).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    let want = consolidate_trades(&trades, 60_000); // the oracle (parity-tested vs Python)
    assert_eq!(n, want.len(), "resampled bar count");
    assert!(!want.is_empty(), "sanity: produced bars");
    assert_bars_bit_eq(&want, &got);

    // idempotent: re-running the same resample commit key is a no-op
    let n2 =
        df.resample_trades_to_bars("binance", "BTCUSDT", "1m", TsRange::all(), Some("r1")).unwrap();
    assert_eq!(n2, 0, "re-resample with the same key is a no-op");
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), want.len());
}

// ---- slice 4: date= partitioning + compaction + retention ----------------------------------

const DAY: i64 = 86_400_000;

/// The bar series leaf dir under a store root (parts live one level deeper under `date=…`).
fn bars_series(root: &Path) -> PathBuf {
    root.join("kind=bar").join("venue=binance").join("symbol=BTCUSDT").join("interval=1m")
}

/// Count `.parquet` part files under a dir (recurses `date=` subdirs); ignores manifests/locks.
fn count_parquets(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += count_parquets(&p);
            } else if p.extension().is_some_and(|x| x == "parquet") {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn date_split_places_parts_under_date_dirs() {
    // one append spanning three UTC days → one sealed part per date=, data bit-eq on read-back.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = vec![
        bar(0, 100.0, None),             // 1970-01-01
        bar(DAY / 2, 101.0, None),       // 1970-01-01
        bar(DAY, 102.0, None),           // 1970-01-02
        bar(DAY + DAY / 2, 103.0, None), // 1970-01-02
        bar(2 * DAY, 104.0, None),       // 1970-01-03
    ];
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("k")).unwrap(), 5);

    let series = bars_series(dir.path());
    for d in ["date=1970-01-01", "date=1970-01-02", "date=1970-01-03"] {
        assert!(series.join(d).exists(), "missing partition {d}");
    }
    assert_eq!(count_parquets(&series), 3, "one part per date");
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
}

#[test]
fn date_boundary_is_bit_identical() {
    // rows straddling 00:00:00.000 UTC — the civil-date split must introduce no value drift.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = vec![
        bar(DAY - 1, 200.0, Some(0.01)),  // last ms of 1970-01-01
        bar(DAY, 201.0, None),            // 1970-01-02 00:00:00.000
        bar(DAY + 1, 202.0, Some(-0.02)), // first ms after
    ];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
    assert!(bars_series(dir.path()).join("date=1970-01-02").exists(), "boundary row → new day");
}

#[test]
fn compaction_merges_and_is_bit_identical() {
    // four same-day fragments → one sealed part; every row bit-preserved, fewer files on disk.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..4i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    let before = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(count_parquets(&series), 4);

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 20);

    assert!(count_parquets(&series) < 4, "compaction reduced part count");
    let after = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&before, &after);
}

/// Schema-evolution tolerance: a series whose parts were written under DIFFERENT schema revisions
/// (an old part lacking a later-added nullable column) must still scan — the decode-time upgrade
/// policy (book-recording plan Task 4). Until Task 5 adds the `local_ts` column there is only ONE
/// schema revision, so this canNOT yet exercise genuinely-mixed parts; it PINS the per-file `collect`
/// union that makes Task 5's real `old_quote_parts_readable_after_local_ts` possible — two normal
/// appends land as two parts and both parts' rows must come back.
#[test]
fn scan_tolerates_mixed_part_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    // one normal quote append (current full schema)
    store.append_quotes("v", "S", &[qt(1_000, 1.0)], Some("k1")).unwrap();
    // a second normal part; per-file reads must union the two parts (the property Task 5 rides)
    store.append_quotes("v", "S", &[qt(2_000, 2.0)], Some("k2")).unwrap();
    let got = store.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2, "per-file collect must union parts");
    assert_eq!(got[0].ts, 1_000);
    assert_eq!(got[1].ts, 2_000);
}

/// Compaction bit-identity for a NON-bar kind: enough same-day quote fragments to trip compaction,
/// then the domain-roundtrip rewrite (decode → sort → re-encode with the CURRENT schema, book-
/// recording plan Task 4) must preserve every f64 field bit-for-bit (`to_bits`) AND row count/order.
/// This is the proof the decode-time schema-upgrade path is lossless for f64 (`Float64Array::value`
/// → `Float64Array::from` round-trips bits).
#[test]
fn compaction_quotes_domain_roundtrip_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // four disjoint ascending windows, all inside UTC day 0 → four fragments in one date=
    let mut want: Vec<QuoteTick> = Vec::new();
    for c in 0..4i64 {
        let batch: Vec<QuoteTick> = (0..5)
            .map(|i| {
                let n = c * 5 + i;
                QuoteTick {
                    ts: 1_000 + n * 1_000,
                    local_ts: 0,
                    bid: 100.0 + n as f64 * 0.25,
                    ask: 100.1 + n as f64 * 0.25,
                    bid_size: 1.0 + n as f64 * 0.01,
                    ask_size: 2.0 + n as f64 * 0.02,
                    symbol: String::new(),
                }
            })
            .collect();
        df.append_quotes("binance", "BTCUSDT", &batch, Some(&format!("q{c}"))).unwrap();
        want.extend(batch);
    }
    let quote_series = dir.path().join("kind=quote").join("venue=binance").join("symbol=BTCUSDT");
    assert_eq!(count_parquets(&quote_series), 4, "four quote fragments before compaction");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("quote", "binance", "BTCUSDT", None, &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 20);
    assert!(count_parquets(&quote_series) < 4, "compaction reduced the part count");

    let got = df.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), want.len(), "row count preserved through compaction");
    for (i, (x, y)) in want.iter().zip(&got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.bid.to_bits(), y.bid.to_bits(), "bid[{i}]");
        assert_eq!(x.ask.to_bits(), y.ask.to_bits(), "ask[{i}]");
        assert_eq!(x.bid_size.to_bits(), y.bid_size.to_bits(), "bid_size[{i}]");
        assert_eq!(x.ask_size.to_bits(), y.ask_size.to_bits(), "ask_size[{i}]");
    }
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "ts ascending after compaction");
    assert_eq!(got[0].symbol, "BTCUSDT", "symbol re-injected on scan");
}

/// Compaction bit-identity for the `kind=properties` series (PIT `SymbolProperties` snapshots) — the
/// `"properties"` arm of `compact_roundtrip`. Three same-day fragments (distinct `commit_key`s so
/// they don't dedup) trip compaction; afterward `scan_symbol_properties` must reproduce every
/// original `(ts, SymbolProperties)` row bit-for-bit and in ts-ascending order.
#[test]
fn compaction_properties_domain_roundtrip_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // three fragments, all inside UTC day 0 → three parts in one date= dir, each its own commit
    let mut want: Vec<(i64, SymbolProperties)> = Vec::new();
    for c in 0..3i64 {
        let ts = 1_000 + c * 1_000;
        let f = SymbolProperties {
            tick_size: 0.01 + c as f64 * 0.001,
            step_size: 0.001 + c as f64 * 0.0001,
            min_qty: 0.001 + c as f64 * 0.0001,
            max_qty: 100.0 + c as f64,
            min_notional: 5.0 + c as f64 * 0.5,
            contract_size: 0.0,
            tick_scheme: None,
            taker_hold_ms: 0,
        };
        df.append_symbol_properties("bybit", "BTCUSDT", &[(ts, f)], Some(&format!("f{c}")))
            .unwrap();
        want.push((ts, f));
    }
    let properties_series =
        dir.path().join("kind=properties").join("venue=bybit").join("symbol=BTCUSDT");
    assert_eq!(count_parquets(&properties_series), 3, "three filter fragments before compaction");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("properties", "bybit", "BTCUSDT", None, &cfg).unwrap();
    assert_eq!(rep.parts_merged, 3);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 3);
    assert!(count_parquets(&properties_series) < 3, "compaction reduced the part count");

    let got = df.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), want.len(), "row count preserved through compaction");
    for (i, ((wts, wf), (gts, gf))) in want.iter().zip(&got).enumerate() {
        assert_eq!(wts, gts, "ts[{i}]");
        assert_eq!(wf.tick_size.to_bits(), gf.tick_size.to_bits(), "tick_size[{i}]");
        assert_eq!(wf.step_size.to_bits(), gf.step_size.to_bits(), "step_size[{i}]");
        assert_eq!(wf.min_qty.to_bits(), gf.min_qty.to_bits(), "min_qty[{i}]");
        assert_eq!(wf.max_qty.to_bits(), gf.max_qty.to_bits(), "max_qty[{i}]");
        assert_eq!(wf.min_notional.to_bits(), gf.min_notional.to_bits(), "min_notional[{i}]");
    }
    assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "ts ascending after compaction");
}

/// Compaction bit-identity for the `kind=book` series — the per-level row-rewrite path of
/// `compact_roundtrip`'s `"book"` arm (book-recording plan Task 5). Enough same-day fragments to
/// trip compaction, spanning a multi-level Snapshot, a Delta with a REMOVAL level `(price>0, 0.0)`,
/// and a zero-level status event (`GapStart`, which persists as a placeholder row) — so every row
/// shape goes through the decode → sort → re-encode rewrite. Afterward `scan_book_updates` must
/// reproduce every original event bit-for-bit (`to_bits` on all f64), proving the row-level book
/// compaction is lossless (incl. placeholder rows surviving the round-trip).
#[test]
fn compaction_book_domain_roundtrip_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // Four fragments, all inside UTC day 0 (ts in the low thousands of epoch-ms) → one date= dir.
    // Delta/Snapshot seqs are monotonic-distinct; status events (GapStart) carry seq 0.
    let parts: Vec<Vec<BookUpdate>> = vec![
        vec![
            bu(
                1_000,
                1,
                BookUpdateKind::Snapshot,
                vec![(0.45, 100.0), (0.44, 20.0)],
                vec![(0.46, 50.0), (0.47, 10.0)],
            ),
            bu(1_005, 2, BookUpdateKind::Delta, vec![(0.44, 0.0)], vec![(0.46, 55.0)]), // removal level
        ],
        vec![
            bu(1_010, 0, BookUpdateKind::GapStart, vec![], vec![]), // status → placeholder row
            bu(1_020, 3, BookUpdateKind::Snapshot, vec![(0.43, 5.0)], vec![(0.48, 8.0)]),
        ],
        vec![
            bu(1_030, 4, BookUpdateKind::Delta, vec![(0.43, 6.0)], vec![]),
            bu(1_040, 5, BookUpdateKind::Delta, vec![], vec![(0.48, 0.0)]), // ask removal
        ],
        vec![
            bu(
                1_050,
                6,
                BookUpdateKind::Snapshot,
                vec![(0.42, 3.0), (0.41, 2.0)],
                vec![(0.49, 4.0)],
            ),
            bu(1_060, 0, BookUpdateKind::LiveResume, vec![], vec![]), // status → placeholder row
        ],
    ];
    let mut want: Vec<BookUpdate> = Vec::new();
    for (c, part) in parts.iter().enumerate() {
        df.append_book_updates("polymarket", "TOK", part, Some(&format!("b{c}"))).unwrap();
        want.extend(part.iter().cloned());
    }
    let book_series = dir.path().join("kind=book").join("venue=polymarket").join("symbol=TOK");
    assert_eq!(count_parquets(&book_series), 4, "four book fragments before compaction");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("book", "polymarket", "TOK", None, &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    assert!(count_parquets(&book_series) < 4, "compaction reduced the part count");

    let got = df.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), want.len(), "event count preserved through compaction");
    for (i, (x, y)) in want.iter().zip(&got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.local_ts, y.local_ts, "local_ts[{i}]");
        assert_eq!(x.seq, y.seq, "seq[{i}]");
        assert_eq!(x.kind, y.kind, "kind[{i}]");
        assert_eq!(x.tick_size.to_bits(), y.tick_size.to_bits(), "tick_size[{i}]");
        assert_eq!(x.bids.len(), y.bids.len(), "bids.len[{i}]");
        assert_eq!(x.asks.len(), y.asks.len(), "asks.len[{i}]");
        for (j, (a, b)) in x.bids.iter().zip(&y.bids).enumerate() {
            assert_eq!(a.0.to_bits(), b.0.to_bits(), "bid price[{i}][{j}]");
            assert_eq!(a.1.to_bits(), b.1.to_bits(), "bid qty[{i}][{j}]");
        }
        for (j, (a, b)) in x.asks.iter().zip(&y.asks).enumerate() {
            assert_eq!(a.0.to_bits(), b.0.to_bits(), "ask price[{i}][{j}]");
            assert_eq!(a.1.to_bits(), b.1.to_bits(), "ask qty[{i}][{j}]");
        }
    }
}

#[test]
fn compaction_preserves_commit_log_idempotency() {
    // after a merge, an already-committed key stays a no-op (GC never resurrects/double-counts).
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let mk =
        |c: i64| -> Vec<Bar> { (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0, None)).collect() };
    for c in 0..4i64 {
        df.append_bars("binance", "BTCUSDT", "1m", &mk(c), Some(&format!("b{c}"))).unwrap();
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &mk(0), Some("b0")).unwrap(), 0);
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 20);
}

#[test]
fn retention_drops_old_date_dirs_and_commit_log() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let old: Vec<Bar> = (0..5).map(|i| bar(i * 1000, 100.0, None)).collect(); // day 0
    let new: Vec<Bar> = (0..5).map(|i| bar(10 * DAY + i * 1000, 200.0, None)).collect(); // day 10
    df.append_bars("binance", "BTCUSDT", "1m", &old, Some("old")).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &new, Some("new")).unwrap();

    let policy = RetentionPolicy { before_ts: Some(5 * DAY), max_age_ms: None };
    let rep = df.apply_retention("bar", "binance", "BTCUSDT", Some("1m"), &policy).unwrap();
    assert_eq!(rep.files_dropped, 1);
    assert_eq!(rep.dates_dropped, 1);
    assert_eq!(rep.rows_dropped, 5);

    assert!(!bars_series(dir.path()).join("date=1970-01-01").exists(), "old date dir unlinked");
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&new, &got);

    // the pruned commit key was GC'd → re-backfill of that window APPENDS (not a false no-op)
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &old, Some("old")).unwrap(), 5);
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 10);
}

#[test]
fn concurrent_append_and_compact_no_lost_update() {
    // the per-series lock serializes the manifest read-modify-write: an append racing a compaction
    // loses no rows.
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let mk = |c: i64| -> Vec<Bar> {
        (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect()
    };
    for c in 0..4i64 {
        df.append_bars("binance", "BTCUSDT", "1m", &mk(c), Some(&format!("s{c}"))).unwrap();
    }
    let d1 = df.clone();
    let d2 = df.clone();
    let appender = std::thread::spawn(move || {
        for c in 4..8i64 {
            let b: Vec<Bar> =
                (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
            d1.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("s{c}"))).unwrap();
        }
    });
    let compactor = std::thread::spawn(move || {
        let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
        d2.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    });
    appender.join().unwrap();
    compactor.join().unwrap();

    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 40, "all 8 batches * 5 rows present");
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "ts unique + ascending");
}

#[test]
fn retention_after_compaction_gcs_commit_log() {
    // compact a day, THEN prune it: the merged part must carry the union of its inputs' keys, so a
    // re-backfill of the pruned window APPENDS (guards the compact→prune→re-ingest false-no-op).
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..4i64 {
        let b: Vec<Bar> = (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("k{c}"))).unwrap();
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    let policy = RetentionPolicy { before_ts: Some(5 * DAY), max_age_ms: None };
    df.apply_retention("bar", "binance", "BTCUSDT", Some("1m"), &policy).unwrap();
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 0);

    // keys k0..k3 (folded into the merged part, then pruned) were GC'd → re-backfill re-appends
    let b0: Vec<Bar> = (0..5).map(|i| bar(i * 1000, 100.0, None)).collect();
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &b0, Some("k0")).unwrap(), 5);
}

// ---- source-ranked window supersession at compaction (opt-in) --------------------------------
// The four Polymarket writers (RecorderSink "live-…", pmxt_backfill "pmxt:…",
// clickhouse_poly_backfill "clickhouse:…", poly_reparse) target the SAME
// venue=polymarket/symbol=<token_id> series with DISJOINT commit-key namespaces, so overlapping
// capture windows DOUBLE-COUNT rows. `compact_series_superseding` keeps one row per natural key by
// source precedence; plain `compact_series` keeps every duplicate (byte-identical to today).

/// Trade literal (symbol filled on read): `tt(ts, price, size)`, a taker print (not a maker).
fn tt(ts: i64, price: f64, size: f64) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: false, symbol: String::new() }
}

#[test]
fn supersession_off_keeps_duplicates_byte_identical() {
    // OFF (plain compact_series): two overlapping parts from different sources — the ts=1000
    // collision stays DOUBLE-COUNTED, exactly as today. Proven by pre-compaction == post-compaction.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    // live source: trades at ts 500 & 1000 (size 10 at the collision ts)
    df.append_trades(
        "polymarket",
        "TOK",
        &[tt(500, 0.40, 5.0), tt(1_000, 0.50, 10.0)],
        Some("live-polymarket-TOK-trade-1-2-3"),
    )
    .unwrap();
    // pmxt source: trades at ts 1000 (size 99 — the duplicate) & 2000
    df.append_trades(
        "polymarket",
        "TOK",
        &[tt(1_000, 0.50, 99.0), tt(2_000, 0.60, 7.0)],
        Some("pmxt:trade:TOK:2026-07-01T00"),
    )
    .unwrap();

    let before = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(before.len(), 4, "both ts=1000 trades present pre-compaction (double-counted)");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let rep = df.compact_series("trade", "polymarket", "TOK", None, &cfg).unwrap();
    assert_eq!(rep.rows, 4, "plain compaction keeps every row");
    assert_eq!(rep.rows_superseded, 0, "plain compaction never supersedes");

    let after = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_trades_bit_eq(&before, &after); // byte-identical to today: nothing dropped or reordered
    // the duplicate is still there: BOTH sizes present at ts=1000
    let at_1000: Vec<u64> =
        after.iter().filter(|t| t.ts == 1_000).map(|t| t.size.to_bits()).collect();
    assert_eq!(at_1000.len(), 2, "OFF: the cross-source duplicate survives");
    assert!(at_1000.contains(&10.0f64.to_bits()) && at_1000.contains(&99.0f64.to_bits()));
}

#[test]
fn supersession_on_drops_lower_ranked_duplicate() {
    // ON: live > pmxt. The ts=1000 collision keeps LIVE's row (size 10), drops pmxt's (size 99).
    // Non-duplicate rows (ts=500 live-only, ts=2000 pmxt-only) are untouched.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades(
        "polymarket",
        "TOK",
        &[tt(500, 0.40, 5.0), tt(1_000, 0.50, 10.0)],
        Some("live-polymarket-TOK-trade-1-2-3"),
    )
    .unwrap();
    df.append_trades(
        "polymarket",
        "TOK",
        &[tt(1_000, 0.50, 99.0), tt(2_000, 0.60, 7.0)],
        Some("pmxt:trade:TOK:2026-07-01T00"),
    )
    .unwrap();

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let policy = SourceRankPolicy::new(["live-", "pmxt:"]);
    let rep =
        df.compact_series_superseding("trade", "polymarket", "TOK", None, &cfg, &policy).unwrap();
    assert_eq!(rep.parts_merged, 2);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 3, "one of the two ts=1000 duplicates dropped");
    assert_eq!(rep.rows_superseded, 1);

    let got = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(
        got.iter().map(|t| t.ts).collect::<Vec<_>>(),
        vec![500, 1_000, 2_000],
        "one row per ts; non-duplicates untouched"
    );
    let win = got.iter().find(|t| t.ts == 1_000).unwrap();
    assert_eq!(win.size.to_bits(), 10.0f64.to_bits(), "the higher-ranked (live) row survived");
    // the untouched rows are exactly the originals
    assert_eq!(got.iter().find(|t| t.ts == 500).unwrap().size.to_bits(), 5.0f64.to_bits());
    assert_eq!(got.iter().find(|t| t.ts == 2_000).unwrap().size.to_bits(), 7.0f64.to_bits());
}

#[test]
fn supersession_precedence_is_the_policy_order_not_the_source_name() {
    // Same two sources, REVERSED precedence (pmxt > live): now pmxt's ts=1000 row (size 99) wins.
    // Proves the survivor is chosen by the policy rank, not source identity or append order.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.50, 10.0)], Some("live-polymarket-TOK-t"))
        .unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.50, 99.0)], Some("pmxt:trade:TOK:h"))
        .unwrap();

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let policy = SourceRankPolicy::new(["pmxt:", "live-"]); // pmxt now outranks live
    let rep =
        df.compact_series_superseding("trade", "polymarket", "TOK", None, &cfg, &policy).unwrap();
    assert_eq!(rep.rows, 1);
    assert_eq!(rep.rows_superseded, 1);

    let got = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].size.to_bits(), 99.0f64.to_bits(), "pmxt won under reversed precedence");
}

#[test]
fn supersession_book_event_superseded_atomically_by_ts_seq() {
    // The book kind's natural key is (ts, seq): a book EVENT captured by two sources collides on
    // (ts, seq) and is superseded ATOMICALLY (all its level-rows together), keeping the
    // higher-ranked source's whole event. Non-duplicate events (live-only, pmxt-only) survive.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    // live: a live-only event (500,4) + the collision event (1000,5) with live's levels
    df.append_book_updates(
        "polymarket",
        "TOK",
        &[
            bu(500, 4, BookUpdateKind::Snapshot, vec![(0.45, 10.0)], vec![(0.46, 20.0)]),
            bu(1_000, 5, BookUpdateKind::Snapshot, vec![(0.45, 100.0)], vec![(0.46, 50.0)]),
        ],
        Some("live-polymarket-TOK-book-1-2-3"),
    )
    .unwrap();
    // pmxt: the SAME (1000,5) event with DIFFERENT sizes + a pmxt-only event (2000,6)
    df.append_book_updates(
        "polymarket",
        "TOK",
        &[
            bu(1_000, 5, BookUpdateKind::Snapshot, vec![(0.45, 999.0)], vec![(0.46, 555.0)]),
            bu(2_000, 6, BookUpdateKind::Snapshot, vec![(0.43, 3.0)], vec![(0.47, 4.0)]),
        ],
        Some("pmxt:book:TOK:2026-07-01T00"),
    )
    .unwrap();

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let policy = SourceRankPolicy::new(["live-", "pmxt:"]);
    let rep =
        df.compact_series_superseding("book", "polymarket", "TOK", None, &cfg, &policy).unwrap();
    // the collision event contributed 2 level-rows per source; pmxt's 2 rows are the ones dropped
    assert_eq!(rep.rows_superseded, 2);

    let got = df.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3, "three events: (500,4), the deduped (1000,5), (2000,6)");
    let mid = got.iter().find(|u| u.seq == 5).unwrap();
    assert_eq!(mid.bids, vec![(0.45, 100.0)], "live's event survived (not pmxt's 999.0)");
    assert_eq!(mid.asks, vec![(0.46, 50.0)]);
    // the non-duplicate events are untouched
    assert_eq!(got.iter().find(|u| u.seq == 4).unwrap().bids, vec![(0.45, 10.0)]);
    assert_eq!(got.iter().find(|u| u.seq == 6).unwrap().bids, vec![(0.43, 3.0)]);
}

// ---- slice 5: ingest an external parquet file (replaces the Python exporter) ----------------

/// Return the first `.parquet` part under a series dir (recurses `date=` subdirs).
fn find_one_parquet(dir: &Path) -> Option<std::path::PathBuf> {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = find_one_parquet(&p) {
                    return Some(f);
                }
            } else if p.extension().is_some_and(|x| x == "parquet") {
                return Some(p);
            }
        }
    }
    None
}

#[test]
fn append_bars_from_parquet_round_trips_bit_for_bit() {
    // Storage-parity-neutral proof: bars written by the store, read back through the external-
    // parquet ingest path, are bit-identical — so the bench reads the SAME bars with no Python.
    let src = tempfile::tempdir().unwrap();
    let a = DataFusionHist::open(src.path()).unwrap();
    let bars: Vec<Bar> = (0..30).map(|i| bar(i * 60_000, 100.0 + i as f64 * 0.1, None)).collect();
    a.append_bars("binance", "BTCUSDT", "1m", &bars, Some("x")).unwrap();
    let part = find_one_parquet(&bars_series(src.path())).expect("a written part file");

    // ingest that raw parquet into a FRESH store via the external-file path
    let dst = tempfile::tempdir().unwrap();
    let b = DataFusionHist::open(dst.path()).unwrap();
    let n = b.append_bars_from_parquet(&part, "binance", "BTCUSDT", "1m", Some("y")).unwrap();
    assert_eq!(n, 30);
    let got = b.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got); // ts/open/high/low/close/volume bit-eq; funding None both sides

    // idempotent like any append
    assert_eq!(
        b.append_bars_from_parquet(&part, "binance", "BTCUSDT", "1m", Some("y")).unwrap(),
        0
    );
}

// ---- slice 6: WAL crash-recovery for in-flight appends -------------------------------------

#[test]
fn wal_recovers_unpublished_append() {
    // Simulate a crash in the seal->publish window: the store seals the parquet part + fsyncs the
    // WAL record but never publishes the manifest (the test-only crash-injection hook). The append is
    // therefore invisible to reads on that (crashed) store. A FRESH open() must replay the WAL and
    // surface the rows, bit-for-bit — the proof that an accepted-but-unpublished append is not lost.
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..7)
        .map(|i| {
            bar(i * 60_000, 100.0 + i as f64, if i % 2 == 0 { Some(0.01 * i as f64) } else { None })
        })
        .collect();
    {
        let df = DataFusionHist::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true); // crash right after the WAL fsync, before manifest publish
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap();
        // the unpublished append is invisible on the crashed store (manifest never advanced)
        assert!(
            df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().is_empty(),
            "pre-recovery: an unpublished append is invisible"
        );
        // drop df == process crash (no clean shutdown, no manifest publish)
    }
    // reopen the SAME dir → open() runs WAL recovery
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got); // rows recovered, every field bit-identical
    // the recovered commit key is now durable → re-appending it is the usual idempotent no-op
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap(), 0);
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 7);

    // a second reopen recovers nothing (the WAL was cleared once applied)
    drop(df2);
    let df3 = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df3.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 7);
}

#[test]
fn wal_replay_is_idempotent() {
    // Two WAL records carrying the SAME commit key (an in-flight batch logged, then retried, both
    // before any publish) must recover to ONE copy of the rows — the commit-key guard makes replay
    // idempotent — and a normal re-append of that key afterwards is still a no-op.
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..5).map(|i| bar(i * 60_000, 50.0 + i as f64, None)).collect();
    {
        let df = DataFusionHist::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true);
        // same key logged twice with no publish in between → two WAL records, key not yet committed
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap();
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap();
    }
    // recovery replays the first record and skips the second (key already committed) → 5 rows, not 10
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 5, "duplicate WAL records for one key recover exactly once");
    assert_bars_bit_eq(&bars, &got);

    // recovery cleared the WAL + published the key → a normal re-append is the usual idempotent no-op
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap(), 0);
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 5);

    // and re-running recovery (reopen) is a no-op — no re-duplication
    drop(df2);
    let df3 = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df3.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 5);
}

// ---- slice 7: whole-store maintenance (list_series + run_maintenance + scheduler) -----------

/// The bar series leaf dir for an arbitrary `symbol` (parts live under `date=…` one level deeper).
fn bars_series_of(root: &Path, symbol: &str) -> PathBuf {
    root.join("kind=bar").join("venue=binance").join(format!("symbol={symbol}")).join("interval=1m")
}

#[test]
fn run_maintenance_compacts_and_prunes_all_series() {
    // Two bar series, each seeded with an OLD date (day 0) of 2 fragments — below the compaction
    // threshold, so retention prunes them — plus a NEW date (day 10) of 4 fragments — above the
    // threshold, so compaction merges them and retention keeps them. ONE run_maintenance pass must
    // compact every eligible series AND prune the old date, leaving the surviving rows bit-identical.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // day `day`, fragment `c`: 5 ts-distinct bars within that UTC day (fragments never overlap in ts).
    let frag = |day: i64, c: i64| -> Vec<Bar> {
        let base = day * DAY + c * 5 * 1000;
        (0..5).map(move |i| bar(base + i * 1000, 100.0 + (c * 5 + i) as f64, None)).collect()
    };

    for sym in ["BTCUSDT", "ETHUSDT"] {
        for c in 0..2i64 {
            // OLD date (day 0): 2 fragments — below min_parts=3, later pruned
            df.append_bars("binance", sym, "1m", &frag(0, c), Some(&format!("{sym}-old-{c}")))
                .unwrap();
        }
        for c in 0..4i64 {
            // NEW date (day 10): 4 fragments — above min_parts=3, merged then kept
            df.append_bars("binance", sym, "1m", &frag(10, c), Some(&format!("{sym}-new-{c}")))
                .unwrap();
        }
        assert_eq!(count_parquets(&bars_series_of(dir.path(), sym)), 6, "6 fragments before {sym}");
    }

    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() },
        retention: Some(RetentionPolicy { before_ts: Some(5 * DAY), max_age_ms: None }),
    };
    let report = df.run_maintenance(&cfg).unwrap();

    // both series visited; compaction totals = 2 series × (4 fragments merged → 1 sealed, 20 rows)
    assert_eq!(report.series_visited(), 2);
    assert_eq!(report.compaction.parts_merged, 8);
    assert_eq!(report.compaction.parts_written, 2);
    assert_eq!(report.compaction.rows, 40);
    // retention totals = 2 series × (2 old fragments dropped, 1 date dir dropped, 10 rows)
    assert_eq!(report.retention.files_dropped, 4);
    assert_eq!(report.retention.dates_dropped, 2);
    assert_eq!(report.retention.rows_dropped, 20);

    // per-series: EVERY eligible series had its day-10 fragments merged AND its old date pruned.
    for sm in &report.series {
        assert_eq!(sm.compaction.parts_merged, 4, "{:?} merged its 4 day-10 frags", sm.series);
        assert_eq!(sm.compaction.parts_written, 1, "{:?} sealed one part", sm.series);
        assert_eq!(sm.retention.dates_dropped, 1, "{:?} old date pruned", sm.series);
    }

    // filesystem + read-back: each series is now ONE merged part holding exactly the day-10 rows.
    for sym in ["BTCUSDT", "ETHUSDT"] {
        let series = bars_series_of(dir.path(), sym);
        assert_eq!(count_parquets(&series), 1, "one merged part remains for {sym}");
        assert!(!series.join("date=1970-01-01").exists(), "old date dir unlinked for {sym}");
        let want: Vec<Bar> = (0..4).flat_map(|c| frag(10, c)).collect();
        let got = df.load_bars("binance", sym, "1m", TsRange::all()).unwrap();
        assert_bars_bit_eq(&want, &got);
    }
}

#[test]
fn list_series_roundtrips_the_tree() {
    // A handful of series across kinds/venues/symbols/intervals (including tick series, interval=None)
    // must enumerate back EXACTLY — with (kind, venue, symbol, interval) parsed straight from the path.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    let bars: Vec<Bar> = (0..4).map(|i| bar(i * 60_000, 100.0, None)).collect();
    let trades: Vec<TradeTick> = (0..4)
        .map(|i| TradeTick {
            ts: i * 1000,
            local_ts: 0,
            price: 10.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        })
        .collect();
    let quotes: Vec<QuoteTick> = (0..4)
        .map(|i| QuoteTick {
            ts: i * 1000,
            local_ts: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        })
        .collect();

    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    df.append_bars("binance", "BTCUSDT", "5m", &bars, None).unwrap(); // same symbol, other interval
    df.append_bars("okx", "ETHUSDT", "1m", &bars, None).unwrap(); // other venue + symbol
    df.append_trades("binance", "BTCUSDT", &trades, None).unwrap(); // tick series → interval None
    df.append_quotes("binance", "BTCUSDT", &quotes, None).unwrap();

    let sid = |kind: &str, venue: &str, symbol: &str, interval: Option<&str>| SeriesId {
        kind: kind.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.map(str::to_string),
        group: None,
    };
    let mut want = vec![
        sid("bar", "binance", "BTCUSDT", Some("1m")),
        sid("bar", "binance", "BTCUSDT", Some("5m")),
        sid("bar", "okx", "ETHUSDT", Some("1m")),
        sid("trade", "binance", "BTCUSDT", None),
        sid("quote", "binance", "BTCUSDT", None),
    ];
    want.sort();

    let got = df.list_series().unwrap(); // list_series returns sorted
    assert_eq!(got, want, "every series enumerates back with the right identity");
}

#[test]
fn scheduler_runs_then_stops_clean() {
    // Seed a store that WILL compact (4 same-day fragments), start the scheduler on a short interval,
    // poll (bounded) until it reports a compaction, then stop() — which must join cleanly — and assert
    // the store is consistent: merged rows bit-identical + fewer part files. Deterministic: the store
    // is fully seeded BEFORE start, so the first pass compacts; the poll is a monotonic counter with a
    // generous 10s ceiling (no fixed sleeps racing the worker).
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    for c in 0..4i64 {
        let b: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("s{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    assert_eq!(count_parquets(&series), 4, "4 fragments seeded");
    let expected = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    // No retention → the scheduler only compacts; every row survives (a clean consistency check).
    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() },
        retention: None,
    };
    let mut sched = MaintenanceScheduler::start(df.clone(), cfg, Duration::from_millis(5));

    // bounded wait: poll the monotonic compaction counter until the first merge lands (or time out).
    let start = Instant::now();
    while sched.parts_compacted() == 0 {
        assert!(start.elapsed() < Duration::from_secs(10), "scheduler did not compact within 10s");
        std::thread::sleep(Duration::from_millis(5));
    }

    sched.stop(); // set the flag, wake the sleep, join the thread — deterministic teardown
    sched.stop(); // idempotent: a second stop() (and the eventual Drop) is a harmless no-op

    assert!(sched.passes_completed() >= 1, "at least one full pass completed");
    assert!(count_parquets(&series) < 4, "compaction reduced the part count");
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&expected, &got); // 20 rows preserved bit-for-bit through compaction
}

// ---- slice 8: bounded-memory streaming scans (execute_stream) -------------------------------
//
// The `scan_*_stream` twins yield rows in bounded batches (one RecordBatch resident at a time) instead
// of materializing the whole range into a `Vec`. The gate: over a series spanning MULTIPLE parts AND
// multiple date= partitions, the STREAMED sequence must equal the existing (materialized) `scan_*` Vec
// bit-for-bit — same values (to_bits on floats), same ts, same flags, same length AND order.

fn assert_trades_bit_eq(want: &[TradeTick], got: &[TradeTick]) {
    assert_eq!(want.len(), got.len(), "trade count");
    for (i, (x, y)) in want.iter().zip(got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.price.to_bits(), y.price.to_bits(), "price[{i}] ({} vs {})", x.price, y.price);
        assert_eq!(x.size.to_bits(), y.size.to_bits(), "size[{i}] ({} vs {})", x.size, y.size);
        assert_eq!(x.is_buyer_maker, y.is_buyer_maker, "is_buyer_maker[{i}]");
        assert_eq!(x.symbol, y.symbol, "symbol[{i}]");
    }
}

fn assert_quotes_bit_eq(want: &[QuoteTick], got: &[QuoteTick]) {
    assert_eq!(want.len(), got.len(), "quote count");
    for (i, (x, y)) in want.iter().zip(got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.bid.to_bits(), y.bid.to_bits(), "bid[{i}]");
        assert_eq!(x.ask.to_bits(), y.ask.to_bits(), "ask[{i}]");
        assert_eq!(x.bid_size.to_bits(), y.bid_size.to_bits(), "bid_size[{i}]");
        assert_eq!(x.ask_size.to_bits(), y.ask_size.to_bits(), "ask_size[{i}]");
        assert_eq!(x.symbol, y.symbol, "symbol[{i}]");
    }
}

#[test]
fn streaming_scan_equals_materialized_bit_eq() {
    // ~4.5k trades across 6 daily appends (6 date= partitions) PLUS a second part inside one date —
    // exercises multi-part AND multi-date streaming; then streamed == materialized `scan_trades`, exactly.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    let mut total = 0usize;
    for day in 0..6i64 {
        // one append per UTC day → one sealed part per date=, each a disjoint ascending window
        let batch: Vec<TradeTick> = (0..700)
            .map(|i| {
                let ts = day * DAY + i * 100; // 0..69_900 ms into the day (well within it)
                TradeTick {
                    ts,
                    local_ts: 0,
                    price: 100.0 + ts as f64 * 1e-7,
                    size: 0.5 + (i % 7) as f64 * 0.01,
                    is_buyer_maker: i % 2 == 0,
                    symbol: String::new(),
                }
            })
            .collect();
        df.append_trades("binance", "BTCUSDT", &batch, Some(&format!("t-day{day}"))).unwrap();
        total += batch.len();
    }
    // a SECOND part inside day 2's partition (later, disjoint window) → multiple parts in one date=
    let extra: Vec<TradeTick> = (0..300)
        .map(|i| {
            let ts = 2 * DAY + 100_000 + i * 100;
            TradeTick {
                ts,
                local_ts: 0,
                price: 200.0 + ts as f64 * 1e-7,
                size: 1.0 + (i % 3) as f64 * 0.02,
                is_buyer_maker: i % 3 == 0,
                symbol: String::new(),
            }
        })
        .collect();
    df.append_trades("binance", "BTCUSDT", &extra, Some("t-day2-extra")).unwrap();
    total += extra.len();

    // full range: the streamed sequence equals the materialized Vec, bit-for-bit and in the same order
    let want = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(want.len(), total, "materialized covers every ingested trade");
    let got: Vec<TradeTick> = df
        .scan_trades_stream("binance", "BTCUSDT", TsRange::all())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_trades_bit_eq(&want, &got);

    // the bounded-memory stream still yields globally ts-ascending across parts AND date= partitions
    assert!(
        got.windows(2).all(|w| w[0].ts < w[1].ts),
        "streamed ts strictly ascending across parts/dates"
    );

    // a bounded sub-range exercises the row-level ts filter inside the streaming plan
    let r = TsRange::of(DAY, 3 * DAY + 50_000);
    let wr = df.scan_trades("binance", "BTCUSDT", r).unwrap();
    let gr: Vec<TradeTick> = df
        .scan_trades_stream("binance", "BTCUSDT", r)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!wr.is_empty() && wr.len() < total, "sub-range is a proper subset");
    assert_trades_bit_eq(&wr, &gr);

    // unknown series streams empty (not an error), mirroring `scan_trades`
    let empty: Vec<TradeTick> = df
        .scan_trades_stream("okx", "ETHUSDT", TsRange::all())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(empty.is_empty(), "unknown series → empty stream");
}

#[test]
fn streaming_quotes_scan_equals_materialized_bit_eq() {
    // Same proof for quotes: multi-append, multi-date (+ a 2nd part in one date), streamed == materialized.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    let mut total = 0usize;
    for day in 0..5i64 {
        let batch: Vec<QuoteTick> = (0..800)
            .map(|i| {
                let ts = day * DAY + i * 100;
                QuoteTick {
                    ts,
                    local_ts: 0,
                    bid: 100.0 + ts as f64 * 1e-7,
                    ask: 100.05 + ts as f64 * 1e-7,
                    bid_size: 1.0 + (i % 5) as f64 * 0.1,
                    ask_size: 2.0 + (i % 4) as f64 * 0.1,
                    symbol: String::new(),
                }
            })
            .collect();
        df.append_quotes("binance", "BTCUSDT", &batch, Some(&format!("q-day{day}"))).unwrap();
        total += batch.len();
    }
    // second part inside day 1's partition
    let extra: Vec<QuoteTick> = (0..200)
        .map(|i| {
            let ts = DAY + 200_000 + i * 100;
            QuoteTick {
                ts,
                local_ts: 0,
                bid: 300.0 + ts as f64 * 1e-7,
                ask: 300.05 + ts as f64 * 1e-7,
                bid_size: 3.0 + (i % 2) as f64 * 0.25,
                ask_size: 4.0 + (i % 3) as f64 * 0.25,
                symbol: String::new(),
            }
        })
        .collect();
    df.append_quotes("binance", "BTCUSDT", &extra, Some("q-day1-extra")).unwrap();
    total += extra.len();

    let want = df.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(want.len(), total);
    let got: Vec<QuoteTick> = df
        .scan_quotes_stream("binance", "BTCUSDT", TsRange::all())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_quotes_bit_eq(&want, &got);
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "streamed quotes ts strictly ascending");

    let r = TsRange::of(DAY, 2 * DAY + 40_000);
    let wr = df.scan_quotes("binance", "BTCUSDT", r).unwrap();
    let gr: Vec<QuoteTick> = df
        .scan_quotes_stream("binance", "BTCUSDT", r)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!wr.is_empty() && wr.len() < total);
    assert_quotes_bit_eq(&wr, &gr);
}

#[test]
fn streaming_bars_scan_equals_materialized_bit_eq() {
    // load_bars_stream mirrors load_bars bit-for-bit (incl. Option<funding>) across multiple date= parts.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for day in 0..4i64 {
        let batch: Vec<Bar> = (0..200)
            .map(|i| {
                let ts = day * DAY + i * 60_000; // 200 minute-bars = ~3.3h, well within a day
                let f = if i % 3 == 0 { Some(0.0001 * i as f64) } else { None };
                bar(ts, 100.0 + i as f64 * 0.25, f)
            })
            .collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b-day{day}"))).unwrap();
    }
    let want = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    let got: Vec<Bar> = df
        .load_bars_stream("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_bars_bit_eq(&want, &got);
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "streamed bars ts strictly ascending");
}

// ---- kind=book series + local_ts columns (book-recording plan Task 5) ------------------------

/// Book-event literal: a `BookUpdate` at `ts` with `local_ts = ts + 2` and the given levels.
fn bu(
    ts: i64,
    seq: u64,
    kind: BookUpdateKind,
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts + 2,
        seq,
        kind,
        tick_size: 0.01,
        bids,
        asks,
        symbol: String::new(),
    }
}

/// The `kind=book` store series round-trips per-level rows back into the exact `BookUpdate`
/// events (bit-for-bit f64), including zero-level status events (placeholder-row decode), the
/// (seq, kind) regroup boundary rule, and batch-level idempotency.
#[test]
fn book_updates_roundtrip_bit_eq() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let sent = vec![
        bu(
            1_000,
            1,
            BookUpdateKind::Snapshot,
            vec![(0.45, 100.0), (0.44, 20.0)],
            vec![(0.46, 50.0)],
        ),
        bu(1_005, 2, BookUpdateKind::Delta, vec![(0.45, 0.0)], vec![]),
        bu(1_010, 0, BookUpdateKind::GapStart, vec![], vec![]), // status: zero levels
        bu(1_020, 0, BookUpdateKind::LiveResume, vec![], vec![]),
        bu(1_021, 3, BookUpdateKind::Snapshot, vec![(0.44, 20.0)], vec![(0.47, 5.0)]),
    ];
    let n = store.append_book_updates("polymarket", "TOK", &sent, Some("k1")).unwrap();
    assert_eq!(n, 5, "returns events, not rows");
    let got = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 5);
    for (w, g) in sent.iter().zip(&got) {
        assert_eq!(w.ts, g.ts);
        assert_eq!(w.local_ts, g.local_ts);
        assert_eq!(w.seq, g.seq);
        assert_eq!(w.kind, g.kind);
        assert_eq!(w.tick_size.to_bits(), g.tick_size.to_bits());
        assert_eq!(w.bids.len(), g.bids.len());
        assert_eq!(w.asks.len(), g.asks.len());
        for (a, b) in w.bids.iter().zip(&g.bids) {
            assert_eq!(a.0.to_bits(), b.0.to_bits());
            assert_eq!(a.1.to_bits(), b.1.to_bits());
        }
        for (a, b) in w.asks.iter().zip(&g.asks) {
            assert_eq!(a.0.to_bits(), b.0.to_bits());
            assert_eq!(a.1.to_bits(), b.1.to_bits());
        }
    }
    // idempotency: same commit key = silent no-op
    assert_eq!(store.append_book_updates("polymarket", "TOK", &sent, Some("k1")).unwrap(), 0);
}

/// The additive `local_ts` column on the quote series round-trips through append/scan, and the
/// decoder path defaults it to 0 for parts written before the column existed (the additive-schema
/// contract — proven bindingly by `quotes_from_batch_defaults_missing_local_ts` in codec.rs).
#[test]
fn quote_local_ts_roundtrips_and_old_parts_stay_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let q = QuoteTick {
        ts: 1_000,
        local_ts: 1_003,
        bid: 1.0,
        ask: 1.1,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    };
    store.append_quotes("v", "S", &[q], Some("k1")).unwrap();
    let got = store.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(got[0].local_ts, 1_003);
}

#[test]
fn symbol_properties_round_trip_and_as_of() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let f1 = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
    };
    let f2 = SymbolProperties {
        tick_size: 0.05,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
    };
    // day 1 (2020-01-01) and day 30 (2020-01-31), distinct commit keys
    let d1 = 1_577_836_800_000i64; // 2020-01-01T00:00:00Z ms
    let d2 = 1_580_428_800_000i64; // 2020-01-31T00:00:00Z ms
    store
        .append_symbol_properties("bybit", "BTCUSDT", &[(d1, f1)], Some("bybit:BTCUSDT:2020-01-01"))
        .unwrap();
    store
        .append_symbol_properties("bybit", "BTCUSDT", &[(d2, f2)], Some("bybit:BTCUSDT:2020-01-31"))
        .unwrap();

    let all = store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(all, vec![(d1, f1), (d2, f2)]); // ts-ascending

    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d1 - 1).unwrap(), None); // before first
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d1).unwrap(), Some(f1)); // at first
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d2 - 1).unwrap(), Some(f1)); // between → older
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d2 + 999).unwrap(), Some(f2)); // after → latest
    assert_eq!(store.properties_as_of("bybit", "UNKNOWN", d2).unwrap(), None); // unknown symbol
}

/// The store-level twin of the codec's `properties_codec_round_trips_the_tick_scheme`: a populated
/// `TickScheme` must survive a real write→Parquet→read round trip (and `properties_as_of`), and a
/// scheme-less row must come back `None`. This is the hole the `SymbolProperties::tick_scheme` field
/// doc documented — a struct field with no codec column is silently dropped on a store round-trip.
#[test]
fn symbol_properties_round_trip_carries_the_tick_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    // the real Deribit BTC-option grid: base 0.0001, 0.0005 above 0.005.
    let scheme = TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }])
        .expect("valid deribit grid");
    let tiered = SymbolProperties {
        tick_size: 0.0001,
        step_size: 0.1,
        min_qty: 0.1,
        max_qty: 0.0,
        min_notional: 0.0,
        contract_size: 1.0,
        tick_scheme: Some(scheme),
        taker_hold_ms: 0,
    };
    let flat = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
    };
    let d1 = 1_577_836_800_000i64; // 2020-01-01
    let d2 = 1_580_428_800_000i64; // 2020-01-31
    store
        .append_symbol_properties(
            "deribit",
            "BTC-OPT",
            &[(d1, tiered)],
            Some("deribit:BTC-OPT:2020-01-01"),
        )
        .unwrap();
    store
        .append_symbol_properties(
            "deribit",
            "BTC-OPT",
            &[(d2, flat)],
            Some("deribit:BTC-OPT:2020-01-31"),
        )
        .unwrap();

    let all = store.scan_symbol_properties("deribit", "BTC-OPT", TsRange::all()).unwrap();
    assert_eq!(all, vec![(d1, tiered), (d2, flat)], "both rows ride back through Parquet intact");
    // the tiered grid genuinely survived — it still resolves by price, not as a flat tick.
    let back = all[0].1.tick_scheme.expect("the scheme must survive the store round-trip");
    assert_eq!(back, scheme);
    assert_eq!(all[0].1.effective_tick(0.05), 0.0005);
    assert_eq!(all[0].1.effective_tick(0.004), 0.0001);
    assert!(all[1].1.tick_scheme.is_none(), "the scheme-less row stays None");
    // and the PIT read carries it too.
    assert_eq!(store.properties_as_of("deribit", "BTC-OPT", d1).unwrap(), Some(tiered));
    assert_eq!(store.properties_as_of("deribit", "BTC-OPT", d2).unwrap(), Some(flat));
}

#[test]
fn symbol_properties_daily_commit_key_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let f = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
    };
    let d1 = 1_577_836_800_000i64;
    store
        .append_symbol_properties(
            "okx",
            "BTC-USDT-SWAP",
            &[(d1, f)],
            Some("okx:BTC-USDT-SWAP:2020-01-01"),
        )
        .unwrap();
    let second = store
        .append_symbol_properties(
            "okx",
            "BTC-USDT-SWAP",
            &[(d1, f)],
            Some("okx:BTC-USDT-SWAP:2020-01-01"),
        )
        .unwrap();
    assert_eq!(second, 0, "same commit_key must dedup to a no-op");
    assert_eq!(
        store.scan_symbol_properties("okx", "BTC-USDT-SWAP", TsRange::all()).unwrap().len(),
        1
    );
}

// ---- SeriesCoverage / inventory --------------------------------------------------------------

#[test]
fn inventory_reports_series_coverage_from_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars = vec![bar(1_000, 100.0, None), bar(61_000, 101.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let inv = df.inventory().unwrap();
    assert_eq!(inv.len(), 1, "one series");
    let (id, cov) = &inv[0];
    assert_eq!(id.kind, "bar");
    assert_eq!(id.venue, "binance");
    assert_eq!(id.symbol, "BTCUSDT");
    assert_eq!(cov.rows, 2);
    assert!(cov.parts >= 1);
    assert!(cov.first_ts <= cov.last_ts);
    assert!(cov.bytes > 0, "part files have a size on disk");

    // series_coverage for the same id matches
    let direct = df.series_coverage(id).unwrap();
    assert_eq!(&direct, cov);
}

/// PR-6: the `HistStore` TRAIT overrides for `list_series`/`inventory`/`series_gaps` reach the REAL
/// manifest walk when the store is erased to `&dyn HistStore` (how vike-datahub's `serve` holds it),
/// NOT the trait default (which now REFUSES the catalog pair outright). This is the whole point of
/// the widening — proving the
/// inherent→trait delegation on `DataFusionHist` so `RemoteHistStore` can serve the catalog.
#[test]
fn trait_object_metadata_verbs_reach_real_data() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars = vec![bar(1_000, 100.0, None), bar(61_000, 101.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    // erase to the trait object exactly as `serve` holds it (`Arc<dyn HistStore + Send + Sync>`).
    let store: &dyn HistStore = &df;

    let series = store.list_series().unwrap();
    assert_eq!(series.len(), 1, "the trait method reaches the real series, NOT the trait default");
    assert_eq!(series[0].venue, "binance");
    assert_eq!(series[0].kind, "bar");

    let inv = store.inventory().unwrap();
    assert_eq!(inv.len(), 1, "trait inventory reaches real coverage");
    assert_eq!(inv[0].1.rows, 2, "coverage is the real manifest fold, not a default zero");

    // series_gaps via the trait matches the concrete inherent call (delegation is behaviour-identical)
    let gaps = store.series_gaps(&series[0]).unwrap();
    assert_eq!(gaps, df.series_gaps(&series[0]).unwrap(), "trait gaps == inherent gaps");
}

#[test]
fn delete_series_removes_the_series_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 100.0, None)], None).unwrap();
    df.append_bars("okx", "BTC-USDT", "1m", &[bar(1_000, 100.0, None)], None).unwrap();
    assert_eq!(df.inventory().unwrap().len(), 2);

    let target = df.list_series().unwrap().into_iter().find(|s| s.venue == "binance").unwrap();
    df.delete_series(&target).unwrap();

    let left = df.list_series().unwrap();
    assert_eq!(left.len(), 1, "only the binance series was deleted");
    assert_eq!(left[0].venue, "okx", "sibling untouched");

    // idempotent: deleting an already-absent series is Ok
    df.delete_series(&target).unwrap();
    assert_eq!(df.list_series().unwrap().len(), 1);
}

/// ⚠ The idempotent path must not CREATE the leaf it came to delete.
///
/// `SeriesLock::acquire` opens its lock file with a `create_dir_all` in front of it, so a locked
/// delete that took the guard before probing would materialize a `kind=/venue=/symbol=` directory
/// holding one empty `_manifest.lock` for every mistyped series anybody ever asked it to remove.
/// The probe therefore comes FIRST, and this is what says so.
#[test]
fn deleting_an_absent_series_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let ghost = SeriesId::per_symbol("bar", "binance", "NOTHING", Some("1m".to_string()));

    df.delete_series(&ghost).unwrap();

    assert!(
        !dir.path().join("kind=bar").exists(),
        "a delete of an absent series must leave no directory behind"
    );
    assert!(df.list_series().unwrap().is_empty());
}

/// **The lock the delete never used to take.** Every other mutating verb on this type serializes on
/// `_manifest.lock`; `delete_series` did not, so a cleanup run against the box that is RECORDING
/// could remove a leaf mid-append.
///
/// The proof holds the OS advisory lock from the TEST (exactly as a live writer's `SeriesLock`
/// does) and asserts the delete cannot proceed — and, crucially, that the series is still whole
/// afterwards, because a delete that failed AFTER removing the parts would be worse than one that
/// never locked.
///
/// ⚠ It costs the full spin budget (`SPIN_ATTEMPTS` × 2 ms ≈ 4 s) by construction: the contended
/// path IS the timeout, and shortening it would need a test-only knob on a code path whose whole
/// value is that it has none.
#[test]
fn a_delete_cannot_take_a_series_a_live_writer_is_holding() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 100.0, None)], Some("a")).unwrap();
    let id = df.list_series().unwrap().pop().unwrap();
    let before = df.series_coverage(&id).unwrap();
    assert_eq!(before.rows, 1);

    // What a live writer holds: the OS lock on `_manifest.lock`, not the file's existence.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(bars_series(dir.path()).join("_manifest.lock"))
        .unwrap();
    held.try_lock().expect("the test must be the holder for this to prove anything");

    let err =
        df.delete_series(&id).expect_err("a held series must not be deleted under the holder");
    assert!(err.to_string().contains("timeout acquiring series lock"), "{err}");
    assert_eq!(
        df.series_coverage(&id).unwrap(),
        before,
        "a refused delete must leave the series exactly as it found it"
    );

    // ...and once the writer is gone the same call succeeds, so the refusal was the LOCK and not
    // something about the series.
    held.unlock().unwrap();
    drop(held);
    df.delete_series(&id).unwrap();
    assert!(df.list_series().unwrap().is_empty());
}

/// The provenance assertion, at the one place it can be trusted: inside the critical section.
///
/// Three shapes, and the middle one is the whole feature — a MIXED series refuses rather than
/// being silently skipped, because a filter would have removed the panel rows and left an operator
/// believing the venue's own candles had gone with them (or vice versa).
#[test]
fn a_checked_delete_refuses_a_key_the_assertion_does_not_cover() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:1"))
        .unwrap();
    df.append_bars("hyperliquid", "BTC", "1h", &[bar(2_000, 101.0, None)], Some("klines:1"))
        .unwrap();
    let id = df.list_series().unwrap().pop().unwrap();
    assert_eq!(df.series_commits(&id).unwrap(), vec!["panel_bars:1", "klines:1"]);

    let err = df.delete_series_checked(&id, Some("panel_bars:")).unwrap_err();
    assert!(err.to_string().contains("klines:1"), "the refusal names the offending key: {err}");
    assert_eq!(df.series_coverage(&id).unwrap().rows, 2, "nothing was deleted");

    // A series whose EVERY key carries the prefix goes.
    let dir2 = tempfile::tempdir().unwrap();
    let df2 = DataFusionHist::open(dir2.path()).unwrap();
    df2.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:1"))
        .unwrap();
    let id2 = df2.list_series().unwrap().pop().unwrap();
    df2.delete_series_checked(&id2, Some("panel_bars:")).unwrap();
    assert!(df2.list_series().unwrap().is_empty());

    // A KEYLESS series records nothing, so it can satisfy no assertion — and refusing is the only
    // safe reading of "I do not know who wrote this".
    let dir3 = tempfile::tempdir().unwrap();
    let df3 = DataFusionHist::open(dir3.path()).unwrap();
    df3.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], None).unwrap();
    let id3 = df3.list_series().unwrap().pop().unwrap();
    assert!(df3.series_commits(&id3).unwrap().is_empty());
    let err = df3.delete_series_checked(&id3, Some("panel_bars:")).unwrap_err();
    assert!(err.to_string().contains("NO commit keys"), "{err}");
    // ...and WITHOUT an assertion the same series is deletable, which is the difference between
    // deleting by name and deleting by a checked property.
    df3.delete_series(&id3).unwrap();
    assert!(df3.list_series().unwrap().is_empty());
}

/// A GROUPED series is deleted by its `group=` leaf, not by a phantom `symbol=` path — the
/// five-bug class `series_dir_of`'s doc records, asserted for the verb that used to be one of them.
#[test]
fn a_grouped_series_is_deleted_by_its_group_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    // A GROUPED row must carry its own symbol — the group leaf tells rows apart by that column.
    let mut in_group = qt(1_000, 1.0);
    in_group.symbol = "0xdead".to_string();
    df.append_quotes_grouped("polymarket", "btc-5m", &[in_group], Some("g1")).unwrap();
    df.append_quotes("polymarket", "0xdead", &[qt(1_000, 1.0)], Some("s1")).unwrap();
    assert_eq!(df.list_series().unwrap().len(), 2);

    let grouped =
        df.list_series().unwrap().into_iter().find(|s| s.group.is_some()).expect("grouped series");
    assert_eq!(df.series_commits(&grouped).unwrap(), vec!["g1"]);
    df.delete_series(&grouped).unwrap();

    let left = df.list_series().unwrap();
    assert_eq!(left.len(), 1, "only the grouped leaf went");
    assert!(left[0].group.is_none(), "the per-symbol sibling is untouched");
}

/// The plan/execute pair over a real store: the selector intersects the store's enumeration, the
/// plan carries provenance, and a refused verdict deletes NOTHING — not even the series that would
/// have passed on their own.
#[test]
fn a_removal_plan_is_all_or_nothing_across_the_selected_set() {
    use vike_data::removal::{SeriesSelector, execute_removal, plan_removal};

    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("hyperliquid", "BTC", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:1"))
        .unwrap();
    df.append_bars("hyperliquid", "ETH", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:2"))
        .unwrap();
    // The mixed one: a legitimate producer wrote into a series the sweep also selects.
    df.append_bars("hyperliquid", "SOL", "1h", &[bar(1_000, 100.0, None)], Some("panel_bars:3"))
        .unwrap();
    df.append_bars("hyperliquid", "SOL", "1h", &[bar(2_000, 101.0, None)], Some("klines:9"))
        .unwrap();
    // ...and a series OUTSIDE the selector, to prove the blast radius.
    df.append_bars("binance", "BTCUSDT", "1h", &[bar(1_000, 100.0, None)], Some("klines:1"))
        .unwrap();

    let sel = SeriesSelector::new("bar", "hyperliquid");
    let plan = plan_removal(&df, &sel, Some("panel_bars:")).unwrap();
    assert_eq!(plan.matched(), 3, "the binance series is outside the selector");
    assert_eq!(plan.rows(), 4);

    let err = execute_removal(&df, &plan).unwrap_err().to_string();
    assert!(err.contains("provenance REFUSED"), "{err}");
    assert_eq!(df.list_series().unwrap().len(), 4, "ONE foreign key refuses the WHOLE run");

    // Naming the mixed series in full is the way through — a different command line with a
    // different plan, rather than a flag that disarms the check.
    let mut narrowed = SeriesSelector::new("bar", "hyperliquid");
    narrowed.symbol = Some("BTC".to_string());
    narrowed.interval = Some("1h".to_string());
    let plan = plan_removal(&df, &narrowed, Some("panel_bars:")).unwrap();
    assert_eq!(plan.matched(), 1);
    let outcome = execute_removal(&df, &plan).unwrap();
    assert!(outcome.is_clean());
    assert_eq!(outcome.deleted.len(), 1);
    assert_eq!(df.list_series().unwrap().len(), 3);

    // Nothing matched is a plan that says so, and executing it is a clean no-op.
    let plan = plan_removal(&df, &narrowed, Some("panel_bars:")).unwrap();
    assert_eq!(plan.matched(), 0);
    let outcome = execute_removal(&df, &plan).unwrap();
    assert!(outcome.is_clean() && outcome.deleted.is_empty());
}

// ---- series_gaps (per-series gap detection over the manifest) ------------------------------

fn bar_series_id(venue: &str, symbol: &str) -> SeriesId {
    SeriesId {
        kind: "bar".to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: Some("1m".to_string()),
        group: None,
    }
}

#[test]
fn series_gaps_finds_the_hole_between_two_non_adjacent_date_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    // present: day 0, day 1 ... then a hole ... present again: day 5. Days 2,3,4 are missing.
    let bars: Vec<Bar> =
        vec![bar(0, 100.0, None), bar(DAY, 101.0, None), bar(5 * DAY, 102.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let id = bar_series_id("binance", "BTCUSDT");
    let gaps = df.series_gaps(&id).unwrap();
    // inclusive epoch-ms range spanning the whole missing days [2,4]: 2*DAY .. (5*DAY - 1)
    assert_eq!(gaps, vec![(2 * DAY, 5 * DAY - 1)], "days 2,3,4 missing between day 1 and day 5");
}

#[test]
fn series_gaps_reports_multiple_holes_across_more_than_two_runs() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    // present days: 0, 2, 3, 7 -> gaps at day 1, and days 4..6
    let bars: Vec<Bar> = vec![
        bar(0, 100.0, None),
        bar(2 * DAY, 101.0, None),
        bar(3 * DAY, 102.0, None),
        bar(7 * DAY, 103.0, None),
    ];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let id = bar_series_id("binance", "BTCUSDT");
    let gaps = df.series_gaps(&id).unwrap();
    assert_eq!(gaps, vec![(DAY, 2 * DAY - 1), (4 * DAY, 7 * DAY - 1)]);
}

#[test]
fn series_gaps_empty_for_contiguous_coverage_and_never_errors_on_absent_series() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> =
        vec![bar(0, 100.0, None), bar(DAY, 101.0, None), bar(2 * DAY, 102.0, None)];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let contiguous = bar_series_id("binance", "BTCUSDT");
    assert_eq!(df.series_gaps(&contiguous).unwrap(), Vec::new(), "contiguous coverage, no gaps");

    // a series with no data at all is Ok(vec![]), never an error
    let absent = bar_series_id("okx", "BTC-USDT");
    assert_eq!(df.series_gaps(&absent).unwrap(), Vec::new());
}

// ---- slice 8: the bulk/offline write profile (BulkIngestSession) -----------------------------
//
// The live-path gates above (bar/quote/trade/book round trips, WAL crash-recovery, maintenance)
// are ALL untouched by this profile's existence — these tests are additive proof that (a) the
// bulk profile actually batches/flushes on its configured window, (b) it stays idempotent across
// a simulated re-run, (c) a crash mid-flush leaves the store readable and safely re-runnable
// (never half-written data that reads as valid), and (d) it genuinely never writes the WAL the
// live profile still does — the one behavior that must NOT have changed.

/// The `kind=trade` series leaf dir for `(venue, symbol)` — no `interval=` segment (ticks_dir's
/// shape). Used to inspect on-disk artifacts (`_wal.arrow` presence) the public API doesn't expose.
fn trade_series_dir(root: &Path, venue: &str, symbol: &str) -> PathBuf {
    root.join("kind=trade").join(format!("venue={venue}")).join(format!("symbol={symbol}"))
}

#[test]
fn bulk_flushes_only_at_the_configured_batch_boundary() {
    // max_batches=3 (rows/bytes set high enough to never trigger first): staging + end_batch
    // twice must NOT flush; the third end_batch call must.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let cfg = BulkConfig { max_batches: 3, max_rows: 1_000_000, max_bytes: 1 << 30 };
    let mut session = df.bulk_session(cfg);

    for i in 0..2i64 {
        session.stage_trades("binance", "BTCUSDT", &[tt(i * 1000, 100.0 + i as f64, 1.0)]);
        let report = session.end_batch("test").unwrap();
        assert_eq!(report, Default::default(), "batch {i}: below the window, no flush yet");
    }
    assert!(
        df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().is_empty(),
        "nothing durable before the window closes"
    );

    session.stage_trades("binance", "BTCUSDT", &[tt(2000, 102.0, 1.0)]);
    let report = session.end_batch("test").unwrap();
    assert_eq!(report.series_flushed, 1, "the 3rd end_batch crosses max_batches=3 and flushes");
    assert_eq!(report.rows_written, 3);
    assert_eq!(session.total_commits(), 1);
    assert_eq!(session.total_rows_written(), 3);

    let got = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3, "all 3 staged rows landed in ONE commit, not three");
}

#[test]
fn bulk_reflush_under_the_same_prefix_is_idempotent() {
    // A fresh session's window index always starts at 0, so replaying the SAME backfill
    // invocation (same key_prefix, same staged rows, same order) from a brand-new session — the
    // shape of "the process restarted and reran the same command" — lands on the identical commit
    // key and must not duplicate rows.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let rows = [tt(0, 1.0, 1.0), tt(1000, 1.1, 1.0), tt(2000, 1.2, 1.0)];

    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "BTCUSDT", &rows);
        let report = session.flush("vikearchive:trade:2026-07-26:bulk").unwrap();
        assert_eq!(report.rows_written, 3);
    }
    assert_eq!(df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(), 3);

    // "re-run": a brand-new session, the identical rows staged again, the identical prefix.
    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "BTCUSDT", &rows);
        let report = session.flush("vikearchive:trade:2026-07-26:bulk").unwrap();
        assert_eq!(report.rows_written, 0, "same window key already durable — no-op, not a dup");
    }
    assert_eq!(
        df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(),
        3,
        "re-running the identical bulk invocation must not duplicate rows"
    );
}

#[test]
fn bulk_crash_mid_flush_leaves_the_store_readable_and_safely_rerunnable() {
    // Series A flushes normally (durable). Series B's flush is interrupted right after its part is
    // sealed but before the manifest publishes (the test-only crash-injection switch this module
    // shares with the live path's WAL-recovery tests) — simulating a process death mid-batch.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let a_rows = [tt(0, 1.0, 1.0), tt(1000, 1.1, 1.0)];
    let b_rows = [tt(0, 2.0, 1.0), tt(1000, 2.1, 1.0), tt(2000, 2.2, 1.0)];

    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "SYM_A", &a_rows);
        let report = session.flush("p").unwrap();
        assert_eq!(report.rows_written, 2, "A commits durably before the simulated crash");
    }
    assert_eq!(df.scan_trades("binance", "SYM_A", TsRange::all()).unwrap().len(), 2);

    df.set_skip_publish_for_test(true);
    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "SYM_B", &b_rows);
        let report = session.flush("p").unwrap(); // seals B's part, but the manifest never publishes
        assert_eq!(report.rows_written, 3, "seal_into_manifest still reports rows sealed");
    }
    df.set_skip_publish_for_test(false);

    // The "crash": B's rows are simply ABSENT (never partially/corruptly visible) — obviously
    // incomplete, not a landmine that later reads as valid data.
    assert!(
        df.scan_trades("binance", "SYM_B", TsRange::all()).unwrap().is_empty(),
        "an unpublished bulk flush must be invisible, exactly like the live path's WAL window"
    );
    // A is completely unaffected by B's crashed flush.
    assert_eq!(df.scan_trades("binance", "SYM_A", TsRange::all()).unwrap().len(), 2);

    // The "re-run": identical invocation (same rows, same prefix) from a fresh session recovers B
    // — and must not duplicate anything, for A or B.
    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "SYM_B", &b_rows);
        let report = session.flush("p").unwrap();
        assert_eq!(report.rows_written, 3, "the redo commits exactly once — no orphan duplication");
    }
    assert_eq!(df.scan_trades("binance", "SYM_B", TsRange::all()).unwrap().len(), 3);
    assert_eq!(df.scan_trades("binance", "SYM_A", TsRange::all()).unwrap().len(), 2, "A untouched");
}

#[test]
fn live_profile_still_writes_a_wal_record_but_bulk_never_does() {
    // The differentiator this whole module trades away: reusing the SAME test-only crash-injection
    // switch (`skip_publish_for_test`) on each profile and checking for `_wal.arrow`'s presence on
    // disk proves the live path is byte-for-byte unchanged (still WAL-then-seal-then-publish) while
    // the bulk path genuinely never appends to a WAL at all (not merely "doesn't need to replay
    // one" — the file is never created).
    let live_dir = tempfile::tempdir().unwrap();
    let live = DataFusionHist::open(live_dir.path()).unwrap();
    live.set_skip_publish_for_test(true);
    live.append_trades("binance", "BTCUSDT", &[tt(0, 1.0, 1.0)], Some("live-batch")).unwrap();
    let live_wal = trade_series_dir(live_dir.path(), "binance", "BTCUSDT").join("_wal.arrow");
    assert!(live_wal.exists(), "the LIVE profile must still WAL-append before a manifest publish");

    let bulk_dir = tempfile::tempdir().unwrap();
    let bulk = DataFusionHist::open(bulk_dir.path()).unwrap();
    bulk.set_skip_publish_for_test(true);
    {
        let mut session = bulk.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "BTCUSDT", &[tt(0, 1.0, 1.0)]);
        session.flush("p").unwrap();
    }
    let bulk_wal = trade_series_dir(bulk_dir.path(), "binance", "BTCUSDT").join("_wal.arrow");
    assert!(!bulk_wal.exists(), "the BULK profile must never write a WAL record at all");
}

// ---- manifest as a rebuildable CACHE, not ground truth ----------------------------------------

/// Deleting `_manifest.json` used to make every part beneath it UNREACHABLE — the read path never
/// LISTs directories (deliberately, spec must-fix #5), so data sitting on disk became invisible with
/// no way back. `rebuild_series_manifest` reconstructs the index from the parts themselves.
///
/// The assertion is round-trip IDENTITY, not "some rows came back": every recovered `FileEntry`
/// must match what the original manifest recorded — including `commit_keys`, which is why parts now
/// stamp them into their Parquet footer. Anything weaker would let a rebuild silently drop the
/// idempotency log and re-admit already-applied appends.
#[test]
fn manifest_rebuilds_from_the_parts_after_it_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();

    // Three keyed appends across TWO UTC days, so the rebuild has to walk >1 `date=` dir and
    // recover several parts with distinct commit keys.
    let day1 = 1_700_000_000_000i64; // some ts inside one UTC day
    let day2 = day1 + 86_400_000;
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(day1, 100.0, None)], Some("k1")).unwrap();
    store
        .append_bars("binance", "BTCUSDT", "1m", &[bar(day1 + 60_000, 101.0, None)], Some("k2"))
        .unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(day2, 102.0, None)], Some("k3")).unwrap();

    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(before.len(), 3, "3 bars ingested");

    let series_dir = dir
        .path()
        .join("kind=bar")
        .join("venue=binance")
        .join("symbol=BTCUSDT")
        .join("interval=1m");
    let manifest_path = series_dir.join("_manifest.json");
    let original = std::fs::read_to_string(&manifest_path).unwrap();

    // The disaster: the index is gone. Parts are all still on disk.
    std::fs::remove_file(&manifest_path).unwrap();
    let store2 = DataFusionHist::open(dir.path()).unwrap();
    assert!(
        store2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().is_empty(),
        "without the index the parts are invisible — this is the failure being fixed"
    );

    // The recovery.
    let id = SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: Some("1m".into()),
        group: None,
    };
    let report = store2.rebuild_series_manifest(&id).unwrap();
    assert_eq!(report.parts_recovered, 3, "one part per keyed append: {report:?}");
    assert_eq!(report.parts_unreadable, 0, "{report:?}");
    assert_eq!(report.parts_without_keys, 0, "every part stamped its commit key: {report:?}");

    // Rows are back, byte-for-byte.
    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(after.len(), 3);
    for (a, b) in before.iter().zip(after.iter()) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.close.to_bits(), b.close.to_bits(), "f64 bit-identical across a rebuild");
    }

    // And the INDEX is back: same files, same ranges, same commit log. Compared as parsed JSON so
    // key order and the `version` counter (which a rebuild resets by design) don't matter.
    let rebuilt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let orig: serde_json::Value = serde_json::from_str(&original).unwrap();
    assert_eq!(rebuilt["files"], orig["files"], "file index recovered field-for-field");
    let mut a: Vec<String> = serde_json::from_value(rebuilt["commits"].clone()).unwrap();
    let mut b: Vec<String> = serde_json::from_value(orig["commits"].clone()).unwrap();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the idempotency log survives the rebuild — k1/k2/k3");
}

/// The point of recovering the commit log: an append whose key is already durable must STILL be a
/// no-op after a rebuild. Without footer-stamped keys this silently duplicated rows.
#[test]
fn idempotency_survives_a_manifest_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;
    store.append_bars("binance", "ETHUSDT", "1m", &[bar(ts, 50.0, None)], Some("dup")).unwrap();

    let series_dir = dir
        .path()
        .join("kind=bar")
        .join("venue=binance")
        .join("symbol=ETHUSDT")
        .join("interval=1m");
    std::fs::remove_file(series_dir.join("_manifest.json")).unwrap();

    let store2 = DataFusionHist::open(dir.path()).unwrap();
    let id = SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        interval: Some("1m".into()),
        group: None,
    };
    store2.rebuild_series_manifest(&id).unwrap();

    // Re-running the SAME keyed append must write nothing.
    let store3 = DataFusionHist::open(dir.path()).unwrap();
    let written = store3
        .append_bars("binance", "ETHUSDT", "1m", &[bar(ts, 50.0, None)], Some("dup"))
        .unwrap();
    assert_eq!(written, 0, "key 'dup' was recovered from the part footer — this must no-op");
    assert_eq!(
        store3.load_bars("binance", "ETHUSDT", "1m", TsRange::all()).unwrap().len(),
        1,
        "still exactly one bar — a lost commit log would have duplicated it"
    );
}

// ---- a rebuild must never count a merge's INPUTS and its OUTPUT both ---------------------------
//
// Compaction has two crash windows in which the inputs of a merge and the merge's output are BOTH
// sitting in the same `date=` directory. A rebuild reads the directory, so before the fix it indexed
// both and the recovered series carried every merged row TWICE. Measured previously by SIGKILLing a
// store mid-compaction: 34–68% row inflation in 7 of 10 kill trials. Anyone calling the rebuild is
// already recovering from a crash, which is the worst possible moment to silently double their data.
//
// Both windows are reproduced DETERMINISTICALLY here — no kill, no timing:
//   * `..._crashed_before_publish...` uses the crash-injection switch to stop compaction between its
//     unlocked merge and its publish (the long window: the merge is the whole cost of compaction).
//   * `..._crashed_before_the_unlinks...` runs a REAL compaction to completion and then restores the
//     input files, which is exactly the state a crash between the manifest publish and the unlink
//     loop leaves behind.

/// The four same-day parts this section's crash tests compact, plus the series dir they live in.
/// Keyed appends, because commit keys are what the rebuild uses to tell a merge's inputs from an
/// unrelated part — see [`rebuild_after_a_compaction_crashed_before_the_unlinks_does_not_duplicate`].
fn four_fragments_for_one_day(root: &Path, store: &DataFusionHist) -> PathBuf {
    for c in 0..4i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        store.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    bars_series(root)
}

fn bars_id() -> SeriesId {
    SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: Some("1m".into()),
        group: None,
    }
}

/// The only `date=` dir under a series (these tests write one UTC day).
fn only_date_dir(series: &Path) -> PathBuf {
    let mut dates: Vec<PathBuf> = std::fs::read_dir(series)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("date="))
        })
        .collect();
    dates.sort();
    assert_eq!(dates.len(), 1, "these tests write exactly one UTC day: {dates:?}");
    dates.pop().unwrap()
}

/// WINDOW 1 — the merge ran, nothing was published. This is the LONG window: compaction deliberately
/// releases the series lock for the decode/sort/re-encode (holding it starves the live recorder), so
/// a crash lands here with overwhelming probability.
///
/// The 20 rows exist once as four input parts and once inside the merge output. A rebuild that reads
/// the directory sees five files and, before the fix, indexed all five — 40 rows out of a 20-row
/// series.
#[test]
fn rebuild_after_a_compaction_crashed_before_publish_does_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = four_fragments_for_one_day(dir.path(), &store);
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(before.len(), 20);
    assert_eq!(count_parquets(&series), 4);

    // The crash: merge, then die before the publish.
    store.set_stop_after_merge_for_test(true);
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_written, 0, "nothing was published — the report must claim nothing");
    assert_eq!(
        count_parquets(&series),
        5,
        "the crash state: four inputs plus an unpublished merge output"
    );

    // The operator's recovery move. (`_manifest.json` is removed because that is WHY a rebuild is
    // ever run — the index is gone; the parts are all that is left.)
    std::fs::remove_file(series.join("_manifest.json")).unwrap();
    let store2 = DataFusionHist::open(dir.path()).unwrap();
    let report = store2.rebuild_series_manifest(&bars_id()).unwrap();

    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(
        after.len(),
        20,
        "a rebuild over a crashed compaction counted the merge's inputs AND its output: {report:?}"
    );
    assert_bars_bit_eq(&before, &after);

    // ...and by the intended mechanism, not by luck: the four fragments were recovered and the
    // unpublished merge output was recognised as one.
    assert_eq!(report.parts_recovered, 4, "{report:?}");
    assert_eq!(report.parts_unpublished_merge, 1, "{report:?}");
    assert_eq!(report.parts_superseded, 0, "nothing published ⇒ nothing supersedes: {report:?}");

    // THE SAFETY PROPERTY, and the reason the fix cannot be "delete whatever the manifest does not
    // list": a rebuild may run while a compaction is mid-merge in ANOTHER process. Keeping the
    // fragments is what lets that merge's publish still find its inputs — the verify it runs under
    // the lock succeeds, so it publishes instead of abandoning and deleting its own output.
    let rebuilt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(series.join("_manifest.json")).unwrap())
            .unwrap();
    let names: Vec<String> = rebuilt["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    for n in 1..=4 {
        let want = format!("part-{n:05}.parquet");
        assert!(names.contains(&want), "an in-flight merge's input vanished: {names:?}");
    }

    // And the series is still compactable: the leftover unpublished file is inert — never planned,
    // never read — so the retry merges the same four fragments and keeps all 20 rows.
    let store3 = DataFusionHist::open(dir.path()).unwrap();
    let rep2 = store3.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep2.parts_merged, 4, "the retry must see the fragments, not the orphan: {rep2:?}");
    assert_eq!(rep2.rows, 20, "{rep2:?}");
    assert_bars_bit_eq(
        &before,
        &store3.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(),
    );
}

/// WINDOW 2 — the manifest was published and the process died before (or during) the unlink loop
/// that removes the merged fragments. Publish-then-unlink is the right order (the reverse would
/// leave a manifest naming files that are gone), so this window is inherent, not a bug in itself.
///
/// Reproduced by restoring the input files after a REAL compaction: byte-for-byte the state a crash
/// in that window leaves. The output is a `part-c…` at its final name here, so the `_tmp-` rule that
/// answers window 1 cannot help — this is what the commit-key containment rule is for.
#[test]
fn rebuild_after_a_compaction_crashed_before_the_unlinks_does_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = four_fragments_for_one_day(dir.path(), &store);
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(before.len(), 20);

    // Snapshot the fragments, compact for real, then put them back — the unlink loop never ran.
    let date_dir = only_date_dir(&series);
    // OUTSIDE the store root: a stray parquet under the root would be walked by `list_series`.
    let stash_dir = tempfile::tempdir().unwrap();
    let stash = stash_dir.path();
    let mut inputs = Vec::new();
    for e in std::fs::read_dir(&date_dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        std::fs::copy(&p, stash.join(&name)).unwrap();
        inputs.push(name);
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    for name in &inputs {
        std::fs::copy(stash.join(name), date_dir.join(name)).unwrap();
    }
    assert_eq!(count_parquets(&series), 5, "the crash state: the sealed part plus its dead inputs");

    std::fs::remove_file(series.join("_manifest.json")).unwrap();
    let store2 = DataFusionHist::open(dir.path()).unwrap();
    let report = store2.rebuild_series_manifest(&bars_id()).unwrap();

    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(
        after.len(),
        20,
        "a rebuild counted the merged fragments AND the sealed part that replaced them: {report:?}"
    );
    assert_bars_bit_eq(&before, &after);

    // Through the containment rule, not by luck: the sealed part is the one recovered, and each of
    // the four fragments was recognised as already inside it by its commit key.
    assert_eq!(report.parts_recovered, 1, "{report:?}");
    assert_eq!(report.parts_superseded, 4, "{report:?}");
    // The idempotency log survives the supersession — the sealed part carries the union of its
    // inputs' keys, so re-running any of those four appends must still be a no-op.
    let store3 = DataFusionHist::open(dir.path()).unwrap();
    let again =
        store3.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 100.0, None)], Some("b0")).unwrap();
    assert_eq!(again, 0, "key 'b0' rode into the sealed part's footer — this must no-op");
}

/// A final-named orphan sitting exactly where the next merge wants to publish must not WEDGE that
/// date. `fs::rename` replaces on unix but FAILS on Windows when the destination exists, and a
/// destination can exist — a build that wrote the output at its final name (every build before the
/// `_tmp-merge-` rename) left one there on every crash. Without the remove-then-retry fallback that
/// date would abandon on every pass, forever, and the store would never compact again on Windows.
///
/// The orphan is manufactured from a real crashed merge rather than by guessing a filename, so the
/// test cannot rot away from the naming rule it depends on.
#[test]
fn a_final_named_orphan_does_not_wedge_the_next_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = four_fragments_for_one_day(dir.path(), &store);
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    store.set_stop_after_merge_for_test(true);
    store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    // Strip the prefix: what an older build left behind is this same file at its final name.
    let date_dir = only_date_dir(&series);
    let tmp = std::fs::read_dir(&date_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("_tmp-merge-"))
        })
        .expect("the merge output is written under the unpublished prefix");
    let final_name = tmp.file_name().unwrap().to_str().unwrap().trim_start_matches("_tmp-merge-");
    std::fs::rename(&tmp, date_dir.join(final_name)).unwrap();

    // The retry re-merges the same four fragments and must publish over that orphan.
    store.set_stop_after_merge_for_test(false);
    let rep = store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(
        rep.parts_merged, 4,
        "the orphan wedged the date instead of being replaced: {rep:?}"
    );
    assert_eq!(rep.rows, 20, "{rep:?}");
    assert_bars_bit_eq(
        &before,
        &store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(),
    );
}

/// A part with NO commit key is the one shape the containment rule cannot see — the empty set is a
/// subset of everything, so treating it as contained would drop every keyless part in a date that
/// also holds a keyed one. Pinned here as a KNOWN residual so nobody later "tidies" the empty-set
/// guard away: a keyless fragment left by a crash in window 2 is still double-counted.
///
/// It is narrow by construction — the recorder, every backfill collector and the bulk importer all
/// key their appends (that is what makes them idempotent), so this is the shape of an append that
/// deliberately opted out of idempotency — and it is REPORTED rather than silent:
/// `parts_without_keys` counts exactly the parts this pass had to take on trust.
#[test]
fn a_keyless_fragment_is_still_double_counted_and_the_report_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..4i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        store.append_bars("binance", "BTCUSDT", "1m", &batch, None).unwrap(); // NO commit key
    }
    let series = bars_series(dir.path());
    let date_dir = only_date_dir(&series);
    let stash_dir = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for e in std::fs::read_dir(&date_dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        std::fs::copy(&p, stash_dir.path().join(&name)).unwrap();
        inputs.push(name);
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    for name in &inputs {
        std::fs::copy(stash_dir.path().join(name), date_dir.join(name)).unwrap();
    }

    std::fs::remove_file(series.join("_manifest.json")).unwrap();
    let report =
        DataFusionHist::open(dir.path()).unwrap().rebuild_series_manifest(&bars_id()).unwrap();
    assert_eq!(report.parts_superseded, 0, "no keys ⇒ no containment to prove: {report:?}");
    assert_eq!(report.parts_without_keys, 5, "the report names the blind spot: {report:?}");
    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(after.len(), 40, "KNOWN residual: keyless parts still double-count (20 real rows)");
}

// ---- symbol as a row-level column (series-grouping prerequisite) -------------------------------

/// The `symbol` column round-trips, and — the part that matters — a part written WITHOUT it still
/// decodes via the path-derived `ctx`.
///
/// Under today's per-symbol layout the column is redundant: the series path already names the
/// symbol. It exists for the grouped layout, where one part holds many symbols and the path can no
/// longer answer "whose row is this?". Writing it unconditionally now means a store can be regrouped
/// later without rewriting its data.
///
/// The additive `str_add` flavor is what makes the old-part case work — it reads through
/// `opt_str_col`, which returns `Ok(None)` for an ABSENT column rather than erroring. This test
/// pins that, because silently failing to decode old parts would be indistinguishable from an
/// empty store.
#[test]
fn symbol_column_round_trips_and_old_parts_still_decode() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();

    let ts = 1_700_000_000_000i64;
    let q = QuoteTick {
        ts,
        local_ts: ts + 1,
        bid: 0.5,
        ask: 0.51,
        bid_size: 10.0,
        ask_size: 12.0,
        symbol: "BTCUSDT".into(),
    };
    let t = TradeTick {
        ts,
        local_ts: ts + 2,
        price: 0.505,
        size: 3.0,
        is_buyer_maker: true,
        symbol: "BTCUSDT".into(),
    };
    store.append_quotes("binance", "BTCUSDT", std::slice::from_ref(&q), Some("q1")).unwrap();
    store.append_trades("binance", "BTCUSDT", std::slice::from_ref(&t), Some("t1")).unwrap();

    let back_q = store.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap();
    let back_t = store.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(back_q.len(), 1);
    assert_eq!(back_t.len(), 1);
    assert_eq!(back_q[0].symbol, "BTCUSDT", "symbol survives the round trip");
    assert_eq!(back_t[0].symbol, "BTCUSDT");
    // Everything else is untouched — this is an ADDITIVE column, not a re-encode.
    assert_eq!(back_q[0].bid.to_bits(), q.bid.to_bits(), "f64 bit-identical");
    assert_eq!(back_q[0].local_ts, q.local_ts);
    assert_eq!(back_t[0].price.to_bits(), t.price.to_bits());
    assert!(back_t[0].is_buyer_maker);

    // The column is really in the file (not just re-injected from the path).
    let part = std::fs::read_dir(
        dir.path().join("kind=quote").join("venue=binance").join("symbol=BTCUSDT"),
    )
    .unwrap()
    .filter_map(|e| e.ok().map(|e| e.path()))
    .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("date=")))
    .map(|d| {
        std::fs::read_dir(d)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
            .unwrap()
    })
    .unwrap();
    let f = std::fs::File::open(&part).unwrap();
    let b = datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(f)
        .unwrap();
    assert!(
        b.schema().field_with_name("symbol_col").is_ok(),
        "the part carries a real symbol column: {:?}",
        b.schema()
    );
}

/// The empty-symbol contract, pinned. A caller on a single-symbol path may leave `symbol` empty and
/// the store tags it from the series path on read — `QuoteTick::symbol`'s own doc says so
/// ("instrument id — empty for single-symbol paths"), and several call sites rely on it.
///
/// This nearly regressed when the `symbol` column was added: preferring the column unconditionally
/// returned "" for exactly these rows. That is why decode falls back to `ctx` when the column is
/// EMPTY, not only when it is absent.
#[test]
fn empty_symbol_still_gets_tagged_from_the_series_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;

    store
        .append_quotes(
            "binance",
            "ETHUSDT",
            &[QuoteTick {
                ts,
                local_ts: 0,
                bid: 1.0,
                ask: 2.0,
                bid_size: 3.0,
                ask_size: 4.0,
                symbol: String::new(), // deliberately empty
            }],
            None,
        )
        .unwrap();
    store
        .append_trades(
            "binance",
            "ETHUSDT",
            &[TradeTick {
                ts,
                local_ts: 0,
                price: 1.5,
                size: 2.5,
                is_buyer_maker: false,
                symbol: String::new(), // deliberately empty
            }],
            None,
        )
        .unwrap();

    let q = store.scan_quotes("binance", "ETHUSDT", TsRange::all()).unwrap();
    let t = store.scan_trades("binance", "ETHUSDT", TsRange::all()).unwrap();
    assert_eq!(q[0].symbol, "ETHUSDT", "empty in, path-derived symbol out");
    assert_eq!(t[0].symbol, "ETHUSDT", "empty in, path-derived symbol out");
}

// ---- grouped series: many symbols, one commit (storage-study item #5) --------------------------

/// The whole point of grouping, end to end: write three symbols' quotes as ONE commit into one
/// `group=` series, then read each symbol back individually and get only its own rows.
///
/// Reads resolve a symbol across BOTH layouts — its own `symbol=` series first (unchanged), then
/// any `group=` series for the same kind/venue, filtering by the row-level symbol column. That is
/// what lets a store hold both layouts at once and migrate one venue at a time.
#[test]
fn grouped_series_writes_many_symbols_in_one_commit_and_reads_them_back_individually() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;

    let mk = |sym: &str, i: i64| QuoteTick {
        ts: ts + i,
        local_ts: ts + i + 1,
        bid: 0.5 + i as f64 * 0.01,
        ask: 0.6 + i as f64 * 0.01,
        bid_size: 10.0 + i as f64,
        ask_size: 20.0 + i as f64,
        symbol: sym.into(),
    };
    // Interleaved on purpose — a grouped part is ts-ordered, not symbol-ordered.
    let rows = vec![mk("AAA", 0), mk("BBB", 1), mk("AAA", 2), mk("CCC", 3), mk("BBB", 4)];

    let written = store.append_quotes_grouped("polymarket", "btc-5m", &rows, Some("g1")).unwrap();
    assert_eq!(written, 5, "all five rows in ONE commit");

    // Exactly one series directory, not three.
    let venue_dir = dir.path().join("kind=quote").join("venue=polymarket");
    let series: Vec<String> = std::fs::read_dir(&venue_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(series, vec!["group=btc-5m".to_string()], "one grouped series, not one per symbol");

    // Each symbol reads back its OWN rows only.
    let a = store.scan_quotes("polymarket", "AAA", TsRange::all()).unwrap();
    let b = store.scan_quotes("polymarket", "BBB", TsRange::all()).unwrap();
    let c = store.scan_quotes("polymarket", "CCC", TsRange::all()).unwrap();
    assert_eq!(a.len(), 2, "AAA: {a:?}");
    assert_eq!(b.len(), 2, "BBB: {b:?}");
    assert_eq!(c.len(), 1, "CCC: {c:?}");
    assert!(a.iter().all(|r| r.symbol == "AAA"));
    assert!(b.iter().all(|r| r.symbol == "BBB"));
    assert_eq!(a[0].ts, ts, "ts order preserved within a symbol");
    assert_eq!(a[1].ts, ts + 2);
    assert_eq!(a[1].bid.to_bits(), rows[2].bid.to_bits(), "f64 bit-identical through grouping");

    // A symbol that is not in the group reads empty, not an error.
    assert!(store.scan_quotes("polymarket", "ZZZ", TsRange::all()).unwrap().is_empty());

    // ts-range filtering still applies inside a grouped series.
    let ranged = store.scan_quotes("polymarket", "AAA", TsRange::of(ts + 1, ts + 10)).unwrap();
    assert_eq!(ranged.len(), 1, "only AAA's ts+2 row: {ranged:?}");
}

/// Both layouts can coexist for one venue, and a read merges them. This is what makes migration
/// incremental rather than a flag day.
#[test]
fn a_symbol_reads_across_both_per_symbol_and_grouped_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;
    let mk = |sym: &str, i: i64| QuoteTick {
        ts: ts + i,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: sym.into(),
    };

    // Old rows in the per-symbol layout; new rows in the grouped one.
    store.append_quotes("polymarket", "AAA", &[mk("AAA", 0)], Some("old")).unwrap();
    store
        .append_quotes_grouped("polymarket", "btc-5m", &[mk("AAA", 5), mk("BBB", 6)], Some("new"))
        .unwrap();

    let a = store.scan_quotes("polymarket", "AAA", TsRange::all()).unwrap();
    assert_eq!(a.len(), 2, "both layouts merged for AAA: {a:?}");
    assert_eq!(a[0].ts, ts, "merged result is ts-ordered across layouts");
    assert_eq!(a[1].ts, ts + 5);
    assert_eq!(store.scan_quotes("polymarket", "BBB", TsRange::all()).unwrap().len(), 1);
}

/// A grouped series tells rows apart by their symbol column, so an empty symbol is unattributable.
/// Rejected at write rather than silently written and then invisible to every scan.
#[test]
fn grouped_append_rejects_an_empty_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bad = QuoteTick {
        ts: 1_700_000_000_000,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: String::new(),
    };
    let err = store
        .append_quotes_grouped("polymarket", "btc-5m", &[bad], Some("k"))
        .expect_err("an empty symbol must not reach a grouped series");
    assert!(format!("{err}").contains("empty symbol"), "{err}");
}

/// `list_series` must SEE grouped series, and maintenance must reach them.
///
/// Before this, `parse_series_id` returned `None` for a `group=` segment ("unexpected segment"), so
/// a grouped series was invisible to `list_series` — and therefore never compacted and never
/// retention-pruned by `run_maintenance`. Latent rather than live (nothing wrote grouped series
/// yet), and exactly the kind of silent omission that surfaces months later as unbounded disk
/// growth on the one venue that adopted the new layout.
#[test]
fn maintenance_sees_and_compacts_a_grouped_series() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;
    let mk = |sym: &str, i: i64| QuoteTick {
        ts: ts + i,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: sym.into(),
    };

    // Several commits into ONE grouped series → several parts in one date partition, which is what
    // compaction merges.
    for c in 0..4i64 {
        store
            .append_quotes_grouped(
                "polymarket",
                "btc-5m",
                &[mk("AAA", c * 2), mk("BBB", c * 2 + 1)],
                Some(&format!("k{c}")),
            )
            .unwrap();
    }

    // The series is VISIBLE, and identified as a group rather than as a symbol.
    let listed = store.list_series().unwrap();
    let grouped: Vec<_> = listed.iter().filter(|s| s.group.is_some()).collect();
    assert_eq!(grouped.len(), 1, "the grouped series must be listed: {listed:?}");
    assert_eq!(grouped[0].group.as_deref(), Some("btc-5m"));
    assert_eq!(grouped[0].symbol, "", "a grouped id carries no symbol — the group is the name");
    assert_eq!(grouped[0].label(), "btc-5m");

    // Maintenance REACHES it: 4 parts merge to 1, and the rows survive.
    let before = store.scan_quotes("polymarket", "AAA", TsRange::all()).unwrap();
    assert_eq!(before.len(), 4);
    let report = store
        .run_maintenance(&MaintenanceConfig {
            compaction: CompactionConfig { min_parts: 2, ..Default::default() },
            retention: None,
        })
        .unwrap();
    assert!(
        report.compaction.parts_merged > 0,
        "maintenance must compact the grouped series, not skip it: {report:?}"
    );

    // Rows unchanged through a grouped compaction, per symbol.
    let after = DataFusionHist::open(dir.path()).unwrap();
    let a = after.scan_quotes("polymarket", "AAA", TsRange::all()).unwrap();
    let b = after.scan_quotes("polymarket", "BBB", TsRange::all()).unwrap();
    assert_eq!(a.len(), 4, "AAA survives compaction: {a:?}");
    assert_eq!(b.len(), 4, "BBB survives compaction: {b:?}");
    assert!(a.iter().all(|r| r.symbol == "AAA"), "symbols preserved through the merge");
    assert_eq!(a[0].bid.to_bits(), before[0].bid.to_bits(), "f64 bit-identical through compaction");
}

/// Grouped BOOK — the layout that actually matters by volume.
///
/// A Polymarket token's market is ~352,935 book rows against ~17,364 quotes and ~1,087 trades, so
/// grouping quotes and trades alone would have left ~95% of the data committing per symbol. This is
/// also the trickiest codec to group: book explodes to one row per LEVEL, and the read regroups
/// consecutive rows sharing `(seq, kind)` back into one event — so in a grouped part, where two
/// symbols can share a seq, the regroup has to split on symbol or it will merge two tokens' levels
/// into a single event carrying the wrong one.
#[test]
fn grouped_book_writes_many_symbols_in_one_commit_and_regroups_per_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;

    let upd = |sym: &str, ts: i64, seq: u64| BookUpdate {
        ts,
        local_ts: ts + 1,
        seq,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        // TWO levels per side, so a wrong split shows up as extra events rather than only bad symbols
        bids: vec![(0.45, 10.0), (0.44, 20.0)],
        asks: vec![(0.46, 5.0), (0.47, 7.0)],
        symbol: sym.into(),
    };

    // AAA and BBB SHARE seq 1 — exactly the collision the regroup must not merge.
    let updates = vec![upd("AAA", ts, 1), upd("BBB", ts, 1), upd("AAA", ts + 10, 2)];
    let written =
        store.append_book_updates_grouped("polymarket", "btc-5m", &updates, Some("g1")).unwrap();
    assert_eq!(written, 12, "3 events x 4 levels, all in ONE commit");

    // One series directory, not one per symbol.
    let series: Vec<String> =
        std::fs::read_dir(dir.path().join("kind=book").join("venue=polymarket"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    assert_eq!(series, vec!["group=btc-5m".to_string()]);

    let a = store.scan_book_updates("polymarket", "AAA", TsRange::all()).unwrap();
    let b = store.scan_book_updates("polymarket", "BBB", TsRange::all()).unwrap();
    assert_eq!(a.len(), 2, "AAA's two events, not merged with BBB's: {a:?}");
    assert_eq!(b.len(), 1, "BBB's one event: {b:?}");
    assert!(a.iter().all(|u| u.symbol == "AAA"));
    assert_eq!(b[0].symbol, "BBB");

    // Levels intact and bit-identical through the group.
    assert_eq!(a[0].bids.len(), 2, "levels not split across events: {:?}", a[0]);
    assert_eq!(a[0].asks.len(), 2);
    assert_eq!(a[0].bids[0].0.to_bits(), 0.45f64.to_bits());
    assert_eq!(a[0].asks[1].1.to_bits(), 7.0f64.to_bits());
    assert_eq!(a[0].seq, 1);
    assert_eq!(a[1].seq, 2);
    // BBB shares seq 1 with AAA and must still be its own event with its own levels.
    assert_eq!(b[0].seq, 1);
    assert_eq!(b[0].bids.len(), 2);

    assert!(store.scan_book_updates("polymarket", "ZZZ", TsRange::all()).unwrap().is_empty());
}

/// An empty symbol can't be attributed inside a grouped book series — rejected at write.
#[test]
fn grouped_book_append_rejects_an_empty_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bad = BookUpdate {
        ts: 1_700_000_000_000,
        local_ts: 0,
        seq: 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![(0.5, 1.0)],
        asks: vec![(0.51, 1.0)],
        symbol: String::new(),
    };
    let err = store
        .append_book_updates_grouped("polymarket", "btc-5m", &[bad], Some("k"))
        .expect_err("an empty symbol must not reach a grouped book series");
    assert!(format!("{err}").contains("empty symbol"), "{err}");
}

/// A grouped bulk session collapses N per-symbol commits into ONE per group per window.
///
/// This is the point of the whole grouping exercise, and staging alone never delivered it: a window
/// holding N symbols still issued N `commit_rows_bulk` calls — one per series — each paying the
/// ~30-39 ms fixed floor. Asserted structurally (series directories and part files on disk) rather
/// than by timing, so it cannot pass on a fast machine for the wrong reason.
#[test]
fn grouped_bulk_session_commits_once_per_group_not_once_per_symbol() {
    use std::sync::Arc;
    let ts = 1_700_000_000_000i64;
    let syms = ["AAA", "BBB", "CCC", "DDD"];
    let mk = |sym: &str, i: i64| QuoteTick {
        ts: ts + i,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: sym.into(),
    };

    // --- ungrouped: one series per symbol (today) ---
    let d1 = tempfile::tempdir().unwrap();
    let s1 = DataFusionHist::open(d1.path()).unwrap();
    {
        let mut sess = s1.bulk_session(BulkConfig::default());
        for (i, sym) in syms.iter().enumerate() {
            sess.stage_quotes("bench", sym, &[mk(sym, i as i64)]);
        }
        sess.flush("k").unwrap();
    }
    let ungrouped: Vec<String> =
        std::fs::read_dir(d1.path().join("kind=quote").join("venue=bench"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    assert_eq!(ungrouped.len(), 4, "one series per symbol: {ungrouped:?}");

    // --- grouped: all four into one series, ONE commit ---
    let d2 = tempfile::tempdir().unwrap();
    let s2 = DataFusionHist::open(d2.path()).unwrap();
    {
        let group: vike_data::GroupResolver =
            Arc::new(|_v: &str, _s: &str| Some("fam".to_string()));
        let mut sess = s2.bulk_session_grouped(BulkConfig::default(), group);
        for (i, sym) in syms.iter().enumerate() {
            sess.stage_quotes("bench", sym, &[mk(sym, i as i64)]);
        }
        sess.flush("k").unwrap();
    }
    let grouped: Vec<String> = std::fs::read_dir(d2.path().join("kind=quote").join("venue=bench"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(grouped, vec!["group=fam".to_string()], "ONE series: {grouped:?}");

    // ONE part file — i.e. one commit, not four.
    let parts: usize = walk_parquet(&d2.path().join("kind=quote"));
    assert_eq!(parts, 1, "one commit produced one part; four commits would produce four");

    // And every symbol still reads back individually, with the same rows as the ungrouped store.
    for (i, sym) in syms.iter().enumerate() {
        let a = s1.scan_quotes("bench", sym, TsRange::all()).unwrap();
        let b = s2.scan_quotes("bench", sym, TsRange::all()).unwrap();
        assert_eq!(a.len(), 1, "{sym} ungrouped");
        assert_eq!(b.len(), 1, "{sym} grouped");
        assert_eq!(a[0].ts, b[0].ts, "{sym} same row either way");
        assert_eq!(b[0].ts, ts + i as i64);
        assert_eq!(b[0].symbol, *sym);
    }
}

/// A resolver returning `None` keeps that series per-symbol — so one venue, or one family, can
/// migrate at a time instead of as a flag day.
#[test]
fn a_none_from_the_group_resolver_keeps_a_series_per_symbol() {
    use std::sync::Arc;
    let ts = 1_700_000_000_000i64;
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mk = |sym: &str| QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: sym.into(),
    };
    {
        // GROUPED only accepts AAA/BBB; CCC stays on its own.
        let group: vike_data::GroupResolver =
            Arc::new(|_v: &str, s: &str| (s != "CCC").then(|| "fam".to_string()));
        let mut sess = store.bulk_session_grouped(BulkConfig::default(), group);
        for sym in ["AAA", "BBB", "CCC"] {
            sess.stage_quotes("bench", sym, &[mk(sym)]);
        }
        sess.flush("k").unwrap();
    }
    let mut dirs: Vec<String> =
        std::fs::read_dir(dir.path().join("kind=quote").join("venue=bench"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    dirs.sort();
    assert_eq!(dirs, vec!["group=fam".to_string(), "symbol=CCC".to_string()], "{dirs:?}");
    for sym in ["AAA", "BBB", "CCC"] {
        assert_eq!(store.scan_quotes("bench", sym, TsRange::all()).unwrap().len(), 1, "{sym}");
    }
}

/// Count `.parquet` files under a tree — a commit produces one part per UTC day, so with a
/// single-day fixture this is the commit count.
fn walk_parquet(p: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                walk_parquet(&path)
            } else {
                usize::from(path.extension().and_then(|x| x.to_str()) == Some("parquet"))
            }
        })
        .sum()
}

/// The cross-kind coverage report over a REAL store, not just the pure fold.
///
/// Pins the case the report exists for: a Polymarket-shaped venue backfill leaves a day with a
/// complete TRADE tape and no book, because no book history exists to fetch. Per series both
/// manifests look unremarkable — trades are contiguous, and the book series simply has no rows
/// there. Joined, that day is a `PartialDay` naming what is missing, which is what stops a
/// market-making backtest from silently running over a window with no book at all.
#[test]
fn coverage_report_lines_kinds_up_and_names_a_trades_only_day() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();

    let day = 86_400_000i64;
    let q = |ts: i64| QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: String::new(),
    };
    let t = |ts: i64| TradeTick {
        ts,
        local_ts: 0,
        price: 1.5,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    };

    // trades: days 1,2,3 — the "a venue backfill filled the middle day" shape.
    for d in 1..=3 {
        store.append_trades("polymarket", "TOK", &[t(d * day)], Some(&format!("t{d}"))).unwrap();
    }
    // quotes: days 1 and 3 only — day 2 was never recorded and cannot be fetched from the venue.
    for d in [1i64, 3] {
        store.append_quotes("polymarket", "TOK", &[q(d * day)], Some(&format!("q{d}"))).unwrap();
    }

    let report = store.coverage_report().unwrap();
    let tok = report.iter().find(|c| c.key.label == "TOK").expect("instrument present");

    assert!(!tok.is_complete());
    let partial = tok.partial_days();
    // ONE partial day: day 2, where trades flowed and quotes did not. `book` was never written at
    // all, and an entirely-absent kind is NOT a partial day — that is `KindDays::absent`, a
    // different fact — or every day here would be flagged for a lane nobody recorded.
    assert_eq!(partial.len(), 1, "only the trades-only day, got {partial:?}");
    assert_eq!(partial[0].day, 2);
    assert_eq!(partial[0].missing_kinds, vec!["quote".to_string()], "{partial:?}");
    assert!(tok.kinds["book"].absent(), "book is absent, and therefore not 'missing'");
    assert_eq!(tok.recorded_kinds(), vec!["trade", "quote"]);

    // The trade lane itself is contiguous — per-series gap detection sees nothing wrong, which is
    // exactly why the cross-kind JOIN is what surfaces this.
    let trade_id = vike_data::SeriesId::per_symbol("trade", "polymarket", "TOK", None);
    assert!(store.series_gaps(&trade_id).unwrap().is_empty());
    // ...and the quote lane's own gap IS reported, per kind, as before.
    let quote_id = vike_data::SeriesId::per_symbol("quote", "polymarket", "TOK", None);
    assert_eq!(store.series_gaps(&quote_id).unwrap().len(), 1);
}

/// A kind that was never recorded is an EMPTY ROW, not an absent map entry — a renderer must not be
/// able to confuse "this instrument has no book" with "I did not look for book".
#[test]
fn coverage_report_always_carries_every_tick_kind() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store
        .append_trades(
            "binance",
            "BTCUSDT",
            &[TradeTick {
                ts: 1_700_000_000_000,
                local_ts: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: String::new(),
            }],
            Some("k"),
        )
        .unwrap();

    let report = store.coverage_report().unwrap();
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].kinds.len(), vike_data::coverage::TICK_KINDS.len());
    assert!(report[0].kinds["book"].absent());
    assert!(report[0].kinds["quote"].absent());
    assert!(!report[0].kinds["trade"].absent());
}

/// The LIVE recorder collapses same-group buffers into ONE commit.
///
/// `RecorderSink` buffers per `(venue, symbol)`, so routing those at a shared group directory
/// WITHOUT merging would be strictly worse than not grouping: every buffer would still be its own
/// commit, now all contending on ONE `SeriesLock` instead of N independent ones. Asserted by part
/// files on disk rather than by timing, so it cannot pass on a fast machine for the wrong reason.
#[test]
fn recorder_flushes_same_group_buffers_as_one_commit() {
    use std::sync::Arc;
    use vike_data::{LiveDataSink, RecorderConfig, RecorderSink};

    let ts = 1_700_000_000_000i64;
    let syms = ["AAA", "BBB", "CCC"];
    // Rows deliberately carry an EMPTY symbol — the per-symbol contract — so this also proves the
    // recorder stamps the buffer's symbol on before a grouped append, which rejects blank rows.
    let q = |i: i64| QuoteTick {
        ts: ts + i,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: String::new(),
    };

    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let cfg = RecorderConfig {
            grouping: Some(Arc::new(|_v: &str, _s: &str| Some("fam".to_string()))),
            ..RecorderConfig::default()
        };
        let (sink, handle) = RecorderSink::spawn(store, cfg).unwrap();
        for (i, s) in syms.iter().enumerate() {
            sink.quote("polymarket", s, q(i as i64));
        }
        handle.shutdown(); // flushes everything
    }

    // ONE series directory, not three.
    let series: Vec<String> =
        std::fs::read_dir(dir.path().join("kind=quote").join("venue=polymarket"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    assert_eq!(series, vec!["group=fam".to_string()], "{series:?}");

    // ONE part file — three per-symbol commits would have produced three.
    assert_eq!(walk_parquet(&dir.path().join("kind=quote")), 1, "one commit, not three");

    // Every symbol still reads back individually, tagged from the stamp rather than the path.
    let store = DataFusionHist::open(dir.path()).unwrap();
    for (i, s) in syms.iter().enumerate() {
        let got = store.scan_quotes("polymarket", s, TsRange::all()).unwrap();
        assert_eq!(got.len(), 1, "{s}: {got:?}");
        assert_eq!(got[0].symbol, *s, "stamped before the grouped append");
        assert_eq!(got[0].ts, ts + i as i64);
    }
}

/// A buffer that fills to `max_rows` must ALSO write grouped.
///
/// The regression this pins, found by the first live recorder run (2026-08-02): `ingest`'s two
/// IMMEDIATE flushes — `max_rows`-full and UTC-date rollover — called the per-symbol append
/// directly and never consulted the resolver, while the age-based sweep and shutdown went through
/// the group-aware path. So a family's HIGH-VOLUME symbols silently wrote `symbol=<id>/` series
/// while its quiet ones wrote `group=<family>/`, in one process, for a whole session — exactly
/// backwards, since the busy symbol is the one grouping exists for. A customer would then have to
/// read two layouts to see one family, and maintenance/retention would treat them as unrelated
/// series.
///
/// The existing grouped tests all missed it because they push a handful of rows under the default
/// `max_rows` of 5,000, so every flush was a shutdown flush.
#[test]
fn a_max_rows_flush_is_grouped_too_not_only_the_aged_and_shutdown_ones() {
    use std::sync::Arc;
    use vike_data::{LiveDataSink, RecorderConfig, RecorderSink};

    let ts = 1_700_000_000_000i64;
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let cfg = RecorderConfig {
            // Small enough that the rows below trip the max-rows path TWICE before shutdown.
            max_rows: 3,
            // Long enough that the age sweep — which was already group-aware — cannot fire and mask
            // the bug by flushing these buffers itself.
            max_age: Duration::from_secs(3_600),
            grouping: Some(Arc::new(|_v: &str, _s: &str| Some("fam".to_string()))),
            ..RecorderConfig::default()
        };
        let (sink, handle) = RecorderSink::spawn(store, cfg).unwrap();
        for i in 0..6i64 {
            sink.quote(
                "polymarket",
                "BUSY",
                QuoteTick {
                    ts: ts + i,
                    local_ts: 0,
                    bid: 1.0,
                    ask: 2.0,
                    bid_size: 3.0,
                    ask_size: 4.0,
                    symbol: String::new(),
                },
            );
        }
        handle.shutdown();
    }

    // The whole point: NO `symbol=` series exists — every flush went to the group.
    let series: Vec<String> =
        std::fs::read_dir(dir.path().join("kind=quote").join("venue=polymarket"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    assert_eq!(
        series,
        vec!["group=fam".to_string()],
        "a max-rows flush escaped to a per-symbol series: {series:?}"
    );

    // And nothing was lost or double-counted on the way.
    let store = DataFusionHist::open(dir.path()).unwrap();
    let got = store.scan_quotes("polymarket", "BUSY", TsRange::all()).unwrap();
    assert_eq!(got.len(), 6, "{got:?}");
    assert!(got.iter().all(|q| q.symbol == "BUSY"), "stamped before every grouped append");
}

/// No resolver ⇒ byte-identical to before: one series per symbol.
#[test]
fn recorder_without_grouping_still_writes_per_symbol_series() {
    use std::sync::Arc;
    use vike_data::{LiveDataSink, RecorderConfig, RecorderSink};

    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let (sink, handle) = RecorderSink::spawn(store, RecorderConfig::default()).unwrap();
        for s in ["AAA", "BBB"] {
            sink.quote(
                "polymarket",
                s,
                QuoteTick {
                    ts: 1_700_000_000_000,
                    local_ts: 0,
                    bid: 1.0,
                    ask: 2.0,
                    bid_size: 3.0,
                    ask_size: 4.0,
                    symbol: String::new(),
                },
            );
        }
        handle.shutdown();
    }
    let mut series: Vec<String> =
        std::fs::read_dir(dir.path().join("kind=quote").join("venue=polymarket"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    series.sort();
    assert_eq!(series, vec!["symbol=AAA".to_string(), "symbol=BBB".to_string()], "{series:?}");
}

// ---- the STORE's persisted source policy: automatic supersession, and the guard on it ----------
//
// `compact_series_superseding` resolved duplicates but had to be called by hand with a policy the
// store did not remember. Persisting the rule in `_sources.json` lets `run_maintenance` apply it —
// which means a row-DROPPING pass now runs on a timer, so these tests are mostly about the guards.

/// Two sources' duplicate rows, and a two-writer store: `run_maintenance` resolves them once the
/// operator has written a policy that ranks BOTH writers.
#[test]
fn run_maintenance_supersedes_once_the_store_has_a_policy() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades(
        "polymarket",
        "TOK",
        &[tt(500, 0.4, 5.0), tt(1_000, 0.5, 10.0)],
        Some("live-a"),
    )
    .unwrap();
    df.append_trades(
        "polymarket",
        "TOK",
        &[tt(1_000, 0.5, 99.0), tt(2_000, 0.6, 7.0)],
        Some("pmxt:b"),
    )
    .unwrap();

    vike_data::save_policy(dir.path(), &vike_data::StoreSourcePolicy::new(["live-", "pmxt:"]))
        .unwrap();
    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() },
        retention: None,
    };
    let rep = df.run_maintenance(&cfg).unwrap();

    assert_eq!(rep.compaction.rows_superseded, 1, "the ts=1000 collision resolved");
    let got = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3);
    // `live-` outranks `pmxt:`, so the LIVE row survived the collision.
    assert_eq!(got.iter().find(|t| t.ts == 1_000).unwrap().size, 10.0);
}

/// **Without a policy, nothing changes.** The default for every existing store: duplicates are kept,
/// byte-identical to before this feature existed.
#[test]
fn run_maintenance_without_a_policy_keeps_every_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 10.0)], Some("live-a")).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 99.0)], Some("pmxt:b")).unwrap();

    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() },
        retention: None,
    };
    let rep = df.run_maintenance(&cfg).unwrap();

    assert_eq!(rep.compaction.rows_superseded, 0);
    assert_eq!(df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap().len(), 2);
}

/// **THE GUARD.** A writer the policy never ranked would be superseded away as lowest-precedence —
/// silently, irreversibly, on a background timer. A strict policy must SKIP that series instead.
#[test]
fn a_writer_the_policy_does_not_rank_is_not_superseded_away() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 10.0)], Some("live-a")).unwrap();
    // A source the operator forgot to list — e.g. a vendor import added later.
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 99.0)], Some("mystery:b")).unwrap();

    // The policy ranks only `live-`.
    vike_data::save_policy(dir.path(), &vike_data::StoreSourcePolicy::new(["live-"])).unwrap();
    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() },
        retention: None,
    };
    let rep = df.run_maintenance(&cfg).unwrap();

    assert_eq!(rep.compaction.rows_superseded, 0, "skipped, not superseded");
    let got = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2, "the unranked writer's row survived");
    assert!(got.iter().any(|t| t.size == 99.0), "specifically ITS row, not just any two");
}

/// ...and `strict = false` is the documented opt-out: the operator has accepted that an unranked
/// source loses. Same store, same data, opposite outcome — which is what makes the guard load-bearing
/// rather than decorative.
#[test]
fn permissive_mode_does_supersede_an_unranked_writer() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 10.0)], Some("live-a")).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 99.0)], Some("mystery:b")).unwrap();

    vike_data::save_policy(dir.path(), &vike_data::StoreSourcePolicy::new(["live-"]).permissive())
        .unwrap();
    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() },
        retention: None,
    };
    let rep = df.run_maintenance(&cfg).unwrap();

    assert_eq!(rep.compaction.rows_superseded, 1);
    let got = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].size, 10.0, "`live-` won");
}

/// An EMPTY prefix list would supersede nothing anyway — it must not cost a rewrite pretending to.
#[test]
fn an_inert_policy_behaves_exactly_like_no_policy() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 10.0)], Some("live-a")).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1_000, 0.5, 99.0)], Some("pmxt:b")).unwrap();

    vike_data::save_policy(dir.path(), &vike_data::StoreSourcePolicy::new(Vec::<String>::new()))
        .unwrap();
    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() },
        retention: None,
    };
    let rep = df.run_maintenance(&cfg).unwrap();

    assert_eq!(rep.compaction.rows_superseded, 0);
    assert_eq!(df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap().len(), 2);
}

// ---- the CONFLATING depth lane (kind=depth) ----------------------------------------------------
//
// Binance/bybit/okx declare `book: false, depth: true` and refuse `subscribe_book`, so for those
// venues `l2_snapshot` IS the L2. It used to hit the trait's default no-op and vanish.

/// A depth snapshot round-trips through its OWN series and does not touch `kind=book`.
///
/// The separation is the feature: a consumer asking for `book` must never silently receive
/// conflated data, because the book lane promises a losslessness depth cannot honour.
#[test]
fn depth_lands_in_its_own_series_and_never_in_the_book_lane() {
    use std::sync::Arc;
    use vike_data::{LiveDataSink, RecorderConfig, RecorderSink};

    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let (sink, handle) = RecorderSink::spawn(store, RecorderConfig::default()).unwrap();
        sink.l2_snapshot("binance", "BTCUSDT", 0.01, vec![(100.0, 2.0)], vec![(101.0, 3.0)], 1_000);
        sink.l2_snapshot("binance", "BTCUSDT", 0.01, vec![(100.5, 1.0)], vec![(101.5, 4.0)], 1_100);
        handle.shutdown();
    }

    let store = DataFusionHist::open(dir.path()).unwrap();
    let got = store.scan_depth("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].ts, 1_000);
    assert_eq!(got[0].bids, vec![(100.0, 2.0)]);
    assert_eq!(got[0].asks, vec![(101.0, 3.0)]);
    assert_eq!(got[1].ts, 1_100);

    // Every row is a full-state anchor — which is what a conflated depth frame IS.
    assert!(got.iter().all(|u| u.kind == vike_model::BookUpdateKind::Snapshot));
    // Seqs are monotonic-DISTINCT, not the venue's update id and not a constant. A constant folds
    // every snapshot into one event: `BookCodec` treats `(seq, kind)` as frame identity.
    assert!(got[0].seq < got[1].seq, "distinct and ordered: {:?}", (got[0].seq, got[1].seq));

    // THE POINT: the book lane is untouched.
    assert!(store.scan_book_updates("binance", "BTCUSDT", TsRange::all()).unwrap().is_empty());
    assert!(!dir.path().join("kind=book").exists(), "no book series was created at all");
    assert!(dir.path().join("kind=depth").exists());
}

/// A snapshot folds into an `L2Book` exactly as the book lane's own anchors do — no delta is lost,
/// because a conflated feed has none to lose. What it cannot support is queue modelling, which is a
/// property of the DATA, not of the fold.
#[test]
fn a_recorded_depth_snapshot_folds_into_a_book() {
    use std::sync::Arc;
    use vike_data::{LiveDataSink, RecorderConfig, RecorderSink};
    use vike_model::L2Book;

    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let (sink, handle) = RecorderSink::spawn(store, RecorderConfig::default()).unwrap();
        sink.l2_snapshot(
            "okx",
            "BTC-USDT",
            0.1,
            vec![(100.0, 2.0), (99.0, 5.0)],
            vec![(101.0, 3.0)],
            7,
        );
        handle.shutdown();
    }

    let store = DataFusionHist::open(dir.path()).unwrap();
    let got = store.scan_depth("okx", "BTC-USDT", TsRange::all()).unwrap();
    let mut book = L2Book::new(0.1);
    book.apply_snapshot(got[0].seq, &got[0].bids, &got[0].asks);
    assert_eq!(book.best_bid().map(|l| l.0), Some(100.0));
    assert_eq!(book.best_ask().map(|l| l.0), Some(101.0));
}

/// **"I serve no depth" and "I hold no depth" are DIFFERENT answers**, and the point of this test
/// is that one call site can now tell them apart.
///
/// Both halves of the seam default to refusing, so a store with no depth lane can neither swallow a
/// recorder's writes and report success nor hand a reader a fabricated empty. The real backend,
/// asked for a series it genuinely does not hold, still answers with an honest empty `Ok` — it read
/// its own lane and found nothing. Before the read half was fixed both stores returned the same
/// `Ok(vec![])` and the assertion below could not be written at all.
///
/// Gated: `MemHistStore` lives behind `test-support`, and the `hist` CI lane runs
/// `--features hist-datafusion` WITHOUT it — the roster lane, which unifies both features into
/// vike-data, is where this runs. The always-compiled twin is
/// `crates/vike-data/tests/store_kind_gate.rs`'s `both_depth_defaults_refuse`.
#[cfg(feature = "test-support")]
#[test]
fn a_store_without_a_depth_lane_refuses_both_halves_while_a_real_store_reads_empty() {
    use vike_data::{HistStore, MemHistStore};

    let m = MemHistStore::default();
    assert!(m.append_depth("binance", "BTCUSDT", &[], Some("k")).is_err());
    let refused = m.scan_depth("binance", "BTCUSDT", TsRange::all()).unwrap_err();
    assert!(
        refused.to_string().contains("scan_depth"),
        "the refusal names the verb that could not be served: {refused}"
    );

    // ...and the store that DOES serve the lane answers the same question with a real empty read.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    assert!(store.scan_depth("binance", "BTCUSDT", TsRange::all()).unwrap().is_empty());
}

/// **A seeded `MemHistStore` lists the series it actually holds** — the catalog twin of the depth
/// test above, proving the two facts the enumeration defaults used to conflate now come from real
/// folds on both stores that serve the verbs.
///
/// Under the trait's OLD inherited default this double answered `Ok(vec![])` for `list_series`
/// WHILE HOLDING a seeded properties series — the fabrication itself, and the red state this test
/// was run against before the fix. The two controls pin the other half of the contract: a store
/// that ANSWERED must still read as empty, never as a failure — a fresh `MemHistStore` folds its
/// real (empty) catalog, and a real `DataFusionHist` over an empty root walks its real (empty)
/// manifest tree.
///
/// Gated like its depth twin: `MemHistStore` lives behind `test-support`, and the roster lane —
/// which unifies both features into vike-data — is where this runs. The always-compiled twin is
/// `crates/vike-data/tests/store_kind_gate.rs`'s `both_catalog_defaults_refuse`.
#[cfg(feature = "test-support")]
#[test]
fn a_seeded_mem_store_lists_its_catalog_while_an_empty_store_still_reads_empty() {
    use vike_data::{HistStore, MemHistStore, SeriesId};
    use vike_model::SymbolProperties;

    // control: an empty double ANSWERS (a real fold over genuinely nothing), never refuses.
    let m = MemHistStore::default();
    assert!(m.list_series().expect("an empty mem store still answers").is_empty());
    assert!(m.inventory().expect("an empty mem store still answers").is_empty());

    // seeded: the catalog is what the double holds, with the fold's true coverage.
    let rows = [(1_000, SymbolProperties::default()), (2_000, SymbolProperties::default())];
    m.append_symbol_properties("binance", "BTCUSDT", &rows, Some("k")).unwrap();
    let series = m.list_series().unwrap();
    assert_eq!(
        series,
        vec![SeriesId::per_symbol("properties", "binance", "BTCUSDT", None)],
        "a seeded double lists the series it holds"
    );
    let inv = m.inventory().unwrap();
    assert_eq!(inv.len(), 1, "one held series, one coverage row");
    assert_eq!(inv[0].0, series[0], "inventory and listing name the same series");
    assert_eq!(inv[0].1.rows, 2, "rows is the held row count");
    assert_eq!(inv[0].1.first_ts, 1_000, "first_ts is the earliest held ts");
    assert_eq!(inv[0].1.last_ts, 2_000, "last_ts is the latest held ts");
    assert_eq!(inv[0].1.bytes, 0, "an in-memory store truly occupies zero bytes on disk");

    // control: the real backend over an empty root answers the same questions with real empty reads.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    assert!(store.list_series().unwrap().is_empty());
    assert!(store.inventory().unwrap().is_empty());
}

/// **An append lands DURING a compaction merge, and neither loses.**
///
/// This is the property the plan/merge/publish split exists to create. Compaction used to hold the
/// series lock across the whole decode→re-encode, so on a large series a concurrent `RecorderSink`
/// flush timed out and DISCARDED its buffer — 110,354 rows lost on the CI box in about an hour before
/// this was found. Now the lock is held only to plan and to publish.
///
/// Two things are asserted, and the ROW COUNTS are only the second of them.
///
/// The counts guard the LOST-UPDATE hazard the unlocked window introduces: the merge and the append
/// each rewrite the manifest, so an appended row could be dropped by a publish that overwrote it.
/// What the counts cannot guard is the hazard this split exists for, because they are satisfied by
/// the pre-fix code too. With the merge inside the lock the appender does not LOSE its rows — it
/// blocks on `SeriesLock::acquire` and then lands, and this series' merge — eight one-row parquet
/// parts — finishes three orders of magnitude inside the 2000 × 2 ms spin that
/// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `SPIN_ATTEMPTS` budgets, so the timeout
/// that IS the production failure can never be reached here. The proof is in this file: the
/// pre-split `concurrent_append_and_compact_no_lost_update` asserts the same shape and passed
/// against the OLD code. (An earlier version of this doc claimed the opposite — that row counts
/// "cannot pass on a fast machine for the wrong reason". The weakness was never that the overlap
/// is UNLIKELY — the merge is the whole cost of compaction. It is that the overlap was not
/// ENFORCED.)
///
/// So the append is driven through the window BY CONSTRUCTION:
/// `DataFusionHist::set_pause_in_merge_for_test` parks the compactor with its merge output written,
/// the lock released and the publish not yet begun, and the appends run to completion there. Move
/// the merge back inside the lock and the compactor parks while HOLDING it, so the first append
/// spins its ~4 s budget out and fails — the exact error `crates/vike-data/src/live_rec.rs`'s
/// `append_with_retry` deliberately does not retry ("it has already burned the lock spin"), which
/// is how the 110,354 rows became a hole in the tape rather than a delay.
#[test]
fn an_append_during_compaction_is_not_lost() {
    use std::sync::Arc;
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    // Enough fragments on one date that compaction has real work to do.
    for i in 0..8i64 {
        df.append_trades("binance", "BTCUSDT", &[tt(1_000 + i, 1.0, 1.0)], Some(&format!("a{i}")))
            .unwrap();
    }
    assert_eq!(df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(), 8);

    // Hold the post-merge/pre-publish window open, then compact on its own thread.
    let (parked_tx, parked_rx) = mpsc::channel::<()>();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    df.set_pause_in_merge_for_test(parked_tx, resume_rx);
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let c = Arc::clone(&df);
    let compactor =
        std::thread::spawn(move || c.compact_series("trade", "binance", "BTCUSDT", None, &cfg));

    // Generous, because it bounds a real merge on a loaded CI box — but it is a FAILURE bound, not
    // a timing assumption: the assertions below start only once the compactor has said it is here.
    let arrived = parked_rx.recv_timeout(Duration::from_secs(60));
    assert!(
        arrived.is_ok(),
        "the compactor never reached the post-merge window — it merged nothing, failed, or the \
         pause seam is gone (in which case this test is back to racing for the overlap)"
    );

    // The appends run HERE, with a compaction demonstrably in flight and its merge output already
    // on disk. Under the pre-fix design this is where the loss happened: the compactor would park
    // holding the series lock, and this call would spin ~4 s and return `timeout acquiring series
    // lock` — on which a real RecorderSink flush discards its buffer, up to `max_rows` rows.
    for i in 0..8i64 {
        df.append_trades("binance", "BTCUSDT", &[tt(9_000 + i, 2.0, 2.0)], Some(&format!("b{i}")))
            .expect("an append blocked on a merge that had already released the lock");
    }
    // Sampled BEFORE the release, so the message can name what the compaction went on to do.
    let finished_while_appending = compactor.is_finished();
    let _ = resume_tx.send(());
    let report = compactor.join().expect("compactor panicked").expect("compaction failed");
    assert!(
        !finished_while_appending,
        "the compaction had already completed before the last append returned, so these rows did \
         not overlap it at all and the counts below prove nothing about concurrency: {report:?}"
    );
    // ...and the publish crossed those appends rather than abandoning the date over them: an
    // abandoned merge leaves all 8 inputs in place, which the 16 rows below would also satisfy.
    assert_eq!(
        report.parts_merged, 8,
        "the merge published across the concurrent appends: {report:?}"
    );

    // Every row survives: the 8 originals (compacted or not) plus the 8 appended concurrently.
    let got = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), 16, "no row lost on either side: {report:?}");
    assert_eq!(
        got.iter().filter(|t| t.ts >= 9_000).count(),
        8,
        "the concurrent appends are all here"
    );

    // ...and the store still reads correctly after a reopen (the manifest agrees with the disk).
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(reopened.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(), 16);
}

/// A BROKEN series must not stop maintenance for the rest of the store.
///
/// Regression for the the CI box outage of 2026-08-02: `run_maintenance` walked series with `?`, so the
/// FIRST series that errored aborted the whole pass — and `MaintenanceScheduler` discarded that
/// error without logging it. Compaction and retention silently stopped store-wide (~270 uncompacted
/// parts in one date, 15+ minutes, zero log lines). The per-kind precedents
/// (`run_maintenance_handles_chain_series`, `..._equity_series`) each patched ONE kind into the
/// dispatch; this pins the general property instead: an unroutable series is isolated, reported,
/// and the healthy series next to it still compacts.
#[test]
fn one_broken_series_does_not_abort_maintenance_for_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();

    // A HEALTHY series with enough parts to compact.
    let bars: Vec<Bar> =
        (0..4).map(|i| bar(1_700_000_000_000 + i * 60_000, 100.0 + i as f64, None)).collect();
    for (c, b) in bars.iter().enumerate() {
        store
            .append_bars(
                "binance",
                "BTCUSDT",
                "1m",
                std::slice::from_ref(b),
                Some(&format!("k{c}")),
            )
            .unwrap();
    }

    // A BROKEN one: a kind the compaction dispatch cannot route (`unknown series kind`). Built by
    // copying the healthy series' on-disk layout under a bogus `kind=`, so it is a well-formed
    // series in every respect EXCEPT that nothing can compact it — the shape a future unhandled
    // kind takes, which is exactly how this bit in production.
    let good = dir.path().join("kind=bar/venue=binance/symbol=BTCUSDT/interval=1m");
    let bad = dir.path().join("kind=bogus/venue=binance/symbol=BTCUSDT/interval=1m");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    copy_tree(&good, &bad);

    let report = store
        .run_maintenance(&MaintenanceConfig {
            compaction: CompactionConfig { min_parts: 2, ..Default::default() },
            retention: None,
        })
        .expect("a broken series must not make the PASS fail");

    assert_eq!(report.failed.len(), 1, "the broken series is reported, not hidden: {report:?}");
    assert_eq!(report.failed[0].0.kind, "bogus");
    assert!(
        report.failed[0].1.contains("unknown series kind"),
        "the recorded error names the cause: {}",
        report.failed[0].1
    );
    assert!(
        report.compaction.parts_merged > 0,
        "the HEALTHY series must still compact despite the broken neighbour: {report:?}"
    );
    // And the healthy series' data is intact through that pass.
    let got = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(got.len(), 4, "no rows lost while a sibling series was failing");
}

/// Recursive directory copy for the fault-injection above (no external dep).
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

// ---- migrate a per-symbol series into its group -------------------------------------------------

/// The repair, end to end: rows move into the group and the per-symbol series is gone.
#[test]
fn migrate_moves_rows_into_the_group_and_removes_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1, 0.4, 5.0), tt(2, 0.5, 6.0)], Some("k")).unwrap();
    assert!(dir.path().join("kind=trade/venue=polymarket/symbol=TOK").exists());

    let moved = df.migrate_series_to_group("trade", "polymarket", "TOK", "fam").unwrap();

    assert_eq!(moved, 2);
    assert!(!dir.path().join("kind=trade/venue=polymarket/symbol=TOK").exists(), "source gone");
    assert!(dir.path().join("kind=trade/venue=polymarket/group=fam").exists());
    // Still readable by symbol — now out of the grouped series, via its symbol column.
    let got = df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|t| t.symbol == "TOK"), "stamped for the grouped layout");
}

/// Re-running is a NO-OP, not a double-append: the `migrate:` commit key makes the second copy
/// idempotent, and the source is already gone so there is nothing left to move.
#[test]
fn migrate_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_trades("polymarket", "TOK", &[tt(1, 0.4, 5.0)], Some("k")).unwrap();

    assert_eq!(df.migrate_series_to_group("trade", "polymarket", "TOK", "fam").unwrap(), 1);
    assert_eq!(
        df.migrate_series_to_group("trade", "polymarket", "TOK", "fam").unwrap(),
        0,
        "nothing left to move"
    );
    assert_eq!(
        df.scan_trades("polymarket", "TOK", TsRange::all()).unwrap().len(),
        1,
        "not doubled"
    );
}

/// An absent source is `Ok(0)`, so a caller can migrate a list without pre-checking each one.
#[test]
fn migrating_a_series_that_does_not_exist_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df.migrate_series_to_group("trade", "polymarket", "NOPE", "fam").unwrap(), 0);
}

/// Quotes and book updates migrate the same way — all three tick kinds have a grouped form.
#[test]
fn every_tick_kind_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let q = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: String::new(),
    };
    df.append_quotes("polymarket", "TOK", &[q], Some("q")).unwrap();
    df.append_book_updates(
        "polymarket",
        "TOK",
        &[BookUpdate {
            ts: 1,
            local_ts: 0,
            seq: 1,
            kind: vike_model::BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![(0.4, 1.0)],
            asks: vec![(0.6, 1.0)],
            symbol: String::new(),
        }],
        Some("b"),
    )
    .unwrap();

    assert_eq!(df.migrate_series_to_group("quote", "polymarket", "TOK", "fam").unwrap(), 1);
    assert_eq!(df.migrate_series_to_group("book", "polymarket", "TOK", "fam").unwrap(), 1);
    assert_eq!(df.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap().len(), 1);
    assert_eq!(df.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap().len(), 1);
}

/// A kind with no grouped form is an ERROR, not a silent skip — `bar` has no `append_bars_grouped`,
/// and quietly doing nothing would leave a caller believing a migration happened.
#[test]
fn a_kind_without_a_grouped_form_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let err = df.migrate_series_to_group("bar", "binance", "BTCUSDT", "fam").unwrap_err();
    assert!(err.to_string().contains("no grouped form"), "{err}");
}

// ============================ grouped series: every SeriesId-taking verb ============================
//
// Five methods built their directory from `id.symbol`, which is EMPTY for a grouped series, so each
// looked at a path ending `symbol=`. `read_manifest` returns an EMPTY manifest for a missing dir
// rather than erroring, so all five silently succeeded while doing nothing. These pin the fix.
//
// Found live: a 148 MB recorded tape whose entire Polymarket group (37.5 M book rows) rendered in
// the Data Manager as `0 rows · 0 B`.

/// A store holding ONE grouped quote series (`polymarket/btc-5m`, three symbols, two days) plus a
/// per-symbol sibling, so every assertion below can also show the per-symbol path still works.
fn grouped_store() -> (tempfile::TempDir, DataFusionHist, SeriesId) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let day = 86_400_000i64;
    let base = 1_700_000_000_000i64 - (1_700_000_000_000i64 % day); // midnight UTC
    let mk = |sym: &str, ts: i64| QuoteTick {
        ts,
        local_ts: ts + 1,
        bid: 0.5,
        ask: 0.6,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: sym.into(),
    };
    store
        .append_quotes_grouped(
            "polymarket",
            "btc-5m",
            &[mk("AAA", base), mk("BBB", base + 1), mk("CCC", base + day)],
            Some("g1"),
        )
        .unwrap();
    store.append_quotes("binance", "BTCUSDT", &[mk("BTCUSDT", base)], Some("s1")).unwrap();
    let id = SeriesId::grouped("quote", "polymarket", "btc-5m");
    (dir, store, id)
}

/// **The one the screenshot exposed.** A grouped series' coverage is its real rows/bytes/span, not
/// the zeroes a missing manifest yields.
#[test]
fn series_coverage_reads_a_grouped_series() {
    let (_d, store, id) = grouped_store();
    let cov = store.series_coverage(&id).unwrap();
    assert_eq!(cov.rows, 3, "grouped rows must be counted, not reported as 0");
    assert!(cov.bytes > 0, "grouped parts have a size on disk");
    assert_eq!(cov.dates, 2, "two `date=` partitions");
    assert!(cov.first_ts < cov.last_ts, "a real span, not the default sentinel");
}

/// `inventory()` is `series_coverage` per listed series — the Data Manager's actual source. It
/// must show the grouped series AND its coverage, not a row of zeroes.
#[test]
fn inventory_reports_grouped_coverage() {
    let (_d, store, _id) = grouped_store();
    let inv = store.inventory().unwrap();
    let (_, cov) = inv
        .iter()
        .find(|(id, _)| id.group.as_deref() == Some("btc-5m"))
        .expect("the grouped series is listed");
    assert_eq!(cov.rows, 3, "listed but empty is the bug this pins");
}

/// The cross-kind report keyed off the same path — a grouped instrument must contribute its days,
/// or the Partial column can never say anything about a grouped family.
#[test]
fn coverage_report_sees_a_grouped_instruments_days() {
    let (_d, store, _id) = grouped_store();
    let report = store.coverage_report().unwrap();
    let c = report
        .iter()
        .find(|c| c.key.grouped && c.key.label == "btc-5m")
        .expect("the grouped instrument is in the report");
    assert_eq!(c.spanned_days().len(), 2, "both recorded days, not zero");
}

/// The gap column AND `vike_archive_backfill --gaps` both read this. Reporting "no data" for a
/// grouped series would make `--gaps` re-fetch history that is already on disk.
#[test]
fn series_gaps_reads_a_grouped_series() {
    let (_d, store, id) = grouped_store();
    // "No gaps" would pass under the bug too — an empty day list has no gaps either. So punch a
    // REAL hole: the fixture holds days 0 and 1; add day 4, leaving days 2-3 missing.
    let day = 86_400_000i64;
    let base = 1_700_000_000_000i64 - (1_700_000_000_000i64 % day);
    let far = QuoteTick {
        ts: base + 4 * day,
        local_ts: base + 4 * day + 1,
        bid: 0.5,
        ask: 0.6,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: "AAA".into(),
    };
    store.append_quotes_grouped("polymarket", "btc-5m", &[far], Some("g2")).unwrap();

    let gaps = store.series_gaps(&id).unwrap();
    assert_eq!(gaps.len(), 1, "the missing days are one range, got {gaps:?}");
    // Inclusive epoch-ms (same convention as `SeriesCoverage::first_ts`/`last_ts`), so the range
    // runs from the START of day 2 to the LAST MILLISECOND of day 3 — not to day 3's start.
    assert_eq!(gaps[0], (base + 2 * day, base + 4 * day - 1), "the gap spans exactly days 2-3");
}

/// **The most dangerous face.** Delete used to return `Ok(())` having removed nothing: the
/// Data Manager's Delete button was a silent no-op on every grouped series.
#[test]
fn delete_series_actually_deletes_a_grouped_series() {
    let (dir, store, id) = grouped_store();
    let group_dir = dir.path().join("kind=quote").join("venue=polymarket").join("group=btc-5m");
    assert!(group_dir.exists(), "fixture sanity");

    store.delete_series(&id).unwrap();

    assert!(!group_dir.exists(), "delete reported success but left the series on disk");
    assert!(
        !store.inventory().unwrap().iter().any(|(i, _)| i.group.as_deref() == Some("btc-5m")),
        "deleted series still listed"
    );
    // The per-symbol sibling under a DIFFERENT venue is untouched.
    assert!(store.inventory().unwrap().iter().any(|(i, _)| i.symbol == "BTCUSDT"));
}

/// Rebuilding from the parts on disk must find the grouped parts, not a phantom empty directory.
#[test]
fn rebuild_series_manifest_reads_a_grouped_series() {
    let (_d, store, id) = grouped_store();
    let report = store.rebuild_series_manifest(&id).unwrap();
    assert!(
        report.parts_recovered > 0,
        "rebuild recovered no parts: {report:?} — the phantom-directory bug (it was looking at a \
         `symbol=` path that does not exist, and an empty dir has nothing to recover)"
    );
    assert_eq!(report.parts_unreadable, 0, "{report:?}");
    assert_eq!(store.series_coverage(&id).unwrap().rows, 3, "rebuild preserved the rows");
}

// ---- crash-safety of the per-series write lock -----------------------------------------------
//
// the CI box, 2026-08-04: the recorder's compaction thread was OOM-killed by the cgroup while holding a
// series lock. SIGKILL runs no `Drop`, so `_manifest.lock` was left on disk — and because the lock
// WAS that file's existence, no later process could tell a dead owner from a live one. Every
// restart timed out opening the store and exited 1; systemd restarted it 2,728 times over ~11 h,
// recording nothing. The lock must be held by the OS (which releases it when the holder dies),
// never by a file that outlives its owner.

/// A lock file left behind by a killed writer must not wedge the next writer.
#[test]
fn a_leftover_lock_file_does_not_block_the_next_writer() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1000, 100.0, None)], Some("a")).unwrap();

    // Exactly what a SIGKILL'd holder leaves: the lock file, with nobody holding it.
    let leftover = bars_series(dir.path()).join("_manifest.lock");
    std::fs::write(&leftover, b"").unwrap();
    assert!(leftover.exists());

    let t0 = Instant::now();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(2000, 101.0, None)], Some("b"))
        .expect("a leftover lock file wedged the writer — a killed holder must not outlive itself");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "append took {:?} — it spun on the leftover lock instead of taking it",
        t0.elapsed()
    );
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 2);

    // And reopening the store (the path that actually crash-looped: WAL recovery locks each series)
    // must work too.
    drop(df);
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 2);
}

/// `target_bytes` must BOUND a compaction pass, not merely describe its ambitions.
///
/// the CI box, 2026-08-04: the planner took every part of a `date=` as one merge and decoded them all
/// into Arrow before writing — 3,634 parts, 658 MB of zstd Parquet, in a 4 GB cgroup. The OOM
/// killer took the recorder down twice, and the second kill left a lock behind that wedged every
/// restart for 11 h. `target_bytes` was plumbed from the profile's `target_mb` all the way into
/// `CompactionConfig` and then never read.
#[test]
fn compaction_splits_a_date_into_merges_bounded_by_max_merge_rows() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..8i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    assert_eq!(count_parquets(&series), 8, "eight same-day fragments");
    let before = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    // 40 rows across 8 equal parts, with room for 10 rows per merge: several merges, not one.
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, max_merge_rows: 10 };
    let rep = df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    assert_eq!(rep.parts_merged, 8, "every fragment was still compacted");
    assert!(
        rep.parts_written > 1,
        "the whole date merged as ONE part despite a row budget a quarter its size — the merge is \
         unbounded, and its peak memory is whatever the biggest date happens to be"
    );
    assert_eq!(rep.rows, 40);
    assert!(count_parquets(&series) < 8, "compaction still reduced the part count");
    assert_bars_bit_eq(&before, &df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap());
}

/// The same date under a budget that comfortably fits it merges as ONE part — a memory bound must
/// not fragment a series it was never meant to constrain.
#[test]
fn compaction_within_the_row_budget_still_merges_a_date_whole() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..8i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    let cfg = CompactionConfig { min_parts: 3, ..Default::default() };
    let rep = df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_merged, 8);
    assert_eq!(rep.parts_written, 1, "40 rows against the 1,000,000-row default is one merge");
}
