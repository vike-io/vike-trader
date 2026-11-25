use super::*;

fn bu(ts: i64, kind: L2BookUpdateKind, bids: &[L2BookLevel], asks: &[L2BookLevel]) -> L2BookUpdate {
    L2BookUpdate {
        timestamp: ts,
        local_timestamp: ts,
        sequence: 0,
        kind,
        tick_size: 0.01,
        bids: bids.to_vec(),
        asks: asks.to_vec(),
        symbol: "TOK".into(),
    }
}

/// The rows numbered as the pmxt mapper numbers one file's rows of an asset: `seq` 0, 1, 2, ...
fn numbered(mut updates: Vec<L2BookUpdate>) -> Vec<L2BookUpdate> {
    for (seq, u) in updates.iter_mut().enumerate() {
        u.sequence = seq as u64;
    }
    updates
}

/// The recorded `seq` restarts every pmxt hour file. A restart that arrives as a fresh snapshot is NO
/// hole: the snapshot re-anchors at its own `seq`, and the new file's deltas fold after it.
#[test]
fn an_hourly_restart_that_arrives_as_a_snapshot_is_no_hole() {
    let mut updates = vec![
        bu(
            1_000,
            L2BookUpdateKind::Snapshot,
            &[L2BookLevel::new(0.19, 10.0)],
            &[L2BookLevel::new(0.20, 100.0)],
        ),
        bu(2_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.21, 50.0)]),
        // hour boundary: the mapper's per-file counter restarts at 0, on the new file's `book` frame
        bu(3_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 90.0)]),
        bu(4_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.22, 70.0)]),
    ];
    for (u, seq) in updates.iter_mut().zip([900, 901, 0, 1]) {
        u.sequence = seq;
    }
    let f = fold_until(&updates, &[], &[], 5_000, false);
    assert!(f.anchored, "the restart re-anchored on its snapshot");
    assert_eq!(f.applied, 4);
    assert_eq!(f.book.ask_quantity_at(0.22), 70.0, "the new file's delta folds after the restart");
    assert_eq!(f.book.quantity_for_price(1, 0.22), 160.0);
}

/// A restart that arrives as a DELTA is a hole: nothing proves the book it would patch, so there is no
/// reading until the next `book` frame re-anchors. The backtest replays the same chain the same way.
#[test]
fn an_hourly_restart_that_arrives_as_a_delta_is_a_hole_until_the_next_snapshot() {
    let mut updates = vec![
        bu(1_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 100.0)]),
        bu(2_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.21, 50.0)]),
        bu(3_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.22, 70.0)]),
    ];
    for (u, seq) in updates.iter_mut().zip([900, 901, 0]) {
        u.sequence = seq;
    }
    let f = fold_until(&updates, &[], &[], 3_500, false);
    assert!(!f.anchored, "the seq-0 delta is no next event of the chain");
    assert_eq!(ask_ladder_at(&updates, &[], &[], 3_500, false), None);
    let mut snap = bu(4_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.22, 70.0)]);
    snap.sequence = 1;
    updates.push(snap);
    let ladder = ask_ladder_at(&updates, &[], &[], 4_500, false).expect("re-anchored");
    assert_eq!((ladder.len(), ladder[0].qty), (1, 70.0));
}

/// A hole in the recorded sequence drops the book; a delta that follows its anchor folds.
#[test]
fn a_hole_in_the_recorded_sequence_drops_the_book_until_the_next_snapshot() {
    let mut updates = numbered(vec![
        bu(1_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 100.0)]),
        bu(2_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.21, 50.0)]),
    ]);
    assert!(fold_until(&updates, &[], &[], 2_500, false).anchored, "contiguous: trusted");
    updates[1].sequence = 7;
    let f = fold_until(&updates, &[], &[], 2_500, false);
    assert!(!f.anchored, "seq 0 then 7: frames are missing");
    assert_eq!(f.applied, 2, "both events were read");
}

/// A repair (the ghost prune, the tape deduction) is no event of the recorded chain: it leaves the
/// book's `last_sequence` alone, so the next recorded delta still folds. A repair that advanced it would make
/// the next delta read as stale (dropped) or, under the chain rule, as a hole.
#[test]
fn a_repair_keeps_the_chain_so_the_next_delta_still_folds() {
    let mut b = L2Book::new(PRICE_GRID);
    b.apply_snapshot(5, &[], &[L2BookLevel::new(0.20, 100.0), L2BookLevel::new(0.25, 40.0)]);
    assert_eq!(prune_ghost_asks(&mut b, 0.25), 1);
    consume_ask(&mut b, 0.25, 10.0);
    assert_eq!(b.last_sequence, 5, "neither repair moved the chain's sequence");

    let updates = numbered(vec![
        bu(
            1_000,
            L2BookUpdateKind::Snapshot,
            &[],
            &[L2BookLevel::new(0.20, 100.0), L2BookLevel::new(0.25, 40.0)],
        ),
        bu(2_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.30, 9.0)]),
    ]);
    let quote = |ts: i64, ask: f64| TickQuote {
        timestamp: ts,
        local_timestamp: ts,
        bid: 0.19,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: "TOK".into(),
    };
    let f = fold_until(&updates, &[quote(1_000, 0.25)], &[], 3_000, false);
    assert!(f.anchored);
    assert_eq!(f.pruned, 1, "the ghost under the venue's best ask is gone");
    assert_eq!(f.book.ask_quantity_at(0.30), 9.0, "the delta after the repair folded");
}

#[test]
fn strict_cutoff_excludes_the_print_timestamp_and_inclusive_includes_it() {
    let updates = numbered(vec![
        bu(1_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 100.0)]),
        // the entry's own block eats the level
        bu(2_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.20, 0.0)]),
    ]);
    assert_eq!(fold_until(&updates, &[], &[], 2_000, true).book.ask_quantity_at(0.20), 100.0);
    assert_eq!(fold_until(&updates, &[], &[], 2_000, false).book.ask_quantity_at(0.20), 0.0);
}

/// The ghost repair: a level the venue's own best ask says is gone must not survive into any
/// depth reading, and a level at or above it must be untouched.
#[test]
fn ghost_asks_below_the_venue_best_ask_are_pruned() {
    let mut b = L2Book::new(PRICE_GRID);
    b.apply_snapshot(
        1,
        &[],
        &[
            L2BookLevel::new(0.20, 100.0),
            L2BookLevel::new(0.22, 50.0),
            L2BookLevel::new(0.25, 70.0),
        ],
    );
    assert_eq!(prune_ghost_asks(&mut b, 0.22), 1, "only the 0.20 level is below 0.22");
    assert_eq!(b.best_ask(), Some(L2BookLevel::new(0.22, 50.0)));
    assert_eq!(b.quantity_for_price(1, 1.0), 120.0);
    // idempotent, and a level exactly AT the reported best ask stays
    assert_eq!(prune_ghost_asks(&mut b, 0.22), 0);
    // an absent/zero best ask is a no-op rather than a book wipe
    assert_eq!(prune_ghost_asks(&mut b, 0.0), 0);
    assert_eq!(b.quantity_for_price(1, 1.0), 120.0);
}

/// The repair must reach the measured book, not just the helper — folding a stream whose L1
/// says the cheap level is gone must not leave it for a sweep to walk.
#[test]
fn the_fold_applies_the_ghost_repair_from_the_l1_series() {
    let updates = vec![bu(
        1_000,
        L2BookUpdateKind::Snapshot,
        &[],
        &[L2BookLevel::new(0.20, 100.0), L2BookLevel::new(0.25, 40.0)],
    )];
    let no_l1 = fold_until(&updates, &[], &[], 2_000, false);
    assert_eq!(
        no_l1.book.best_ask(),
        Some(L2BookLevel::new(0.20, 100.0)),
        "unrepaired: the ghost survives"
    );

    let l1 = vec![TickQuote {
        timestamp: 1_000,
        local_timestamp: 1_000,
        bid: 0.19,
        ask: 0.25,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: "TOK".into(),
    }];
    let repaired = fold_until(&updates, &l1, &[], 2_000, false);
    assert_eq!(repaired.book.best_ask(), Some(L2BookLevel::new(0.25, 40.0)));
    assert_eq!(repaired.pruned, 1);
    assert_eq!(repaired.book.quantity_for_price(1, 1.0), 40.0, "phantom depth is gone");
}

/// The tape deduction: a taker buy eats the level it printed at, never goes negative, and never
/// invents liquidity at a price the book does not show.
#[test]
fn a_taker_buy_deducts_from_the_ask_level_it_printed_at() {
    let mut b = L2Book::new(PRICE_GRID);
    b.apply_snapshot(1, &[], &[L2BookLevel::new(0.20, 100.0), L2BookLevel::new(0.21, 50.0)]);
    consume_ask(&mut b, 0.20, 30.0);
    assert_eq!(b.ask_quantity_at(0.20), 70.0);
    // an oversized print empties the level rather than going negative
    consume_ask(&mut b, 0.20, 999.0);
    assert_eq!(b.ask_quantity_at(0.20), 0.0);
    // `price_of` reconstitutes a level price as `tick * PRICE_GRID`, so 0.21 comes back as
    // 0.21000000000000002 — compared with a tolerance rather than pinned to that artefact.
    let L2BookLevel { price: px, qty } =
        b.best_ask().expect("the 0.21 level is now the top of book");
    assert!((px - 0.21).abs() < PRICE_GRID / 2.0, "{px}");
    assert_eq!(qty, 50.0);
    // a price the book does not show is a no-op, not a synthesized negative level
    consume_ask(&mut b, 0.30, 10.0);
    assert_eq!(b.quantity_for_price(1, 1.0), 50.0);
}

/// The fold must apply it to the book the measurement reads, and only for TAKER BUYS — a taker
/// sell hits the bid and must leave the ask side untouched.
#[test]
fn the_fold_consumes_ask_liquidity_only_for_taker_buys() {
    let updates =
        vec![bu(1_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 100.0)])];
    let trade = |ts: i64, buyer_maker: bool| TickTrade {
        timestamp: ts,
        local_timestamp: ts,
        price: 0.20,
        size: 40.0,
        is_buyer_maker: buyer_maker,
        symbol: "TOK".into(),
    };
    // the snapshot is folded first, then the same-ts trade is applied on the NEXT update
    let updates2 = {
        let mut v = updates.clone();
        v.push(bu(2_000, L2BookUpdateKind::Delta, &[], &[]));
        numbered(v)
    };
    let taker_buy = fold_until(&updates2, &[], &[trade(1_500, false)], 3_000, false);
    assert_eq!(taker_buy.book.ask_quantity_at(0.20), 60.0);
    assert_eq!(taker_buy.consumed, 40.0);

    let taker_sell = fold_until(&updates2, &[], &[trade(1_500, true)], 3_000, false);
    assert_eq!(taker_sell.book.ask_quantity_at(0.20), 100.0, "a taker sell hits the bid");
    assert_eq!(taker_sell.consumed, 0.0);
}

#[test]
fn deltas_without_a_snapshot_are_not_anchored_and_yield_no_ladder() {
    let updates = vec![bu(1_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.20, 100.0)])];
    let f = fold_until(&updates, &[], &[], 9_999, false);
    assert!(!f.anchored, "a book built from deltas alone is not measurable");
    assert_eq!(f.applied, 1);
    assert_eq!(ask_ladder_at(&updates, &[], &[], 9_999, false), None);
}

#[test]
fn a_gap_marker_invalidates_the_anchor_until_the_next_snapshot() {
    let updates = numbered(vec![
        bu(1_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 100.0)]),
        bu(2_000, L2BookUpdateKind::GapStart, &[], &[]),
        bu(3_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.21, 5.0)]),
    ]);
    assert!(!fold_until(&updates, &[], &[], 3_500, false).anchored);
    assert_eq!(ask_ladder_at(&updates, &[], &[], 3_500, false), None);
    let mut recovered = updates.clone();
    recovered.push(bu(4_000, L2BookUpdateKind::Snapshot, &[], &[L2BookLevel::new(0.20, 7.0)]));
    assert!(fold_until(&recovered, &[], &[], 4_500, false).anchored);
    assert!(ask_ladder_at(&recovered, &[], &[], 4_500, false).is_some());
}

/// The integrity gate must PASS on a complete delta stream and FAIL on one with a row removed —
/// otherwise a green report would be worth nothing. A row the ARCHIVE lost leaves no hole in the
/// recorded `seq` (the mapper numbers the rows a file holds), so it shows as a mismatch; a hole in
/// the recorded sequence breaks the chain instead, and the next snapshot is a reseed, not a checkpoint.
#[test]
fn checkpoint_verification_catches_a_dropped_delta() {
    let complete = vec![
        bu(
            1_000,
            L2BookUpdateKind::Snapshot,
            &[],
            &[L2BookLevel::new(0.20, 100.0), L2BookLevel::new(0.21, 50.0)],
        ),
        bu(2_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.20, 60.0)]),
        bu(3_000, L2BookUpdateKind::Delta, &[], &[L2BookLevel::new(0.22, 5.0)]),
        bu(
            4_000,
            L2BookUpdateKind::Snapshot,
            &[],
            &[
                L2BookLevel::new(0.20, 60.0),
                L2BookLevel::new(0.21, 50.0),
                L2BookLevel::new(0.22, 5.0),
            ],
        ),
    ];
    let complete = numbered(complete);
    let ok = verify_checkpoints(&complete, &[], &[]);
    assert_eq!((ok.checked, ok.exact, ok.level_mismatches), (1, 1, 0));

    let mut dropped = complete.clone();
    dropped.remove(1); // the 0.20 → 60 delta never reached the archive
    let bad = verify_checkpoints(&numbered(dropped), &[], &[]);
    assert_eq!((bad.checked, bad.exact), (1, 0), "a dropped delta must not verify");
    assert_eq!(bad.level_mismatches, 1, "exactly the level whose size went stale");

    let mut holed = complete.clone();
    holed.remove(1); // ...a recorded seq hole (0, 2, 3): the chain is broken, nothing to check against
    let reseeded = verify_checkpoints(&holed, &[], &[]);
    assert_eq!(reseeded.checked, 0, "the snapshot after a hole is a reseed");
}
