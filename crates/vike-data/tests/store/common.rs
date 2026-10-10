//! Helpers shared by two or more modules: row literals and series paths. The bit-exact bar check
//! is `vike_model::test_support::bars::assert_bars_bit_eq`, shared with other crates.

use std::path::{Path, PathBuf};

use vike_data::{DataFusionHist, HistStore, SeriesId};
use vike_model::{Bar, BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

pub(crate) fn bar(ts: i64, close: f64, funding: Option<f64>) -> Bar {
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
pub(crate) fn qt(ts: i64, val: f64) -> QuoteTick {
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

pub(crate) const DAY: i64 = 86_400_000;

/// The bar series leaf dir under a store root (parts live one level deeper under `date=…`).
pub(crate) fn bars_series(root: &Path) -> PathBuf {
    root.join("kind=bar").join("venue=binance").join("symbol=BTCUSDT").join("interval=1m")
}

/// Count `.parquet` part files under a dir (recurses `date=` subdirs); ignores manifests/locks.
pub(crate) fn count_parquets(dir: &Path) -> usize {
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

/// Trade literal (symbol filled on read): `tt(ts, price, size)`, a taker print (not a maker).
pub(crate) fn tt(ts: i64, price: f64, size: f64) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: false, symbol: String::new() }
}

/// Book-event literal: a `BookUpdate` at `ts` with `local_ts = ts + 2` and the given levels.
pub(crate) fn bu(
    ts: i64,
    seq: u64,
    kind: BookUpdateKind,
    bids: Vec<BookLevel>,
    asks: Vec<BookLevel>,
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

/// The four same-day parts this section's crash tests compact, plus the series dir they live in.
/// Keyed appends, because commit keys are what the rebuild uses to tell a merge's inputs from an
/// unrelated part — see [`rebuild_after_a_compaction_crashed_before_the_unlinks_does_not_duplicate`].
pub(crate) fn four_fragments_for_one_day(root: &Path, store: &DataFusionHist) -> PathBuf {
    for c in 0..4i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        store.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    bars_series(root)
}

pub(crate) fn bars_id() -> SeriesId {
    SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: Some("1m".into()),
        group: None,
        source: None,
    }
}

/// The only `date=` dir under a series (these tests write one UTC day).
pub(crate) fn only_date_dir(series: &Path) -> PathBuf {
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

/// Count `.parquet` files under a tree — a commit produces one part per UTC day, so with a
/// single-day fixture this is the commit count.
pub(crate) fn walk_parquet(p: &Path) -> usize {
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
