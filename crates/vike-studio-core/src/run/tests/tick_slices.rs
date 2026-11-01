//! Tick slices and the store catalog listings: replayable ticks, depth-only disclosure, the fold.

use std::assert_matches;
use std::sync::Arc;
use vike_data::StoreKindId;
use vike_data::{DataError, HistStore, StoreHandle, TsRange};
use vike_marketdata::BookLevel;
use vike_marketdata::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};
use vike_sim::EngineParams;
use vike_store::DataFusionHist;

use super::support::{bar_at, rhai};
use crate::run::{
    DataSlice, REPLAYABLE_TICK_KINDS, RunError, UNREPLAYABLE_TICK_KIND, bar_series,
    depth_only_series, run_slice, run_walkforward_slice, series_lists, tick_series,
};
use crate::spec::{StrategySpec, params_from_rows};
use crate::test_support::cross_script::CROSS_SCRIPT;

// ---- tick slices -------------------------------------------------------------------------

fn tick_store() -> (tempfile::TempDir, StoreHandle) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let quotes: Vec<QuoteTick> = (0..200)
        .map(|i| QuoteTick {
            ts: 1_000 * (i as i64 + 1),
            local_ts: 1_000 * (i as i64 + 1),
            bid: 100.0 + (i % 5) as f64,
            ask: 100.5 + (i % 5) as f64,
            bid_size: 5.0,
            ask_size: 5.0,
            symbol: "TKN".to_string(),
        })
        .collect();
    let trades: Vec<TradeTick> = (0..200)
        .map(|i| TradeTick {
            ts: 1_000 * (i as i64 + 1),
            local_ts: 1_000 * (i as i64 + 1),
            price: 100.25 + (i % 5) as f64,
            size: 1.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".to_string(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).unwrap();
    store.append_trades("polymarket", "TKN", &trades, None).unwrap();
    (dir, Arc::new(store) as StoreHandle)
}

#[test]
fn tick_series_lists_each_venue_symbol_once_across_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let q = vec![QuoteTick {
        ts: 1,
        local_ts: 1,
        bid: 1.0,
        ask: 1.1,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "TKN".into(),
    }];
    let t = vec![TradeTick {
        ts: 1,
        local_ts: 1,
        price: 1.05,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "TKN".into(),
    }];
    store.append_quotes("polymarket", "TKN", &q, None).unwrap();
    store.append_trades("polymarket", "TKN", &t, None).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0)], None).unwrap();

    let ticks = tick_series(&store).unwrap();
    assert_eq!(ticks, vec![("polymarket".to_string(), "TKN".to_string())]);
    // and the bar filter still sees only the bar series
    assert_eq!(
        bar_series(&store).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())]
    );
}

/// One conflating-L2 snapshot, the shape `LiveDataSink::l2_snapshot` records.
fn depth_snapshot(ts: i64, symbol: &str) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts,
        seq: 0,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(100.0, 1.0)],
        asks: vec![BookLevel::new(100.5, 1.0)],
        symbol: symbol.to_string(),
    }
}

/// **The instrument that used to vanish.** Recorded through `subscribe_depth` alone, it is a
/// real series on disk that `tick_series` cannot replay — and it appeared in NO list, so the
/// Studio showed the same nothing for "not recorded" and for "recorded, unrunnable".
/// [`depth_only_series`] is that instrument's one appearance.
#[test]
fn a_depth_only_instrument_is_not_replayable_but_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_depth("binance", "BTCUSDT", &[depth_snapshot(1, "BTCUSDT")], None).unwrap();

    assert!(
        tick_series(&store).unwrap().is_empty(),
        "depth is the CONFLATING lane — replay_ticks reads no depth, so no runnable row exists"
    );
    assert_eq!(
        depth_only_series(&store).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "...and the instrument is NAMED rather than dropped in silence"
    );
}

/// The complement: an instrument with a REPLAYABLE lane is a tick row, so its depth series is
/// not a second entry anywhere. The disclosure reports what the filter COSTS, never a second
/// opinion about an instrument the picker already lists.
#[test]
fn depth_beside_a_replayable_lane_is_not_reported_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let t = vec![TradeTick {
        ts: 1,
        local_ts: 1,
        price: 100.25,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "BTCUSDT".into(),
    }];
    store.append_trades("binance", "BTCUSDT", &t, None).unwrap();
    store.append_depth("binance", "BTCUSDT", &[depth_snapshot(1, "BTCUSDT")], None).unwrap();

    assert_eq!(
        tick_series(&store).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "the trade tape makes it runnable"
    );
    assert!(depth_only_series(&store).unwrap().is_empty(), "already listed — nothing is lost");
}

/// THE equality pin of the one-call fold: [`series_lists`] answers exactly what the three
/// separate enumerations answer, over a store holding all three shapes at once (a bar series,
/// a replayable tick lane, and a depth-only instrument). Without a fixture carrying all three
/// the assertion would pass on empty vectors and prove nothing.
#[test]
fn series_lists_equals_the_three_separate_enumerations() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0), bar_at(60_000)], None).unwrap();
    let t = vec![TradeTick {
        ts: 1,
        local_ts: 1,
        price: 100.25,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "ETHUSDT".into(),
    }];
    store.append_trades("binance", "ETHUSDT", &t, None).unwrap();
    store.append_depth("okx", "SOL-USDT", &[depth_snapshot(1, "SOL-USDT")], None).unwrap();

    let one = series_lists(&store).unwrap();
    assert_eq!(one.bars, bar_series(&store).unwrap());
    assert_eq!(one.ticks, tick_series(&store).unwrap());
    assert_eq!(one.depth_only, depth_only_series(&store).unwrap());
    // ...and the fixture actually exercises all three lists, so the equality above is not
    // three comparisons of empty vectors.
    assert_eq!(one.bars.len(), 1);
    assert_eq!(one.ticks, vec![("binance".to_string(), "ETHUSDT".to_string())]);
    assert_eq!(one.depth_only, vec![("okx".to_string(), "SOL-USDT".to_string())]);
}

/// The two sets are complements over the tick lanes, not two independently maintained lists:
/// every `kind=` either replays or is the one disclosed exclusion, and nothing is both.
#[test]
fn the_replayable_and_disclosed_tick_kinds_do_not_overlap() {
    assert!(
        !REPLAYABLE_TICK_KINDS.contains(&UNREPLAYABLE_TICK_KIND),
        "a kind cannot both replay and be disclosed as unreplayable"
    );
}

/// A `SliceKind::Ticks` slice routes to `replay_ticks` (NOT `load_bars`): the store below has
/// no bar series at all, so a bar-path run could not possibly produce this result.
#[test]
fn tick_slice_replays_recorded_ticks() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
    let spec =
        StrategySpec::native("buy_hold", params_from_rows(&[("symbol".into(), "TKN".into())]));
    let res =
        run_slice(&spec, &slice, &store, EngineParams::default()).expect("tick replay should run");
    assert!(!res.equity_curve.is_empty(), "the tick replay stamps an equity curve");
}

#[test]
fn tick_slice_with_no_recorded_ticks_is_a_data_error() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["MISSING".to_string()], TsRange::all());
    let out = run_slice(&rhai("fn on_bar() {}"), &slice, &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Data(_)), "got {out:?}");
}

#[test]
fn a_bar_only_runner_rejects_a_tick_slice_instead_of_panicking() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
    assert_matches!(
        run_walkforward_slice(&rhai(CROSS_SCRIPT), &slice, &store, 4),
        Err(RunError::Data(_))
    );
}

/// split-plane B12: `bar_series`/`tick_series` are TRAIT enumerations — they must answer over
/// a [`StoreHandle`] whose backing store is not a `DataFusionHist` at all (the production case
/// is the RPC-backed `RemoteHistStore`; here the smallest seeded fake stands in, because
/// `vike_data::MemHistStore` can only ever list the seams it stores for real —
/// it cannot hold the `kind=bar` series this test needs). Every verb except `list_series`
/// refuses, so the test also PROVES the two
/// functions touch nothing but the catalog verb.
#[test]
fn series_enumeration_needs_only_the_trait() {
    use vike_data::{SeriesCoverage, SeriesId};

    /// `list_series` answers from the seeded list; every other verb is a hard error.
    struct CatalogOnlyStore(Vec<SeriesId>);
    fn refuse(method: &str) -> DataError {
        DataError::Query(format!("CatalogOnlyStore: {method} must not be called"))
    }
    impl HistStore for CatalogOnlyStore {
        fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
            Ok(self.0.clone())
        }
        fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
            Err(refuse("inventory"))
        }
        vike_data::hist_store_stubs!(refuse(refuse): all);
    }

    let store: StoreHandle = Arc::new(CatalogOnlyStore(vec![
        SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m".to_string())),
        SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "TKN", None),
        SeriesId::per_symbol(StoreKindId::Trade, "polymarket", "TKN", None),
    ]));

    assert_eq!(
        bar_series(store.as_ref()).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())]
    );
    // quote + trade for the same (venue, symbol) still dedupe to ONE runnable tick row.
    assert_eq!(
        tick_series(store.as_ref()).unwrap(),
        vec![("polymarket".to_string(), "TKN".to_string())]
    );
}
