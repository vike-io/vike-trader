//! The cross-kind coverage report over a real store, and the recorder's grouped flushes.

use std::time::Duration;

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{QuoteTick, TradeTick};

use crate::common::walk_parquet;

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
    assert_eq!(report[0].kinds.len(), vike_data::store::coverage::TICK_KINDS.len());
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
