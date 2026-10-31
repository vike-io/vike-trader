//! **Which trade an open trade really is.** A PARTIAL close splits its trade: the old id is closed for the part, and the rest
//! comes back as a Trades row under a NEW id whose `TradeIDOrigin` names the old one (MEASURED 2026-10-26, the same open
//! order and open time). The node folded the trade under the id the OPENING fill carried, so reconcile, which reports every
//! open trade by the id the Trades table gives it, raised a `MissingFill` for a trade the core already holds. The chain's
//! first id is the trade's ROOT, and this table is how the reconcile fill report finds it.
//!
//! Two sources feed it, both in the one process that holds the node's FXCM sessions:
//!
//! * the exec thread's drain: the closing fill's envelope carries the closed row's `TradeIDRemain` (`fcshim.cpp`'s
//!   `pushFill`), so a split seen LIVE is linked at once, at any depth of chain;
//! * the reconcile client's own reads: a Trades row that names an origin links it ([`TradeRoots::link`]), so a split nobody
//!   watched (the node was down, or the exec session was another) still maps one level, and a second split of the
//!   remainder maps to the first root when the first remainder was ever seen.
//!
//! ⚠ **What it cannot do:** after a node restart the table is empty, so a remainder of a remainder reads as the first
//! remainder, which the core never folded (a `MissingFill` held under `quarantine`, as every split was before this). A
//! chain needs the venue's closed-trade history to be followed from nothing; no read of it exists here.
//!
//! Process-wide on purpose, like [`crate::reads`]'s registry: the exec session and the reconcile session are two
//! sessions of one login on two threads and neither can reach the other. Trade ids are the venue's, unique across its
//! accounts, so one table serves every account of the process.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// Links kept. A link is a remainder id and its root (about 40 bytes), and a chain only grows by a partial close, so the cap
/// is a memory bound far above what a session produces; past it the OLDEST goes (an old remainder is the least likely to
/// still be open).
const MAX_LINKS: usize = 4096;

/// Remainder trade id -> the root trade id of its chain, bounded FIFO.
#[derive(Debug, Default)]
pub(crate) struct TradeRoots {
    root: HashMap<String, String>,
    inserted: VecDeque<String>,
}

impl TradeRoots {
    /// `child` is what `parent` came back as after a partial close: from now on `child`'s root is `parent`'s. An empty id,
    /// a child that is its own parent and a link that would make a cycle are ignored.
    pub(crate) fn link(&mut self, parent: &str, child: &str) {
        if parent.is_empty() || child.is_empty() || parent == child {
            return;
        }
        let root = self.root_of(parent);
        if root == child {
            return;
        }
        if self.root.insert(child.to_string(), root).is_none() {
            self.inserted.push_back(child.to_string());
        }
        while self.inserted.len() > MAX_LINKS {
            if let Some(oldest) = self.inserted.pop_front() {
                self.root.remove(&oldest);
            }
        }
    }

    /// The root of `id`'s chain: `id` itself unless it is a remainder this table has been told about.
    pub(crate) fn root_of(&self, id: &str) -> String {
        self.root.get(id).cloned().unwrap_or_else(|| id.to_string())
    }
}

/// The process's table. A poisoned lock is read through: the table holds ids only, and a panic elsewhere must not stop
/// the reconcile pass or the fill lane from asking.
pub(crate) fn process() -> MutexGuard<'static, TradeRoots> {
    static TABLE: OnceLock<Mutex<TradeRoots>> = OnceLock::new();
    TABLE
        .get_or_init(|| Mutex::new(TradeRoots::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[path = "trade_roots_tests.rs"]
#[cfg(test)]
mod trade_roots_tests;
