use super::test_support::*;
use super::*;
use crate::ui::feed_lifecycle::{BackfillRetries, should_spawn_backfill};
use vike_core::CoreSnapshot;

const KEY: &str = "BTCUSDT@1m";
const SYM: &str = "BTCUSDT";
const OTS: &[i64] = &[1_000];

/// One frame of the real fold with an optional backfill batch pushed first. Returns nothing —
/// every assertion reads `st`.
fn frame(st: &mut State, batch: Option<Vec<vike_model::TradeTick>>) {
    let snap = CoreSnapshot::empty(DEFAULT_VENUE, SYM);
    let (tx, rx) = bf_channel();
    if let Some(b) = batch {
        tx.send((SYM.to_string(), b)).unwrap();
    }
    let trades = TradeStore::default();
    let fs = Mutex::new(String::new());
    sync(st, &snap, &keys(&[KEY]), &HashSet::new(), &trades, &rx, &fs);
}

/// The whole reported bug, end to end: the orderflow window goes away mid-backfill (a
/// Data-manager delete / `reap_orphaned_feeds` sweep — `of_aggs.remove`), several pages land
/// while it is gone, and the window comes back. Every page must reach the returned
/// aggregator — the pages are the ONLY copy, since `bf_spawned` guarantees no refetch.
#[test]
fn every_page_that_arrived_while_the_chart_was_gone_reaches_the_aggregator_when_it_returns() {
    let mut st = State::default();
    open_orderflow(&mut st, KEY, SYM, OTS);

    // The window is torn down while the backfill worker is still paging.
    close_orderflow(&mut st, KEY);
    for size in [1.0, 2.0, 4.0] {
        frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, size)]));
    }
    assert_eq!(
        st.bf_pending[SYM].len(),
        3,
        "all three pages must be held while no aggregator exists"
    );

    // Orderflow re-enabled: `vike-desktop`'s `of_aggs.entry(..).or_insert_with(..)` builds a FRESH
    // aggregator, so the staged pages are the only history it can ever get.
    open_orderflow(&mut st, KEY, SYM, OTS);
    frame(&mut st, None);

    assert_eq!(
        of_total(&st.of_aggs[KEY].2),
        7.0,
        "the returned aggregator must hold EVERY tick staged while it was gone (1+2+4)"
    );
    assert!(st.bf_pending.is_empty(), "delivered pages are released from the staging buffer");
}

/// The run-once spawn guard (`feed_lifecycle::should_spawn_backfill`'s insert-only
/// `bf_spawned`) still refuses to re-spawn on a re-enable — that is deliberate, and it is
/// exactly why dropping a page was permanent. Recovery must therefore come from the staging
/// buffer, WITHOUT a refetch: the guard may not stand between the held pages and the chart.
///
/// `BackfillRetries` does not change this, and the walk scripted here is the reason: it
/// DELIVERED (the batch below reached `bf_rx`), and a delivering walk never re-walks — so the
/// guard is as shut here as it ever was, and staging is still the only recovery.
#[test]
fn the_run_once_spawn_guard_does_not_prevent_recovery_of_held_pages() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, SYM), "first enable spawns");

    let mut st = State::default();
    open_orderflow(&mut st, KEY, SYM, OTS);
    close_orderflow(&mut st, KEY);
    frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));

    // Re-enable: the guard says "already spawned", so NOTHING will refetch these ticks.
    open_orderflow(&mut st, KEY, SYM, OTS);
    assert!(
        !should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, SYM),
        "the run-once guard is still closed — there is no second fetch to fall back on"
    );
    frame(&mut st, None);

    assert_eq!(
        of_total(&st.of_aggs[KEY].2),
        5.0,
        "recovery must come from the staged pages, not from a refetch the guard forbids"
    );
}

/// Staging is invisible on the happy path: with an aggregator present the batch is applied in
/// the SAME call and leaves no entry behind — so a steady-state process holds nothing.
#[test]
fn a_batch_with_an_aggregator_present_is_applied_in_the_same_frame_and_leaves_nothing_staged() {
    let mut st = State::default();
    open_orderflow(&mut st, KEY, SYM, OTS);
    frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));

    assert_eq!(of_total(&st.of_aggs[KEY].2), 5.0);
    assert!(st.bf_pending.is_empty(), "nothing is held once it has been ingested");
}

/// Delivery must be exactly-once: a staged page is released only after `ingest`, and later
/// frames must not re-apply it (double-counted CVD is as wrong as missing CVD).
#[test]
fn a_delivered_page_is_never_ingested_twice() {
    let mut st = State::default();
    open_orderflow(&mut st, KEY, SYM, OTS);
    close_orderflow(&mut st, KEY);
    frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));
    open_orderflow(&mut st, KEY, SYM, OTS);

    frame(&mut st, None);
    frame(&mut st, None);
    frame(&mut st, None);
    assert_eq!(of_total(&st.of_aggs[KEY].2), 5.0, "three more frames must not re-ingest it");
}

/// A non-Binance orderflow chart is live-only by design, so it is NOT an aggregator that can
/// take a backfill batch: the page stays staged (and warned about) rather than being applied
/// to the wrong venue or discarded.
#[test]
fn a_non_binance_aggregator_does_not_satisfy_a_staged_binance_page() {
    let mut st = State::default();
    st.charts.insert("okx:BTCUSDT@1m".to_string(), chart_with_ots(OTS));
    st.of_aggs.insert(
        "okx:BTCUSDT@1m".to_string(),
        ("okx".to_string(), SYM.to_string(), OrderflowAgg::new(0.5)),
    );
    frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));

    assert_eq!(of_total(&st.of_aggs["okx:BTCUSDT@1m"].2), 0.0, "still live-only");
    assert_eq!(st.bf_pending[SYM].len(), 1, "the page is held for a Binance aggregator");
}

/// An empty batch is not a fault and must not create a staging entry (the worker's final
/// `send` is guarded, but the lane should not depend on that).
#[test]
fn an_empty_batch_stages_nothing() {
    let mut st = State::default();
    frame(&mut st, Some(Vec::new()));
    assert!(st.bf_pending.is_empty());
}

/// [`trim_pending_backfill`] is a no-op at or below the cap — the only case production ever
/// reaches, since a symbol's whole backfill is ≤ 300 pages × 1000 ticks.
#[test]
fn trim_is_a_no_op_at_or_below_the_cap() {
    let mut held: Vec<vike_model::TradeTick> =
        (0..4).map(|i| trade(SYM, 1_000 + i, 10.0, 1.0)).collect();
    assert_eq!(trim_pending_backfill(&mut held, 4), 0, "exactly at the cap keeps everything");
    assert_eq!(held.len(), 4);
    assert_eq!(trim_pending_backfill(&mut held, 10), 0);
    assert_eq!(held.len(), 4);
}

/// Over the cap, the trim drops from the FRONT: the worker emits oldest-first (its pager
/// replays collected pages `.rev()`), so the surviving tail is the history nearest the live
/// splice — the part a chart's visible bars can still attach footprints to.
#[test]
fn trim_over_the_cap_drops_the_oldest_ticks_and_keeps_the_newest() {
    let mut held: Vec<vike_model::TradeTick> =
        (0..5).map(|i| trade(SYM, 1_000 + i, 10.0, 1.0)).collect();
    assert_eq!(trim_pending_backfill(&mut held, 2), 3, "3 of 5 dropped");
    assert_eq!(
        held.iter().map(|t| t.ts).collect::<Vec<_>>(),
        vec![1_003, 1_004],
        "the two NEWEST ticks survive"
    );
}
