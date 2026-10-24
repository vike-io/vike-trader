use super::*;
#[test]
fn nice_tick_scales_with_price() {
    // ~2bps, rounded to a 1/2/5 nice step
    assert_eq!(nice_orderflow_tick(64000.0), 10.0); // 64000*0.0002=12.8 → nice 10
    assert_eq!(nice_orderflow_tick(3000.0), 0.5); // 0.6 → nice 0.5
    assert_eq!(nice_orderflow_tick(0.5), 0.0001); // 0.0001 → nice 0.0001
    assert_eq!(nice_orderflow_tick(0.0), 1.0); // guard
    assert!(nice_orderflow_tick(64000.0) > 0.0);
}
fn tk(ts: i64, p: f64, s: f64, ibm: bool) -> TradeTick {
    TradeTick { ts, local_ts: 0, price: p, size: s, is_buyer_maker: ibm, symbol: "BTCUSDT".into() }
}
/// DROP AND REBUILD, and the two properties the interim rests on: the footprint history is
/// GONE (a wrong CVD is worse than a missing one) and `generation` still MOVED FORWARD, so
/// vike-chart's `(generation, len)` CVD cache key cannot serve a pre-gap frame back.
#[test]
fn a_tape_gap_drops_the_history_and_still_advances_the_generation() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1000, 100.0, 1.0, false), tk(2000, 101.0, 2.0, true)], &[1000, 2000]);
    assert_eq!(a.footprints().len(), 2);
    let before = a.generation();
    assert!(before > 0, "an ingest that changed cells bumps the generation");

    assert!(a.mark_gap(1), "the epoch moved — this call repaired");
    assert!(a.footprints().is_empty(), "the corrupted footprint history is dropped whole");
    assert!(
        a.generation() > before,
        "generation must never go BACKWARDS — vike-chart's CVD cache keys on it"
    );

    assert!(!a.mark_gap(1), "same epoch: idempotent, because this runs every frame");
    a.ingest(&[tk(3000, 99.0, 4.0, false)], &[3000]);
    assert_eq!(a.footprints().len(), 1, "post-gap prints rebuild from empty");
}

/// A gap arriving while ticks are HELD (no bar grid yet) discards the held ticks too — they
/// belong to the same corrupted stretch, and folding them in after the repair would put the
/// hole straight back.
#[test]
fn a_tape_gap_discards_held_pre_grid_ticks() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1000, 100.0, 1.0, false)], &[]);
    assert_eq!(a.pending_len(), 1, "no grid yet, so the tick is held");
    assert!(a.mark_gap(1));
    assert_eq!(a.pending_len(), 0, "held ticks are part of what the hole corrupted");
}

#[test]
fn buckets_trades_into_bars_by_ot() {
    let mut a = OrderflowAgg::new(1.0);
    // bars open at ots 1000, 2000, 3000
    let ots = [1000, 2000, 3000];
    // trades: t@1500→bar0, t@2500→bar1, t@2999→bar1, t@3001→bar2, t@500→discarded (before
    // bar0 of a REAL grid: unplaceable by construction — but COUNTED, see the assert below).
    a.ingest(
        &[
            tk(1500, 100.0, 2.0, false),
            tk(2500, 101.0, 3.0, true),
            tk(2999, 101.0, 1.0, false),
            tk(3001, 102.0, 4.0, false),
            tk(500, 99.0, 9.0, false),
        ],
        &ots,
    );
    let fps = a.footprints();
    assert_eq!(fps.len(), 3);
    // bar0: buy 2@100
    assert_eq!(fps[0].cells, vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]);
    // bar1: sell 3@101 + buy 1@101 → one bucket 101
    assert_eq!(fps[1].cells, vec![PriceBin { price: 101.0, buy_vol: 1.0, sell_vol: 3.0 }]);
    // bar2: buy 4@102
    assert_eq!(fps[2].cells, vec![PriceBin { price: 102.0, buy_vol: 4.0, sell_vol: 0.0 }]);
    // The t@500 tick predates bar 0 of a real grid, so it is discarded — but ACCOUNTED FOR.
    assert_eq!(a.dropped_before_grid(), 1, "a pre-bar-0 discard must be counted, not silent");
    assert_eq!(a.pending_len(), 0, "a real grid existed, so nothing is held");
}
#[test]
fn incremental_ingest_accumulates() {
    let mut a = OrderflowAgg::new(1.0);
    let ots = [1000, 2000];
    a.ingest(&[tk(1500, 100.0, 1.0, false)], &ots);
    a.ingest(&[tk(1600, 100.0, 2.0, false)], &ots); // same bar0, later frame
    assert_eq!(
        a.footprints()[0].cells,
        vec![PriceBin { price: 100.0, buy_vol: 3.0, sell_vol: 0.0 }]
    );
}
#[test]
fn zero_size_and_growth() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1500, 100.0, 0.0, false)], &[1000]); // zero-size skipped
    assert_eq!(a.footprints()[0].cells.len(), 0);
}

// === SP3 Task B #1 (SP2 review finding B): footprints() cache =========================

/// The cache must equal a fresh rebuild after `ingest`, AND a repeat `footprints()` call
/// with no `ingest` in between must be O(1) — proved via `Arc::ptr_eq`, not just content
/// equality, since two independently-rebuilt-but-equal `Vec`s would pass a content check
/// while still failing to demonstrate the cache actually skipped the rebuild.
#[test]
fn footprints_cache_equals_fresh_rebuild_and_is_stable_across_repeat_calls() {
    let mut a = OrderflowAgg::new(1.0);
    let ots = [1000, 2000];
    a.ingest(&[tk(1500, 100.0, 2.0, false), tk(2500, 101.0, 1.0, true)], &ots);

    let first = a.footprints();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].cells, vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]);
    assert_eq!(first[1].cells, vec![PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 1.0 }]);

    // repeat call, no ingest between: same allocation (served from cache), not a coincidental
    // content match from an independent rebuild.
    let second = a.footprints();
    assert!(Arc::ptr_eq(&first, &second), "an unchanged aggregator must hand out the SAME Arc");
    assert_eq!(*first, *second);
}

/// `ingest` must NOT rebuild the cache when nothing actually changed (empty trades, no new
/// bars) — this is the exact shape `main.rs`'s per-frame `sync_from_core` drain calls
/// `ingest` with on a quiet market. A genuine trade afterward must still invalidate it.
#[test]
fn footprints_cache_survives_a_no_op_ingest_but_invalidates_on_real_change() {
    let mut a = OrderflowAgg::new(1.0);
    let ots = [1000, 2000];
    a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
    let before = a.footprints();

    a.ingest(&[], &ots); // no trades, same bar_ots → true no-op
    let after_noop = a.footprints();
    assert!(
        Arc::ptr_eq(&before, &after_noop),
        "a no-op ingest (empty trades) must not rebuild the footprint cache"
    );

    a.ingest(&[tk(1600, 100.0, 1.0, false)], &ots); // a real trade lands
    let after_real = a.footprints();
    assert!(
        !Arc::ptr_eq(&before, &after_real),
        "a real ingest must invalidate (rebuild) the footprint cache"
    );
    assert_eq!(after_real[0].cells, vec![PriceBin { price: 100.0, buy_vol: 3.0, sell_vol: 0.0 }]);
}

/// A zero-size (skipped) trade must NOT count as a change — it never touches `cells`.
#[test]
fn footprints_cache_survives_a_zero_size_trade() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1500, 100.0, 1.0, false)], &[1000]);
    let before = a.footprints();
    a.ingest(&[tk(1600, 100.0, 0.0, false)], &[1000]); // zero-size: skipped, no real change
    let after = a.footprints();
    assert!(Arc::ptr_eq(&before, &after), "a zero-size trade must not rebuild the footprint cache");
}

// === SP3 TB-fix (post-Task-B review finding, MEDIUM): monotonic `generation` ============

/// `generation()` must bump exactly in lockstep with a genuine cache rebuild — mirrors
/// `footprints_cache_survives_a_no_op_ingest_but_invalidates_on_real_change` above, from the
/// generation side: a no-op `ingest` (empty trades, unchanged `bar_ots`) must NOT bump it, a
/// real trade landing must, and it must keep climbing (never reset) across successive
/// rebuilds. This is the property vike-chart's CVD cache leans on to trust "content changed"
/// without re-deriving it from the footprint slice's own (ABA-able) address.
#[test]
fn generation_bumps_only_on_real_cache_rebuild() {
    let mut a = OrderflowAgg::new(1.0);
    let ots = [1000, 2000];
    assert_eq!(a.generation(), 0, "a fresh aggregator starts at generation 0");

    a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
    assert_eq!(a.generation(), 1, "a real trade must bump the generation");

    a.ingest(&[], &ots); // no-op: empty trades, unchanged bar_ots
    assert_eq!(a.generation(), 1, "a no-op ingest must not bump the generation");

    a.ingest(&[tk(1600, 100.0, 0.0, false)], &ots); // zero-size: also a no-op
    assert_eq!(a.generation(), 1, "a zero-size trade must not bump the generation");

    a.ingest(&[tk(1700, 100.0, 1.0, false)], &ots); // a second real trade
    assert_eq!(a.generation(), 2, "generation must be monotonic across successive rebuilds");
}

/// Two `footprints()` snapshots taken across a real `ingest` must carry different
/// generations — the exact property `ChartState::cvd_shared`'s `(generation, len)` key relies
/// on to treat them as distinct even if the allocator happened to hand the second `Arc`'s
/// backing `Vec<FootprintBar>` the SAME address the first's occupied (freed in between) —
/// the toggle-off/on ABA scenario the review finding was about.
#[test]
fn footprints_across_an_ingest_carry_different_generations() {
    let mut a = OrderflowAgg::new(1.0);
    let ots = [1000, 2000];
    a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
    let gen_before = a.generation();
    let _first = a.footprints();

    a.ingest(&[tk(1600, 101.0, 1.0, true)], &ots);
    let gen_after = a.generation();
    let _second = a.footprints();

    assert_ne!(gen_before, gen_after, "an ingest that changes content must change the generation");
    assert_eq!(gen_after, gen_before + 1);
}

// === SP3 Task 3: background aggTrades backfill — accumulate, no double-count ============

/// `sync_from_core` feeds a symbol's backfill batch and its live batch through TWO SEPARATE
/// `ingest` calls (the backfill drain runs first, but on whatever frame(s) its batches
/// arrive — generally not the same frame as the live drain). Because `ingest` has NO id
/// dedup — it only ACCUMULATES, relying entirely on the CALLER to keep backfill ids
/// strictly below every live id (`global-constraints.md`'s no-double-count invariant) — two
/// `ingest` calls over a disjoint split of a trade set must land the exact same footprint as
/// one `ingest` call over the union, INCLUDING at a "boundary bar": a bar that receives
/// trades from both sides of the split. bar0 below is exactly that case: two backfill trades
/// and one live trade all land in it. This is the property the whole feature leans on for
/// correctness.
#[test]
fn backfill_then_live_ingest_equals_one_ingest_of_the_union() {
    let ots = [1000, 2000];
    // Backfill trades (ids conceptually < the live feed's earliest id — strictly older).
    let backfill = vec![tk(1200, 100.0, 3.0, true), tk(1500, 100.0, 1.0, false)];
    // Live trades (ids >= the live feed's earliest id): one more in the SAME bar0, plus one
    // in bar1.
    let live = vec![tk(1900, 100.0, 1.0, false), tk(2500, 101.0, 2.0, true)];

    let mut staged = OrderflowAgg::new(1.0);
    staged.ingest(&backfill, &ots); // backfill drains first (sync_from_core's doc)
    staged.ingest(&live, &ots); // ... then the live drain, same or a later frame

    let mut union_batch = backfill.clone();
    union_batch.extend(live.clone());
    let mut combined = OrderflowAgg::new(1.0);
    combined.ingest(&union_batch, &ots); // one ingest over the union, for comparison

    assert_eq!(*staged.footprints(), *combined.footprints());
    // Exact boundary-bar content: bar0 accumulated across BOTH `ingest` calls (2 backfill +
    // 1 live trade, all bucket 100) — nothing dropped, nothing double-counted.
    assert_eq!(
        staged.footprints()[0].cells,
        vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 3.0 }]
    );
    assert_eq!(
        staged.footprints()[1].cells,
        vec![PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 2.0 }]
    );
}

/// `backfill_before_id`'s boundary (SP3-final-review M1 fix): the returned `before_id`
/// EQUALS `min_live` — the pager it feeds (`backfill_agg_trades_backward`) already treats
/// `before_id` as exclusive, so passing `min_live` through unmodified is what correctly
/// includes the boundary trade (id `min_live - 1`) while staying disjoint from the live feed
/// — with no room to page at all once there's no id below it (`min_live <= 1`). See the
/// function's doc for the full no-double-count argument.
#[test]
fn backfill_before_id_equals_min_live_since_the_pager_bound_is_exclusive() {
    assert_eq!(backfill_before_id(10_000), Some(10_000));
    assert_eq!(backfill_before_id(2), Some(2));
    assert_eq!(backfill_before_id(1), None); // no room below id 1
    assert_eq!(backfill_before_id(0), None);
}

// === #937 follow-up: ticks preceding bar 0 are no longer silently dropped ================

/// The defect, at its worst: an `ingest` whose `bar_ots` is EMPTY used to drop the ENTIRE
/// batch — `partition_point` returns `0` for every tick, and `idx == 0` was a bare `continue`.
/// #937 made the backfill lane's version of this observable with a `warn!` but left the loss
/// in place; the live drain's version (`core_sync`'s `of_by_venue_symbol` loop, which passes
/// `charts.get(key)…unwrap_or_default()`) was not even logged. Now the batch is HELD.
#[test]
fn ticks_arriving_before_any_bar_exists_are_held_not_dropped() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1500, 100.0, 2.0, false), tk(2500, 101.0, 3.0, true)], &[]);

    assert_eq!(a.pending_len(), 2, "an empty bar grid must HOLD the batch, never drop it");
    assert_eq!(a.dropped_before_grid(), 0, "holding is not a discard — nothing is counted lost");
    assert_eq!(a.evicted_ticks(), 0);
    // Nothing was placed, so nothing was rendered: no cells, no cache rebuild, no generation
    // bump (the `(generation, len)` CVD key must not see a change that did not happen).
    assert!(a.footprints().is_empty());
    assert_eq!(a.generation(), 0, "a held tick changes no footprint, so no cache rebuild");
}

/// Held ticks must fold in at THEIR bar once the grid appears — the ordering requirement.
/// Appending them to the newest bar (the naive "flush at the tail" fix) would smear a whole
/// backfill page onto the forming bar and produce a CVD step that never happened.
#[test]
fn held_ticks_land_in_their_own_bar_once_the_grid_appears() {
    let mut a = OrderflowAgg::new(1.0);
    // Frame 1: chart deleted / not yet synced — no bars at all.
    a.ingest(
        &[
            tk(1500, 100.0, 2.0, false), // → bar0
            tk(2500, 101.0, 3.0, true),  // → bar1
            tk(3500, 102.0, 4.0, false), // → bar2
        ],
        &[],
    );
    assert_eq!(a.pending_len(), 3);

    // Frame 2: the chart's bars arrive. Nothing new on the tape this frame.
    let ots = [1000, 2000, 3000];
    a.ingest(&[], &ots);

    let fps = a.footprints();
    assert_eq!(fps.len(), 3);
    assert_eq!(fps[0].cells, vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]);
    assert_eq!(fps[1].cells, vec![PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 3.0 }]);
    assert_eq!(fps[2].cells, vec![PriceBin { price: 102.0, buy_vol: 4.0, sell_vol: 0.0 }]);
    assert_eq!(a.pending_len(), 0, "the hold is released once it has been folded in");
    assert_eq!(a.dropped_before_grid(), 0);
    assert_eq!(a.generation(), 1, "the flush is a real content change: exactly one rebuild");
}

/// The whole point, stated as an equivalence: a batch that arrives DURING the grid outage must
/// end up exactly where it would have landed had the outage never happened. This is the #937
/// trigger end-to-end — a Data-manager delete drops `charts[k]` alongside `of_aggs[k]`, so a
/// re-add briefly presents an aggregator with an empty grid — and it is the property a reader
/// should check first if this file ever regresses.
#[test]
fn a_grid_outage_costs_nothing_versus_the_same_batch_ingested_in_grid() {
    let ots = [1000, 2000, 3000];
    let batch = vec![
        tk(1500, 100.0, 2.0, false),
        tk(2500, 101.0, 3.0, true),
        tk(2999, 101.0, 1.0, false),
        tk(3001, 102.0, 4.0, false),
    ];

    let mut outage = OrderflowAgg::new(1.0);
    outage.ingest(&batch, &[]); // the chart's bars are gone this frame
    outage.ingest(&[], &ots); // ... and back the next one

    let mut healthy = OrderflowAgg::new(1.0);
    healthy.ingest(&batch, &ots); // the same batch, no outage

    assert_eq!(*outage.footprints(), *healthy.footprints());
    assert_eq!(outage.dropped_before_grid(), 0);
    assert_eq!(outage.evicted_ticks(), 0);
}

/// Held ticks and a fresh batch in the SAME `ingest` must both place correctly — the held ones
/// are flushed first (they are older on both feeding lanes), and accumulation across the two
/// sources is plain addition in the shared bucket.
#[test]
fn a_flush_and_the_same_frames_fresh_trades_accumulate_in_one_bucket() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1500, 100.0, 2.0, false)], &[]); // held
    let ots = [1000, 2000];
    a.ingest(&[tk(1600, 100.0, 5.0, false)], &ots); // flush + this frame's live tick

    assert_eq!(
        a.footprints()[0].cells,
        vec![PriceBin { price: 100.0, buy_vol: 7.0, sell_vol: 0.0 }]
    );
    assert_eq!(a.pending_len(), 0);
}

/// A held tick that is STILL older than bar 0 when the grid finally appears takes the
/// unplaceable-by-construction path — discarded (no future grid can contain it: `cells` is
/// index-aligned to a forward-only bar list), but COUNTED. This is also why the hold needs no
/// separate age bound: staleness resolves itself into a counted discard at flush time.
#[test]
fn held_ticks_older_than_the_grid_that_finally_appears_are_counted_not_silent() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(500, 99.0, 9.0, false), tk(1500, 100.0, 2.0, false)], &[]);
    assert_eq!(a.pending_len(), 2);

    let ots = [1000, 2000]; // bar 0 opens AFTER the first held tick
    a.ingest(&[], &ots);

    assert_eq!(a.pending_len(), 0, "a flushed tick is never re-held — that would pin it");
    assert_eq!(a.dropped_before_grid(), 1, "the un-placeable held tick is counted");
    // The placeable one still landed.
    assert_eq!(
        a.footprints()[0].cells,
        vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]
    );
}

/// The bound, behaving as documented: past `pending_cap` the OLDEST held ticks are evicted
/// (keeping the ticks adjacent to the live splice, the ones a chart's visible bars can still
/// attach footprints to) and the loss is counted. Run at a test-sized cap — the production
/// `PENDING_MAX_TICKS` is deliberately above what any producer can emit.
#[test]
fn the_hold_bound_evicts_the_oldest_and_counts_the_loss() {
    let mut a = OrderflowAgg::new(1.0);
    a.set_pending_cap(3);
    // Five ticks, one per price, all inside bar0 of the grid that arrives later.
    a.ingest(
        &[
            tk(1100, 100.0, 1.0, false),
            tk(1200, 101.0, 1.0, false),
            tk(1300, 102.0, 1.0, false),
            tk(1400, 103.0, 1.0, false),
            tk(1500, 104.0, 1.0, false),
        ],
        &[],
    );
    assert_eq!(a.pending_len(), 3, "the hold is capped");
    assert_eq!(a.evicted_ticks(), 2, "and the overflow is counted, not silent");

    a.ingest(&[], &[1000, 2000]);
    // The two OLDEST (100.0, 101.0) are the ones gone; the newest three survived.
    assert_eq!(
        a.footprints()[0].cells,
        vec![
            PriceBin { price: 102.0, buy_vol: 1.0, sell_vol: 0.0 },
            PriceBin { price: 103.0, buy_vol: 1.0, sell_vol: 0.0 },
            PriceBin { price: 104.0, buy_vol: 1.0, sell_vol: 0.0 },
        ]
    );
}

/// The cap holds ACROSS calls too — a chart whose kline feed never delivers keeps receiving
/// live ticks frame after frame, and that is exactly the case the bound exists for.
#[test]
fn the_hold_bound_holds_across_successive_gridless_frames() {
    let mut a = OrderflowAgg::new(1.0);
    a.set_pending_cap(4);
    for i in 0..10 {
        a.ingest(&[tk(1000 + i, 100.0, 1.0, false)], &[]);
    }
    assert_eq!(a.pending_len(), 4, "the buffer never exceeds the cap, however many frames run");
    assert_eq!(a.evicted_ticks(), 6);
}

/// A zero-size tick is a `place` no-op, so it must not consume hold capacity either.
#[test]
fn zero_size_ticks_are_not_held() {
    let mut a = OrderflowAgg::new(1.0);
    a.ingest(&[tk(1500, 100.0, 0.0, false), tk(1600, 100.0, 1.0, false)], &[]);
    assert_eq!(a.pending_len(), 1);
}

/// A grid-less `ingest` must stay a no-op for the render side: no cache rebuild, no generation
/// bump, and the SAME `Arc` handed out — the invariant vike-chart's CVD cache key relies on.
#[test]
fn a_gridless_ingest_does_not_rebuild_the_cache_or_bump_the_generation() {
    let mut a = OrderflowAgg::new(1.0);
    let ots = [1000, 2000];
    a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
    let before = a.footprints();
    let gen_before = a.generation();

    a.ingest(&[tk(1600, 100.0, 1.0, false)], &[]); // grid vanished for a frame: held
    assert!(
        Arc::ptr_eq(&before, &a.footprints()),
        "holding a tick changes no footprint, so the cache must not be rebuilt"
    );
    assert_eq!(a.generation(), gen_before);

    a.ingest(&[], &ots); // grid back: the held tick lands and the cache rebuilds once
    assert_eq!(a.generation(), gen_before + 1);
    assert_eq!(
        a.footprints()[0].cells,
        vec![PriceBin { price: 100.0, buy_vol: 3.0, sell_vol: 0.0 }]
    );
}

/// [`due_to_log`]'s doubling schedule: the FIRST loss of a kind reports immediately, an
/// unchanged counter never re-reports (idempotence — this runs on a per-frame path), and the
/// threshold doubles off the observed count so a pathological aggregator costs ≤ 64 lines
/// instead of 60/s.
#[test]
fn due_to_log_reports_the_first_loss_then_doubles() {
    // Fresh counter: nothing lost yet, nothing to say.
    assert_eq!(due_to_log(0, 1), None);
    // First loss reports, and arms the next line at 2×.
    assert_eq!(due_to_log(1, 1), Some(2));
    // Unchanged counter, re-checked next frame: silent.
    assert_eq!(due_to_log(1, 2), None);
    assert_eq!(due_to_log(2, 2), Some(4));
    assert_eq!(due_to_log(3, 4), None);
    assert_eq!(due_to_log(9, 4), Some(18)); // a jump reports and re-arms off the JUMPED-TO count
    // Saturating, not panicking/looping, at the top of the range.
    assert_eq!(due_to_log(u64::MAX, u64::MAX), Some(u64::MAX));
}
