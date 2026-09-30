use super::test_support::*;
use super::*;
use vike_core::CoreSnapshot;
use vike_orderflow::tickvol::BarKind;

/// A series whose key is not in `spawned` (never subscribed GUI-side) or is in `hidden`
/// (Data-manager delete) is skipped, even though the chart slot exists.
#[test]
fn unspawned_and_hidden_series_are_skipped() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    for sym in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        snap.bars.insert(
            ("binance".to_string(), sym.to_string(), "1m".to_string()),
            series(&[core_bar(1_000, 10.0)]),
        );
    }
    let mut st = State::default();
    for sym in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        st.charts.insert(format!("{sym}@1m"), model::ChartState::default());
    }
    let spawned = keys(&["BTCUSDT@1m", "ETHUSDT@1m"]); // SOLUSDT never subscribed
    let hidden = keys(&["ETHUSDT@1m"]); // ETHUSDT deleted GUI-side
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &spawned, &hidden, &trades, &rx, &fs);

    assert_eq!(st.charts["BTCUSDT@1m"].bars.len(), 1, "spawned + not hidden folds");
    assert_eq!(st.charts["ETHUSDT@1m"].bars.len(), 0, "hidden is skipped");
    assert_eq!(st.charts["SOLUSDT@1m"].bars.len(), 0, "unspawned is skipped");
    assert_eq!(st.last_seq, 1, "the seq is still recorded as folded");
}

/// "One drain, N consumers": a tick/volume aggregator and an orderflow aggregator on the
/// SAME `(venue, symbol)` both see the same batch — the tape is drained once per pair, not
/// once per consumer (which would give the second consumer nothing).
#[test]
fn one_drain_feeds_both_the_tickvol_and_the_orderflow_aggregator_on_a_pair() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    st.charts.insert("BTCUSDT@2t".to_string(), model::ChartState::default());
    st.charts.insert("BTCUSDT@1m".to_string(), chart_with_ots(&[1_000]));
    st.aggs.insert(
        "BTCUSDT@2t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            TickVolAgg::new(&BarKind::Tick(2)).expect("Tick(2) is an aggregator kind"),
        ),
    );
    st.of_aggs.insert(
        "BTCUSDT@1m".to_string(),
        ("binance".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
    );

    let trades = TradeStore::default();
    trades.push("binance", &trade("BTCUSDT", 2_000, 10.0, 2.0));
    trades.push("binance", &trade("BTCUSDT", 2_001, 10.0, 2.0));
    let (_tx, rx) = bf_channel();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &keys(&["BTCUSDT@2t", "BTCUSDT@1m"]), &HashSet::new(), &trades, &rx, &fs);

    assert_eq!(st.aggs["BTCUSDT@2t"].2.closed.len(), 1, "tick/vol saw both trades");
    assert_eq!(of_total(&st.of_aggs["BTCUSDT@1m"].2), 4.0, "orderflow saw the SAME two trades");
}

/// Backfill batches are Binance-only BY DESIGN: a batch for `symbol` only ever reaches the
/// `(DEFAULT_VENUE, symbol)` orderflow entry. A non-Binance orderflow chart on the same symbol
/// is live-only and must receive nothing from the backfill lane.
#[test]
fn backfill_batches_reach_only_the_default_venue_orderflow_entry() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    for (key, venue) in [("BTCUSDT@1m", DEFAULT_VENUE), ("okx:BTCUSDT@1m", "okx")] {
        st.charts.insert(key.to_string(), chart_with_ots(&[1_000]));
        st.of_aggs.insert(
            key.to_string(),
            (venue.to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
        );
    }

    let (tx, rx) = bf_channel();
    tx.send(("BTCUSDT".to_string(), vec![trade("BTCUSDT", 1_500, 10.0, 5.0)])).unwrap();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(
        &mut st,
        &snap,
        &keys(&["BTCUSDT@1m", "okx:BTCUSDT@1m"]),
        &HashSet::new(),
        &trades,
        &rx,
        &fs,
    );

    assert_eq!(of_total(&st.of_aggs["BTCUSDT@1m"].2), 5.0, "the Binance entry gets the batch");
    assert_eq!(
        of_total(&st.of_aggs["okx:BTCUSDT@1m"].2),
        0.0,
        "a non-Binance orderflow chart is live-only — no backfill"
    );
}

/// A core fault outranks the feed-status line; with no fault the status mirrors the feed.
#[test]
fn status_shows_the_core_fault_when_set_and_the_feed_status_otherwise() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new("binance: connected".to_string());

    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
    assert_eq!(st.status, "binance: connected");

    snap.fault = Some("handler panicked".to_string());
    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
    assert_eq!(st.status, "CORE FAULT: handler panicked");
}

/// The whole fold over empty state is a genuine no-op — the property the "drain every frame"
/// fix (bug 3) rests on: an empty `aggs`/`of_aggs` plus a non-blocking `try_iter` costs
/// nothing, so running it unconditionally is free.
#[test]
fn an_empty_frame_is_a_no_op() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
    assert!(st.charts.is_empty() && st.aggs.is_empty() && st.of_aggs.is_empty());
    assert!(st.bf_pending.is_empty(), "an empty frame stages nothing");
    assert_eq!(st.last_seq, 0, "seq 0 == last_seq 0 leaves the gate closed");
}

/// **THE DOUBLE-FOLD GUARD (split-plane B2)** — a series present in BOTH `snap.bars` and the
/// local tick path renders from exactly ONE source, decided by
/// `split_plane::series_render_source`. A tick-interval key (`"…@100t"`) is tape-rendered:
/// its chart folds from the `TickVolAgg` drain and the snapshot fold must SKIP a bar series a
/// (remote) snapshot publishes under that key — while a kline key in the same snapshot still
/// folds normally. Without the guard the same `ChartState` would be `sync`ed from two sources
/// in one frame, which is the exact bug class `clear_session_state`'s clear-list exists for.
#[test]
fn a_snapshot_bar_series_under_a_tick_interval_never_repaints_a_tape_rendered_chart() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "100t".to_string()),
        series(&[core_bar(1_000, 10.0), core_bar(2_000, 11.0)]),
    );
    snap.bars.insert(
        ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(1_000, 10.0)]),
    );

    let mut st = State::default();
    st.charts.insert("BTCUSDT@100t".to_string(), model::ChartState::default());
    st.charts.insert("BTCUSDT@1m".to_string(), model::ChartState::default());
    st.aggs.insert(
        "BTCUSDT@100t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            vike_orderflow::tickvol::TickVolAgg::new(&BarKind::Tick(100)).expect("tick agg"),
        ),
    );
    let spawned = keys(&["BTCUSDT@100t", "BTCUSDT@1m"]);
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &spawned, &HashSet::new(), &trades, &rx, &fs);

    assert_eq!(
        st.charts["BTCUSDT@100t"].bars.len(),
        0,
        "the tick-interval chart is TAPE-rendered — snapshot bars under its key are skipped"
    );
    assert_eq!(
        st.charts["BTCUSDT@1m"].bars.len(),
        1,
        "the kline series in the same snapshot still folds — the guard is per series"
    );
}
