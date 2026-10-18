//! The recent-submit set: `Submit` is deduplicated by client order id inside one account.
//!
//! The coid is minted by the tradehub's core (the pre-minted rule), so every retry of an order names the
//! same id, and a second `Submit` carrying an id the account has already seen is a retry, not a new
//! order: it is acknowledged (`CmdAck`, the transport ack) and NOT sent to the venue again.
//!
//! # What it is not
//!
//! * **Not durable.** The set lives in the adapter's dispatch thread; a gatewayhub restart empties it
//!   (the wave-2 plan's B.7 records the residual: after a restart the only protection against a double
//!   send is that nobody re-sends, and the venue's own uniqueness of the client id where it enforces one).
//! * **Bounded.** The oldest id is forgotten past the bound. An id forgotten and re-submitted is sent: a
//!   retry that old is a new decision of the core's, not a transport retry.
//! * Cancels and modifies are not in it: they carry absolute terms and are idempotent by content.

use std::collections::{HashSet, VecDeque};

/// A bounded set of client order ids, forgetting the oldest first.
#[derive(Debug)]
pub struct DedupSet {
    cap: usize,
    order: VecDeque<String>,
    seen: HashSet<String>,
}

impl DedupSet {
    /// An empty set holding at most `cap` ids (at least one).
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), order: VecDeque::new(), seen: HashSet::new() }
    }

    /// Remember `coid`. `true` = it was NEW (send the order), `false` = already seen (a duplicate).
    pub fn first_sight(&mut self, coid: &str) -> bool {
        if self.seen.contains(coid) {
            return false;
        }
        self.seen.insert(coid.to_string());
        self.order.push_back(coid.to_string());
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// Whether `coid` is remembered.
    pub fn contains(&self, coid: &str) -> bool {
        self.seen.contains(coid)
    }

    /// How many ids are remembered.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_sight_of_a_coid_is_a_duplicate() {
        let mut set = DedupSet::new(8);
        assert!(set.is_empty());
        assert!(set.first_sight("a"));
        assert!(!set.first_sight("a"));
        assert!(set.first_sight("b"));
        assert!(set.contains("a") && set.contains("b") && !set.contains("c"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn the_oldest_id_is_forgotten_past_the_bound_and_may_be_sent_again() {
        let mut set = DedupSet::new(2);
        assert!(set.first_sight("a"));
        assert!(set.first_sight("b"));
        assert!(set.first_sight("c"));
        assert_eq!(set.len(), 2);
        assert!(!set.contains("a"));
        // `b` and `c` are still duplicates; `a` is new again.
        assert!(!set.first_sight("b"));
        assert!(!set.first_sight("c"));
        assert!(set.first_sight("a"));
    }

    #[test]
    fn a_bound_of_zero_still_holds_one_id() {
        let mut set = DedupSet::new(0);
        assert!(set.first_sight("a"));
        assert!(!set.first_sight("a"));
    }
}
