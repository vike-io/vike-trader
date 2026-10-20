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

use vike_marketdata::{BookLevel, L2Book};

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
    prev: Option<(Vec<BookLevel>, Vec<BookLevel>)>,
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
fn diff_counts(old: &[BookLevel], new: &[BookLevel]) -> (u64, u64, u64) {
    let old_map: BTreeMap<u64, f64> =
        old.iter().map(|&BookLevel { price: p, qty: q }| (p.to_bits(), q)).collect();
    let (mut adds, mut updates) = (0u64, 0u64);
    for &BookLevel { price: p, qty: q } in new {
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
        if !new.iter().any(|&BookLevel { price: p, .. }| p.to_bits() == *k) {
            cancels += 1;
        }
    }
    (adds, updates, cancels)
}

#[path = "otr_tests.rs"]
#[cfg(test)]
mod otr_tests;
