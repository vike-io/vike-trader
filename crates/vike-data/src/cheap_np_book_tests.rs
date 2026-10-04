use super::*;

fn bu(ts: i64, kind: BookUpdateKind, bids: &[BookLevel], asks: &[BookLevel]) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts,
        seq: 0,
        kind,
        tick_size: 0.01,
        bids: bids.to_vec(),
        asks: asks.to_vec(),
        symbol: "TOK".into(),
    }
}

/// The recorded `seq` restarts every pmxt hour file; folding must NOT drop the post-restart
/// deltas. This is the one bug that would silently produce a frozen (and far too thin) book.
#[test]
fn a_recorded_seq_restart_does_not_drop_later_deltas() {
    let mut updates = vec![
        bu(
            1_000,
            BookUpdateKind::Snapshot,
            &[BookLevel::new(0.19, 10.0)],
            &[BookLevel::new(0.20, 100.0)],
        ),
        bu(2_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.21, 50.0)]),
    ];
    // hour boundary: the mapper's per-file counter restarts at 0
    updates[0].seq = 900;
    updates[1].seq = 901;
    updates.push(bu(3_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.22, 70.0)]));
    updates.last_mut().unwrap().seq = 0;

    let f = fold_until(&updates, &[], &[], 4_000, false);
    assert!(f.anchored);
    assert_eq!(f.applied, 3, "the seq-0 delta after the restart must still fold");
    assert_eq!(f.book.ask_qty_at(0.22), 70.0);
    assert_eq!(f.book.quantity_for_price(1, 0.22), 220.0);
}

#[test]
fn strict_cutoff_excludes_the_print_ts_and_inclusive_includes_it() {
    let updates = vec![
        bu(1_000, BookUpdateKind::Snapshot, &[], &[BookLevel::new(0.20, 100.0)]),
        // the entry's own block eats the level
        bu(2_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.20, 0.0)]),
    ];
    assert_eq!(fold_until(&updates, &[], &[], 2_000, true).book.ask_qty_at(0.20), 100.0);
    assert_eq!(fold_until(&updates, &[], &[], 2_000, false).book.ask_qty_at(0.20), 0.0);
}

/// The ghost repair: a level the venue's own best ask says is gone must not survive into any
/// depth reading, and a level at or above it must be untouched.
#[test]
fn ghost_asks_below_the_venue_best_ask_are_pruned() {
    let mut b = L2Book::new(PRICE_GRID);
    b.apply_snapshot(
        1,
        &[],
        &[BookLevel::new(0.20, 100.0), BookLevel::new(0.22, 50.0), BookLevel::new(0.25, 70.0)],
    );
    assert_eq!(prune_ghost_asks(&mut b, 0.22), 1, "only the 0.20 level is below 0.22");
    assert_eq!(b.best_ask(), Some(BookLevel::new(0.22, 50.0)));
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
        BookUpdateKind::Snapshot,
        &[],
        &[BookLevel::new(0.20, 100.0), BookLevel::new(0.25, 40.0)],
    )];
    let no_l1 = fold_until(&updates, &[], &[], 2_000, false);
    assert_eq!(
        no_l1.book.best_ask(),
        Some(BookLevel::new(0.20, 100.0)),
        "unrepaired: the ghost survives"
    );

    let l1 = vec![QuoteTick {
        ts: 1_000,
        local_ts: 1_000,
        bid: 0.19,
        ask: 0.25,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: "TOK".into(),
    }];
    let repaired = fold_until(&updates, &l1, &[], 2_000, false);
    assert_eq!(repaired.book.best_ask(), Some(BookLevel::new(0.25, 40.0)));
    assert_eq!(repaired.pruned, 1);
    assert_eq!(repaired.book.quantity_for_price(1, 1.0), 40.0, "phantom depth is gone");
}

/// The tape deduction: a taker buy eats the level it printed at, never goes negative, and never
/// invents liquidity at a price the book does not show.
#[test]
fn a_taker_buy_deducts_from_the_ask_level_it_printed_at() {
    let mut b = L2Book::new(PRICE_GRID);
    b.apply_snapshot(1, &[], &[BookLevel::new(0.20, 100.0), BookLevel::new(0.21, 50.0)]);
    consume_ask(&mut b, 0.20, 30.0);
    assert_eq!(b.ask_qty_at(0.20), 70.0);
    // an oversized print empties the level rather than going negative
    consume_ask(&mut b, 0.20, 999.0);
    assert_eq!(b.ask_qty_at(0.20), 0.0);
    // `price_of` reconstitutes a level price as `tick * PRICE_GRID`, so 0.21 comes back as
    // 0.21000000000000002 — compared with a tolerance rather than pinned to that artefact.
    let BookLevel { price: px, qty } = b.best_ask().expect("the 0.21 level is now the top of book");
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
    let updates = vec![bu(1_000, BookUpdateKind::Snapshot, &[], &[BookLevel::new(0.20, 100.0)])];
    let trade = |ts: i64, buyer_maker: bool| TradeTick {
        ts,
        local_ts: ts,
        price: 0.20,
        size: 40.0,
        is_buyer_maker: buyer_maker,
        symbol: "TOK".into(),
    };
    // the snapshot is folded first, then the same-ts trade is applied on the NEXT update
    let updates2 = {
        let mut v = updates.clone();
        v.push(bu(2_000, BookUpdateKind::Delta, &[], &[]));
        v
    };
    let taker_buy = fold_until(&updates2, &[], &[trade(1_500, false)], 3_000, false);
    assert_eq!(taker_buy.book.ask_qty_at(0.20), 60.0);
    assert_eq!(taker_buy.consumed, 40.0);

    let taker_sell = fold_until(&updates2, &[], &[trade(1_500, true)], 3_000, false);
    assert_eq!(taker_sell.book.ask_qty_at(0.20), 100.0, "a taker sell hits the bid");
    assert_eq!(taker_sell.consumed, 0.0);
}

#[test]
fn deltas_without_a_snapshot_are_not_anchored_and_yield_no_ladder() {
    let updates = vec![bu(1_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.20, 100.0)])];
    let f = fold_until(&updates, &[], &[], 9_999, false);
    assert!(!f.anchored, "a book built from deltas alone is not measurable");
    assert_eq!(f.applied, 1);
    assert_eq!(ask_ladder_at(&updates, &[], &[], 9_999, false), None);
}

#[test]
fn a_gap_marker_invalidates_the_anchor_until_the_next_snapshot() {
    let updates = vec![
        bu(1_000, BookUpdateKind::Snapshot, &[], &[BookLevel::new(0.20, 100.0)]),
        bu(2_000, BookUpdateKind::GapStart, &[], &[]),
        bu(3_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.21, 5.0)]),
    ];
    assert!(!fold_until(&updates, &[], &[], 3_500, false).anchored);
    assert_eq!(ask_ladder_at(&updates, &[], &[], 3_500, false), None);
    let mut recovered = updates.clone();
    recovered.push(bu(4_000, BookUpdateKind::Snapshot, &[], &[BookLevel::new(0.20, 7.0)]));
    assert!(fold_until(&recovered, &[], &[], 4_500, false).anchored);
    assert!(ask_ladder_at(&recovered, &[], &[], 4_500, false).is_some());
}

/// The integrity gate must PASS on a complete delta stream and FAIL on one with a row removed —
/// otherwise a green report would be worth nothing.
#[test]
fn checkpoint_verification_catches_a_dropped_delta() {
    let complete = vec![
        bu(
            1_000,
            BookUpdateKind::Snapshot,
            &[],
            &[BookLevel::new(0.20, 100.0), BookLevel::new(0.21, 50.0)],
        ),
        bu(2_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.20, 60.0)]),
        bu(3_000, BookUpdateKind::Delta, &[], &[BookLevel::new(0.22, 5.0)]),
        bu(
            4_000,
            BookUpdateKind::Snapshot,
            &[],
            &[BookLevel::new(0.20, 60.0), BookLevel::new(0.21, 50.0), BookLevel::new(0.22, 5.0)],
        ),
    ];
    let ok = verify_checkpoints(&complete, &[], &[]);
    assert_eq!((ok.checked, ok.exact, ok.level_mismatches), (1, 1, 0));

    let mut dropped = complete.clone();
    dropped.remove(1); // the 0.20 → 60 delta never arrives
    let bad = verify_checkpoints(&dropped, &[], &[]);
    assert_eq!((bad.checked, bad.exact), (1, 0), "a dropped delta must not verify");
    assert_eq!(bad.level_mismatches, 1, "exactly the level whose size went stale");
}
