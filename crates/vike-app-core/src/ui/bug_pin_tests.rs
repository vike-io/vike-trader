use super::test_support::*;
use super::*;
use vike_core::CoreSnapshot;
use vike_orderflow::tickvol::BarKind;

/// **BUG 1 PIN** — a non-Binance series must fold into ITS OWN venue-prefixed `charts` slot,
/// and must NOT reach a same-named Binance chart. The old fold keyed with
/// `format!("{symbol}@{interval}")`, which sent every venue's bars to the Binance-shaped key:
/// the OKX chart stayed empty forever and the Binance chart got OKX's candles.
#[test]
fn bug1_pin_non_binance_bars_fold_into_the_venue_prefixed_key_not_the_binance_one() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 1;
    snap.bars.insert(
        ("okx".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(1_000, 10.0), core_bar(2_000, 11.0)]),
    );

    let mut st = State::default();
    // Both charts exist and are both subscribed; only the OKX one may receive these bars.
    st.charts.insert("okx:BTCUSDT@1m".to_string(), model::ChartState::default());
    st.charts.insert("BTCUSDT@1m".to_string(), model::ChartState::default());
    let spawned = keys(&["okx:BTCUSDT@1m", "BTCUSDT@1m"]);
    let (_tx, rx) = bf_channel();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &spawned, &HashSet::new(), &trades, &rx, &fs);

    assert_eq!(
        st.charts["okx:BTCUSDT@1m"].bars.len(),
        2,
        "the OKX series must fold into the venue-prefixed key"
    );
    assert_eq!(
        st.charts["BTCUSDT@1m"].bars.len(),
        0,
        "the Binance chart must NOT receive the OKX venue's bars (the old venue-discarding key)"
    );
    assert_eq!(
        st.charts["okx:BTCUSDT@1m"].symbol, "okx:BTCUSDT@1m",
        "ChartState::symbol carries the full venue-aware series key"
    );
}

/// **BUG 2 PIN** — orderflow must group by the `(venue, symbol)` STORED in the `of_aggs`
/// value, never by parsing the chart key. The old code split the key at the first `'@'`,
/// turning `"okx:BTCUSDT@1m"` into the symbol `"okx:BTCUSDT"` — a pair the `TradeStore` never
/// holds — so every non-Binance orderflow chart silently received nothing, forever.
#[test]
fn bug2_pin_orderflow_groups_by_the_stored_venue_symbol_not_by_splitting_the_key_at_at() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    st.charts.insert("okx:BTCUSDT@1m".to_string(), chart_with_ots(&[1_000]));
    st.of_aggs.insert(
        "okx:BTCUSDT@1m".to_string(),
        ("okx".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
    );

    let trades = TradeStore::default();
    trades.push("okx", &trade("BTCUSDT", 2_000, 10.0, 3.0));
    let (_tx, rx) = bf_channel();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &keys(&["okx:BTCUSDT@1m"]), &HashSet::new(), &trades, &rx, &fs);

    let agg = &st.of_aggs["okx:BTCUSDT@1m"].2;
    assert_eq!(
        of_total(agg),
        3.0,
        "the OKX orderflow agg must be fed from the ('okx','BTCUSDT') tape it stores"
    );
    assert!(agg.generation() > 0, "the footprint cache must have actually rebuilt");
}

/// **BUG 2 PIN (the other half)** — the two venues' tapes for the SAME symbol stay separate:
/// a Binance trade must never reach the OKX aggregator, and vice versa. The key-splitting bug
/// and any future "just use the symbol" shortcut both break this.
#[test]
fn bug2_pin_two_venues_same_symbol_orderflow_never_cross_feed() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    for (key, venue) in [("okx:BTCUSDT@1m", "okx"), ("BTCUSDT@1m", "binance")] {
        st.charts.insert(key.to_string(), chart_with_ots(&[1_000]));
        st.of_aggs.insert(
            key.to_string(),
            (venue.to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
        );
    }

    let trades = TradeStore::default();
    trades.push("okx", &trade("BTCUSDT", 2_000, 10.0, 3.0));
    trades.push("binance", &trade("BTCUSDT", 2_000, 10.0, 7.0));
    let (_tx, rx) = bf_channel();
    let fs = Mutex::new(String::new());

    sync(
        &mut st,
        &snap,
        &keys(&["okx:BTCUSDT@1m", "BTCUSDT@1m"]),
        &HashSet::new(),
        &trades,
        &rx,
        &fs,
    );

    assert_eq!(of_total(&st.of_aggs["okx:BTCUSDT@1m"].2), 3.0, "OKX gets only OKX's tape");
    assert_eq!(of_total(&st.of_aggs["BTCUSDT@1m"].2), 7.0, "Binance gets only Binance's tape");
}

/// **BUG 3 PIN** — the tick/volume + orderflow drain must run even when `snap.seq` did NOT
/// advance. It used to sit inside the `seq` gate, so a workspace with ONLY tick/volume charts
/// (nothing bumps `seq`) stalled: bars stopped forming until unrelated core activity happened
/// to publish a new snapshot. The same test also pins that the KLINE half is still gated —
/// a spawned+charted kline series in the snapshot must NOT re-fold on an unchanged `seq`.
#[test]
fn bug3_pin_tick_volume_drain_runs_even_when_the_snapshot_seq_did_not_advance() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 7;
    snap.bars.insert(
        ("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string()),
        series(&[core_bar(1_000, 10.0)]),
    );

    // `last_seq == snap.seq`: this snapshot is already folded, so the kline gate is CLOSED
    // for this frame — the whole point of the pin.
    let mut st = State { last_seq: 7, ..Default::default() };
    st.charts.insert("ETHUSDT@1m".to_string(), model::ChartState::default());
    st.charts.insert("BTCUSDT@2t".to_string(), model::ChartState::default());
    st.aggs.insert(
        "BTCUSDT@2t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            TickVolAgg::new(&BarKind::Tick(2)).expect("Tick(2) is an aggregator kind"),
        ),
    );

    let trades = TradeStore::default();
    for (i, px) in [10.0, 11.0, 12.0].into_iter().enumerate() {
        trades.push("binance", &trade("BTCUSDT", 1_000 + i as i64, px, 1.0));
    }
    let (_tx, rx) = bf_channel();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &keys(&["ETHUSDT@1m", "BTCUSDT@2t"]), &HashSet::new(), &trades, &rx, &fs);

    let agg = &st.aggs["BTCUSDT@2t"].2;
    assert_eq!(agg.closed.len(), 1, "2 of the 3 trades must have closed one tick bar");
    assert!(agg.forming().is_some(), "the 3rd trade must be the forming bar");
    assert_eq!(
        st.charts["BTCUSDT@2t"].bars.len(),
        2,
        "the tick chart must be synced (1 closed + 1 forming) on an unchanged-seq frame"
    );
    assert_eq!(
        st.charts["ETHUSDT@1m"].bars.len(),
        0,
        "the KLINE half must still be gated on seq — it may not re-fold on an unchanged seq"
    );
}

/// **BUG 4 PIN** — a backfill batch that arrives while the symbol has NO orderflow
/// aggregator must NOT be discarded. The old fold drained `bf_rx` unconditionally and
/// `continue`d the batch away; combined with the insert-only `bf_spawned` spawn gate (which
/// only ever reopens for a walk that delivered NOTHING — never for one whose batch got this
/// far), that truncated the symbol's historical CVD for the rest of the process.
#[test]
fn bug4_pin_a_batch_with_no_aggregator_is_staged_not_discarded() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default(); // no `of_aggs` entry: the window was closed/reaped
    let (tx, rx) = bf_channel();
    tx.send(("BTCUSDT".to_string(), vec![trade("BTCUSDT", 1_500, 10.0, 5.0)])).unwrap();
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());

    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);

    assert_eq!(
        st.bf_pending.get("BTCUSDT").map(|v| v.len()),
        Some(1),
        "the batch must be HELD for a later frame, not dropped on the floor"
    );
}

/// **THE TAPE-GAP SEAM** — a disclosed hole repairs BOTH aggregator kinds for that key and
/// DISCARDS the batch drained on the same frame.
///
/// The discard is the half that is easy to get wrong and impossible to see afterwards: the
/// drain and the epoch read are two instants, so a batch taken on a frame whose epoch has moved
/// spans the hole. Folding it would put one frame of a WRONG CVD on screen, which is precisely
/// the outcome the `mark_gap` API exists to make impossible.
#[test]
fn a_disclosed_tape_gap_repairs_both_aggregators_and_discards_that_frames_batch() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    st.charts.insert("BTCUSDT@2t".to_string(), chart_with_ots(&[1_000]));
    st.aggs.insert(
        "BTCUSDT@2t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            TickVolAgg::new(&BarKind::Tick(2)).expect("Tick(2) is an aggregator kind"),
        ),
    );
    st.of_aggs.insert(
        "BTCUSDT@2t".to_string(),
        ("binance".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(1.0)),
    );
    let trades = TradeStore::default();
    let (_tx, rx) = bf_channel();
    let fs = Mutex::new(String::new());

    // Frame 1: an ordinary drain, no gap.
    for (i, px) in [10.0, 11.0].into_iter().enumerate() {
        trades.push("binance", &trade("BTCUSDT", 1_000 + i as i64, px, 1.0));
    }
    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
    assert_eq!(st.aggs["BTCUSDT@2t"].2.closed.len(), 1, "two prints closed one 2t bar");
    assert!(!st.of_aggs["BTCUSDT@2t"].2.footprints().is_empty(), "footprints accumulated");

    // Frame 2: prints land AND the producer discloses a hole covering them.
    trades.push("binance", &trade("BTCUSDT", 1_002, 12.0, 1.0));
    st.tape_gaps.insert(("binance".to_string(), "BTCUSDT".to_string()), 1);
    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);

    assert_eq!(
        st.aggs["BTCUSDT@2t"].2.closed.len(),
        1,
        "TickVolAgg keeps bars that closed BEFORE the hole"
    );
    assert!(
        st.aggs["BTCUSDT@2t"].2.forming().is_none(),
        "...and drops the forming bar, which straddles it — the batch was NOT folded"
    );
    assert!(
        st.of_aggs["BTCUSDT@2t"].2.footprints().is_empty(),
        "OrderflowAgg drops the whole history: CVD is cumulative and cannot be repaired \
             narrowly"
    );

    // Frame 3: the epoch is unchanged, so the fold resumes normally on fresh prints.
    trades.push("binance", &trade("BTCUSDT", 1_003, 13.0, 1.0));
    sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
    assert!(
        st.aggs["BTCUSDT@2t"].2.forming().is_some(),
        "an unchanged epoch must not keep rebuilding — this fold runs every frame"
    );
}

/// **THE ORDERING PIN: the tape-gap epoch is read AFTER the drain, not sampled before it.**
///
/// The test above sets `st.tape_gaps` before calling `sync`, which is the well-ordered case and
/// therefore cannot see the defect this pins: the shell used to pass
/// `tape_gaps: &self.md_session.tape_gaps()` — a map cloned in the ARGUMENT LIST, i.e. strictly
/// before the fold was entered, while the drain it must follow happens hundreds of lines
/// inside. Everything in between (the destructure, the per-series kline dispatch, the backfill
/// drain) was a window in which the reader could disclose a hole, drain the buffered ticks and
/// push post-hole prints — which this fold then took, checked against a STALE epoch, and folded
/// into an unrepaired aggregator. `TickVolAgg::mark_gap` keeps `closed` whole, so a bar that
/// closed inside that batch is permanent and unmarked.
///
/// The probe is the drain itself. The epoch source drains the same key when it is called: if
/// the fold has already drained, it gets NOTHING and the aggregator has the prints; if it were
/// called first it would STEAL them, the fold's own drain would come back empty, and the
/// aggregator would be empty too. Both halves are asserted, so neither ordering can pass.
#[test]
fn the_tape_gap_epoch_is_read_after_the_drain() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut st = State::default();
    st.charts.insert("BTCUSDT@2t".to_string(), chart_with_ots(&[1_000]));
    st.aggs.insert(
        "BTCUSDT@2t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            TickVolAgg::new(&BarKind::Tick(2)).expect("Tick(2) is an aggregator kind"),
        ),
    );
    let trades = TradeStore::default();
    let (_tx, rx) = bf_channel();
    let fs = Mutex::new(String::new());
    trades.push("binance", &trade("BTCUSDT", 1_000, 10.0, 1.0));
    trades.push("binance", &trade("BTCUSDT", 1_001, 11.0, 1.0));

    let stolen = std::cell::Cell::new(0usize);
    {
        let probe = |venue: &str, symbol: &str| -> u64 {
            stolen.set(stolen.get() + trades.drain(venue, symbol).len());
            0
        };
        sync_gaps(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs, &probe);
    }

    assert_eq!(
        stolen.get(),
        0,
        "the epoch source saw prints still in the store — it was consulted BEFORE the drain, \
             which is the ordering every comment at the drain site forbids"
    );
    assert_eq!(
        st.aggs["BTCUSDT@2t"].2.closed.len(),
        1,
        "...and the fold's own drain must still have received the batch"
    );
}
