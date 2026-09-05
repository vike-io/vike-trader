//! Tick::Book fold through run_ticks: anchor/delta/gap integrity + on_order_book delivery.
//!
//! Task 8: the engine folds recorded `BookUpdate` events into `Strategy::on_order_book` with
//! the replayed gap-sentinel rule (Snapshot re-anchors; Delta applies only at
//! `seq == last_seq + 1`; §B markers / seq breaks drop the book until the next Snapshot).

use std::cell::RefCell;
use std::rc::Rc;

use vike_backtest::engine::{EngineParams, SimBroker, StrategyEngine, Tick};
use vike_model::{BookUpdate, BookUpdateKind, L2Book, Strategy};

/// A `#[cfg(test)]`-style strategy that records the top-of-book `(best_bid_px, best_ask_px)`
/// at every `on_order_book` delivery into a shared cell. `Rc<RefCell<_>>` (not `Arc<Mutex<_>>`)
/// mirrors the crate's other single-threaded strategy-spy tests (see `hist_replay.rs`): the
/// engine takes the strategy BY VALUE, so the observations are read back through a retained
/// `Rc` clone of the same cell after `run_ticks` returns.
#[derive(Clone, Default)]
struct BookSpy {
    tops: Rc<RefCell<Vec<(f64, f64)>>>,
}

impl Strategy<SimBroker> for BookSpy {
    fn on_order_book(&mut self, _b: &mut SimBroker, book: &L2Book) {
        if let (Some((bb, _)), Some((ba, _))) = (book.best_bid(), book.best_ask()) {
            self.tops.borrow_mut().push((bb, ba));
        }
    }
}

/// Top-of-book prices survive a tick-quantization round-trip (`price → i64 tick → price`), so
/// compare within a tick-scale epsilon rather than by exact bits.
fn top_eq(got: (f64, f64), want: (f64, f64)) -> bool {
    (got.0 - want.0).abs() < 1e-9 && (got.1 - want.1).abs() < 1e-9
}

fn bu(
    ts: i64,
    seq: u64,
    kind: BookUpdateKind,
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
) -> Tick {
    Tick::Book(BookUpdate {
        ts,
        local_ts: 0,
        seq,
        kind,
        tick_size: 0.01,
        bids,
        asks,
        symbol: "TOK".to_string(),
    })
}

#[test]
fn book_fold_applies_anchors_deltas_and_drops_on_gap() {
    let ticks = vec![
        bu(1, 1, BookUpdateKind::Snapshot, vec![(0.45, 10.0)], vec![(0.47, 5.0)]),
        bu(2, 2, BookUpdateKind::Delta, vec![(0.46, 3.0)], vec![]), // best bid moves to 0.46
        bu(3, 4, BookUpdateKind::Delta, vec![(0.48, 1.0)], vec![]), // seq GAP (3 missing) → dropped
        bu(4, 5, BookUpdateKind::Delta, vec![(0.49, 1.0)], vec![]), // still no book → ignored
        bu(5, 6, BookUpdateKind::Snapshot, vec![(0.44, 2.0)], vec![(0.50, 2.0)]), // re-anchor
    ];
    let strategy = BookSpy::default();
    let tops = Rc::clone(&strategy.tops);
    let mut eng = StrategyEngine::new(
        vec![("TOK".to_string(), Vec::new())],
        strategy,
        EngineParams::default(),
    );
    let result = eng.run_ticks(&[("TOK".to_string(), ticks)]);

    // equity curve untouched by book events (no price ticks at all in this run):
    assert!(result.equity_curve.is_empty());
    assert_eq!(result.equity_ts.len(), 0);

    // Deliveries: anchor top (0.45/0.47), delta top (best bid → 0.46), then NOTHING for the
    // gapped delta (seq 4 ≠ 2+1 → drop) or the orphan delta (seq 5 with no book), then the
    // re-anchor top (0.44/0.50). Exactly three deliveries, in that order.
    let seen = tops.borrow();
    assert_eq!(seen.len(), 3, "expected anchor + in-seq delta + re-anchor, got {seen:?}");
    assert!(top_eq(seen[0], (0.45, 0.47))); // anchor
    assert!(top_eq(seen[1], (0.46, 0.47))); // in-sequence delta raised the best bid
    assert!(top_eq(seen[2], (0.44, 0.50))); // re-anchor after the gap
}

#[test]
fn gap_start_marker_drops_book_until_snapshot() {
    let ticks = vec![
        bu(1, 1, BookUpdateKind::Snapshot, vec![(0.45, 10.0)], vec![(0.47, 5.0)]),
        bu(2, 0, BookUpdateKind::GapStart, vec![], vec![]),
        bu(3, 2, BookUpdateKind::Delta, vec![(0.46, 3.0)], vec![]), // after gap marker → ignored
        bu(4, 0, BookUpdateKind::LiveResume, vec![], vec![]),
        bu(5, 3, BookUpdateKind::Snapshot, vec![(0.40, 1.0)], vec![(0.60, 1.0)]),
    ];
    let strategy = BookSpy::default();
    let tops = Rc::clone(&strategy.tops);
    let mut eng = StrategyEngine::new(
        vec![("TOK".to_string(), Vec::new())],
        strategy,
        EngineParams::default(),
    );
    let result = eng.run_ticks(&[("TOK".to_string(), ticks)]);

    assert!(result.equity_curve.is_empty());

    // Exactly 2 deliveries: the two Snapshots. The GapStart marker drops the book, so the
    // Delta at ts=3 (even though its seq would be contiguous with the pre-gap snapshot) is
    // suppressed; LiveResume is a no-op and the re-seed Snapshot re-anchors.
    let seen = tops.borrow();
    assert_eq!(seen.len(), 2, "GapStart must suppress the intervening delta, got {seen:?}");
    assert!(top_eq(seen[0], (0.45, 0.47))); // first snapshot
    assert!(top_eq(seen[1], (0.40, 0.60))); // re-seed snapshot after LiveResume
}
