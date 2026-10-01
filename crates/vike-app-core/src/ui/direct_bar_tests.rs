use super::test_support::*;
use super::*;
use vike_core::CoreSnapshot;

const KEY: &str = "BTCUSDT@1m";

/// One third-mode frame: the real fold with the store MOUNTED.
fn frame(st: &mut State, snap: &CoreSnapshot, spawned: &HashSet<String>, store: &DirectBarStore) {
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    sync_direct(st, snap, spawned, &HashSet::new(), &trades, &rx, &fs, store);
}

/// THE UNIQUENESS PIN, direct-bar edition — both inputs driven, one wins: a binance kline
/// series present in `snap.bars` AND in the store paints from the STORE, and a later
/// snapshot bump (with the direct fold quiescent — store generation unchanged) must NOT
/// repaint it from the snapshot: the seq-gated kline fold really skips the series, rather
/// than being papered over by the direct fold running afterwards.
#[test]
fn a_direct_bars_series_never_folds_from_snap_bars_even_when_both_carry_it() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(60_000, 10.0), core_bar(120_000, 10.0), core_bar(180_000, 10.0)]),
    );
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 99.0), core_bar(120_000, 99.0)]);

    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    let spawned = keys(&[KEY]);

    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 2, "the chart paints the STORE's two bars");
    assert_eq!(st.charts[KEY].bars[0].o, 99.0, "…with the store's prices, not the snapshot's");
    let (closed, _) = store.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed[0].close, 99.0, "a non-empty series refuses the backend tail");

    // The snapshot bumps; the store does not. A broken guard would repaint 3 backend bars.
    snap.seq = 2;
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 2, "the snapshot fold must SKIP the DirectBars key");
    assert_eq!(st.charts[KEY].bars[0].o, 99.0);
}

/// ⚠ **THE BACKEND-STORE PLANE — the shipped desktop's, and the OPPOSITE split on the same two
/// inputs.** Under `BarPlane::VenueFeeds` the store wins a binance kline because the VENUE
/// feeds it; under `BarPlane::BackendStore` the LIVE SNAPSHOT wins that same series, because
/// the daemon is publishing it — and the store serves only the key the daemon does not. Both
/// halves in one frame, so a regression to the venue predicate (which would hand the published
/// 1m series to the store) reddens here rather than only in the pure decision test.
#[test]
fn under_the_backend_store_plane_the_published_series_folds_live_and_the_rest_from_the_store() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    // The daemon streams binance 1m…
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
    );
    // …and a store read has landed 1m (a stale read of the same key) AND 5m (the interval
    // nobody streams). Only the 5m one may reach a chart.
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 99.0)]);
    store.seed("binance", "BTCUSDT", "5m", vec![core_bar(300_000, 42.0), core_bar(600_000, 43.0)]);

    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    st.charts.insert("BTCUSDT@5m".to_string(), model::ChartState::default());
    let spawned = keys(&[KEY, "BTCUSDT@5m"]);
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    sync_backend_store(&mut st, &snap, &spawned, &HashSet::new(), &trades, &rx, &fs, &store);

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

/// The pre-existing modes are unchanged: with the store UNMOUNTED (fat local / thin observe
/// — `sync`, not `sync_direct`) the same snapshot folds the kline series exactly as before.
#[test]
fn without_the_store_mounted_the_same_snapshot_folds_as_before() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
    );
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &keys(&[KEY]), &HashSet::new(), &trades, &rx, &fs);
    assert_eq!(st.charts[KEY].bars.len(), 2, "unmounted store: snapshot klines fold as ever");
    assert_eq!(st.charts[KEY].bars[0].o, 10.0);
}

/// A kline series on a venue WITHOUT a local bar feed (polymarket refuses `subscribe_bars`;
/// deribit mounts no local feed at all) keeps the backend tail in the third mode — folded
/// from `snap.bars`, with nothing seeded into the store for it.
#[test]
fn a_venue_without_a_local_bar_feed_keeps_the_snapshot_tail_in_the_third_mode() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    for venue in ["polymarket", "deribit"] {
        snap.bars.insert(
            (venue.to_string(), "X".to_string(), "1m".to_string()),
            series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
        );
    }
    let store = DirectBarStore::default();
    let mut st = State::default();
    st.charts.insert("polymarket:X@1m".to_string(), model::ChartState::default());
    st.charts.insert("deribit:X@1m".to_string(), model::ChartState::default());
    let spawned = keys(&["polymarket:X@1m", "deribit:X@1m"]);

    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts["polymarket:X@1m"].bars.len(), 2, "backend tail renders");
    assert_eq!(st.charts["deribit:X@1m"].bars.len(), 2, "backend tail renders");
    assert!(store.keys().is_empty(), "nothing is seeded into the store for snapshot venues");
}

/// THE HISTORY SEAM, end to end: a direct series whose venue feed serves no warmup
/// (hyperliquid) is seeded ONCE from the backend's streamed tail, live closes then append
/// through the store's boundary-ts dedup rule, and a later snapshot cannot re-apply the
/// tail — the boundary bar appears exactly once, updated to the venue's own close.
#[test]
fn the_backend_tail_seeds_an_empty_direct_series_once_then_live_bars_append() {
    let hl_key = "hyperliquid:BTC@1m";
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("hyperliquid".to_string(), "BTC".to_string(), "1m".to_string()),
        series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
    );
    let store = DirectBarStore::default();
    let mut st = State::default();
    st.charts.insert(hl_key.to_string(), model::ChartState::default());
    let spawned = keys(&[hl_key]);

    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[hl_key].bars.len(), 2, "the tail seeds the empty series and paints");

    // The venue re-closes the boundary window, then closes the next — the live splice.
    store.close("hyperliquid", "BTC", "1m", core_bar(120_000, 11.5));
    store.close("hyperliquid", "BTC", "1m", core_bar(180_000, 12.0));
    frame(&mut st, &snap, &spawned, &store); // seq unchanged — the tape-cadence fold paints
    assert_eq!(
        st.charts[hl_key].bars.len(),
        3,
        "the boundary bar appears exactly ONCE (replaced, never duplicated) and the next \
             bar appends — the double-paint class this seam exists to prevent"
    );
    assert_eq!(st.charts[hl_key].bars[2].c, 12.0, "the live splice renders");
    // The DATA plane holds the venue's own close at the boundary. (The RENDER of that one
    // bar keeps the seeded value until any full rebuild: `ChartState::sync`'s incremental
    // key is `(len, first_ts, last_ts)`, blind to an in-place same-ts replacement — the
    // SAME trade-off the fat arm's snapshot path has for the core's idempotent re-close. In
    // practice both sides of the boundary are the same venue kline, so the replace is
    // content-identical and the trade-off invisible.)
    let (closed, _) = store.series("hyperliquid", "BTC", "1m").unwrap();
    assert_eq!(closed[1].close, 11.5, "the venue's own close wins the boundary in the store");

    // A later snapshot (same tail) must not re-seed under the accumulated venue bars.
    snap.seq = 2;
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[hl_key].bars.len(), 3, "the tail applies ONCE, never again");
    let (closed, _) = store.series("hyperliquid", "BTC", "1m").unwrap();
    assert_eq!(closed[1].close, 11.5, "…and the store still holds the venue's boundary close");
}

/// The direct fold runs on the tape cadence, OUTSIDE the `snap.seq` gate (bug-3's precedent):
/// a store write with an unchanged snapshot repaints the chart on the next frame.
#[test]
fn the_direct_fold_runs_even_when_the_snapshot_seq_did_not_advance() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT"); // seq 0 == last_seq 0: gate closed
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 10.0)]);
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    let spawned = keys(&[KEY]);

    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 1);
    store.close("binance", "BTCUSDT", "1m", core_bar(120_000, 11.0));
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 2, "a live close paints without any seq bump");
}

/// The GUI-side gates hold for the direct fold exactly as for the snapshot fold: an
/// unspawned series neither paints nor seeds (no store pollution from the backend tail),
/// and a hidden one stops painting.
#[test]
fn the_direct_fold_respects_spawned_and_hidden() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(60_000, 10.0)]),
    );
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 10.0)]);
    let mut st = State::default();
    st.charts.insert(KEY.to_string(), model::ChartState::default());
    st.charts.insert("ETHUSDT@1m".to_string(), model::ChartState::default());

    // ETHUSDT is in the snapshot but UNSPAWNED: no paint, and no tail seeded into the store.
    frame(&mut st, &snap, &keys(&[KEY]), &store);
    assert_eq!(st.charts[KEY].bars.len(), 1, "the spawned series paints");
    assert_eq!(st.charts["ETHUSDT@1m"].bars.len(), 0, "unspawned: skipped");
    assert!(
        store.series("binance", "ETHUSDT", "1m").is_none(),
        "an unspawned series must not have the backend tail seeded for it"
    );

    // Hidden: the store still holds the series, the chart stops receiving it.
    let mut hidden_st = State::default();
    hidden_st.charts.insert(KEY.to_string(), model::ChartState::default());
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    let hidden = keys(&[KEY]);
    sync_direct(&mut hidden_st, &snap, &keys(&[KEY]), &hidden, &trades, &rx, &fs, &store);
    assert_eq!(hidden_st.charts[KEY].bars.len(), 0, "hidden: skipped");
}

/// The generation gate's accepted residual, pinned: a chart entry (re)created AFTER the last
/// store write stays empty until the store's next write — and that next write (a forming
/// snapshot, ~1 s on any live symbol; or a fresh seed on re-subscribe) heals it. Same class
/// as a reopened tick/vol chart waiting for its next trade.
#[test]
fn a_recreated_chart_self_heals_on_the_next_store_write() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let store = DirectBarStore::default();
    store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 10.0)]);
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

    // …and the venue's next forming frame repaints it.
    store.forming("binance", "BTCUSDT", "1m", core_bar(120_000, 10.5));
    frame(&mut st, &snap, &spawned, &store);
    assert_eq!(st.charts[KEY].bars.len(), 2, "the next store write self-heals the chart");
}
