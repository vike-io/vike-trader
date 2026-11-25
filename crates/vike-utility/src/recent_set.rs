//! A bounded memory of strings that forgets the OLDEST first: the one spelling of the "ids seen
//! lately" set two crates used to copy (the gatewayhub client's ended and unanswered order ids, the
//! cTrader bridge's own non-v2 order ids).
//!
//! The bound is a parameter of every [`RecentSet::insert`], so each caller keeps its own number.
//! [`RecentSet::remove`] is O(1): it forgets the id at once and leaves its slot in the age order to
//! go when its turn comes (an `insert` past the bound, or [`RecentSet::take`]).

use std::collections::{HashSet, VecDeque};

/// The ids held, and their age order (oldest first).
#[derive(Debug, Default)]
pub struct RecentSet {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl RecentSet {
    /// Hold `id`, then forget the oldest while more than `memory` slots are kept. An id already held
    /// keeps its age.
    pub fn insert(&mut self, id: &str, memory: usize) {
        if self.set.insert(id.to_string()) {
            self.order.push_back(id.to_string());
            if self.order.len() > memory
                && let Some(oldest) = self.order.pop_front()
            {
                self.set.remove(&oldest);
            }
        }
    }

    /// Whether `id` is held.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    /// Forget `id` now. Its place in the age order goes when its turn comes (`insert`, `take`), which
    /// keeps this O(1).
    pub fn remove(&mut self, id: &str) {
        self.set.remove(id);
    }

    /// Everything held, oldest first, leaving nothing.
    pub fn take(&mut self) -> Vec<String> {
        let mut order = std::mem::take(&mut self.order);
        order.retain(|id| self.set.remove(id));
        order.into()
    }
}

#[cfg(test)]
#[path = "recent_set_tests.rs"]
mod tests;
