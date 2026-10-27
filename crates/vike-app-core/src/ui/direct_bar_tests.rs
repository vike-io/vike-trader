use super::test_support::*;
use super::*;
use vike_exec::CoreSnapshot;
use vike_marketdata::test_support::flat_bar_unit_volume;

const KEY: &str = "BTCUSDT@1m";

/// One shipped-desktop frame: the real fold with the store MOUNTED under
/// `BarPlane::BackendStore`.
fn frame(st: &mut State, snap: &CoreSnapshot, spawned: &HashSet<String>, store: &DirectBarStore) {
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    sync_backend_store(st, snap, spawned, &HashSet::new(), &trades, &fs, store);
}

/// ⚠ **THE BACKEND-STORE PLANE — the shipped desktop's split, on both inputs populated.** The
/// LIVE SNAPSHOT wins a series the daemon is publishing, and the store serves only the key the
/// daemon does not. Both halves in one frame, so a regression to a per-venue predicate (which
/// would hand the published 1m series to the store) reddens here rather than only in the pure
/// decision test.
#[test]
fn under_the_backend_store_plane_the_published_series_folds_live_and_the_rest_from_the_store() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    // The daemon streams binance 1m…
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[flat_bar_unit_volume(60_000, 10.0), flat_bar_unit_volume(120_000, 11.0)]),
    );
    // …and a store read has landed 1m (a stale read of the same key) AND 5m (the interval
    // nobody streams). Only the 5m one may reach a chart.
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 99.0)]);
    store.seed(
        "binance",
        "BTCUSDT",
        "5m",
        vec![flat_bar_unit_volume(300_000, 42.0), flat_bar_unit_volume(600_000, 43.0)],
    );

    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    st.charts.insert("BTCUSDT@5m".to_string(), model::ChartState::default());
    let spawned = keys(&[KEY, "BTCUSDT@5m"]);
    frame(&mut st, &snap, &spawned, &store);

    assert_eq!(st.charts[KEY].bars.len(), 2, "the PUBLISHED series folds from the snapshot");
    assert_eq!(
        st.charts[KEY].bars[0].o, 10.0,
        "…with the daemon's prices — the store's stale copy of the same key may not win"
    );
    assert_eq!(
        st.charts["BTCUSDT@5m"].bars.len(),
        2,
        "THE DEFECT: the interval the daemon does not stream paints from the backend's store"
    );
    assert_eq!(st.charts["BTCUSDT@5m"].bars[0].o, 42.0);
}

/// The pre-existing mode is unchanged: with the store UNMOUNTED (`sync`, not `frame`) the same
/// snapshot folds the kline series exactly as before.
#[test]
fn without_the_store_mounted_the_same_snapshot_folds_as_before() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[flat_bar_unit_volume(60_000, 10.0), flat_bar_unit_volume(120_000, 11.0)]),
    );
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &keys(&[KEY]), &HashSet::new(), &trades, &fs);
    assert_eq!(st.charts[KEY].bars.len(), 2, "unmounted store: snapshot klines fold as ever");
    assert_eq!(st.charts[KEY].bars[0].o, 10.0);
}

/// A kline series the backend PUBLISHES keeps the backend tail on EVERY venue — folded from
/// `snap.bars`, with nothing seeded into the store for it. (No venue list decides this: the
/// daemon's publication does.)
#[test]
fn a_published_series_keeps_the_snapshot_tail_on_every_venue() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    let venues = ["polymarket", "deribit", "hyperliquid"];
    for venue in venues {
        snap.bars.insert(
            (venue.to_string(), "X".to_string(), "1m".to_string()),
            series(&[flat_bar_unit_volume(60_000, 10.0), flat_bar_unit_volume(120_000, 11.0)]),
        );
    }
    let store = DirectBarStore::default();
    let mut st = State::default();
    let mut spawned = Vec::new();
    for venue in venues {
        let key = format!("{venue}:X@1m");
        st.charts.insert(key.clone(), model::ChartState::default());
        spawned.push(key);
    }
    let spawned: HashSet<String> = spawned.into_iter().collect();

    frame(&mut st, &snap, &spawned, &store);
    for venue in venues {
        assert_eq!(
            st.charts[&format!("{venue}:X@1m")].bars.len(),
            2,
            "{venue}: backend tail renders"
        );
    }
    assert!(store.keys().is_empty(), "nothing is seeded into the store for a published series");
}

/// The store fold runs on the tape cadence, OUTSIDE the `snap.seq` gate (bug-3's precedent): a
/// store write with an unchanged snapshot repaints the chart on the next frame.
#[test]
fn the_direct_fold_runs_even_when_the_snapshot_seq_did_not_advance() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT"); // seq 0 == last_seq 0: gate closed
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 10.0)]);
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    let spawned = keys(&[KEY]);

    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 1);
    store.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.0));
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 2, "a store write paints without any seq bump");
}

/// The GUI-side gates hold for the store fold exactly as for the snapshot fold: an unspawned
/// series neither paints nor reaches the store, and a hidden one stops painting.
#[test]
fn the_direct_fold_respects_spawned_and_hidden() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string()),
        series(&[flat_bar_unit_volume(60_000, 10.0)]),
    );
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 10.0)]);
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    st.charts.insert("ETHUSDT@1m".to_string(), model::ChartState::default());

    // ETHUSDT is in the snapshot but UNSPAWNED: no paint, and nothing lands in the store.
    frame(&mut st, &snap, &keys(&[KEY]), &store);
    assert_eq!(st.charts[KEY].bars.len(), 1, "the spawned series paints");
    assert_eq!(st.charts["ETHUSDT@1m"].bars.len(), 0, "unspawned: skipped");
    assert!(
        store.series("binance", "ETHUSDT", "1m").is_none(),
        "an unspawned series must not reach the store"
    );

    // Hidden: the store still holds the series, the chart stops receiving it.
    let mut hidden_st = State::default();
    hidden_st.charts.insert(KEY.to_string(), model::ChartState::default());
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    let hidden = keys(&[KEY]);
    sync_backend_store(&mut hidden_st, &snap, &keys(&[KEY]), &hidden, &trades, &fs, &store);
    assert_eq!(hidden_st.charts[KEY].bars.len(), 0, "hidden: skipped");
}

/// The generation gate's accepted residual, pinned: a chart entry (re)created AFTER the last
/// store write stays empty until the store's next write — and that next write (a fresh read of
/// the backend's store on re-subscribe) heals it. Same class as a reopened tick/vol chart
/// waiting for its next trade.
#[test]
fn a_recreated_chart_self_heals_on_the_next_store_write() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 10.0)]);
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    let spawned = keys(&[KEY]);

    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 1);

    // The window is closed and reopened within the store's quiet period: the fresh entry
    // stays empty this frame (the residual)…
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 0, "generation unchanged: the residual, as pinned");

    // …and the next store write repaints it.
    store.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 10.5));
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 2, "the next store write self-heals the chart");
}
