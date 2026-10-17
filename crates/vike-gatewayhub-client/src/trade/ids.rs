//! The wire-id cache behind `ExecutionClient::wire_id_for`.
//!
//! `wire_id_for` is called per restored entry on the fold thread, at reconcile time
//! (`vike_core::runtime::restore`'s `wire_ids_of_refs`), and the contract says it is **pure, cheap and
//! never blocks or touches the network**. A hyperliquid answer is `keccak(coid)`, which a keyless
//! remote client cannot compute (the signer crates sit behind the bridge). So the answers are fetched
//! BEFORE the fold runs: the composition root knows the restored coids at boot (the ownership file and
//! the journal restore load them before `spawn_core`), asks for them in ONE `WireIds` request on the
//! boot thread, and the cache answers the fold. A coid nobody asked about is a cache MISS, which is an
//! invariant violation and is logged loudly (once) rather than answered as "the venue echoes the coid":
//! a silent `None` there would restore nothing and look healthy.
//!
//! **Until the prefetch succeeded, a reconcile is not attempted** (`RemoteReconClient` answers
//! "not ready"): the restore must never plan against an empty translation map, because the planner
//! reads an empty report as cancellation.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the boot prefetch is owed, and whether it has been paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prefetch {
    /// Nobody asked for a prefetch (a fresh account with nothing restored).
    NotRequested,
    /// Asked for, and the daemon has not answered yet (or the ask failed).
    Pending,
    /// Answered: every asked coid has an answer in the cache.
    Ready,
}

/// What the cache says about one coid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// The venue names this order by this id.
    Wire(String),
    /// Asked about, and the venue echoes the coid itself.
    Echo,
    /// Never asked about (or the prefetch is still owed): an invariant violation.
    Miss,
}

/// The wire-id cache of one lease.
#[derive(Debug)]
pub(crate) struct WireIds {
    state: Prefetch,
    wanted: Vec<String>,
    asked: HashSet<String>,
    map: HashMap<String, String>,
    /// A miss has been said for THIS lease (one `error` line per account, not per process).
    miss_said: AtomicBool,
}

impl Default for WireIds {
    fn default() -> Self {
        Self {
            state: Prefetch::NotRequested,
            wanted: Vec::new(),
            asked: HashSet::new(),
            map: HashMap::new(),
            miss_said: AtomicBool::new(false),
        }
    }
}

impl WireIds {
    /// The boot thread is about to ask for `coids`: the prefetch is owed until `filled`.
    pub fn want(&mut self, coids: &[String]) {
        self.state = Prefetch::Pending;
        self.wanted = coids.to_vec();
    }

    /// The daemon answered: `pairs` are `(coid, wire id)` for every asked coid whose wire id is not the
    /// coid itself. Every wanted coid is now ANSWERED (an absent one echoes).
    pub fn filled(&mut self, pairs: Vec<(String, String)>) {
        self.asked.extend(self.wanted.drain(..));
        self.map.extend(pairs);
        self.state = Prefetch::Ready;
    }

    /// The coids still to fetch (empty once `filled`).
    pub fn wanted(&self) -> &[String] {
        &self.wanted
    }

    /// Whether the prefetch is owed and unpaid.
    pub fn is_pending(&self) -> bool {
        self.state == Prefetch::Pending
    }

    /// The prefetch state.
    #[cfg(test)]
    pub fn state(&self) -> Prefetch {
        self.state
    }

    /// Whether this is the first miss this lease says: the caller logs it at `error`, once per ACCOUNT (a
    /// process-wide `Once` hid a second account's miss).
    pub fn first_miss(&self) -> bool {
        !self.miss_said.swap(true, Ordering::Relaxed)
    }

    /// Answer one coid from the cache.
    pub fn lookup(&self, coid: &str) -> Lookup {
        if let Some(wire) = self.map.get(coid) {
            return Lookup::Wire(wire.clone());
        }
        if self.asked.contains(coid) { Lookup::Echo } else { Lookup::Miss }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn nothing_is_answered_before_the_prefetch_and_the_gate_is_owed() {
        let mut ids = WireIds::default();
        assert_eq!(ids.state(), Prefetch::NotRequested);
        assert!(!ids.is_pending());
        ids.want(&strings(&["a", "b"]));
        assert!(ids.is_pending());
        assert_eq!(ids.wanted(), ["a", "b"]);
        assert_eq!(ids.lookup("a"), Lookup::Miss, "a cache miss is not an echo");
    }

    #[test]
    fn a_prefetch_answers_wire_ids_and_echoes_and_everything_else_is_a_miss() {
        let mut ids = WireIds::default();
        ids.want(&strings(&["hl1", "bn1"]));
        ids.filled(vec![("hl1".into(), "0xabc".into())]);
        assert!(!ids.is_pending());
        assert_eq!(ids.state(), Prefetch::Ready);
        assert!(ids.wanted().is_empty());
        assert_eq!(ids.lookup("hl1"), Lookup::Wire("0xabc".into()));
        assert_eq!(ids.lookup("bn1"), Lookup::Echo, "asked, and the venue echoes the coid");
        assert_eq!(ids.lookup("never-asked"), Lookup::Miss);
    }

    #[test]
    fn a_second_prefetch_keeps_what_the_first_learned() {
        let mut ids = WireIds::default();
        ids.want(&strings(&["a"]));
        ids.filled(vec![("a".into(), "wa".into())]);
        ids.want(&strings(&["b"]));
        assert!(ids.is_pending());
        ids.filled(vec![("b".into(), "wb".into())]);
        assert_eq!(ids.lookup("a"), Lookup::Wire("wa".into()));
        assert_eq!(ids.lookup("b"), Lookup::Wire("wb".into()));
    }
}
