//! R8 L2 book gates: snapshot/delta folding, best-bid/ask, mid/spread/imbalance, zero-qty removal.

use super::*;

#[test]
fn qty_at_reads_exact_levels_and_zero_when_absent() {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(
        1,
        &[BookLevel::new(0.45, 100.0), BookLevel::new(0.44, 20.0)],
        &[BookLevel::new(0.46, 50.0)],
    );
    assert_eq!(b.bid_qty_at(0.45), 100.0);
    assert_eq!(b.bid_qty_at(0.44), 20.0);
    assert_eq!(b.ask_qty_at(0.46), 50.0);
    // absent levels (either side) read 0.0
    assert_eq!(b.bid_qty_at(0.43), 0.0);
    assert_eq!(b.ask_qty_at(0.47), 0.0);
    assert_eq!(b.bid_qty_at(0.46), 0.0); // wrong side reads 0 too
    // a qty-0 delta removes the level → 0.0
    b.apply_delta(2, &[BookLevel::new(0.45, 0.0)], &[]);
    assert_eq!(b.bid_qty_at(0.45), 0.0);
}

#[test]
fn snapshot_then_deltas() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(
        10,
        &[BookLevel::new(100.0, 3.0), BookLevel::new(99.5, 5.0), BookLevel::new(99.0, 2.0)],
        &[BookLevel::new(100.5, 4.0), BookLevel::new(101.0, 1.0), BookLevel::new(101.5, 6.0)],
    );
    assert_eq!(book.best_bid(), Some(BookLevel::new(100.0, 3.0)));
    assert_eq!(book.best_ask(), Some(BookLevel::new(100.5, 4.0)));
    assert_eq!(book.mid(), Some(100.25));
    assert_eq!(book.spread(), Some(0.5));
    assert_eq!(book.bid_levels(), 3);
    assert_eq!(book.ask_levels(), 3);

    // top-of-book imbalance: (3 - 4) / (3 + 4)
    assert_eq!(book.imbalance(), Some((3.0 - 4.0) / (3.0 + 4.0)));

    // delta: improve the bid, remove a level (qty 0), add a crossing ask (on-grid)
    assert!(book.apply_delta(
        11,
        &[BookLevel::new(100.5, 2.0), BookLevel::new(99.0, 0.0)],
        &[BookLevel::new(100.0, 1.5)]
    ));
    assert_eq!(book.best_bid(), Some(BookLevel::new(100.5, 2.0)));
    assert_eq!(book.best_ask(), Some(BookLevel::new(100.0, 1.5))); // crossed book is allowed at L2
    assert_eq!(book.bid_levels(), 3); // added 100.5, removed 99.0 (net 0)
    assert_eq!(book.ask_levels(), 4);

    // stale seq dropped
    assert!(!book.apply_delta(11, &[BookLevel::new(50.0, 9.0)], &[]));
    assert!(!book.apply_delta(5, &[BookLevel::new(50.0, 9.0)], &[]));
    assert_eq!(book.best_bid(), Some(BookLevel::new(100.5, 2.0))); // unchanged

    // seq==0 (venue with no seq) always applies
    assert!(book.apply_delta(0, &[BookLevel::new(102.0, 7.0)], &[]));
    assert_eq!(book.best_bid(), Some(BookLevel::new(102.0, 7.0)));
}

#[test]
fn empty_book_reads_none() {
    let book = L2Book::new(0.01);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
    assert_eq!(book.mid(), None);
    assert_eq!(book.spread(), None);
    assert_eq!(book.imbalance(), None);
}

#[test]
fn top_n_ordering() {
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(
        1,
        &[
            BookLevel::new(100.0, 1.0),
            BookLevel::new(98.0, 2.0),
            BookLevel::new(99.0, 3.0),
            BookLevel::new(97.0, 4.0),
        ],
        &[BookLevel::new(103.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    let (bids, asks) = book.top_n(2);
    assert_eq!(bids, vec![BookLevel::new(100.0, 1.0), BookLevel::new(99.0, 3.0)]); // high → low
    assert_eq!(asks, vec![BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)]); // low → high
}

#[test]
fn snapshot_clears_prior() {
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(1, &[BookLevel::new(100.0, 5.0)], &[BookLevel::new(101.0, 5.0)]);
    book.apply_snapshot(2, &[BookLevel::new(200.0, 1.0)], &[BookLevel::new(201.0, 1.0)]);
    assert_eq!(book.bid_levels(), 1);
    assert_eq!(book.best_bid(), Some(BookLevel::new(200.0, 1.0)));
}
