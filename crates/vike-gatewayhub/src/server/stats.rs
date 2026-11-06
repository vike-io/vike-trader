//! The daemon's live counters: what `GwStatus` reports and what the caps are counted against.
//!
//! Names and counts only, never a value (`GwStatusReport`'s own rule). Every count that bounds
//! something (connections, stream sessions, options feeds) is a [`Counter`] whose [`Slot`] is released
//! by `Drop`, so a connection thread that PANICS gives its place back: a plain `fetch_sub` at the end
//! of the thread body would leak it past an unwind, and a leaked slot is a cap that closes for good.
//! (`crates/vike-datahub/src/server/serve.rs`'s `ConnSlot`, generalised.)

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use vike_gatewayhub_client::proto::{BudgetCounter, GwStatusReport};
use vike_hub_proto::auth::fresh_nonce;

/// A count with a release-on-drop slot.
#[derive(Debug, Default)]
pub struct Counter(AtomicUsize);

impl Counter {
    /// How many slots are held.
    pub fn get(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    /// Take a slot unconditionally. The caller compares [`Counter::get`] with its cap and drops the
    /// slot to refuse (the connection cap's shape: reserve BEFORE spawning, so a burst of accepts
    /// cannot race the count past the cap).
    pub fn enter(self: &Arc<Self>) -> Slot {
        self.0.fetch_add(1, Ordering::AcqRel);
        Slot(Arc::clone(self))
    }

    /// Take a slot unless `cap` are already held.
    pub fn try_enter(self: &Arc<Self>, cap: usize) -> Option<Slot> {
        let before = self.0.fetch_add(1, Ordering::AcqRel);
        if before >= cap {
            self.0.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(Arc::clone(self)))
    }
}

/// One held place in a [`Counter`]; released on drop.
#[derive(Debug)]
pub struct Slot(Arc<Counter>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The daemon's counters.
#[derive(Debug)]
pub struct Stats {
    /// A random id minted per process start: a client that sees it change across a reconnect knows
    /// the daemon restarted (every session it held is gone, not merely the link).
    pub epoch: u64,
    started: Instant,
    started_ms: i64,
    /// Accepted connections, authenticated or not.
    pub connections: Arc<Counter>,
    /// Open `stream` sessions.
    pub stream_sessions: Arc<Counter>,
    /// Open options `feed` sockets.
    pub options_feeds: Arc<Counter>,
    /// Fetches running right now.
    pub fetches_running: Arc<Counter>,
    next_session: AtomicU64,
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

impl Stats {
    /// Counters for a daemon starting now.
    pub fn new() -> Self {
        let nonce = fresh_nonce();
        let mut word = [0u8; 8];
        word.copy_from_slice(&nonce[..8]);
        Self {
            epoch: u64::from_be_bytes(word),
            started: Instant::now(),
            started_ms: vike_model::now_ms(),
            connections: Arc::new(Counter::default()),
            stream_sessions: Arc::new(Counter::default()),
            options_feeds: Arc::new(Counter::default()),
            fetches_running: Arc::new(Counter::default()),
            next_session: AtomicU64::new(1),
        }
    }

    /// The next session id: unique per process, starting at 1.
    pub fn next_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// The `GwStatus` answer for a daemon serving `md` and `cd`, with the lane's `budget` rows.
    pub fn report(
        &self,
        md: &[String],
        cd: &[String],
        budget: Vec<BudgetCounter>,
    ) -> GwStatusReport {
        let count = |c: &Counter| u32::try_from(c.get()).unwrap_or(u32::MAX);
        GwStatusReport {
            epoch: self.epoch,
            started_ms: self.started_ms,
            uptime_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            connections: count(&self.connections),
            stream_sessions: count(&self.stream_sessions),
            fetches_running: count(&self.fetches_running),
            md_venues: md.to_vec(),
            cd_venues: cd.to_vec(),
            budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_is_released_on_drop_even_through_a_panic() {
        let counter = Arc::new(Counter::default());
        let slot = counter.enter();
        assert_eq!(counter.get(), 1);
        drop(slot);
        assert_eq!(counter.get(), 0);

        let c = Arc::clone(&counter);
        let result = std::thread::Builder::new()
            .name("stats-test".into())
            .spawn(move || {
                let _slot = c.enter();
                panic!("a connection thread panicked");
            })
            .expect("spawn")
            .join();
        assert!(result.is_err());
        assert_eq!(counter.get(), 0, "an unwinding thread leaked its slot");
    }

    #[test]
    fn try_enter_refuses_at_the_cap_and_does_not_leak_the_refusal() {
        let counter = Arc::new(Counter::default());
        let a = counter.try_enter(2).expect("first");
        let b = counter.try_enter(2).expect("second");
        assert!(counter.try_enter(2).is_none(), "a third entered a cap of two");
        assert_eq!(counter.get(), 2, "the refused attempt leaked a count");
        drop(a);
        assert!(counter.try_enter(2).is_some());
        drop(b);
    }

    #[test]
    fn the_report_carries_names_and_counts_only() {
        let stats = Stats::new();
        let _conn = stats.connections.enter();
        let _session = stats.stream_sessions.enter();
        let md = vec!["binance".to_string()];
        let report = stats.report(&md, &[], Vec::new());
        assert_eq!(report.epoch, stats.epoch);
        assert_eq!((report.connections, report.stream_sessions, report.fetches_running), (1, 1, 0));
        assert_eq!(report.md_venues, md);
        assert!(report.cd_venues.is_empty() && report.budget.is_empty());
        assert!(report.started_ms > 0);
    }

    #[test]
    fn session_ids_are_unique_and_start_at_one() {
        let stats = Stats::new();
        assert_eq!(stats.next_session(), 1);
        assert_eq!(stats.next_session(), 2);
    }
}
