//! Compaction over a GROUPED series (`kind=…/venue=…/group=…`): one part holds MANY symbols, told
//! apart by the row-level `symbol_col` column rather than by the path. Driven through
//! `run_maintenance`, because that is the only verb that reaches a grouped directory —
//! `compact_series`/`compact_series_superseding` build a `symbol=` path and cannot name one.
//!
//! ## Source-ranked supersession keys on the SYMBOL in a grouped series
//!
//! `run_maintenance` applies the store's persisted source policy (`_sources.json`) to grouped
//! directories exactly as to per-symbol ones. Its natural key was `C::sort_key` alone — `(ts, 0)`
//! for a quote or trade, `(ts, seq)` for a book row — and that is a row's whole identity only when
//! the PATH names the instrument. In a grouped part two DIFFERENT instruments' rows at one ts formed
//! ONE run, so every lower-ranked source's row of every OTHER symbol at that ts was dropped as a
//! "superseded duplicate" of a row it did not duplicate. The fixtures below are the realistic shape:
//! the recorder's `live-…` flushes, plus a symbol's stray per-symbol tape folded in under a
//! `migrate:{kind}:{symbol}` key (the deleted fold tool's key, which real grouped series still
//! carry) — two prefixes on one grouped series.
//!
//! A per-symbol series must NOT gain the symbol in its key: a row's own symbol cell there may be
//! empty (the tag-from-path contract) or stamped, so keying on it would stop two copies of ONE
//! instrument's row from colliding. `a_per_symbol_series_still_supersedes_by_ts_…` holds that.
//!
//! ## A grouped part is compacted SYMBOL-MAJOR
//!
//! The grouped writer sorts `(symbol, ts)` so a one-symbol read can prune by the `symbol_col`
//! statistics; compaction used to re-sort the merged part by ts and undo it, so every page of the
//! one big Sealed row group spanned every symbol and a one-symbol read decoded the whole part. The
//! second half of this file pins the order (both compaction paths), what the PAGE index then
//! admits, that no row is lost or reordered within its symbol, and that a per-symbol series keeps
//! its ts order.
#![cfg(feature = "hist-datafusion")]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{Array, Float64Array, Int64Array, StringArray};
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::file::page_index::column_index::ColumnIndexMetaData;
use datafusion::parquet::file::reader::{FileReader, SerializedFileReader};
use datafusion::parquet::file::serialized_reader::ReadOptionsBuilder;
use datafusion::physical_plan::{ExecutionPlan, collect};
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext, col, lit};
use vike_data::{
    CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, RetentionPolicy,
    StoreSourcePolicy, TsRange, save_policy,
};
use vike_model::{Bar, BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

const V: &str = "polymarket";
const G: &str = "btc-5m";

/// A store whose `_sources.json` ranks the recorder above the `migrate:` fold above a backfill —
/// every key the fixtures write matches a prefix, so the strict guard does not skip any series.
fn ranked_store() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    save_policy(dir.path(), &StoreSourcePolicy::new(["live-", "migrate:", "pmxt:"])).unwrap();
    (dir, df)
}

/// Two parts in one `date=` are enough to merge; no retention.
fn maint() -> MaintenanceConfig {
    MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() },
        retention: None,
    }
}

fn trade(symbol: &str, ts: i64, size: f64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: ts + 1,
        price: 0.5,
        size,
        is_buyer_maker: false,
        symbol: symbol.to_string(),
    }
}

fn quote(symbol: &str, ts: i64, bid: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: ts + 1,
        bid,
        ask: bid + 0.01,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: symbol.to_string(),
    }
}

/// One single-level snapshot event; `size` tells the two sources' copies apart.
fn book(symbol: &str, ts: i64, seq: u64, size: f64) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts + 2,
        seq,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.45, size)],
        asks: vec![BookLevel::new(0.46, size)],
        symbol: symbol.to_string(),
    }
}

fn trade_rows(df: &DataFusionHist, symbol: &str) -> Vec<(i64, f64)> {
    df.scan_trades(V, symbol, TsRange::all()).unwrap().iter().map(|t| (t.ts, t.size)).collect()
}

/// The trade lane: B's migrated tape holds B@1000 — a DIFFERENT instrument at the ts of A's live
/// trade, which must survive — and B@3000, a true duplicate of B's own live row, which must not.
#[test]
fn superseding_a_grouped_trade_series_keeps_other_symbols_rows_at_a_shared_ts() {
    let (_dir, df) = ranked_store();
    df.append_trades_grouped(
        V,
        G,
        &[trade("A", 1_000, 10.0), trade("B", 3_000, 30.0)],
        Some("live-polymarket-btc-5m-trade-1000-3000-1"),
    )
    .unwrap();
    df.append_trades_grouped(
        V,
        G,
        &[trade("B", 1_000, 77.0), trade("B", 3_000, 33.0)],
        Some("migrate:trade:B"),
    )
    .unwrap();

    let rep = df.run_maintenance(&maint()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.parts_merged, 2, "the grouped series was compacted, not skipped");
    assert_eq!(rep.compaction.rows_superseded, 1, "only B's own duplicate at ts=3000 is dropped");

    assert_eq!(trade_rows(&df, "A"), vec![(1_000, 10.0)]);
    assert_eq!(
        trade_rows(&df, "B"),
        vec![(1_000, 77.0), (3_000, 30.0)],
        "B@1000 duplicates nothing (A traded then, not B); at ts=3000 the live row won"
    );
}

/// The quote lane, same shape: the run at ts=1000 holds A's live quote and B's migrated one.
#[test]
fn superseding_a_grouped_quote_series_keeps_other_symbols_rows_at_a_shared_ts() {
    let (_dir, df) = ranked_store();
    df.append_quotes_grouped(
        V,
        G,
        &[quote("A", 1_000, 0.40), quote("B", 3_000, 0.60)],
        Some("live-polymarket-btc-5m-quote-1000-3000-1"),
    )
    .unwrap();
    df.append_quotes_grouped(
        V,
        G,
        &[quote("B", 1_000, 0.55), quote("B", 3_000, 0.65)],
        Some("migrate:quote:B"),
    )
    .unwrap();

    let rep = df.run_maintenance(&maint()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.rows_superseded, 1, "only B's own duplicate at ts=3000 is dropped");

    let rows = |s: &str| -> Vec<(i64, f64)> {
        df.scan_quotes(V, s, TsRange::all()).unwrap().iter().map(|q| (q.ts, q.bid)).collect()
    };
    assert_eq!(rows("A"), vec![(1_000, 0.40)]);
    assert_eq!(rows("B"), vec![(1_000, 0.55), (3_000, 0.60)]);
}

/// The book lane, whose natural key is `(ts, seq)`: B's migrated event shares A's live event's
/// `(ts, seq)` and must survive WHOLE; B's own duplicated event is superseded whole, as before.
#[test]
fn superseding_a_grouped_book_series_keeps_other_symbols_events_at_a_shared_ts_seq() {
    let (_dir, df) = ranked_store();
    df.append_book_updates_grouped(
        V,
        G,
        &[book("A", 1_000, 5, 100.0), book("B", 2_000, 6, 20.0)],
        Some("live-polymarket-btc-5m-book-1000-2000-1"),
    )
    .unwrap();
    df.append_book_updates_grouped(
        V,
        G,
        &[book("B", 1_000, 5, 999.0), book("B", 2_000, 6, 555.0)],
        Some("migrate:book:B"),
    )
    .unwrap();

    let rep = df.run_maintenance(&maint()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.rows_superseded, 2, "B's duplicated (2000,6) event: its two levels");

    let events = |s: &str| -> Vec<(i64, u64, f64)> {
        df.scan_book_updates(V, s, TsRange::all())
            .unwrap()
            .iter()
            .map(|u| (u.ts, u.seq, u.bids[0].qty))
            .collect()
    };
    assert_eq!(events("A"), vec![(1_000, 5, 100.0)]);
    assert_eq!(
        events("B"),
        vec![(1_000, 5, 999.0), (2_000, 6, 20.0)],
        "B's (1000,5) is its own event, not A's; at (2000,6) the live event won"
    );
}

/// A PER-SYMBOL series keeps the old key: the path names the one instrument, so two sources' rows
/// at one ts collide even when one writer left the row's symbol cell EMPTY (tag-from-path) and the
/// other stamped it. Keying a per-symbol series on that cell would stop this collision and keep
/// both copies — so this pins that the symbol joins the key for grouped series ONLY.
#[test]
fn a_per_symbol_series_still_supersedes_by_ts_whatever_its_symbol_cells_hold() {
    let (_dir, df) = ranked_store();
    df.append_trades(V, "TOK", &[trade("", 1_000, 10.0)], Some("live-polymarket-TOK-trade-1-2-3"))
        .unwrap();
    df.append_trades(V, "TOK", &[trade("TOK", 1_000, 99.0)], Some("pmxt:trade:TOK:h")).unwrap();

    let rep = df.run_maintenance(&maint()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.rows_superseded, 1, "one instrument, one ts: a true duplicate");
    assert_eq!(trade_rows(&df, "TOK"), vec![(1_000, 10.0)], "the live row won");
}

// ---- the ORDER a grouped part is compacted into ------------------------------------------------

/// A UTC midnight, so every row of the order fixtures lands in ONE `date=` partition.
const DAY0: i64 = 1_785_024_000_000;

/// A store with NO `_sources.json`: `run_maintenance` takes the plain, duplicate-keeping path.
fn plain_store() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    (dir, df)
}

/// A series leaf: `kind=…/venue=…/` then `group=…` or `symbol=…`.
fn leaf(root: &Path, kind: &str, last: &str) -> PathBuf {
    root.join(format!("kind={kind}")).join(format!("venue={V}")).join(last)
}

/// Every `part-*.parquet` under `dir`, sorted.
fn parts_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if p.is_dir() {
                stack.push(p);
            } else if name.starts_with("part-") && name.ends_with(".parquet") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The ONE part a fully-merged series holds.
fn only_part(dir: &Path) -> PathBuf {
    let parts = parts_under(dir);
    assert_eq!(parts.len(), 1, "every fragment merged into one part: {parts:?}");
    parts.into_iter().next().unwrap()
}

/// `(symbol_col, ts, <f64 column>)` of every row of one part, in FILE order.
fn file_rows(part: &Path, f64_col: &str) -> Vec<(String, i64, f64)> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(part).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let column = |name: &str| batch.column_by_name(name).unwrap_or_else(|| panic!("{name}"));
        let symbol = column("symbol_col").as_any().downcast_ref::<StringArray>().expect("Utf8");
        let ts = column("ts").as_any().downcast_ref::<Int64Array>().expect("Int64 ts");
        let value = column(f64_col).as_any().downcast_ref::<Float64Array>().expect("Float64");
        for i in 0..batch.num_rows() {
            out.push((symbol.value(i).to_string(), ts.value(i), value.value(i)));
        }
    }
    out
}

/// What the `symbol_col` PAGE INDEX of one part admits for `symbol_col == symbol` — the input a
/// reader's page-index pruning consults, computed from the file itself (column index min/max per
/// page, offset index first row per page), so it depends on no DataFusion default.
struct PageAdmission {
    row_groups: usize,
    pages: usize,
    pages_admitted: usize,
    rows: usize,
    rows_admitted: usize,
}

fn page_admission(part: &Path, symbol: &str) -> PageAdmission {
    let opts = ReadOptionsBuilder::new().with_page_index().build();
    let reader = SerializedFileReader::new_with_options(File::open(part).unwrap(), opts).unwrap();
    let md = reader.metadata();
    let col_idx = md
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .position(|c| c.name() == "symbol_col")
        .expect("symbol_col");
    let column_index = md.column_index().expect("the writer emits a column index");
    let offset_index = md.offset_index().expect("the writer emits an offset index");
    let x = symbol.as_bytes();
    let mut a = PageAdmission {
        row_groups: md.num_row_groups(),
        pages: 0,
        pages_admitted: 0,
        rows: 0,
        rows_admitted: 0,
    };
    for (rg, meta) in md.row_groups().iter().enumerate() {
        let rg_rows = meta.num_rows() as usize;
        let ColumnIndexMetaData::BYTE_ARRAY(ci) = &column_index[rg][col_idx] else {
            panic!("symbol_col carries no byte-array page index in row group {rg}");
        };
        let locs = offset_index[rg][col_idx].page_locations();
        for (p, loc) in locs.iter().enumerate() {
            let first = loc.first_row_index as usize;
            let end = locs.get(p + 1).map_or(rg_rows, |l| l.first_row_index as usize);
            let admits = matches!(
                (ci.min_value(p), ci.max_value(p)),
                (Some(lo), Some(hi)) if lo <= x && x <= hi
            );
            a.pages += 1;
            a.rows += end - first;
            if admits {
                a.pages_admitted += 1;
                a.rows_admitted += end - first;
            }
        }
    }
    a
}

/// `(rows the Parquet scan DECODED, rows it returned)` for a one-symbol read of one part.
///
/// The frame is `query.rs`'s `map_part_frames` for a grouped part — one session with ONE target
/// partition, the default `ParquetReadOptions`, `symbol_col == symbol` pushed — and the decoded
/// count is the scan leaf's `output_rows`, the way the inspection probes measured it (filter
/// pushdown is off by default, so the leaf emits every row of every page it did not prune).
/// ⚠ A COPY of that private frame, not the frame itself; [`page_admission`] is the half of the
/// measurement no DataFusion default can move.
fn one_symbol_read(rt: &tokio::runtime::Runtime, part: &Path, symbol: &str) -> (usize, usize) {
    rt.block_on(async {
        let ctx = SessionContext::new_with_config(SessionConfig::new().with_target_partitions(1));
        let df = ctx
            .read_parquet(vec![part.to_string_lossy().to_string()], ParquetReadOptions::default())
            .await
            .unwrap()
            .filter(col("symbol_col").eq(lit(symbol)))
            .unwrap();
        let plan = df.create_physical_plan().await.unwrap();
        let kept = collect(plan.clone(), ctx.task_ctx()).await.unwrap();
        (leaf_output_rows(&plan), kept.iter().map(|b| b.num_rows()).sum())
    })
}

fn leaf_output_rows(plan: &Arc<dyn ExecutionPlan>) -> usize {
    let children = plan.children();
    if children.is_empty() {
        return plan.metrics().and_then(|m| m.output_rows()).unwrap_or(0);
    }
    children.into_iter().map(leaf_output_rows).sum()
}

/// (1) THE PLAIN PATH, at a size where pages matter: 30 recorder-style flushes of a 60-symbol
/// family whose rows interleave in ts, compacted with the recorder's default knobs into ONE part.
/// It must be written `(symbol, ts)` in ONE Sealed row group, and the page index must admit a
/// one-symbol read to one or two of its ~15 pages. Compacted ts-sorted (the old order), every page
/// spans every symbol and the same read decodes the whole part.
#[test]
fn compacting_a_grouped_series_writes_it_symbol_major_so_a_one_symbol_read_prunes_pages() {
    const SYMBOLS: i64 = 60;
    const FLUSHES: i64 = 30;
    const PER_FLUSH: i64 = 167; // rows per symbol per flush: ~5,000 per symbol, ~300,000 in all
    let symbol = |s: i64| format!("S{s:03}");
    let (dir, df) = plain_store();
    for f in 0..FLUSHES {
        let mut rows = Vec::with_capacity((SYMBOLS * PER_FLUSH) as usize);
        for j in 0..PER_FLUSH {
            for s in 0..SYMBOLS {
                // In ts order consecutive rows cycle through every symbol, as a live family's do.
                let ts = DAY0 + (f * PER_FLUSH + j) * 1_000 + s;
                rows.push(quote(&symbol(s), ts, 0.40 + s as f64 * 0.001));
            }
        }
        df.append_quotes_grouped(V, G, &rows, Some(&format!("live-polymarket-{G}-quote-{f}")))
            .unwrap();
    }
    let total = (SYMBOLS * FLUSHES * PER_FLUSH) as usize;

    let rep = df.run_maintenance(&MaintenanceConfig::default()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.parts_merged, FLUSHES as usize);
    assert_eq!(rep.compaction.parts_written, 1);
    assert_eq!(rep.compaction.rows, total);
    let part = only_part(&leaf(dir.path(), "quote", &format!("group={G}")));

    // Measure first, so a failing order assertion below still prints what the read decoded.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let mut measured = Vec::new();
    for s in [0, 29, SYMBOLS - 1] {
        let sym = symbol(s);
        let a = page_admission(&part, &sym);
        let (decoded, kept) = one_symbol_read(&rt, &part, &sym);
        eprintln!(
            "{sym}: decoded {decoded} of {} rows ({:.2}%), returned {kept}; pages admitted {}/{} \
             ({} rows); row groups {}",
            a.rows,
            100.0 * decoded as f64 / a.rows as f64,
            a.pages_admitted,
            a.pages,
            a.rows_admitted,
            a.row_groups
        );
        assert_eq!(kept, (FLUSHES * PER_FLUSH) as usize, "{sym}: the read is complete");
        assert_eq!(kept, df.scan_quotes(V, &sym, TsRange::all()).unwrap().len());
        measured.push((sym, a, decoded));
    }

    let order: Vec<(String, i64)> =
        file_rows(&part, "bid").into_iter().map(|(s, ts, _)| (s, ts)).collect();
    assert_eq!(order.len(), total);
    assert!(order.is_sorted(), "the compacted grouped part is not written (symbol, ts)");

    for (sym, a, decoded) in measured {
        assert_eq!(a.row_groups, 1, "compaction keeps Sealed's one big row group");
        assert_eq!(a.rows, total);
        assert!(a.pages >= 10, "enough pages for the page index to matter: {}", a.pages);
        assert!(a.pages_admitted <= 2, "{sym}: {}/{} pages admitted", a.pages_admitted, a.pages);
        assert!(decoded <= a.rows_admitted, "{sym}: decoded {decoded} > {}", a.rows_admitted);
        assert!(decoded * 100 < total * 15, "{sym}: decoded {decoded} of {total}");
    }
}

/// (1) THE SUPERSEDING PATH writes the same symbol-major order — both paths sort through one
/// function — while it still drops a true duplicate. In ts order this part would read A, B, A, B.
#[test]
fn the_superseding_path_writes_the_same_symbol_major_order() {
    let (dir, df) = ranked_store();
    df.append_trades_grouped(
        V,
        G,
        &[trade("A", 2_000, 10.0), trade("B", 1_000, 20.0)],
        Some("live-polymarket-btc-5m-trade-1000-2000-1"),
    )
    .unwrap();
    df.append_trades_grouped(
        V,
        G,
        &[trade("A", 500, 30.0), trade("B", 1_000, 21.0), trade("B", 3_000, 40.0)],
        Some("pmxt:trade:btc-5m:h"),
    )
    .unwrap();

    let rep = df.run_maintenance(&maint()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.rows_superseded, 1, "B@1000 is the one true duplicate");

    let part = only_part(&leaf(dir.path(), "trade", &format!("group={G}")));
    assert_eq!(
        file_rows(&part, "size"),
        vec![
            ("A".to_string(), 500, 30.0),
            ("A".to_string(), 2_000, 10.0),
            ("B".to_string(), 1_000, 20.0),
            ("B".to_string(), 3_000, 40.0),
        ],
        "symbol-major, and the live copy of B@1000 is the one kept"
    );
}

/// A PER-SYMBOL series is still compacted in ts order, whatever its rows' symbol cells hold: one
/// writer left them EMPTY (tag-from-path) and another stamped them. Ordered like a grouped series
/// it would come out `"", "", "TOK", "TOK"` — out of ts order.
#[test]
fn a_per_symbol_series_is_still_compacted_in_ts_order() {
    let (dir, df) = plain_store();
    df.append_quotes(V, "TOK", &[quote("TOK", 1_000, 0.1), quote("TOK", 3_000, 0.3)], Some("k1"))
        .unwrap();
    df.append_quotes(V, "TOK", &[quote("", 2_000, 0.2), quote("", 4_000, 0.4)], Some("k2"))
        .unwrap();

    let rep = df.run_maintenance(&maint()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.parts_written, 1);

    let part = only_part(&leaf(dir.path(), "quote", "symbol=TOK"));
    let ts: Vec<i64> = file_rows(&part, "bid").into_iter().map(|(_, ts, _)| ts).collect();
    assert_eq!(ts, vec![1_000, 2_000, 3_000, 4_000]);
}

/// Six grouped flushes whose ranges overlap and arrive OUT of order, one re-sending another's rows
/// verbatim (exact duplicates) and one holding equal-(symbol, ts) rows with a different payload.
fn messy_quote_flushes() -> Vec<Vec<QuoteTick>> {
    let span = |syms: &[&str], from: i64, to: i64, bid: f64| -> Vec<QuoteTick> {
        (from..to).flat_map(|ts| syms.iter().map(move |s| quote(s, ts, bid))).collect()
    };
    let abc = ["A", "B", "C"];
    vec![
        span(&abc[..], 1_000, 1_010, 0.10),
        span(&abc[..], 5_000, 5_010, 0.20),
        span(&abc[..], 3_000, 3_010, 0.30),
        span(&abc[..], 1_000, 1_010, 0.10),
        span(&abc[..], 1_005, 1_015, 0.40),
        span(&["B"][..], 9_000, 9_003, 0.50),
    ]
}

/// The book twin: multi-level events, so the per-level rows of ONE event must stay adjacent and in
/// feed order through the symbol-major sort (`BookCodec::sort_key` is `(ts, seq)`).
fn messy_book_flushes() -> Vec<Vec<BookUpdate>> {
    let event = |s: &str, ts: i64, size: f64| BookUpdate {
        ts,
        local_ts: ts + 2,
        seq: ts as u64,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.45, size), BookLevel::new(0.44, size + 1.0)],
        asks: vec![BookLevel::new(0.46, size + 2.0)],
        symbol: s.to_string(),
    };
    let span = |from: i64, to: i64, size: f64| -> Vec<BookUpdate> {
        (from..to).flat_map(|ts| ["A", "B"].map(|s| event(s, ts, size))).collect()
    };
    vec![
        span(1_000, 1_006, 1.0),
        span(4_000, 4_006, 2.0),
        span(1_000, 1_006, 1.0),
        span(1_003, 1_009, 3.0),
    ]
}

/// (2) No row is lost: across messy fragments — out of order, overlapping, exact duplicates, ties
/// with different payloads — every symbol's read returns the same MULTISET of rows after
/// compaction as before, and the part holds every input row.
#[test]
fn grouped_compaction_keeps_every_row_ties_and_duplicates_included() {
    let (dir, df) = plain_store();
    let quotes = messy_quote_flushes();
    for (i, rows) in quotes.iter().enumerate() {
        df.append_quotes_grouped(V, G, rows, Some(&format!("q{i}"))).unwrap();
    }
    for (i, events) in messy_book_flushes().iter().enumerate() {
        df.append_book_updates_grouped(V, G, events, Some(&format!("b{i}"))).unwrap();
    }
    let quote_rows: usize = quotes.iter().map(Vec::len).sum();

    let multiset_quotes = |s: &str| -> Vec<String> {
        let mut v: Vec<String> = df
            .scan_quotes(V, s, TsRange::all())
            .unwrap()
            .iter()
            .map(|q| format!("{q:?}"))
            .collect();
        v.sort();
        v
    };
    let multiset_books = |s: &str| -> Vec<String> {
        let mut v: Vec<String> = df
            .scan_book_updates(V, s, TsRange::all())
            .unwrap()
            .iter()
            .map(|u| format!("{u:?}"))
            .collect();
        v.sort();
        v
    };
    let before_q: Vec<Vec<String>> = ["A", "B", "C"].map(multiset_quotes).to_vec();
    let before_b: Vec<Vec<String>> = ["A", "B"].map(multiset_books).to_vec();

    let rep = df.run_maintenance(&MaintenanceConfig::default()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.parts_written, 2, "one part per kind");

    let qpart = only_part(&leaf(dir.path(), "quote", &format!("group={G}")));
    assert_eq!(file_rows(&qpart, "bid").len(), quote_rows, "every quote row is in the part");
    assert_eq!(["A", "B", "C"].map(multiset_quotes).to_vec(), before_q, "quote rows changed");
    assert_eq!(["A", "B"].map(multiset_books).to_vec(), before_b, "book events changed");
}

/// (3) Equal-(symbol, ts) ties keep a DETERMINISTIC order through compaction: their APPEND order
/// (parts in manifest order, each in file order — the merge sort is stable). Every read returns
/// exactly what it returned before compacting, and compacting an identical store writes an
/// identical part, byte for byte.
///
/// Sized so an UNSTABLE sort would visibly break it: four flushes each hold 300 rows of every
/// symbol at ONE ts, so the merge sorts 3,612 rows with tie runs of 1,200, far past the small-slice
/// threshold below which Rust's sorts happen to keep equal elements in place. The flushes arrive
/// interleaved (A, B, C, then A again), so the merge input is not one pre-sorted run either.
#[test]
fn equal_symbol_ts_ties_keep_their_append_order_through_compaction() {
    const FLUSHES: i64 = 4;
    const TIES: i64 = 300;
    const SYMBOLS: [&str; 3] = ["A", "B", "C"];
    // The bid names its own (flush, position), so any reordering of a tie run is visible.
    let bid = |f: i64, i: i64| (f * 1_000 + i) as f64;
    let build = || {
        let (dir, df) = plain_store();
        for f in 0..FLUSHES {
            let mut rows = Vec::new();
            for i in 0..TIES {
                for s in SYMBOLS {
                    rows.push(quote(s, 1_000, bid(f, i)));
                }
            }
            for s in SYMBOLS {
                rows.push(quote(s, 2_000 + f, -1.0 - f as f64));
            }
            df.append_quotes_grouped(V, G, &rows, Some(&format!("k{f}"))).unwrap();
        }
        (dir, df)
    };
    // Per symbol: the tie run in append order (flush, then position), then one row per flush.
    let want: Vec<(i64, f64)> = (0..FLUSHES)
        .flat_map(|f| (0..TIES).map(move |i| (1_000, bid(f, i))))
        .chain((0..FLUSHES).map(|f| (2_000 + f, -1.0 - f as f64)))
        .collect();
    let read = |df: &DataFusionHist, s: &str| -> Vec<(i64, f64)> {
        df.scan_quotes(V, s, TsRange::all()).unwrap().iter().map(|q| (q.ts, q.bid)).collect()
    };

    let (dir, df) = build();
    for s in SYMBOLS {
        assert_eq!(read(&df, s), want, "{s} before compacting");
    }
    let rep = df.run_maintenance(&MaintenanceConfig::default()).unwrap();
    assert_eq!(rep.compaction.parts_merged, FLUSHES as usize);
    for s in SYMBOLS {
        assert_eq!(read(&df, s), want, "{s} after compacting: the same, ties too");
    }

    let part = only_part(&leaf(dir.path(), "quote", &format!("group={G}")));
    let want_file: Vec<(String, i64, f64)> = SYMBOLS
        .iter()
        .flat_map(|s| want.iter().map(move |&(ts, b)| (s.to_string(), ts, b)))
        .collect();
    assert!(
        file_rows(&part, "bid") == want_file,
        "the part is not symbol-major with each symbol's ties in append order"
    );

    let (dir2, df2) = build();
    df2.run_maintenance(&MaintenanceConfig::default()).unwrap();
    let part2 = only_part(&leaf(dir2.path(), "quote", &format!("group={G}")));
    assert_eq!(
        std::fs::read(&part).unwrap(),
        std::fs::read(&part2).unwrap(),
        "the same input compacts to the same bytes"
    );
}

/// One snapshot event with three levels — the reviewer's probe shape: 8,192-row groups do not
/// divide by 3, so events straddle row-group boundaries.
fn three_level_event(symbol: &str, i: i64) -> BookUpdate {
    let px = 0.30 + (i % 997) as f64 * 1e-4;
    let qty = |k: i64| ((i * 7_919 + k * 104_729) % 10_007) as f64 + 1.0;
    BookUpdate {
        ts: DAY0 + i,
        local_ts: DAY0 + i + 3,
        seq: i as u64 + 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.0001,
        bids: vec![BookLevel::new(px, qty(1)), BookLevel::new(px - 0.001, qty(2))],
        asks: vec![BookLevel::new(px + 0.001, qty(3))],
        symbol: symbol.to_string(),
    }
}

/// Every event of one symbol as `(ts, seq, bids, asks)`, levels IN ORDER.
type EventSig = (i64, u64, Vec<(f64, f64)>, Vec<(f64, f64)>);

fn event_sigs(df: &DataFusionHist, symbol: &str) -> Vec<EventSig> {
    df.scan_book_updates(V, symbol, TsRange::all())
        .unwrap()
        .iter()
        .map(|u| {
            let levels = |ls: &[BookLevel]| -> Vec<(f64, f64)> {
                ls.iter().map(|l| (l.price, l.qty)).collect()
            };
            (u.ts, u.seq, levels(u.bids.as_slice()), levels(u.asks.as_slice()))
        })
        .collect()
}

/// (3, the READ half) A book event's levels keep their order through compaction even when an input
/// part is big enough for DataFusion to split. The tied rows of a book merge are ONE event's
/// levels, so the stable sort keeps whatever order the read delivered — and a part over
/// `repartition_file_min_size` read under a multi-partition session arrives in ARRIVAL order. The
/// big part is one grouped append of a whole series, many 8,192-row groups; the three fragments
/// make the date due for a merge.
#[test]
fn compaction_keeps_book_levels_in_order_when_an_input_part_is_split() {
    let (dir, df) = plain_store();
    let big: Vec<BookUpdate> = (0..100_000).map(|i| three_level_event("A", i)).collect();
    df.append_book_updates_grouped(V, G, &big, Some("migrate:book:A")).unwrap();
    let big_part = only_part(&leaf(dir.path(), "book", &format!("group={G}")));
    let bytes = std::fs::metadata(&big_part).unwrap().len();
    let row_groups = SerializedFileReader::new(File::open(&big_part).unwrap())
        .unwrap()
        .metadata()
        .num_row_groups();
    assert!(bytes > 1 << 20 && row_groups > 1, "a splittable part: {bytes} B, {row_groups} RGs");
    for k in 0..3 {
        df.append_book_updates_grouped(
            V,
            G,
            &[three_level_event("C", 300_000 + k)],
            Some(&format!("live-{k}")),
        )
        .unwrap();
    }

    let before = event_sigs(&df, "A");
    let rep = df.run_maintenance(&MaintenanceConfig::default()).unwrap();
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.compaction.parts_merged, 4);
    let after = event_sigs(&df, "A");

    assert_eq!(before.len(), after.len(), "event count");
    let changed: Vec<(&EventSig, &EventSig)> =
        before.iter().zip(&after).filter(|(b, a)| b != a).collect();
    assert!(
        changed.is_empty(),
        "{} of {} events changed through compaction; first: {:?}",
        changed.len(),
        before.len(),
        changed.first()
    );
}

/// `ts` of every row of one part, in FILE order (for kinds that carry no `symbol_col`).
fn file_ts(part: &Path) -> Vec<i64> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(part).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let ts = batch.column_by_name("ts").expect("ts");
        let ts = ts.as_any().downcast_ref::<Int64Array>().expect("Int64 ts");
        out.extend((0..batch.num_rows()).map(|i| ts.value(i)));
    }
    out
}

/// A `group=` leaf of a kind with NO row symbol — hand-made here, because no writer produces one —
/// is merged and retention-pruned by the PLAIN pass like any other leaf: the plain merge keeps
/// every row in any order, so refusing it protected nothing and silently skipped its retention on
/// every pass. Only a store SOURCE POLICY still refuses it, because the superseding merge could
/// drop rows it cannot tell apart; that refusal happens before anything is read or written.
#[test]
fn a_grouped_leaf_without_a_row_symbol_is_merged_and_pruned_and_refused_only_under_a_policy() {
    for with_policy in [false, true] {
        let (dir, df) = plain_store();
        if with_policy {
            save_policy(dir.path(), &StoreSourcePolicy::new(["k"])).unwrap();
        }
        let bar = |ts: i64| Bar {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        };
        // Two dates, two parts each: a merge is due on both and retention will drop the older.
        for (k, ts) in [1_000, 2_000, DAY0, DAY0 + 1].into_iter().enumerate() {
            df.append_bars(V, "X", "1m", &[bar(ts)], Some(&format!("k{k}"))).unwrap();
        }
        let grouped = leaf(dir.path(), "bar", "group=g");
        std::fs::rename(leaf(dir.path(), "bar", "symbol=X").join("interval=1m"), &grouped).unwrap();
        let cfg = MaintenanceConfig {
            compaction: CompactionConfig {
                target_bytes: 1 << 20,
                min_parts: 2,
                ..Default::default()
            },
            retention: Some(RetentionPolicy { before_ts: Some(DAY0 - 1), max_age_ms: None }),
        };
        let rep = df.run_maintenance(&cfg).unwrap();

        if with_policy {
            assert_eq!(rep.failed.len(), 1, "{:?}", rep.failed);
            assert!(rep.failed[0].1.contains("carry no symbol"), "{:?}", rep.failed);
            assert_eq!(rep.compaction.parts_merged, 0);
            assert_eq!(parts_under(&grouped).len(), 4, "refused before a part was touched");
        } else {
            assert!(rep.failed.is_empty(), "{:?}", rep.failed);
            assert_eq!(rep.compaction.parts_merged, 4, "both dates merged");
            assert_eq!(rep.compaction.rows, 4, "every row kept");
            assert_eq!(rep.retention.files_dropped, 1, "and the older date pruned");
            let left = parts_under(&grouped);
            assert_eq!(left.len(), 1, "{left:?}");
            assert_eq!(file_ts(&left[0]), vec![DAY0, DAY0 + 1]);
        }
    }
}
