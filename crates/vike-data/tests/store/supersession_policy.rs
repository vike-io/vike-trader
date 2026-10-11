//! Source-ranked supersession at compaction, and the store's persisted source policy.

use vike_data::{
    CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, SourceRankPolicy, TsRange,
};
use vike_model::{BookLevel, BookUpdateKind, TradeTick};

use crate::common::{bu, tt};

// ---- source-ranked window supersession at compaction (opt-in) --------------------------------
// The Polymarket writers (RecorderSink "live-…", pmxt_backfill "pmxt:…", vike_archive_backfill
// "vikearchive:…", poly_reparse "its own" until docs/decisions/0094 deleted it 2026-09-28) target
// the SAME venue=polymarket/symbol=<token_id> series with DISJOINT commit-key namespaces, so overlapping
// capture windows DOUBLE-COUNT rows. The "clickhouse:…" namespace below is a fifth, now
// WRITER-LESS one — clickhouse_poly_backfill minted it and was retired 2026-09-19 — kept in these
// fixtures because parts carrying it are still on disk and still have to rank.
// `compact_series_superseding` keeps one row per natural key by
// source precedence; plain `compact_series` keeps every duplicate (byte-identical to today).

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
            bu(
                500,
                4,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.45, 10.0)],
                vec![BookLevel::new(0.46, 20.0)],
            ),
            bu(
                1_000,
                5,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.45, 100.0)],
                vec![BookLevel::new(0.46, 50.0)],
            ),
        ],
        Some("live-polymarket-TOK-book-1-2-3"),
    )
    .unwrap();
    // pmxt: the SAME (1000,5) event with DIFFERENT sizes + a pmxt-only event (2000,6)
    df.append_book_updates(
        "polymarket",
        "TOK",
        &[
            bu(
                1_000,
                5,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.45, 999.0)],
                vec![BookLevel::new(0.46, 555.0)],
            ),
            bu(
                2_000,
                6,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.43, 3.0)],
                vec![BookLevel::new(0.47, 4.0)],
            ),
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
    assert_eq!(
        mid.bids,
        vec![BookLevel::new(0.45, 100.0)],
        "live's event survived (not pmxt's 999.0)"
    );
    assert_eq!(mid.asks, vec![BookLevel::new(0.46, 50.0)]);
    // the non-duplicate events are untouched
    assert_eq!(got.iter().find(|u| u.seq == 4).unwrap().bids, vec![BookLevel::new(0.45, 10.0)]);
    assert_eq!(got.iter().find(|u| u.seq == 6).unwrap().bids, vec![BookLevel::new(0.43, 3.0)]);
}

// ---- bit-exact trade comparison ---------------------------------------------------------------

/// `want` and `got` are the same trades to the last bit: count, ts, `to_bits`-equal price and size,
/// side flag and symbol, in the same order.
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
