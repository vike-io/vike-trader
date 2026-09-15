//! Order-to-trade ratio (OTR) — quoting-intensity study inferred from consecutive book
//! snapshots (VisualHFT-style mechanism; independent implementation). Without an
//! order-by-order feed, messaging activity is APPROXIMATED by diffing the visible top-N
//! level lists of consecutive snapshots, per side: a price present only in the new list
//! is an **add**, present in both with a different qty an **update**, present only in
//! the old list a **cancel**. A level scrolling out of the top-N window (pushed off by a
//! better price) counts as a cancel by construction — this is a visible-window proxy,
//! not exchange message truth; document-level contract, not a bug.
//!
//! Per fixed EVENT-TIME window (default 100 ms, configurable):
//! `OTR = (adds + 2·updates + cancels) / max(trades, 1) − 1` — an update is one
//! cancel+replace (two messages); 0 means one message per trade above baseline parity.
//!
//! Streaming struct fed `(ts, book_snapshot, trade_count)`. Windows are event-anchored:
//! the first push after a close starts the next window at its own ts (idle gaps don't
//! emit empty windows). A push whose ts reaches `start + window_ms` FIRST closes the
//! window (its own diff opens the next one). `forming()`/`last()` mirror the crate's
//! forming/committed convention.
use std::collections::BTreeMap;

use vike_model::{L2Book, Level};

#[derive(Clone, Copy, Debug)]
pub struct OtrConfig {
    /// levels per side in the visible diff window
    pub depth: usize,
    /// event-time window length (ms)
    pub window_ms: i64,
}

impl Default for OtrConfig {
    fn default() -> Self {
        OtrConfig { depth: 10, window_ms: 100 }
    }
}

/// One committed (or forming — see [`OtrTracker::forming`]) window of message counts.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OtrSample {
    pub window_start: i64,
    /// the scheduled close boundary, `window_start + window_ms`
    pub window_end: i64,
    pub adds: u64,
    pub updates: u64,
    pub cancels: u64,
    pub trades: u64,
    /// (adds + 2·updates + cancels)/max(trades, 1) − 1
    pub otr: f64,
}

pub struct OtrTracker {
    cfg: OtrConfig,
    /// previous visible top-N (bids, asks) — the diff baseline
    prev: Option<(Vec<Level>, Vec<Level>)>,
    window_start: Option<i64>,
    adds: u64,
    updates: u64,
    cancels: u64,
    trades: u64,
    last: Option<OtrSample>,
}

impl OtrTracker {
    pub fn new(cfg: OtrConfig) -> Self {
        assert!(cfg.depth >= 1, "OtrConfig depth must be >= 1");
        assert!(cfg.window_ms >= 1, "OtrConfig window_ms must be >= 1");
        OtrTracker {
            cfg,
            prev: None,
            window_start: None,
            adds: 0,
            updates: 0,
            cancels: 0,
            trades: 0,
            last: None,
        }
    }

    /// Fold one observation: `ts` (event time, ms), the book to snapshot, and how many
    /// trades printed since the previous push. Returns the committed sample when this
    /// push crossed the window boundary (the push's own diff lands in the NEXT window).
    pub fn push(&mut self, ts: i64, book: &L2Book, trade_count: u32) -> Option<OtrSample> {
        let mut emitted = None;
        match self.window_start {
            None => self.window_start = Some(ts),
            Some(start) if ts >= start + self.cfg.window_ms => {
                emitted = Some(self.close_window(start));
                self.window_start = Some(ts);
            }
            Some(_) => {}
        }
        let cur = book.top_n(self.cfg.depth);
        if let Some((pb, pa)) = &self.prev {
            let (a, u, c) = diff_counts(pb, &cur.0);
            self.adds += a;
            self.updates += u;
            self.cancels += c;
            let (a, u, c) = diff_counts(pa, &cur.1);
            self.adds += a;
            self.updates += u;
            self.cancels += c;
        }
        self.prev = Some(cur);
        self.trades += u64::from(trade_count);
        emitted
    }

    fn close_window(&mut self, start: i64) -> OtrSample {
        let msgs = (self.adds + 2 * self.updates + self.cancels) as f64;
        let s = OtrSample {
            window_start: start,
            window_end: start + self.cfg.window_ms,
            adds: self.adds,
            updates: self.updates,
            cancels: self.cancels,
            trades: self.trades,
            otr: msgs / self.trades.max(1) as f64 - 1.0,
        };
        self.adds = 0;
        self.updates = 0;
        self.cancels = 0;
        self.trades = 0;
        self.last = Some(s);
        s
    }

    /// The current OPEN window's counts + interim OTR. Pure read; None before any push.
    pub fn forming(&self) -> Option<OtrSample> {
        let start = self.window_start?;
        let msgs = (self.adds + 2 * self.updates + self.cancels) as f64;
        Some(OtrSample {
            window_start: start,
            window_end: start + self.cfg.window_ms,
            adds: self.adds,
            updates: self.updates,
            cancels: self.cancels,
            trades: self.trades,
            otr: msgs / self.trades.max(1) as f64 - 1.0,
        })
    }

    /// Last committed window. Pure read.
    pub fn last(&self) -> Option<OtrSample> {
        self.last
    }
}

/// (adds, updates, cancels) between two same-side visible level lists. Levels are keyed
/// by exact price bits — both lists come off the SAME integer-tick grid
/// (`L2Book::top_n` prices are `tick · tick_size`), so equal price ⇒ equal bits.
fn diff_counts(old: &[Level], new: &[Level]) -> (u64, u64, u64) {
    let old_map: BTreeMap<u64, f64> = old.iter().map(|&(p, q)| (p.to_bits(), q)).collect();
    let (mut adds, mut updates) = (0u64, 0u64);
    for &(p, q) in new {
        match old_map.get(&p.to_bits()) {
            None => adds += 1,
            Some(&oq) => {
                if oq != q {
                    updates += 1;
                }
            }
        }
    }
    let mut cancels = 0u64;
    for k in old_map.keys() {
        if !new.iter().any(|&(p, _)| p.to_bits() == *k) {
            cancels += 1;
        }
    }
    (adds, updates, cancels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(seq: u64, bids: &[Level], asks: &[Level]) -> L2Book {
        let mut b = L2Book::new(0.5);
        b.apply_snapshot(seq, bids, asks);
        b
    }

    #[test]
    fn diff_counts_add_update_cancel() {
        let old = [(99.0, 10.0), (98.0, 5.0)];
        let new = [(99.5, 2.0), (99.0, 7.0)]; // 99.5 add, 99.0 qty change, 98.0 gone
        assert_eq!(diff_counts(&old, &new), (1, 1, 1));
        assert_eq!(diff_counts(&old, &old), (0, 0, 0)); // identical → nothing
        assert_eq!(diff_counts(&[], &old), (2, 0, 0)); // from empty → all adds
        assert_eq!(diff_counts(&old, &[]), (0, 0, 2)); // to empty → all cancels
    }

    // Scripted window: baseline at t=0; at t=50 one bid add + one ask update (3 trades);
    // t=100 crosses the boundary → sample {adds:1, updates:1, cancels:0, trades:3},
    // otr = (1 + 2·1 + 0)/3 − 1 = 0. The t=100 diff (one bid cancel) opens window 2.
    #[test]
    fn otr_window_expected_values() {
        let cfg = OtrConfig { depth: 10, window_ms: 100 };
        let mut t = OtrTracker::new(cfg);
        assert_eq!(t.forming(), None);
        let b0 = book(1, &[(99.0, 10.0)], &[(101.0, 10.0)]);
        assert_eq!(t.push(0, &b0, 0), None); // baseline: no prev to diff
        let b1 = book(2, &[(99.0, 10.0), (98.5, 4.0)], &[(101.0, 6.0)]);
        assert_eq!(t.push(50, &b1, 3), None);
        let f = t.forming().unwrap();
        assert_eq!((f.adds, f.updates, f.cancels, f.trades), (1, 1, 0, 3));
        assert_eq!(f.otr, 0.0);
        let b2 = book(3, &[(99.0, 10.0)], &[(101.0, 6.0)]); // 98.5 bid pulled
        let s = t.push(100, &b2, 0).unwrap(); // ts == start+window → closes FIRST
        assert_eq!(s, t.last().unwrap());
        assert_eq!(
            s,
            OtrSample {
                window_start: 0,
                window_end: 100,
                adds: 1,
                updates: 1,
                cancels: 0,
                trades: 3,
                otr: 0.0
            }
        );
        // the boundary push's own diff went into window 2:
        let f2 = t.forming().unwrap();
        assert_eq!(
            (f2.window_start, f2.adds, f2.updates, f2.cancels, f2.trades),
            (100, 0, 0, 1, 0)
        );
        assert_eq!(f2.otr, 0.0); // 1 msg / max(0,1)=1 − 1 = 0
    }

    #[test]
    fn otr_boundary_is_half_open() {
        let mut t = OtrTracker::new(OtrConfig { depth: 5, window_ms: 100 });
        let b = book(1, &[(99.0, 1.0)], &[(101.0, 1.0)]);
        t.push(0, &b, 0);
        assert_eq!(t.push(99, &b, 1), None); // ts 99 < 100 → still window 1
        let s = t.push(100, &b, 0).unwrap(); // ts 100 ≥ 100 → closes
        assert_eq!((s.window_start, s.window_end, s.trades), (0, 100, 1));
    }

    // Idle gap: next window is anchored at the first event AFTER the close, not at the
    // old boundary — no empty windows are synthesized.
    #[test]
    fn otr_windows_are_event_anchored() {
        let mut t = OtrTracker::new(OtrConfig { depth: 5, window_ms: 100 });
        let b = book(1, &[(99.0, 1.0)], &[(101.0, 1.0)]);
        t.push(0, &b, 0);
        let s = t.push(730, &b, 0).unwrap(); // one close, however long the gap
        assert_eq!((s.window_start, s.window_end), (0, 100));
        assert_eq!(t.forming().unwrap().window_start, 730);
    }

    // A better price pushing a level off the visible top-N counts as a cancel (window
    // proxy semantics), plus the add of the new level.
    #[test]
    fn scroll_out_of_depth_counts_as_cancel() {
        let mut t = OtrTracker::new(OtrConfig { depth: 2, window_ms: 1000 });
        let b0 = book(1, &[(99.0, 1.0), (98.5, 1.0)], &[(101.0, 1.0)]);
        t.push(0, &b0, 0);
        // 99.5 arrives → visible bids become [99.5, 99.0]; 98.5 scrolls out
        let b1 = book(2, &[(99.5, 2.0), (99.0, 1.0), (98.5, 1.0)], &[(101.0, 1.0)]);
        t.push(10, &b1, 0);
        let f = t.forming().unwrap();
        assert_eq!((f.adds, f.updates, f.cancels), (1, 0, 1));
    }

    // trades=0 floors to 1 in the denominator: 5 msgs (2 adds + 2·1 update + 1 cancel)
    // / 1 − 1 = 4.
    #[test]
    fn otr_trade_floor() {
        let mut t = OtrTracker::new(OtrConfig { depth: 5, window_ms: 100 });
        t.push(0, &book(1, &[(99.0, 1.0)], &[(101.0, 1.0)]), 0);
        // bids: add 98.5, update 99; asks: 101 → 101.5 (add + cancel)
        let b1 = book(2, &[(99.0, 3.0), (98.5, 1.0)], &[(101.5, 1.0)]);
        t.push(10, &b1, 0);
        let s = t.push(200, &b1, 0).unwrap();
        assert_eq!((s.adds, s.updates, s.cancels, s.trades), (2, 1, 1, 0));
        assert_eq!(s.otr, (2.0 + 2.0 + 1.0) / 1.0 - 1.0); // = 4.0
    }
}
