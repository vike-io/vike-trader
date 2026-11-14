//! The store catalog listings the slice picker draws: replayable ticks, depth-only disclosure, the
//! one-call fold. (The tick slices these rows pick are run on the backtest server; their tests are
//! `crates/vike-backtesthub/src/studio_run/tests/tick_slices.rs`.)

use std::sync::Arc;
use vike_data::StoreKindId;
use vike_data::{HistoryStore, HistoryStoreError};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_store::HistoryStoreDataFusion;

use super::*;

fn bar_at(ts: i64) -> Bar {
    vike_marketdata::test_support::flat_bar_zero_volume(ts, 1.0)
}

#[test]
fn tick_series_lists_each_venue_symbol_once_across_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let q = vec![TickQuote {
        timestamp: 1,
        local_timestamp: 1,
        bid: 1.0,
        ask: 1.1,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "TKN".into(),
    }];
    let t = vec![TickTrade {
        timestamp: 1,
        local_timestamp: 1,
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

/// One conflating-L2 snapshot, the shape `DataClientSink::l2_snapshot` records.
fn depth_snapshot(ts: i64, symbol: &str) -> L2BookUpdate {
    L2BookUpdate {
        timestamp: ts,
        local_timestamp: ts,
        sequence: 0,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(100.0, 1.0)],
        asks: vec![L2BookLevel::new(100.5, 1.0)],
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
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
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
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let t = vec![TickTrade {
        timestamp: 1,
        local_timestamp: 1,
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
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0), bar_at(60_000)], None).unwrap();
    let t = vec![TickTrade {
        timestamp: 1,
        local_timestamp: 1,
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
        !StoreKindId::TICK_LANES.contains(&UNREPLAYABLE_TICK_KIND),
        "a kind cannot both replay and be disclosed as unreplayable"
    );
}

/// split-plane B12: `bar_series`/`tick_series` are TRAIT enumerations — they must answer over
/// an `Arc<dyn HistoryStore + Send + Sync>` whose backing store is not a `HistoryStoreDataFusion` at all (the production case
/// is the RPC-backed `HistoryStoreDatahub`; here the smallest seeded fake stands in, because
/// `vike_data::HistoryStoreMemory` can only ever list the seams it stores for real —
/// it cannot hold the `kind=bar` series this test needs). Every verb except `list_series`
/// refuses, so the test also PROVES the two
/// functions touch nothing but the catalog verb.
#[test]
fn series_enumeration_needs_only_the_trait() {
    use vike_data::{SeriesCoverage, SeriesId};

    /// `list_series` answers from the seeded list; every other verb is a hard error.
    struct CatalogOnlyStore(Vec<SeriesId>);
    fn refuse(method: &str) -> HistoryStoreError {
        HistoryStoreError::Query(format!("CatalogOnlyStore: {method} must not be called"))
    }
    impl HistoryStore for CatalogOnlyStore {
        fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
            Ok(self.0.clone())
        }
        fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
            Err(refuse("inventory"))
        }
        vike_data::hist_store_stubs!(refuse(refuse): all);
    }

    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(CatalogOnlyStore(vec![
        SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m".to_string())),
        SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "TKN", None),
        SeriesId::per_symbol(StoreKindId::Trade, "polymarket", "TKN", None),
        // A lane no replay reads is no runnable slice.
        SeriesId::per_symbol(StoreKindId::Quote, "dukascopy", "EURUSD", None)
            .with_source("recorder"),
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
