//! Symbol as a row column, grouped series, and migrating a per-symbol series into its group.

use vike_data::{
    BulkConfig, CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, TsRange,
};
use vike_model::{BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use crate::common::{tt, walk_parquet};

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
    // Interleaved on purpose: the INPUT is ts-ordered with symbols mixed, and the grouped writer
    // (`append_grouped`) is what sorts it symbol-major before the part is written.
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
        bids: vec![BookLevel::new(0.45, 10.0), BookLevel::new(0.44, 20.0)],
        asks: vec![BookLevel::new(0.46, 5.0), BookLevel::new(0.47, 7.0)],
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
    assert_eq!(a[0].bids[0].price.to_bits(), 0.45f64.to_bits());
    assert_eq!(a[0].asks[1].qty.to_bits(), 7.0f64.to_bits());
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
        bids: vec![BookLevel::new(0.5, 1.0)],
        asks: vec![BookLevel::new(0.51, 1.0)],
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
            bids: vec![BookLevel::new(0.4, 1.0)],
            asks: vec![BookLevel::new(0.6, 1.0)],
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
